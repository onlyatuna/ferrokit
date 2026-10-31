// SPDX-License-Identifier: LGPL-3.0-only
use crate::objc_runtime::{Id, ObjcMsg, Sel};
use crate::surface::PlatformSurface;
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;

#[cfg(target_os = "macos")]
mod core_graphics {
    use std::ffi::c_void;

    pub type CGDataProviderRef = *mut c_void;
    pub type CGImageRef = *mut c_void;
    pub type CGColorSpaceRef = *mut c_void;

    pub const K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST: u32 = 1;
    pub const K_CG_BITMAP_BYTE_ORDER_32_BIG: u32 = 4 << 12;
    pub const K_CG_RENDERING_INTENT_DEFAULT: u32 = 0;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        pub fn CGColorSpaceCreateDeviceRGB() -> CGColorSpaceRef;
        pub fn CGColorSpaceRelease(space: CGColorSpaceRef);
        pub fn CFDataCreateMutable(allocator: *const c_void, capacity: isize) -> *mut c_void;
        pub fn CFDataSetLength(data: *mut c_void, length: isize);
        pub fn CFDataGetMutableBytePtr(data: *mut c_void) -> *mut u8;
        pub fn CFRelease(object: *const c_void);
        pub fn CGDataProviderCreateWithCFData(data: *const c_void) -> CGDataProviderRef;
        pub fn CGDataProviderRelease(provider: CGDataProviderRef);
        pub fn CGImageCreate(
            width: usize,
            height: usize,
            bitsPerComponent: usize,
            bitsPerPixel: usize,
            bytesPerRow: usize,
            space: CGColorSpaceRef,
            bitmapInfo: u32,
            provider: CGDataProviderRef,
            decode: *const f64,
            shouldInterpolate: bool,
            intent: u32,
        ) -> CGImageRef;
        pub fn CGImageRelease(image: CGImageRef);
    }
}

pub struct CocoaLayerSurface {
    layer_id: usize,
    width: u32,
    height: u32,
    pixel_buffer: Vec<u8>,
}

impl CocoaLayerSurface {
    pub fn new(layer_id: usize, width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }

        let buffer_size = buffer_len(width, height)?;
        let pixel_buffer = vec![0u8; buffer_size];

