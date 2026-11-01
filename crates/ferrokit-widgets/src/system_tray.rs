// SPDX-License-Identifier: LGPL-3.0-only
//! System tray icon integration with custom-drawn menus (`QSystemTrayIcon`).

use std::marker::PhantomData;

use ferrokit_core::object::ThreadContext;
use ferrokit_core::signal::{ConnectionId, Signal};
use ferrokit_gui::geometry::primitives::{Point, Rect};
use ferrokit_gui::image::Icon;
use ferrokit_gui::paint::Pixmap;
use ferrokit_gui::tiny_skia::Color;
pub use ferrokit_platform::platform_tray::TrayMessageIcon;
pub use ferrokit_platform::tray_icon::TrayActivation;
use ferrokit_platform::tray_icon::TrayIcon;

use crate::menu::{Menu, MenuRef};

/// Type alias matching Qt's `QSystemTrayIcon::ActivationReason`.
pub type ActivationReason = TrayActivation;

/// Type alias matching Qt's `QSystemTrayIcon::MessageIcon`.
pub type MessageIcon = TrayMessageIcon;

/// Thread-affine wrapper enabling capturing `MenuRef` inside `Signal::connect` closure.
struct MainThread<T>(T);

// SAFETY: `SystemTrayIcon` is strictly thread-affine (`!Send + !Sync`), created and accessed
// only on the main thread (enforced by [`ThreadContext::assert_main_thread`]). The wrapped
// value is only accessed and dropped on the main thread.
unsafe impl<T> Send for MainThread<T> {}
// SAFETY: Same as `Send`; access is restricted to the main thread.
unsafe impl<T> Sync for MainThread<T> {}

impl<T> MainThread<T> {
    fn get(&self) -> &T {
        &self.0
    }
}
fn default_blank_icon() -> Icon {
    let mut pixmap = Pixmap::new(16, 16).expect("pixmap allocation");
    pixmap.fill(Color::TRANSPARENT);
    Icon::from_pixmap(&pixmap)
}

/// System tray icon (`QSystemTrayIcon`).
///
/// Bridges the platform-native tray icon (`ferrokit-platform`) with the custom-drawn
/// widget [`Menu`] (`QMenu`), following Qt's `QSystemTrayIcon::setContextMenu(QMenu*)`
/// architecture.
pub struct SystemTrayIcon {
    platform_tray: Box<TrayIcon>,
    menu: Option<MenuRef>,
    pub activated: Signal<TrayActivation>,
    pub message_clicked: Signal<()>,
    menu_connection: Option<ConnectionId>,
    current_icon: Option<Icon>,
    tooltip: String,
    _marker: PhantomData<*const ()>,
}

/// Qt alias for [`SystemTrayIcon`].
pub type QSystemTrayIcon = SystemTrayIcon;

