// SPDX-License-Identifier: LGPL-3.0-only
//! Desktop services aligned with `QDesktopServices` and `QPlatformServices` (qtbase)
use std::path::Path;
use std::sync::RwLock;

pub type UrlHandler = Box<dyn Fn(&str) -> bool + Send + Sync>;

static URL_HANDLER: RwLock<Option<UrlHandler>> = RwLock::new(None);

/// Sets or unsets a custom URL handler (aligned with `QDesktopServices::setUrlHandler`).
/// If a handler is set and returns `true`, default OS launching is bypassed.
pub fn set_url_handler(handler: Option<UrlHandler>) {
    let mut guard = URL_HANDLER.write().unwrap();
    *guard = handler;
}

/// Opens the specified URL in the desktop's default browser / application.
/// Aligned with `QDesktopServices::openUrl(const QUrl &url)`.
pub fn open_url(url: &str) -> bool {
    if url.trim().is_empty() {
        return false;
    }

    // Check if custom or test handler is registered
    {
        let guard = URL_HANDLER.read().unwrap();
        if let Some(ref handler) = *guard {
            return handler(url);
        }
    }

    #[cfg(windows)]
    {
        use std::ptr;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let wide_op: Vec<u16> = "open\0".encode_utf16().collect();
        let wide_url: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: `wide_op` and `wide_url` are NUL-terminated UTF-16 buffers (terminator appended above)
        // that live until after the call; the null HWND, parameters and directory are permitted by the
        // ShellExecuteW documentation.
        let hinst = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                wide_op.as_ptr(),
                wide_url.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        (hinst as isize) > 32
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }
}

/// Opens the specified local file or directory with the system default file manager or viewer.
/// Aligned with `QDesktopServices::openUrl(QUrl::fromLocalFile(path))`.
pub fn open_file(path: impl AsRef<Path>) -> bool {
    let p = path.as_ref();
    if !p.exists() {
        return false;
    }

    #[cfg(windows)]
    {
        use std::ptr;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let wide_op: Vec<u16> = "open\0".encode_utf16().collect();
        let path_str = p.to_string_lossy();
        let wide_path: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();

        // SAFETY: `wide_op` and `wide_path` are NUL-terminated UTF-16 buffers (terminator appended above)
        // that live until after the call; the null HWND, parameters and directory are permitted by the
        // ShellExecuteW documentation.
        let hinst = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                wide_op.as_ptr(),
                wide_path.as_ptr(),
                ptr::null(),
                ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        (hinst as isize) > 32
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(p.as_os_str())
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }

    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(p.as_os_str())
            .spawn()
            .map(|_| true)
            .unwrap_or(false)
    }
}