        Ok(Self {
            layer_id,
            width,
            height,
            pixel_buffer,
        })
    }

    pub fn layer_id(&self) -> usize {
        self.layer_id
    }

    pub fn pixel_buffer(&self) -> &[u8] {
        &self.pixel_buffer
    }

    /// Window opacity is not applied here: like `QCocoaWindow::setOpacity`
    /// (qcocoawindow.mm:1206-1213) it lives only in `[NSWindow alphaValue]`
    /// (`CocoaNativeWindow::set_opacity`); the layer stays at opacity 1.
    pub fn commit_to_layer(&self) -> Result<(), &'static str> {
        let layer = Id(self.layer_id as *mut std::ffi::c_void);
        if layer.is_nil() {
            return Ok(());
        }

        #[cfg(target_os = "macos")]
        // SAFETY: `layer` is non-nil (checked above) and is the content view's backing layer,
        // captured by `CocoaNativeWindow::new` and owned by that view, which outlives the
        // surface's `present` calls [inferred: the layer keeps the same identity for the view's
        // life]. Every Core Graphics/CoreFoundation object here is created in this block and
        // released once after its last use: `data` (checked non-null before `CFRelease`, and
        // before `CFDataSetLength`/`CFDataGetMutableBytePtr`; the slice is `len` bytes because
        // `CFDataSetLength(data, len)` just sized it), `provider`, `cg_image` and `color_space`
        // (each null-checked before release; Core Graphics accepts NULL arguments by returning
        // NULL [inferred: Core Graphics documentation]). `CGImageCreate` is given the buffer
        // dimensions, whose `pixel_buffer` size equals `width * height * 4` (`buffer_len`),
        // matching bytes-per-row `width * 4`; the provider holds its own retained copy, so the
        // image never reads `pixel_buffer` after the call. `setContents:` takes an object and a
        // `CGImageRef` is accepted there; `contentsAreFlipped` returns BOOL; `CATransaction`
        // `begin`/`commit` take no arguments.
        unsafe {
            use core_graphics::*;
            let color_space = CGColorSpaceCreateDeviceRGB();
            // Qt never lets Core Animation hold memory it will write again: the flushed
            // IOSurface stays untouched while in use (qcocoabackingstore.mm:141,163,347,392-402).
            // The layer keeps this image after we return, so it gets its own copy of the frame;
            // `pixel_buffer` is overwritten by the next present and reallocated by `resize`.
            // The content view is flipped like QNSView (qnsview_drawing.mm:67-70), so its layer
            // flips contents when rendering ([CALayer contentsAreFlipped]); the copy then stores
            // the rows bottom-up, and the frame's first row still shows at the top.
            let len = self.pixel_buffer.len();
            let data = CFDataCreateMutable(std::ptr::null(), len as isize);
            if !data.is_null() {
                CFDataSetLength(data, len as isize);
                let copy = std::slice::from_raw_parts_mut(CFDataGetMutableBytePtr(data), len);
                if ObjcMsg::send_bool_return(layer, Sel::register("contentsAreFlipped")) {
                    let stride = (self.width * 4) as usize;
                    for (dst, src) in copy
                        .chunks_exact_mut(stride)
                        .zip(self.pixel_buffer.chunks_exact(stride).rev())
                    {
                        dst.copy_from_slice(src);
                    }
                } else {
                    copy.copy_from_slice(&self.pixel_buffer);
                }
            }
            let provider = CGDataProviderCreateWithCFData(data);
            if !data.is_null() {
                CFRelease(data);
            }

            // The pixmap is premultiplied RGBA8 in memory (tiny-skia), which Qt describes as
            // QImage::Format_RGBA8888_Premultiplied (qimage.cpp:6235) ->
            // kCGImageAlphaPremultipliedLast | kCGImageByteOrder32Big (qcoregraphics.mm:46-64).
            let bitmap_info = K_CG_IMAGE_ALPHA_PREMULTIPLIED_LAST | K_CG_BITMAP_BYTE_ORDER_32_BIG;
            let cg_image = CGImageCreate(
                self.width as usize,
                self.height as usize,
                8,
                32,
                (self.width * 4) as usize,
                color_space,
                bitmap_info,
                provider,
                std::ptr::null(),
                false,
                K_CG_RENDERING_INTENT_DEFAULT,
            );

            let ca_transaction = crate::objc_runtime::Class::get("CATransaction")
                .unwrap_or(crate::objc_runtime::Class::NIL);
            ObjcMsg::send_class_0(ca_transaction, Sel::register("begin"));
            ObjcMsg::send_bool(
                Id(ca_transaction.0),
                Sel::register("setDisableActions:"),
                true,
            );

            ObjcMsg::send_id(
                layer,
                Sel::register("setContents:"),
                Id(cg_image as *mut std::ffi::c_void),
            );
            ObjcMsg::send_class_0(ca_transaction, Sel::register("commit"));

            if !cg_image.is_null() {
                CGImageRelease(cg_image);
            }
            if !provider.is_null() {
                CGDataProviderRelease(provider);
            }
            if !color_space.is_null() {
                CGColorSpaceRelease(color_space);
            }
        }

        #[cfg(not(target_os = "macos"))]
        {
            // SAFETY: `layer` is the non-nil handle checked above. Only the mock runtime
            // compiles here: it records the `setContents:` argument (the buffer address) as an
            // opaque value and never dereferences it, so the borrow of `pixel_buffer` need not
            // outlive the call.
            unsafe {
                ObjcMsg::send_id(
                    layer,
                    Sel::register("setContents:"),
                    Id(self.pixel_buffer.as_ptr() as *mut std::ffi::c_void),
                )
            };
        }

        Ok(())
    }

    /// Copies the `rects` of `pixmap` (physical pixels) into the frame and shows it, committing
    /// once. A pixmap of another size replaces the whole frame, resizing it; `rects` that miss the
    /// frame show nothing new. The pixmap is only read, so no copy of it is needed.
    pub fn present_region(&mut self, pixmap: &Pixmap, rects: &[Rect]) -> Result<(), &'static str> {
        let (width, height) = (pixmap.physical_width(), pixmap.physical_height());
        let src = pixmap.data();
        if width != self.width || height != self.height {
            self.resize(width, height)?;
            let len = src.len().min(self.pixel_buffer.len());
            self.pixel_buffer[..len].copy_from_slice(&src[..len]);
            return self.commit_to_layer();
        }
        let frame = Rect::new(0, 0, width as i32, height as i32);
        let stride = width as usize * 4;
        let mut copied = false;
        for rect in rects {
            let clipped = frame.intersected(rect);
            if clipped.is_empty() {
                continue;
            }
            copied = true;
            let column = clipped.x as usize * 4;
            let row_bytes = clipped.width as usize * 4;
            for y in clipped.y..clipped.y + clipped.height {
                let offset = y as usize * stride + column;
                if let (Some(dst), Some(row)) = (
                    self.pixel_buffer.get_mut(offset..offset + row_bytes),
                    src.get(offset..offset + row_bytes),
                ) {
                    dst.copy_from_slice(row);
                }
            }
        }
        if copied {
            self.commit_to_layer()
        } else {
            Ok(())
        }
    }
}

