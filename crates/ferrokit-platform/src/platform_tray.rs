// SPDX-License-Identifier: LGPL-3.0-only
use crate::menu::PlatformMenu;
use crate::tray_icon::TrayActivation;
use ferrokit_gui::image::Icon;
use ferrokit_gui::paint::Pixmap;

/// Aligned with `QPlatformSystemTrayIcon::MessageIcon` (qtbase)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TrayMessageIcon {
    #[default]
    NoIcon = 0,
    Information = 1,
    Warning = 2,
    Critical = 3,
}

/// Makes `tray` emit every activation it reports on `signal` (`QSystemTrayIcon::activated`).
/// The non-Windows `TrayIcon` wires its `on_activated` signal this way.
pub fn forward_activations(
    tray: &mut dyn PlatformTrayIcon,
    signal: &ferrokit_core::signal::Signal<TrayActivation>,
) {
    let emitter = signal.clone();
    tray.set_activation_handler(Box::new(move |reason| emitter.emit(&reason)));
}

/// One pixmap for each size `icon` has in its normal, off state (what a desktop tray that takes
/// pixmaps, such as StatusNotifierItem, is given for `QSystemTrayIcon::setIcon`); smallest first,
/// empty for a null icon.
pub fn icon_pixmaps(icon: &Icon) -> Vec<Pixmap> {
    use ferrokit_gui::image::{IconMode, IconState};
    icon.available_sizes(IconMode::Normal, IconState::Off)
        .into_iter()
        .map(|size| icon.pixmap(size, IconMode::Normal, IconState::Off))
        .collect()
}

pub trait PlatformTrayIcon {
    /// Shows `icon` (`QSystemTrayIcon::setIcon`).
    fn set_icon(&mut self, icon: &Icon) -> Result<(), &'static str>;
    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str>;
    fn set_menu(&mut self, menu: Box<dyn PlatformMenu>);
    /// Shows the icon. The D-Bus back end only queues its StatusNotifierWatcher registration:
    /// `Ok` then means queued, and `DbusStatusNotifierItem::is_registered` confirms it after
    /// [`process_events`](Self::process_events) has read the watcher's reply.
    fn show(&mut self) -> Result<(), &'static str>;
    fn hide(&mut self) -> Result<(), &'static str>;

    /// Installs the receiver of `QPlatformSystemTrayIcon::activated(ActivationReason)`: the
    /// back end calls `handler` with the reason each time the user activates the icon. The
    /// default drops the handler, for back ends that never report activations (the headless
    /// [`GenericTrayIcon`]); the Windows icon reports through `TrayIcon::on_activated` instead.
    fn set_activation_handler(&mut self, handler: Box<dyn Fn(TrayActivation)>) {
        let _ = handler;
    }

    /// Reads and dispatches whatever the desktop sent to the icon since the last call, which
    /// may invoke the activation handler. The D-Bus back end receives `Activate`,
    /// `ContextMenu` and `SecondaryActivate` method calls, `org.freedesktop.DBus.Properties`
    /// reads, the StatusNotifierWatcher's registration reply and `NameOwnerChanged` signals on
    /// the session bus socket and needs this to be driven (see
    /// `DbusStatusNotifierItem::bind_event_dispatcher`); its `show` only queues the
    /// registration, so the registration is confirmed here. Back ends that
    /// are called back by the OS (AppKit) have nothing to do.
    fn process_events(&mut self) {}

    /// Watches the descriptor whose readability means [`PlatformTrayIcon::process_events`] has
    /// something to do; `Ok(None)` when the back end has no descriptor to watch. Connect the
    /// returned notifier's `activated` signal to `process_events`.
    fn bind_event_dispatcher(
        &mut self,
        dispatcher: &mut dyn ferrokit_core::event_loop::EventDispatcher,
    ) -> Result<
        Option<std::sync::Arc<ferrokit_core::event_loop::SocketNotifier>>,
        ferrokit_core::event_loop::SocketNotifierError,
    > {
        let _ = dispatcher;
        Ok(None)
    }