impl SystemTrayIcon {
    /// Creates a new system tray icon (`QSystemTrayIcon()`).
    pub fn new() -> Result<Self, &'static str> {
        ThreadContext::assert_main_thread("SystemTrayIcon::new");
        let icon = default_blank_icon();
        let mut platform_tray = TrayIcon::from_icon("", &icon)?;
        let _ = platform_tray.hide(); // Icons are initially invisible in Qt
        Self::with_tray(platform_tray)
    }

    /// Creates a system tray icon showing the given icon (`QSystemTrayIcon(const QIcon &)`).
    pub fn from_icon(icon: &Icon) -> Result<Self, &'static str> {
        ThreadContext::assert_main_thread("SystemTrayIcon::new");
        let mut platform_tray = TrayIcon::from_icon("", icon)?;
        let _ = platform_tray.hide(); // Icons are initially invisible in Qt
        let mut tray = Self::with_tray(platform_tray)?;
        tray.current_icon = Some(icon.clone());
        Ok(tray)
    }
    /// Wraps an existing platform tray icon.
    pub fn with_tray(platform_tray: Box<TrayIcon>) -> Result<Self, &'static str> {
        ThreadContext::assert_main_thread("SystemTrayIcon::new");
        let activated = platform_tray.on_activated.clone();
        let message_clicked = platform_tray.on_message_clicked.clone();
        Ok(Self {
            platform_tray,
            menu: None,
            activated,
            message_clicked,
            menu_connection: None,
            current_icon: None,
            tooltip: String::new(),
            _marker: PhantomData,
        })
    }

    /// Sets the context menu (`QSystemTrayIcon::setContextMenu`).
    ///
    /// When the tray icon requests a context menu (e.g. on right-click),
    /// the custom menu is popped up modally at the anchor point via [`Menu::exec_popup`].
    pub fn set_context_menu(&mut self, menu: MenuRef) {
        if let Some(conn) = self.menu_connection.take() {
            self.platform_tray
                .on_context_menu_requested
                .disconnect(conn);
        }
        let menu_holder = MainThread(menu.clone());
        let conn = self
            .platform_tray
            .on_context_menu_requested
            .connect(move |pos: &Point| {
                ThreadContext::assert_main_thread("SystemTrayIcon context menu");
                let pos = *pos;
                Menu::exec_popup(menu_holder.get(), pos);
            });
        self.menu = Some(menu);
        self.menu_connection = Some(conn);
    }

    /// Returns the context menu (`QSystemTrayIcon::contextMenu`), or `None` if unset.
    pub fn context_menu(&self) -> Option<MenuRef> {
        self.menu.clone()
    }

    /// Clears the context menu.
    pub fn clear_context_menu(&mut self) {
        if let Some(conn) = self.menu_connection.take() {
            self.platform_tray
                .on_context_menu_requested
                .disconnect(conn);
        }
        self.menu = None;
    }

    /// Sets the tray icon (`QSystemTrayIcon::setIcon`).
    pub fn set_icon(&mut self, icon: &Icon) -> Result<(), &'static str> {
        self.platform_tray.set_icon_from(icon)?;
        self.current_icon = Some(icon.clone());
        Ok(())
    }

    /// Returns the current icon (`QSystemTrayIcon::icon`), if set.
    pub fn icon(&self) -> Option<&Icon> {
        self.current_icon.as_ref()
    }

    /// Sets the tooltip shown on hover (`QSystemTrayIcon::setToolTip`).
    pub fn set_tooltip(&mut self, tooltip: impl Into<String>) -> Result<(), &'static str> {
        let s = tooltip.into();
        self.platform_tray.set_tooltip(s.as_str())?;
        self.tooltip = s;
        Ok(())
    }

    /// Returns the tooltip text (`QSystemTrayIcon::toolTip`).
    pub fn tooltip(&self) -> &str {
        &self.tooltip
    }

    /// Shows the icon in the system tray (`QSystemTrayIcon::show`).
    pub fn show(&mut self) -> Result<(), &'static str> {
        self.platform_tray.show()
    }

    /// Hides the icon from the system tray (`QSystemTrayIcon::hide`).
    pub fn hide(&mut self) -> Result<(), &'static str> {
        self.platform_tray.hide()
    }

    /// Returns whether the icon is visible (`QSystemTrayIcon::isVisible`).
    pub fn is_visible(&self) -> bool {
        self.platform_tray.is_visible()
    }

    /// Displays a balloon message notification (`QSystemTrayIcon::showMessage`).
    pub fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        self.platform_tray
            .show_message(title, msg, icon, duration_ms)
    }

    /// Displays a balloon message with a custom icon (`QSystemTrayIcon::showMessage`).
    pub fn show_message_with_icon(
        &mut self,
        title: &str,
        msg: &str,
        icon: &Icon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        self.platform_tray
            .show_message_with_icon(title, msg, icon, duration_ms)
    }

    /// Returns whether the system tray is available on the desktop (`QSystemTrayIcon::isSystemTrayAvailable`).
    pub fn is_system_tray_available() -> bool {
        #[cfg(windows)]
        {
            TrayIcon::is_system_tray_available()
        }
        #[cfg(not(windows))]
        {
            true
        }
    }

    /// Returns whether balloon messages are supported by the OS (`QSystemTrayIcon::supportsMessages`).
    pub fn supports_messages(&self) -> bool {
        #[cfg(windows)]
        {
            self.platform_tray.supports_messages()
        }
        #[cfg(not(windows))]
        {
            true
        }
    }

    /// Returns the screen rectangle occupied by the tray icon (`QSystemTrayIcon::geometry`).
    pub fn geometry(&self) -> Rect {
        #[cfg(windows)]
        {
            self.platform_tray.geometry()
        }
        #[cfg(not(windows))]
        {
            Rect::new(0, 0, 0, 0)
        }
    }

    /// Reference to underlying platform tray icon.
    pub fn platform_tray(&self) -> &TrayIcon {
        &self.platform_tray
    }

    /// Mutable reference to underlying platform tray icon.
    pub fn platform_tray_mut(&mut self) -> &mut TrayIcon {
        &mut self.platform_tray
    }

    /// The underlying tray window handle.
    #[cfg(windows)]
    pub fn hwnd(&self) -> windows_sys::Win32::Foundation::HWND {
        self.platform_tray.hwnd()
    }

    /// The underlying tray window handle on non-Windows.
    #[cfg(not(windows))]
    pub fn hwnd(&self) -> isize {
        self.platform_tray.hwnd()
    }
}

impl Drop for SystemTrayIcon {
    fn drop(&mut self) {
        if let Some(conn) = self.menu_connection.take() {
            self.platform_tray
                .on_context_menu_requested
                .disconnect(conn);
        }
    }
}
