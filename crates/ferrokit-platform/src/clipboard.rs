// SPDX-License-Identifier: LGPL-3.0-only
#[cfg(windows)]
use crate::win32_handles::{ClipboardSession, GlobalMem};
use std::collections::BTreeMap;
use std::sync::RwLock;
#[cfg(windows)]
use windows_sys::Win32::System::DataExchange::{
    EmptyClipboard, EnumClipboardFormats, GetClipboardData, GetClipboardFormatNameW,
    RegisterClipboardFormatW, SetClipboardData,
};
#[cfg(windows)]
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
#[cfg(windows)]
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

/// MIME data container used for clipboard transfer.
///
/// Payloads are raw bytes keyed by MIME type. `text/plain` and `text/html` helpers use UTF-8.
/// The Win32 backend maps plain text to `CF_UNICODETEXT`, HTML to `HTML Format`/CF_HTML,
/// and other MIME types to registered clipboard formats. `GenericClipboard` is in-memory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MimeData {
    data: BTreeMap<String, Vec<u8>>,
}

impl MimeData {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_data(&mut self, mime_type: impl Into<String>, data: impl Into<Vec<u8>>) {
        self.data.insert(mime_type.into(), data.into());
    }

    pub fn data(&self, mime_type: &str) -> Option<&[u8]> {
        self.data.get(mime_type).map(Vec::as_slice)
    }

    pub fn has_format(&self, mime_type: &str) -> bool {
        self.data.contains_key(mime_type)
    }

    pub fn formats(&self) -> impl Iterator<Item = &str> {
        self.data.keys().map(String::as_str)
    }

    pub fn set_text(&mut self, text: &str) {
        self.set_data("text/plain", text.as_bytes());
    }

    pub fn text(&self) -> Option<String> {
        std::str::from_utf8(self.data("text/plain")?)
            .ok()
            .map(str::to_owned)
    }

    pub fn set_html(&mut self, html: &str) {
        self.set_data("text/html", html.as_bytes());
    }

    pub fn html(&self) -> Option<String> {
        std::str::from_utf8(self.data("text/html")?)
            .ok()
            .map(str::to_owned)
    }

    pub fn clear(&mut self) {
        self.data.clear();
    }
}

pub trait PlatformClipboard: Send + Sync {
    fn set_mime_data(&self, data: &MimeData) -> Result<(), &'static str>;
    fn mime_data(&self) -> Result<MimeData, &'static str>;
    fn clear(&self) -> Result<(), &'static str>;

    fn set_text(&self, text: &str) -> Result<(), &'static str> {
        let mut data = MimeData::new();
        data.set_text(text);
        self.set_mime_data(&data)
    }

    fn text(&self) -> Result<String, &'static str> {
        Ok(self.mime_data()?.text().unwrap_or_default())
    }
}

#[derive(Debug, Default)]
pub struct GenericClipboard {
    content: RwLock<MimeData>,
}

impl GenericClipboard {
    pub fn new() -> Self {
        Self::default()
    }
}

impl PlatformClipboard for GenericClipboard {
    fn set_mime_data(&self, data: &MimeData) -> Result<(), &'static str> {
        let mut guard = self
            .content
            .write()
            .map_err(|_| "Clipboard lock poisoned")?;
        *guard = data.clone();
        Ok(())
    }

    fn mime_data(&self) -> Result<MimeData, &'static str> {
        let guard = self.content.read().map_err(|_| "Clipboard lock poisoned")?;
        Ok(guard.clone())
    }

    fn clear(&self) -> Result<(), &'static str> {
        let mut guard = self
            .content
            .write()
            .map_err(|_| "Clipboard lock poisoned")?;
        guard.clear();
        Ok(())
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Win32Clipboard;

