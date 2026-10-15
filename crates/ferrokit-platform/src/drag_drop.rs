// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_gui::geometry::primitives::Point;
#[allow(unused_imports)]
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropAction {
    Ignore = 0,
    Copy = 1,
    Move = 2,
    Link = 4,
}

#[derive(Debug, Clone)]
pub enum DropEvent {
    Enter {
        pos: Point,
        formats: Vec<String>,
        effect: u32,
    },
    Over {
        pos: Point,
        effect: u32,
    },
    Leave,
    Drop {
        pos: Point,
        formats: Vec<String>,
        data: Vec<(String, Vec<u8>)>,
        effect: u32,
    },
}

#[cfg(windows)]
pub mod win32_ole {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{HWND, POINTL, S_OK};
    use windows_sys::Win32::System::Ole::{RegisterDragDrop, RevokeDragDrop, DROPEFFECT_COPY};

    pub type HRESULT = i32;

    #[repr(C)]
    pub struct IDropTargetVtbl {
        pub query_interface: unsafe extern "system" fn(
            *mut c_void,
            *const windows_sys::core::GUID,
            *mut *mut c_void,
        ) -> HRESULT,
        pub add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        pub release: unsafe extern "system" fn(*mut c_void) -> u32,
        pub drag_enter:
            unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
        pub drag_over: unsafe extern "system" fn(*mut c_void, u32, POINTL, *mut u32) -> HRESULT,
        pub drag_leave: unsafe extern "system" fn(*mut c_void) -> HRESULT,
        pub drop:
            unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
    }

    #[repr(C)]
    pub struct OleDropTarget {
        pub vtbl: *const IDropTargetVtbl,
        pub ref_count: AtomicU32,
        pub hwnd: HWND,
        pub callback: Box<dyn Fn(DropEvent) + Send + Sync>,
    }

    impl OleDropTarget {
        pub fn new<F>(hwnd: HWND, callback: F) -> *mut Self
        where
            F: Fn(DropEvent) + Send + Sync + 'static,
        {
            let target = Box::new(Self {
                vtbl: &OLE_DROP_TARGET_VTBL,
                ref_count: AtomicU32::new(1),
                hwnd,
                callback: Box::new(callback),
            });
            Box::into_raw(target)
        }
    }

    unsafe extern "system" fn query_interface(
        this: *mut c_void,
        riid: *const windows_sys::core::GUID,
        ppv_object: *mut *mut c_void,
    ) -> HRESULT {
        const E_NOINTERFACE: HRESULT = -2147467262; // 0x80004002
        if ppv_object.is_null() || riid.is_null() {
            return E_NOINTERFACE;
        }

        let target = this as *mut OleDropTarget;
        (*target).ref_count.fetch_add(1, Ordering::SeqCst);
        *ppv_object = this;
        S_OK
    }

    unsafe extern "system" fn add_ref(this: *mut c_void) -> u32 {
        let target = this as *mut OleDropTarget;
        (*target).ref_count.fetch_add(1, Ordering::SeqCst) + 1
    }

    unsafe extern "system" fn release(this: *mut c_void) -> u32 {
        let target = this as *mut OleDropTarget;
        let count = (*target).ref_count.fetch_sub(1, Ordering::SeqCst) - 1;
        if count == 0 {
            let _ = Box::from_raw(target);
        }
        count
    }

    unsafe extern "system" fn drag_enter(
        this: *mut c_void,
        _p_data_obj: *mut c_void,
        _grf_key_state: u32,
        pt: POINTL,
        pdw_effect: *mut u32,
    ) -> HRESULT {
        let target = &*(this as *mut OleDropTarget);
        if !pdw_effect.is_null() {
            *pdw_effect = DROPEFFECT_COPY;
        }
        // Python/PySide6 contains a raising handler; unwinding out of an extern "system" fn aborts.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Enter {
                pos: Point::new(pt.x, pt.y),
                formats: vec!["text/plain".to_string()],
                effect: DROPEFFECT_COPY,
            })
        }));
        S_OK
    }

    unsafe extern "system" fn drag_over(
        this: *mut c_void,
        _grf_key_state: u32,
        pt: POINTL,
        pdw_effect: *mut u32,
    ) -> HRESULT {
        let target = &*(this as *mut OleDropTarget);
        if !pdw_effect.is_null() {
            *pdw_effect = DROPEFFECT_COPY;
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Over {
                pos: Point::new(pt.x, pt.y),
                effect: DROPEFFECT_COPY,
            })
        }));
        S_OK
    }

    unsafe extern "system" fn drag_leave(this: *mut c_void) -> HRESULT {
        let target = &*(this as *mut OleDropTarget);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Leave)
        }));
        S_OK
    }

    unsafe extern "system" fn drop(
        this: *mut c_void,
        _p_data_obj: *mut c_void,
        _grf_key_state: u32,
        pt: POINTL,
        pdw_effect: *mut u32,
    ) -> HRESULT {
        let target = &*(this as *mut OleDropTarget);
        if !pdw_effect.is_null() {
            *pdw_effect = DROPEFFECT_COPY;
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (target.callback)(DropEvent::Drop {
                pos: Point::new(pt.x, pt.y),
                formats: vec!["text/plain".to_string()],
                data: vec![("text/plain".to_string(), b"dropped".to_vec())],
                effect: DROPEFFECT_COPY,
            })
        }));
        S_OK
    }

    static OLE_DROP_TARGET_VTBL: IDropTargetVtbl = IDropTargetVtbl {
        query_interface,
        add_ref,
        release,
        drag_enter,
        drag_over,
        drag_leave,
        drop,
    };

    pub fn register_drop_target<F>(
        hwnd: HWND,
        callback: F,
    ) -> Result<*mut OleDropTarget, &'static str>
    where
        F: Fn(DropEvent) + Send + Sync + 'static,
    {
        use windows_sys::Win32::System::Ole::OleInitialize;
        unsafe {
            OleInitialize(std::ptr::null_mut());
            let target_ptr = OleDropTarget::new(hwnd, callback);
            let hr = RegisterDragDrop(hwnd, target_ptr as *mut _);
            if hr == S_OK {
                Ok(target_ptr)
            } else {
                let _ = Box::from_raw(target_ptr);
                Err("RegisterDragDrop failed")
            }
        }
    }

    pub fn revoke_drop_target(hwnd: HWND, target: *mut OleDropTarget) {
        unsafe {
            RevokeDragDrop(hwnd);
            if !target.is_null() {
                release(target as *mut c_void);
            }
        }
    }
}