impl PlatformSurface for CocoaLayerSurface {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }
        if self.width == width && self.height == height {
            return Ok(());
        }

        let new_size = buffer_len(width, height)?;
        self.width = width;
        self.height = height;
        self.pixel_buffer.resize(new_size, 0);

        Ok(())
    }

    fn present(&mut self, pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
        let p_width = pixmap.physical_width();
        let p_height = pixmap.physical_height();

        if p_width != self.width || p_height != self.height {
            self.resize(p_width, p_height)?;
        }

        let src_data = pixmap.data();
        let target_len = self.pixel_buffer.len();
        let copy_len = src_data.len().min(target_len);

        self.pixel_buffer[..copy_len].copy_from_slice(&src_data[..copy_len]);

        self.commit_to_layer()?;
        Ok(())
    }

    fn present_dirty(
        &mut self,
        pixmap: &mut Pixmap,
        opacity: f32,
        dirty: Rect,
    ) -> Result<(), &'static str> {
        let full_window_rect = Rect::new(0, 0, self.width as i32, self.height as i32);
        let clipped = full_window_rect.intersected(&dirty);
        if clipped.is_empty() {
            return Ok(());
        }

        if clipped == full_window_rect {
            return self.present(pixmap, opacity);
        }

        let stride = (self.width * 4) as usize;
        let src_data = pixmap.data();
        let dirty_x = clipped.x as usize;
        let dirty_w = clipped.width as usize;
        let copy_bytes = dirty_w * 4;

        for y in clipped.y..(clipped.y + clipped.height) {
            let y = y as usize;
            let offset = y * stride + dirty_x * 4;
            if offset + copy_bytes <= self.pixel_buffer.len()
                && offset + copy_bytes <= src_data.len()
            {
                self.pixel_buffer[offset..offset + copy_bytes]
                    .copy_from_slice(&src_data[offset..offset + copy_bytes]);
            }
        }

        self.commit_to_layer()?;
        Ok(())
    }
}

/// `width * height * 4` without the u32 overflow of the naive product.
fn buffer_len(width: u32, height: u32) -> Result<usize, &'static str> {
    if width > i32::MAX as u32 || height > i32::MAX as u32 {
        return Err("Surface size overflows");
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
        .filter(|&n| n <= isize::MAX as usize)
        .ok_or("Surface size overflows")
}
