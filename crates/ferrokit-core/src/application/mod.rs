// SPDX-License-Identifier: LGPL-3.0-only
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use crate::event::{Event, EventKind};
use crate::event_loop::{
    install_application_event_filter, remove_application_event_filter, DispatcherKind, EventLoop,
};
use crate::meta::MetaObject;
use crate::object::{
    query_object_thread, unregister_qobject, ObjectData, ObjectId, QObject, ThreadContext, ThreadId,
};
use crate::signal::Signal;

/// Qt application attributes matching `Qt::ApplicationAttribute`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApplicationAttribute {
    AaDontShowIconsInMenus,
    AaNativeWindows,
    AaDontUseNativeMenuBar,
    AaMacDontSwapCtrlAndMeta,
    AaUse96Dpi,
    AaDisableNativeVirtualKeyboard,
    AaSynthesizeTouchForUnhandledMouseEvents,
    AaSynthesizeMouseForUnhandledTouchEvents,
    AaUseHighDpiPixmaps,
    AaForceRasterWidgets,
    AaUseDesktopOpenGL,
    AaUseOpenGLES,
    AaUseSoftwareOpenGL,
    AaShareOpenGLContexts,
    AaSetPalette,
    AaEnableHighDpiScaling,
    AaDisableHighDpiScaling,
    AaUseStyleSheetPropagationInWidgetStyles,
    AaDontUseNativeDialogs,
    AaSynthesizeMouseForUnhandledTabletEvents,
    AaCompressHighFrequencyEvents,
    AaDontCheckOpenGLContextThreadAffinity,
    AaDisableShaderDiskCache,
    AaPluginApplication,
}

/// Global metadata storage for the application.
#[derive(Debug, Default, Clone)]
pub struct ApplicationMetadata {
    pub application_name: String,
    pub application_version: String,
    pub organization_name: String,
    pub organization_domain: String,
    pub arguments: Vec<String>,
    pub library_paths: Vec<PathBuf>,
}

static GLOBAL_METADATA: RwLock<Option<ApplicationMetadata>> = RwLock::new(None);
static GLOBAL_ATTRIBUTES: RwLock<Option<HashSet<ApplicationAttribute>>> = RwLock::new(None);
static APP_INSTANCE_EXISTS: AtomicBool = AtomicBool::new(false);
static APP_OBJECT_ID: RwLock<Option<ObjectId>> = RwLock::new(None);

thread_local! {
    static LOCAL_EVENT_LOOP: RefCell<Option<Rc<EventLoop>>> = const { RefCell::new(None) };
}

/// Shared handle to the application's `aboutToQuit` signal; clones share subscribers.
static ABOUT_TO_QUIT: RwLock<Option<Signal<()>>> = RwLock::new(None);
/// `QCoreApplicationPrivate::in_exec` (qcoreapplication.cpp:1467, 1484-1486).
static IN_EXEC: AtomicBool = AtomicBool::new(false);
/// `QCoreApplicationPrivate::aboutToQuitEmitted` (qcoreapplication.cpp:1468, 1532-1534).
static ABOUT_TO_QUIT_EMITTED: AtomicBool = AtomicBool::new(false);
/// Thread that owns the application; `aboutToQuit` is only emitted there.
static APP_THREAD: RwLock<Option<ThreadId>> = RwLock::new(None);
/// Window veto installed by the widget layer (`QApplication::event(Quit)`, qapplication.cpp:1679-1697).
/// Returns false when a visible window refuses to close; `None` accepts every Quit.
static QUIT_GATE: parking_lot::RwLock<Option<fn() -> bool>> = parking_lot::RwLock::new(None);

fn is_app_thread() -> bool {
    APP_THREAD
        .read()
        .unwrap()
        .as_ref()
        .is_some_and(|t| *t == ThreadId::current())
}

/// The application's thread, if an application object exists.
fn app_thread() -> Option<ThreadId> {
    APP_THREAD.read().ok().and_then(|t| *t)
}

/// Installs the window veto that decides whether a `Quit` for the application object is accepted.
pub fn set_quit_gate(gate: fn() -> bool) {
    *QUIT_GATE.write() = Some(gate);
}

