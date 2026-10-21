// SPDX-License-Identifier: LGPL-3.0-only
use crate::menu::PlatformMenu;
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

pub trait PlatformTrayIcon {
    fn set_icon(&mut self, pixmap: &Pixmap) -> Result<(), &'static str>;
    fn set_tooltip(&mut self, tooltip: &str) -> Result<(), &'static str>;
    fn set_menu(&mut self, menu: Box<dyn PlatformMenu>);
    fn show(&mut self) -> Result<(), &'static str>;
    fn hide(&mut self) -> Result<(), &'static str>;

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

    /// Returns true if this platform tray implementation supports showing system messages.
    fn supports_messages(&self) -> bool {
        true
    }
}

pub struct GenericTrayIcon {
    tooltip: String,
    pixmap: Option<Pixmap>,
    visible: bool,
    menu: Option<Box<dyn PlatformMenu>>,
    last_message: Option<(String, String, TrayMessageIcon, u32)>,
}

impl GenericTrayIcon {
    pub fn new(tooltip: &str, pixmap: &Pixmap) -> Self {
        Self {
            tooltip: tooltip.to_string(),
            pixmap: Some(pixmap.clone()),
            visible: false,
            menu: None,
            last_message: None,
        }
    }

    pub fn tooltip(&self) -> &str {
        &self.tooltip
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
}

impl PlatformTrayIcon for GenericTrayIcon {
    fn set_icon(&mut self, pixmap: &Pixmap) -> Result<(), &'static str> {
        self.pixmap = Some(pixmap.clone());
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
        Ok(())
    }

    fn supports_messages(&self) -> bool {
        true
    }
}