#[cfg(windows)]
impl Win32Clipboard {
    fn register_mime_format(mime_type: &str) -> Result<u32, &'static str> {
        let name: Vec<u16> = mime_type.encode_utf16().chain(std::iter::once(0)).collect();
        let format = unsafe { RegisterClipboardFormatW(name.as_ptr()) };
        if format == 0 {
            Err("RegisterClipboardFormatW failed")
        } else {
            Ok(format)
        }
    }

    fn get_format_name(format: u32) -> Option<String> {
        let mut name = [0u16; 256];
        let len = unsafe { GetClipboardFormatNameW(format, name.as_mut_ptr(), name.len() as i32) };
        (len > 0).then(|| String::from_utf16_lossy(&name[..len as usize]))
    }
    fn cf_html_payload(html: &str) -> Vec<u8> {
        const START_MARKER: &str = "<!--StartFragment-->";
        const END_MARKER: &str = "<!--EndFragment-->";
        let body = format!("<html><body>{START_MARKER}{html}{END_MARKER}</body></html>");
        let header = "Version:1.0\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
        let start_html = header.len();
        let start_fragment = start_html + "<html><body>".len() + START_MARKER.len();
        let end_fragment = start_fragment + html.len();
        let end_html = start_html + body.len();
        let header = header
            .replace(
                "StartHTML:0000000000",
                &format!("StartHTML:{start_html:010}"),
            )
            .replace("EndHTML:0000000000", &format!("EndHTML:{end_html:010}"))
            .replace(
                "StartFragment:0000000000",
                &format!("StartFragment:{start_fragment:010}"),
            )
            .replace(
                "EndFragment:0000000000",
                &format!("EndFragment:{end_fragment:010}"),
            );
        format!("{header}{body}").into_bytes()
    }

    fn html_from_cf_html(bytes: &[u8]) -> Vec<u8> {
        let Ok(value) = std::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };
        match (
            value.find("<!--StartFragment-->"),
            value.find("<!--EndFragment-->"),
        ) {
            (Some(start), Some(end)) if end >= start => {
                value.as_bytes()[start + "<!--StartFragment-->".len()..end].to_vec()
            }
            _ => bytes.to_vec(),
        }
    }

    pub fn set_mime_data_static(data: &MimeData) -> Result<(), &'static str> {
        let mut prepared = Vec::with_capacity(data.data.len());
        for (mime_type, bytes) in &data.data {
            let (format, payload) = if mime_type == "text/plain" {
                let text =
                    String::from_utf8(bytes.clone()).map_err(|_| "text/plain is not UTF-8")?;
                let utf16: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                (
                    CF_UNICODETEXT as u32,
                    utf16.iter().flat_map(|unit| unit.to_ne_bytes()).collect(),
                )
            } else if mime_type == "text/html" {
                (
                    Self::register_mime_format("HTML Format")?,
                    Self::cf_html_payload(
                        std::str::from_utf8(bytes).map_err(|_| "text/html is not UTF-8")?,
                    ),
                )
            } else {
                (Self::register_mime_format(mime_type)?, bytes.clone())
            };
            prepared.push((format, payload));
        }

        let _session = ClipboardSession::open_with_retry().ok_or("Failed to open clipboard")?;
        unsafe { EmptyClipboard() };
        for (format, payload) in prepared {
            let hmem = GlobalMem::alloc_copy(&payload)?;
            if unsafe { SetClipboardData(format, hmem.as_raw()) }.is_null() {
                return Err("SetClipboardData failed");
            }
            // The clipboard owns the block now.
            let _ = hmem.into_raw();
        }
        Ok(())
    }
    /// Whether `format` is backed by an `HGLOBAL`. GDI-handle formats (bitmaps, palettes,
    /// metafiles and the private GDI object range) are not, and `GlobalLock` fails on them.
    fn is_global_memory_format(format: u32) -> bool {
        const CF_METAFILEPICT: u32 = 3;
        const CF_BITMAP: u32 = 2;
        const CF_PALETTE: u32 = 9;
        const CF_ENHMETAFILE: u32 = 14;
        const CF_DSPBITMAP: u32 = 0x0082;
        const CF_DSPMETAFILEPICT: u32 = 0x0083;
        const CF_DSPENHMETAFILE: u32 = 0x008E;
        const CF_GDIOBJ_RANGE: std::ops::RangeInclusive<u32> = 0x0300..=0x03FF;
        !matches!(
            format,
            CF_BITMAP
                | CF_METAFILEPICT
                | CF_PALETTE
                | CF_ENHMETAFILE
                | CF_DSPBITMAP
                | CF_DSPMETAFILEPICT
                | CF_DSPENHMETAFILE
        ) && !CF_GDIOBJ_RANGE.contains(&format)
    }

    pub fn mime_data_static() -> Result<MimeData, &'static str> {
        let session = ClipboardSession::open_with_retry().ok_or("Failed to open clipboard")?;
        let mut result = MimeData::new();
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            if !Self::is_global_memory_format(format) {
                continue;
            }
            let hmem = unsafe { GetClipboardData(format) };
            if hmem.is_null() {
                continue;
            }
            let ptr = unsafe { GlobalLock(hmem) as *const u8 };
            if ptr.is_null() {
                // One unreadable format must not discard the others.
                continue;
            }
            let size = unsafe { GlobalSize(hmem) };
            let bytes = unsafe { std::slice::from_raw_parts(ptr, size).to_vec() };
            unsafe { GlobalUnlock(hmem) };

            if format == CF_UNICODETEXT as u32 {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_ne_bytes([chunk[0], chunk[1]]))
                    .take_while(|unit| *unit != 0)
                    .collect();
                result.set_text(&String::from_utf16_lossy(&units));
            } else if let Some(name) = Self::get_format_name(format) {
                if name == "HTML Format" {
                    result.set_data("text/html", Self::html_from_cf_html(&bytes));
                } else {
                    result.set_data(name, bytes);
                }
            }
        }
        drop(session);
        Ok(result)
    }
    pub fn set_text_static(text: &str) -> Result<(), &'static str> {
        let bytes: Vec<u8> = text
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_ne_bytes)
            .collect();
        let hmem = GlobalMem::alloc_copy(&bytes)?;

        let _session = ClipboardSession::open_with_retry().ok_or("Failed to open clipboard")?;

        unsafe { EmptyClipboard() };
        if unsafe { SetClipboardData(CF_UNICODETEXT as u32, hmem.as_raw()) }.is_null() {
            return Err("SetClipboardData failed");
        }
        // The clipboard owns the block now.
        let _ = hmem.into_raw();
        Ok(())
    }

    pub fn text_static() -> Result<String, &'static str> {
        let _session = ClipboardSession::open_with_retry().ok_or("Failed to open clipboard")?;

        unsafe {
            let hmem = GetClipboardData(CF_UNICODETEXT as u32);
            if hmem.is_null() {
                return Ok(String::new());
            }

            let ptr = GlobalLock(hmem) as *const u16;
            if ptr.is_null() {
                return Err("GlobalLock failed to read");
            }

            // Never scan past the allocation when the producer omitted the terminator.
            let max_units = GlobalSize(hmem) / std::mem::size_of::<u16>();
            let mut len = 0;
            while len < max_units && *ptr.add(len) != 0 {
                len += 1;
            }

            let slice = std::slice::from_raw_parts(ptr, len);
            let s = String::from_utf16_lossy(slice);

            GlobalUnlock(hmem);
            Ok(s)
        }
    }

    pub fn clear_static() -> Result<(), &'static str> {
        let _session = ClipboardSession::open_with_retry().ok_or("Failed to open clipboard")?;
        unsafe { EmptyClipboard() };
        Ok(())
    }
}