/// Whether `receiver` is the application object (`QCoreApplication::self`).
pub(crate) fn is_application_object(receiver: ObjectId) -> bool {
    *APP_OBJECT_ID.read().unwrap() == Some(receiver)
}

/// Delivers `Quit` to the application object: `QCoreApplication::event` (qcoreapplication.cpp:2052-2058)
/// behind the window veto of `QApplication::event` (qapplication.cpp:1679-1697). Accepted quits
/// exit with code 0 as in Qt; refused quits are ignored and the loop keeps running.
pub(crate) fn deliver_application_quit(event: &mut Event) {
    if !is_app_thread() {
        // The application object lives on the application thread: repost there.
        CoreApplication::post_event(
            CoreApplication::instance_id().expect("application object"),
            Event::new(EventKind::Quit { exit_code: 0 }),
        );
        return;
    }
    let gate = *QUIT_GATE.read();
    match gate {
        Some(accepts) if !accepts() => event.ignore(),
        _ => {
            event.accept();
            CoreApplication::exit(0);
        }
    }
}

/// The calling thread's application event loop, cloned so no slot borrow is held while it runs.
fn current_event_loop() -> Option<Rc<EventLoop>> {
    LOCAL_EVENT_LOOP
        .try_with(|el| el.borrow().clone())
        .ok()
        .flatten()
}

/// The application object's id when called on the application thread, for removing its posted
/// `Quit` events on loop entry (qeventloop.cpp:192-195).
pub(crate) fn app_object_on_current_thread() -> Option<ObjectId> {
    if is_app_thread() {
        *APP_OBJECT_ID.read().unwrap()
    } else {
        None
    }
}

/// Emits `aboutToQuit` at most once per exec (qcoreapplication.cpp:1532-1535). The flag is set
/// before emitting so a slot that calls `exit()` does not recurse.
pub(crate) fn emit_about_to_quit_once() {
    if !is_app_thread() {
        return;
    }
    if ABOUT_TO_QUIT_EMITTED.swap(true, Ordering::SeqCst) {
        return;
    }
    let signal = ABOUT_TO_QUIT.read().unwrap().clone();
    if let Some(signal) = signal {
        signal.emit(&());
    }
}

/// Core application singleton equivalent to Qt's `QCoreApplication`.
///
/// Manages the application-wide event loop, metadata, arguments, attributes,
/// and shutdown signals (`about_to_quit`).
pub struct CoreApplication {
    data: ObjectData,
    about_to_quit: Signal<()>,
}

impl CoreApplication {
    /// Initializes a new `CoreApplication` instance with optional command-line arguments.
    ///
    /// # Panics
    /// Panics if a `CoreApplication` or derivative application instance already exists.
    pub fn new(args: Vec<String>) -> Self {
        Self::with_dispatcher_kind(args, DispatcherKind::Core)
    }

    /// Like `new`, with the event loop's dispatcher from the `kind` creation path.
    /// `GuiApplication` passes `DispatcherKind::Gui`, as `QGuiApplication` takes its dispatcher
    /// from the platform plugin.
    pub fn with_dispatcher_kind(args: Vec<String>, kind: DispatcherKind) -> Self {
        assert!(
            !APP_INSTANCE_EXISTS.swap(true, Ordering::SeqCst),
            "QCoreApplication: there should be only one application object"
        );
        // Initialize main thread context
        ThreadContext::init_current(true, None);
        ThreadContext::set_main_thread_id(ThreadId::current());

        let id = ObjectId::next();
        let about_to_quit = Signal::new();
        *ABOUT_TO_QUIT.write().unwrap() = Some(about_to_quit.clone());
        *APP_THREAD.write().unwrap() = Some(ThreadId::current());
        let app = Self {
            data: ObjectData::new(id),
            about_to_quit,
        };

        // Initialize metadata if not already set
        {
            let mut meta_guard = GLOBAL_METADATA.write().unwrap();
            let mut meta = meta_guard.take().unwrap_or_default();
            meta.arguments = args;
            if meta.application_name.is_empty() {
                if let Some(first_arg) = meta.arguments.first() {
                    let path = Path::new(first_arg);
                    if let Some(file_stem) = path.file_stem().and_then(|s| s.to_str()) {
                        meta.application_name = file_stem.to_string();
                    }
                }
            }
            if meta.library_paths.is_empty() {
                if let Ok(exe) = std::env::current_exe() {
                    if let Some(dir) = exe.parent() {
                        meta.library_paths.push(dir.to_path_buf());
                    }
                }
            }
            *meta_guard = Some(meta);
        }

        // Setup local thread event loop; EventLoop registers the thread's handle itself.
        let _ = LOCAL_EVENT_LOOP.try_with(|el| {
            *el.borrow_mut() = Some(Rc::new(EventLoop::with_dispatcher_kind(kind)));
        });

        *APP_OBJECT_ID.write().unwrap() = Some(id);

        app
    }

