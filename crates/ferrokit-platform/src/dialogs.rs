// SPDX-License-Identifier: LGPL-3.0-only
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogIcon {
    Info,
    Warning,
    Error,
    Question,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBoxButtons {
    Ok,
    OkCancel,
    YesNo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageBoxResult {
    Ok,
    Cancel,
    Yes,
    No,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    pub patterns: Vec<String>,
    pub description: String,
}

/// Synchronous native dialog operations provided by the platform backend.
///
/// On macOS every method must be called on the main thread (AppKit constraint) and panics
/// otherwise, before any dialog is created. Other platforms accept calls from any thread.
pub trait PlatformDialogs: Send + Sync {
    fn message_box(
        &self,
        title: &str,
        message: &str,
        icon: DialogIcon,
        buttons: MessageBoxButtons,
    ) -> MessageBoxResult;

    fn open_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf>;

    fn open_files(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<Vec<PathBuf>>;

    fn save_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf>;

    fn select_folder(&self, title: &str, initial_path: &str) -> Option<PathBuf>;
}

#[derive(Debug, Default)]
pub struct SystemDialogs;

impl SystemDialogs {
    fn native_icon(icon: DialogIcon) -> tinyfiledialogs::MessageBoxIcon {
        match icon {
            DialogIcon::Info => tinyfiledialogs::MessageBoxIcon::Info,
            DialogIcon::Warning => tinyfiledialogs::MessageBoxIcon::Warning,
            DialogIcon::Error => tinyfiledialogs::MessageBoxIcon::Error,
            DialogIcon::Question => tinyfiledialogs::MessageBoxIcon::Question,
        }
    }

    fn filter_parts(filter: Option<&FileFilter>) -> Option<(Vec<&str>, &str)> {
        filter.map(|filter| {
            (
                filter.patterns.iter().map(String::as_str).collect(),
                filter.description.as_str(),
            )
        })
    }
}

impl PlatformDialogs for SystemDialogs {
    fn message_box(
        &self,
        title: &str,
        message: &str,
        icon: DialogIcon,
        buttons: MessageBoxButtons,
    ) -> MessageBoxResult {
        let icon = Self::native_icon(icon);
        match buttons {
            MessageBoxButtons::Ok => {
                tinyfiledialogs::message_box_ok(title, message, icon);
                MessageBoxResult::Ok
            }
            MessageBoxButtons::OkCancel => {
                match tinyfiledialogs::message_box_ok_cancel(
                    title,
                    message,
                    icon,
                    tinyfiledialogs::OkCancel::Ok,
                ) {
                    tinyfiledialogs::OkCancel::Ok => MessageBoxResult::Ok,
                    tinyfiledialogs::OkCancel::Cancel => MessageBoxResult::Cancel,
                }
            }
            MessageBoxButtons::YesNo => {
                match tinyfiledialogs::message_box_yes_no(
                    title,
                    message,
                    icon,
                    tinyfiledialogs::YesNo::No,
                ) {
                    tinyfiledialogs::YesNo::Yes => MessageBoxResult::Yes,
                    tinyfiledialogs::YesNo::No => MessageBoxResult::No,
                }
            }
        }
    }

    fn open_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf> {
        let filter = Self::filter_parts(filter);
        tinyfiledialogs::open_file_dialog(
            title,
            initial_path,
            filter.as_ref().map(|(p, d)| (&p[..], *d)),
        )
        .map(PathBuf::from)
    }

    fn open_files(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<Vec<PathBuf>> {
        let filter = Self::filter_parts(filter);
        tinyfiledialogs::open_file_dialog_multi(
            title,
            initial_path,
            filter.as_ref().map(|(p, d)| (&p[..], *d)),
        )
        .map(|paths| paths.into_iter().map(PathBuf::from).collect())
    }

    fn save_file(
        &self,
        title: &str,
        initial_path: &str,
        filter: Option<&FileFilter>,
    ) -> Option<PathBuf> {
        let filter = Self::filter_parts(filter);
        tinyfiledialogs::save_file_dialog_with_filter(
            title,
            initial_path,
            filter.as_ref().map(|(p, _)| &p[..]).unwrap_or(&[]),
            filter.as_ref().map(|(_, d)| *d).unwrap_or("All files"),
        )
        .map(PathBuf::from)
    }

    fn select_folder(&self, title: &str, initial_path: &str) -> Option<PathBuf> {
        tinyfiledialogs::select_folder_dialog(title, initial_path).map(PathBuf::from)
    }
}

#[cfg(not(target_os = "macos"))]
static SYSTEM_DIALOGS: SystemDialogs = SystemDialogs;
#[cfg(target_os = "macos")]
static COCOA_DIALOGS: crate::dialogs_cocoa::CocoaDialogs<crate::dialogs_cocoa::NativeRuntime> =
    crate::dialogs_cocoa::CocoaDialogs::new(crate::dialogs_cocoa::NativeRuntime);

/// Returns the OS-native dialog implementation (AppKit panels and alerts on macOS).
pub fn platform_dialogs() -> &'static dyn PlatformDialogs {
    #[cfg(target_os = "macos")]
    {
        &COCOA_DIALOGS
    }
    #[cfg(not(target_os = "macos"))]
    {
        &SYSTEM_DIALOGS
    }
}