#[cfg(windows)]
impl PlatformClipboard for Win32Clipboard {
    fn set_mime_data(&self, data: &MimeData) -> Result<(), &'static str> {
        Self::set_mime_data_static(data)
    }

    fn mime_data(&self) -> Result<MimeData, &'static str> {
        Self::mime_data_static()
    }

    fn clear(&self) -> Result<(), &'static str> {
        Self::clear_static()
    }
}

#[cfg(windows)]
pub type Clipboard = Win32Clipboard;

#[cfg(not(windows))]
pub type Clipboard = GenericClipboard;

#[cfg(windows)]
impl Win32Clipboard {
    pub fn set_text(text: &str) -> Result<(), &'static str> {
        Self::set_text_static(text)
    }

    pub fn text() -> Result<String, &'static str> {
        Self::text_static()
    }

    pub fn clear() -> Result<(), &'static str> {
        Self::clear_static()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::ptr;

    #[cfg(windows)]
    #[test]
    fn test_win32_clipboard_trait_and_static_cycle() {
        let clipboard = Win32Clipboard;
        let mut data = MimeData::new();
        data.set_text("plain \u{1f680}");
        data.set_html("<strong>rich</strong>");
        data.set_data("image/png", [0, 1, 2, 255]);
        data.set_data("application/x-ferrokit-test", [9, 0, 8]);

        clipboard.set_mime_data(&data).unwrap();
        let actual = clipboard.mime_data().unwrap();
        assert_eq!(actual.text().as_deref(), Some("plain \u{1f680}"));
        assert_eq!(actual.html().as_deref(), Some("<strong>rich</strong>"));
        assert_eq!(actual.data("image/png"), Some(&[0, 1, 2, 255][..]));
        assert_eq!(
            actual.data("application/x-ferrokit-test"),
            Some(&[9, 0, 8][..])
        );

        Win32Clipboard::clear().expect("failed to clear");
        assert!(clipboard.mime_data().unwrap().formats().next().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn test_win32_clipboard_skips_gdi_formats_and_bounds_unterminated_text() {
        use windows_sys::Win32::Graphics::Gdi::CreateBitmap;
        use windows_sys::Win32::System::Memory::{GlobalAlloc, GMEM_MOVEABLE};
        // Single test: both cases share the process-wide clipboard.
        let hbm = unsafe { CreateBitmap(1, 1, 1, 32, ptr::null()) };
        assert!(!hbm.is_null());
        // Unterminated CF_UNICODETEXT: 8 non-zero units, exactly the allocation size.
        let units = [0x41u16; 8];
        unsafe {
            let hmem = GlobalAlloc(GMEM_MOVEABLE, 16);
            assert!(!hmem.is_null());
            let p = GlobalLock(hmem) as *mut u16;
            ptr::copy_nonoverlapping(units.as_ptr(), p, 8);
            GlobalUnlock(hmem);
            let session = ClipboardSession::open_with_retry().expect("open clipboard");
            EmptyClipboard();
            assert!(!SetClipboardData(CF_UNICODETEXT as u32, hmem).is_null());
            // CF_BITMAP (2) is a GDI handle, not an HGLOBAL.
            assert!(!SetClipboardData(2, hbm as _).is_null());
            drop(session);
        }
        let data = Win32Clipboard::mime_data_static().expect("GDI format must not fail the read");
        // Windows may force a terminator into the last unit, so 7 or 8 units
        // come back; anything longer means the read ran past the allocation.
        let bounded = |text: &str| matches!(text, "AAAAAAA" | "AAAAAAAA");
        assert!(
            bounded(data.text().as_deref().unwrap_or("")),
            "{:?}",
            data.text()
        );
        let text = Win32Clipboard::text_static().unwrap();
        assert!(bounded(&text), "{text:?}");
        Win32Clipboard::clear().unwrap();
    }

    #[test]
    fn generic_clipboard_round_trips_multiple_mime_types_and_clears() {
        let clipboard = GenericClipboard::new();
        let mut data = MimeData::new();
        data.set_text("plain text");
        data.set_html("<b>rich</b>");
        data.set_data("image/png", [0, 1, 2, 255]);
        data.set_data("application/x-example", [7, 8]);
        data.set_data("application/custom+binary", [0, 255]);

        clipboard.set_mime_data(&data).unwrap();
        let actual = clipboard.mime_data().unwrap();
        assert_eq!(actual.text().as_deref(), Some("plain text"));
        assert_eq!(actual.html().as_deref(), Some("<b>rich</b>"));
        assert_eq!(actual.data("image/png"), Some(&[0, 1, 2, 255][..]));
        assert_eq!(actual.data("application/x-example"), Some(&[7, 8][..]));
        assert_eq!(
            actual.data("application/custom+binary"),
            Some(&[0, 255][..])
        );
        assert!(actual.has_format("text/plain"));
        assert!(actual.formats().any(|format| format == "text/html"));

        clipboard.clear().unwrap();
        assert!(clipboard.mime_data().unwrap().formats().next().is_none());
    }
    #[test]
    fn test_generic_clipboard_read_write_clear() {
        let clipboard = GenericClipboard::new();
        assert_eq!(clipboard.text().unwrap(), "");

        clipboard.set_text("Cross-platform clipboard").unwrap();
        assert_eq!(clipboard.text().unwrap(), "Cross-platform clipboard");

        clipboard.clear().unwrap();
        assert_eq!(clipboard.text().unwrap(), "");
    }
}