    /// Returns `true` if an application instance is currently active.
    pub fn instance_exists() -> bool {
        APP_INSTANCE_EXISTS.load(Ordering::SeqCst)
    }

    /// Returns the `ObjectId` of the application object if it exists.
    pub fn instance_id() -> Option<ObjectId> {
        *APP_OBJECT_ID.read().unwrap()
    }

    /// Signal emitted when the application is about to exit the main event loop (`aboutToQuit`).
    pub fn about_to_quit(&self) -> &Signal<()> {
        &self.about_to_quit
    }

    /// Runs the main event loop until `exit()` or `quit()` stops it.
    /// Returns the exit code passed to `exit()`.
    pub fn exec(&mut self) -> i32 {
        // qcoreapplication.cpp:1467-1470: a new exec starts with in_exec set and aboutToQuit
        // re-armed, so an exit() issued before exec() does not end it.
        ABOUT_TO_QUIT_EMITTED.store(false, Ordering::SeqCst);
        IN_EXEC.store(true, Ordering::SeqCst);
        let code = current_event_loop().map_or(0, |el| el.exec());
        // execCleanup: sendPostedEvents(nullptr, DeferredDelete) once the loop has returned
        // (qcoreapplication.cpp:1472-1473, 1484-1489).
        if let Some(el) = current_event_loop() {
            el.flush_deferred_deletes();
        }
        IN_EXEC.store(false, Ordering::SeqCst);
        code
    }

    /// Processes pending events for the calling thread (`QCoreApplication::processEvents`).
    pub fn process_events(can_wait: bool) -> bool {
        // Pumps even while exec() is on the stack: the loop is shared through a clone, not a borrow.
        current_event_loop().is_some_and(|el| el.process_events(can_wait))
    }

