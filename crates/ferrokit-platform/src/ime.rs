// SPDX-License-Identifier: LGPL-3.0-only
#[allow(unused_imports)]
use ferrokit_gui::geometry::primitives::Point;

/// Representation of IME composition context.
#[derive(Debug, Clone, Default)]
pub struct CompositionContext {
    pub is_composing: bool,
    pub composition_string: String,
    pub cursor_position: i32,
}

#[cfg(windows)]
pub struct Win32InputContext {
    hwnd: windows_sys::Win32::Foundation::HWND,
    context: CompositionContext,
    candidate_pos: Point,
}

/// Associates (`true`) or dissociates (`false`) the window's default IME input context.
///
/// A window starts without an input context and gets it back when a text widget asks for an input
/// method (`set_micro_focus`), as `QWindowsInputContext` does. The first activation of a window
/// that still has its default context initializes the process' text services, which measured
/// ~800 ms inside `DefWindowProc(WM_ACTIVATE)` on a machine with Chinese IMEs installed, even for a bare Win32 window.
#[cfg(windows)]
// `hwnd` is passed only to `ImmAssociateContextEx`, which validates it; nothing dereferences it.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub fn set_window_ime_enabled(hwnd: windows_sys::Win32::Foundation::HWND, enabled: bool) {
    use windows_sys::Win32::UI::Input::Ime::{IACE_DEFAULT, ImmAssociateContextEx};
    let _t =
        ferrokit_gui::startup_trace::span(|| format!("ImmAssociateContextEx(enabled={enabled})"));
    // SAFETY: `ImmAssociateContextEx` takes only by-value arguments; `hwnd` is validated by imm32
    // (invalid handle -> returns FALSE), and a null `HIMC` is the documented value when `IACE_DEFAULT`
    // or 0 is passed (per the ImmAssociateContextEx documentation). Nothing is dereferenced.
    unsafe {
        ImmAssociateContextEx(
            hwnd,
            std::ptr::null_mut(),
            if enabled { IACE_DEFAULT } else { 0 },
        );
    }
}

#[cfg(windows)]
impl Win32InputContext {
    pub fn new(hwnd: windows_sys::Win32::Foundation::HWND) -> Self {
        Self {
            hwnd,
            context: CompositionContext::default(),
            candidate_pos: Point::new(0, 0),
        }
    }

    /// Sets the IME candidate list position relative to the window client area.
    pub fn set_micro_focus(&mut self, pos: Point) {
        use windows_sys::Win32::UI::Input::Ime::{
            CANDIDATEFORM, CFS_CANDIDATEPOS, ImmGetContext, ImmReleaseContext,
            ImmSetCandidateWindow,
        };

        self.candidate_pos = pos;
        if self.hwnd.is_null() {
            return;
        }

        // SAFETY: `self.hwnd` was checked non-null above and is validated by imm32. `himc` is checked
        // non-null before use, `&form` is a live, fully initialized `CANDIDATEFORM` (the all-zero
        // `RECT` is valid for a plain integer struct) that outlives the call, and `himc` is released
        // with the same `hwnd` it was obtained from.
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if !himc.is_null() {
                let form = CANDIDATEFORM {
                    dwIndex: 0,
                    dwStyle: CFS_CANDIDATEPOS,
                    ptCurrentPos: windows_sys::Win32::Foundation::POINT { x: pos.x, y: pos.y },
                    rcArea: std::mem::zeroed(),
                };
                ImmSetCandidateWindow(himc, &form);
                ImmReleaseContext(self.hwnd, himc);
            }
        }
    }

    /// Handles WM_IME_STARTCOMPOSITION
    pub fn handle_start_composition(&mut self) -> bool {
        self.context.is_composing = true;
        self.context.composition_string.clear();
        self.context.cursor_position = 0;
        true
    }

    /// Handles WM_IME_COMPOSITION. Returns `(commit_string, preedit_string, cursor_pos)`.
    pub fn handle_composition(&mut self, lparam: isize) -> (Option<String>, Option<String>, i32) {
        use windows_sys::Win32::UI::Input::Ime::{
            GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, ImmGetCompositionStringW, ImmGetContext,
            ImmReleaseContext,
        };

        if self.hwnd.is_null() {
            return (None, None, 0);
        }

        // SAFETY: `self.hwnd` was checked non-null just above and is validated by imm32; the returned
        // `himc` is null-checked before use and released at the end of this function.
        let himc = unsafe { ImmGetContext(self.hwnd) };
        if himc.is_null() {
            return (None, None, 0);
        }

        let mut commit_str = None;
        let mut preedit_str = None;
        let mut cursor_pos = 0;

        let flags = lparam as u32;

        // SAFETY: `himc` is the non-null context from `ImmGetContext(self.hwnd)` above, released once
        // at the end of this block with the same `hwnd`. Each `buffer` holds `bytes_needed / 2` u16s
        // (the byte count imm32 itself reported, always even for UTF-16), so the write of at most
        // `bytes_needed` bytes stays in bounds; the null/0 calls are the documented size query.
        unsafe {
            // Read intermediate composition string if present
            if (flags & GCS_COMPSTR) != 0 {
                let bytes_needed =
                    ImmGetCompositionStringW(himc, GCS_COMPSTR, std::ptr::null_mut(), 0);
                if bytes_needed > 0 {
                    let chars_count = (bytes_needed as usize) / 2;
                    let mut buffer: Vec<u16> = vec![0; chars_count];
                    ImmGetCompositionStringW(
                        himc,
                        GCS_COMPSTR,
                        buffer.as_mut_ptr() as *mut _,
                        bytes_needed as u32,
                    );
                    let preedit = String::from_utf16_lossy(&buffer);
                    self.context.composition_string = preedit.clone();
                    preedit_str = Some(preedit);
                } else {
                    self.context.composition_string.clear();
                    preedit_str = Some(String::new());
                }

                if (flags & GCS_CURSORPOS) != 0 {
                    cursor_pos =
                        ImmGetCompositionStringW(himc, GCS_CURSORPOS, std::ptr::null_mut(), 0);
                    self.context.cursor_position = cursor_pos;
                }
            }

            // Read committed result string if finalized
            if (flags & GCS_RESULTSTR) != 0 {
                let bytes_needed =
                    ImmGetCompositionStringW(himc, GCS_RESULTSTR, std::ptr::null_mut(), 0);
                if bytes_needed > 0 {
                    let chars_count = (bytes_needed as usize) / 2;
                    let mut buffer: Vec<u16> = vec![0; chars_count];
                    ImmGetCompositionStringW(
                        himc,
                        GCS_RESULTSTR,
                        buffer.as_mut_ptr() as *mut _,
                        bytes_needed as u32,
                    );
                    commit_str = Some(String::from_utf16_lossy(&buffer));
                }
            }

            ImmReleaseContext(self.hwnd, himc);
        }

        (commit_str, preedit_str, cursor_pos)
    }

    /// Handles WM_IME_ENDCOMPOSITION
    pub fn handle_end_composition(&mut self) -> bool {
        self.context.is_composing = false;
        self.context.composition_string.clear();
        self.context.cursor_position = 0;
        true
    }

    pub fn is_composing(&self) -> bool {
        self.context.is_composing
    }
}