    /// Displays a balloon or toast message aligned with `QPlatformSystemTrayIcon::showMessage`.
    /// `duration_ms` defaults to 10,000 ms if 0.
    fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        let _ = (title, msg, icon, duration_ms);
        Ok(())
    }

    /// Displays a balloon or toast message with a custom icon, aligned with the
    /// `QSystemTrayIcon::showMessage(title, msg, QIcon, msecs)` overload.
    /// `duration_ms` defaults to 10,000 ms if 0.
    ///
    /// The default falls back to [`TrayMessageIcon::NoIcon`]: back ends that cannot show an
    /// arbitrary image in a notification (D-Bus notifications, macOS user notifications) keep
    /// the title and text but drop the icon. A null `icon` also shows no icon.
    fn show_message_with_icon(
        &mut self,
        title: &str,
        msg: &str,
        icon: &Icon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        let _ = icon;
        self.show_message(title, msg, TrayMessageIcon::NoIcon, duration_ms)
    }

    /// Returns true if this platform tray implementation supports showing system messages.
    fn supports_messages(&self) -> bool {
        true
    }
}

pub struct GenericTrayIcon {
    tooltip: String,
    icon: Icon,
    visible: bool,
    menu: Option<Box<dyn PlatformMenu>>,
    last_message: Option<(String, String, TrayMessageIcon, u32)>,
    last_message_icon: Option<Icon>,
}

impl GenericTrayIcon {
    pub fn new(tooltip: &str, icon: &Icon) -> Self {
        Self {
            tooltip: tooltip.to_string(),
            icon: icon.clone(),
            visible: false,
            menu: None,
            last_message: None,
            last_message_icon: None,
        }
    }

    pub fn tooltip(&self) -> &str {
        &self.tooltip
    }

    /// The icon last given to [`new`](Self::new) or `set_icon`.
    pub fn icon(&self) -> &Icon {
        &self.icon
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn menu(&self) -> Option<&dyn PlatformMenu> {
        self.menu.as_deref()
    }

    pub fn last_message(&self) -> Option<(&str, &str, TrayMessageIcon, u32)> {
        self.last_message
            .as_ref()
            .map(|(t, m, i, d)| (t.as_str(), m.as_str(), *i, *d))
    }

    /// The custom icon of the last `show_message_with_icon` call; `None` after a message
    /// shown with a standard `TrayMessageIcon` (or when the icon was null).
    pub fn last_message_icon(&self) -> Option<&Icon> {
        self.last_message_icon.as_ref()
    }
}

impl PlatformTrayIcon for GenericTrayIcon {
    fn set_icon(&mut self, icon: &Icon) -> Result<(), &'static str> {
        self.icon = icon.clone();
        Ok(())
    }

    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str> {
        self.tooltip = tooltip.to_string();
        Ok(())
    }

    fn set_menu(&mut self, menu: Box<dyn PlatformMenu>) {
        self.menu = Some(menu);
    }

    fn show(&mut self) -> Result<(), &'static str> {
        self.visible = true;
        Ok(())
    }

    fn hide(&mut self) -> Result<(), &'static str> {
        self.visible = false;
        Ok(())
    }

    fn show_message(
        &mut self,
        title: &str,
        msg: &str,
        icon: TrayMessageIcon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        let dur = if duration_ms == 0 { 10000 } else { duration_ms };
        self.last_message = Some((title.to_string(), msg.to_string(), icon, dur));
        self.last_message_icon = None;
        Ok(())
    }

    fn show_message_with_icon(
        &mut self,
        title: &str,
        msg: &str,
        icon: &Icon,
        duration_ms: u32,
    ) -> Result<(), &'static str> {
        self.show_message(title, msg, TrayMessageIcon::NoIcon, duration_ms)?;
        self.last_message_icon = (!icon.is_null()).then(|| icon.clone());
        Ok(())
    }

    fn supports_messages(&self) -> bool {
        true
    }
}