    /// Type name of the calling thread's application event dispatcher, or `None` when this
    /// thread has no application event loop (`QCoreApplication::eventDispatcher()` class name).
    pub fn event_dispatcher_class_name() -> Option<&'static str> {
        use crate::event_loop::EventDispatcher;
        current_event_loop().map(|l| l.dispatcher().class_name())
    }

    /// Stops the event loop and makes `exec()` return `return_code` (`QCoreApplication::exit`).
    pub fn exit(return_code: i32) {
        // qcoreapplication.cpp:1532-1535: aboutToQuit is emitted from exit() itself.
        emit_about_to_quit_once();
        // qcoreapplication.cpp:1534-1540: exit() ends the loops of the application thread from any thread.
        let target = app_thread().unwrap_or_else(ThreadId::current);
        if let Some(handle) = crate::event_loop::get_thread_event_sender(target) {
            handle.exit(return_code);
        }
    }

    /// Tells the application to quit (`QCoreApplication::quit`): a sendEvent of Quit on the
    /// application thread, or a posted one from another thread (qcoreapplication.cpp:2138-2159).
    pub fn quit() {
        // qcoreapplication.cpp:2140-2144: no application object, or not in exec, means nothing to do.
        let Some(app) = Self::instance_id() else {
            return;
        };
        if !IN_EXEC.load(Ordering::SeqCst) {
            return;
        }
        if is_app_thread() {
            Self::send_event(app, &mut Event::new(EventKind::Quit { exit_code: 0 }));
        } else {
            Self::post_event(app, Event::new(EventKind::Quit { exit_code: 0 }));
        }
    }

    /// Whether `exec` is running. `QCoreApplication::quit` acts only then (qcoreapplication.cpp:2138-2147).
    pub fn in_exec() -> bool {
        IN_EXEC.load(Ordering::SeqCst)
    }

    /// Posts an event to a receiver object with priority.
    pub fn post_event_with_priority(receiver: ObjectId, event: Event, priority: i32) {
        let app_thread = if is_application_object(receiver) {
            *APP_THREAD.read().unwrap()
        } else {
            None
        };
        let target_thread = query_object_thread(receiver)
            .or(app_thread)
            .unwrap_or_else(ThreadId::current);
        crate::event_loop::post_event_to_thread_with_priority(
            target_thread,
            receiver,
            event,
            priority,
        );
    }

    /// Posts an event to a receiver object with normal priority.
    pub fn post_event(receiver: ObjectId, event: Event) {
        Self::post_event_with_priority(receiver, event, 0);
    }
    /// Sends an event directly to a receiver object synchronously (`QCoreApplication::sendEvent`).
    pub fn send_event(receiver: ObjectId, event: &mut Event) -> bool {
        crate::event_loop::notify_helper(receiver, event)
    }

    /// Installs an application-wide event filter (`QCoreApplication::installEventFilter`).
    pub fn install_event_filter(filter_id: ObjectId) {
        install_application_event_filter(filter_id);
    }

    /// Removes an application-wide event filter (`QCoreApplication::removeEventFilter`).
    pub fn remove_event_filter(filter_id: ObjectId) {
        remove_application_event_filter(filter_id);
    }

    /// Installs a native event filter (`QCoreApplication::installNativeEventFilter`).
    pub fn install_native_event_filter(filter: Box<dyn crate::event::NativeEventFilter>) {
        if let Some(loop_ref) = current_event_loop() {
            loop_ref.install_native_event_filter(filter);
        }
    }

    // --- Application Metadata ---

    pub fn application_name() -> String {
        GLOBAL_METADATA
            .read()
            .unwrap()
            .as_ref()
            .map(|m| m.application_name.clone())
            .unwrap_or_default()
    }

    pub fn set_application_name(name: impl Into<String>) {
        let mut guard = GLOBAL_METADATA.write().unwrap();
        if let Some(m) = guard.as_mut() {
            m.application_name = name.into();
        } else {
            *guard = Some(ApplicationMetadata {
                application_name: name.into(),
                ..Default::default()
            });
        }
    }

    pub fn application_version() -> String {
        GLOBAL_METADATA
            .read()
            .unwrap()
            .as_ref()
            .map(|m| m.application_version.clone())
            .unwrap_or_default()
    }

    pub fn set_application_version(version: impl Into<String>) {
        let mut guard = GLOBAL_METADATA.write().unwrap();
        if let Some(m) = guard.as_mut() {
            m.application_version = version.into();
        } else {
            *guard = Some(ApplicationMetadata {
                application_version: version.into(),
                ..Default::default()
            });
        }
    }

    pub fn organization_name() -> String {
        GLOBAL_METADATA
            .read()
            .unwrap()
            .as_ref()
            .map(|m| m.organization_name.clone())
            .unwrap_or_default()
    }

    pub fn set_organization_name(name: impl Into<String>) {
        let mut guard = GLOBAL_METADATA.write().unwrap();
        if let Some(m) = guard.as_mut() {
            m.organization_name = name.into();
        } else {
            *guard = Some(ApplicationMetadata {
                organization_name: name.into(),
                ..Default::default()
            });
        }
    }

    pub fn organization_domain() -> String {
        GLOBAL_METADATA
            .read()
            .unwrap()
            .as_ref()
            .map(|m| m.organization_domain.clone())
            .unwrap_or_default()
    }

    pub fn set_organization_domain(domain: impl Into<String>) {
        let mut guard = GLOBAL_METADATA.write().unwrap();
        if let Some(m) = guard.as_mut() {
            m.organization_domain = domain.into();
        } else {
            *guard = Some(ApplicationMetadata {
                organization_domain: domain.into(),
                ..Default::default()
            });
        }
    }

    pub fn arguments() -> Vec<String> {
        GLOBAL_METADATA
            .read()
            .unwrap()
            .as_ref()
            .map(|m| m.arguments.clone())
            .unwrap_or_else(|| std::env::args().collect())
    }

    pub fn application_dir_path() -> PathBuf {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|dir| dir.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn application_file_path() -> PathBuf {
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("."))
    }

    pub fn application_pid() -> u32 {
        std::process::id()
    }

    // --- Library Paths ---

    pub fn library_paths() -> Vec<PathBuf> {
        GLOBAL_METADATA
            .read()
            .unwrap()
            .as_ref()
            .map(|m| m.library_paths.clone())
            .unwrap_or_default()
    }

    pub fn set_library_paths(paths: Vec<PathBuf>) {
        let mut guard = GLOBAL_METADATA.write().unwrap();
        if let Some(m) = guard.as_mut() {
            m.library_paths = paths;
        }
    }

    pub fn add_library_path(path: PathBuf) {
        let mut guard = GLOBAL_METADATA.write().unwrap();
        if let Some(m) = guard.as_mut() {
            if !m.library_paths.contains(&path) {
                m.library_paths.push(path);
            }
        }
    }

    // --- Application Attributes ---

    pub fn set_attribute(attribute: ApplicationAttribute, on: bool) {
        let mut guard = GLOBAL_ATTRIBUTES.write().unwrap();
        let set = guard.get_or_insert_with(HashSet::new);
        if on {
            set.insert(attribute);
        } else {
            set.remove(&attribute);
        }
    }

    pub fn test_attribute(attribute: ApplicationAttribute) -> bool {
        GLOBAL_ATTRIBUTES
            .read()
            .unwrap()
            .as_ref()
            .map(|set| set.contains(&attribute))
            .unwrap_or(false)
    }

    /// Resets internal singleton flags for testing isolation.
    #[doc(hidden)]
    pub fn reset_for_test() {
        APP_INSTANCE_EXISTS.store(false, Ordering::SeqCst);
        *APP_OBJECT_ID.write().unwrap() = None;
        *ABOUT_TO_QUIT.write().unwrap() = None;
        *APP_THREAD.write().unwrap() = None;
        IN_EXEC.store(false, Ordering::SeqCst);
        ABOUT_TO_QUIT_EMITTED.store(false, Ordering::SeqCst);
        *GLOBAL_METADATA.write().unwrap() = None;
        *GLOBAL_ATTRIBUTES.write().unwrap() = None;
        let _ = LOCAL_EVENT_LOOP.try_with(|el| {
            *el.borrow_mut() = None;
        });
        crate::event_loop::clear_application_event_filters();
    }
}

impl Drop for CoreApplication {
    fn drop(&mut self) {
        APP_INSTANCE_EXISTS.store(false, Ordering::SeqCst);
        *APP_OBJECT_ID.write().unwrap() = None;
        *ABOUT_TO_QUIT.write().unwrap() = None;
        *APP_THREAD.write().unwrap() = None;
        IN_EXEC.store(false, Ordering::SeqCst);
        ABOUT_TO_QUIT_EMITTED.store(false, Ordering::SeqCst);
        // SAFETY: Drop is on the application object's owning thread after dispatch ends.
        unsafe { unregister_qobject(self.data.id) };
        let _ = LOCAL_EVENT_LOOP.try_with(|el| {
            *el.borrow_mut() = None;
        });
    }
}

static CORE_APP_META_OBJECT: MetaObject = MetaObject::new(
    "QCoreApplication",
    Some(&crate::meta::QOBJECT_META_OBJECT),
    &[],
    &[],
    &[],
    &[],
);

impl QObject for CoreApplication {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> &mut ObjectData {
        &mut self.data
    }

    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn meta_object(&self) -> &'static MetaObject {
        &CORE_APP_META_OBJECT
    }
}
