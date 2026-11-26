// SPDX-License-Identifier: LGPL-3.0-only
use crate::surface::PlatformSurface;
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use x11rb::connection::{Connection, RequestConnection};
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{ConnectionExt as _, CreateGCAux, ImageFormat};
#[cfg(target_os = "linux")]
use x11rb::rust_connection::RustConnection;

#[cfg(target_os = "linux")]
struct X11LiveSurface {
    conn: Arc<RustConnection>,
    gc: u32,
    depth: u8,
}

#[cfg(target_os = "linux")]
impl Drop for X11LiveSurface {
    fn drop(&mut self) {
        let _ = self.conn.free_gc(self.gc);
        let _ = self.conn.flush();
    }
}

pub struct X11ShmSurface {
    window_id: u32,
    width: u32,
    height: u32,
    shm_seg: u32,
    shm_buffer: Vec<u8>,
    #[cfg(target_os = "linux")]
    live: Option<X11LiveSurface>,
}

impl X11ShmSurface {
    pub fn new(window_id: u32, width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }

        let buffer_size = (width * height * 4) as usize;
        let shm_buffer = vec![0u8; buffer_size];

        Ok(Self {
            window_id,
            width,
            height,
            shm_seg: 1,
            shm_buffer,
            #[cfg(target_os = "linux")]
            live: None,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn new_with_connection(
        conn: Arc<RustConnection>,
        window_id: u32,
        depth: u8,
        width: u32,
        height: u32,
    ) -> Result<Self, &'static str> {
        let mut surface = Self::new(window_id, width, height)?;
        let gc = conn
            .generate_id()
            .map_err(|_| "Failed to generate X11 GC id")?;
        conn.create_gc(gc, window_id, &CreateGCAux::new())
            .map_err(|_| "Failed to create X11 GC")?;
        surface.live = Some(X11LiveSurface { conn, gc, depth });
        Ok(surface)
    }

    #[cfg(target_os = "linux")]
    fn upload_rect(&self, rect: Rect) -> Result<(), &'static str> {
        let Some(live) = &self.live else {
            return Ok(());
        };
        if rect.is_empty() {
            return Ok(());
        }
        let max_bytes = live
            .conn
            .maximum_request_bytes()
            .saturating_sub(64)
            .max(4096);
        let row_bytes = (rect.width as usize) * 4;
        let max_rows = (max_bytes / row_bytes.max(1))
            .max(1)
            .min(rect.height as usize);
        let stride = (self.width as usize) * 4;

        let mut y_off = 0usize;
        let total_h = rect.height as usize;
        while y_off < total_h {
            let chunk_h = (total_h - y_off).min(max_rows);
            let mut bgra = Vec::with_capacity(chunk_h * row_bytes);
            for row in 0..chunk_h {
                let src_y = (rect.y as usize) + y_off + row;
                let start = src_y * stride + (rect.x as usize) * 4;
                let end = start + row_bytes;
                if end > self.shm_buffer.len() {
                    break;
                }
                for &px in self.shm_buffer[start..end].as_chunks::<4>().0.iter() {
                    bgra.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
                }
            }
            if !bgra.is_empty() {
                live.conn
                    .put_image(
                        ImageFormat::Z_PIXMAP,
                        self.window_id,
                        live.gc,
                        rect.width as u16,
                        chunk_h as u16,
                        rect.x as i16,
                        (rect.y as usize + y_off) as i16,
                        0,
                        live.depth,
                        &bgra,
                    )
                    .map_err(|_| "X11 put_image failed")?;
            }
            y_off += chunk_h;
        }
        live.conn.flush().map_err(|_| "X11 flush failed")?;
        Ok(())
    }

    pub fn shm_seg(&self) -> u32 {
        self.shm_seg
    }

    pub fn window_id(&self) -> u32 {
        self.window_id
    }

    pub fn shm_buffer(&self) -> &[u8] {
        &self.shm_buffer
    }
}

impl PlatformSurface for X11ShmSurface {
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

        self.width = width;
        self.height = height;
        let new_size = (width * height * 4) as usize;
        self.shm_buffer.resize(new_size, 0);

        Ok(())
    }

    fn present(&mut self, pixmap: &mut Pixmap, _opacity: f32) -> Result<(), &'static str> {
        let p_width = pixmap.physical_width();
        let p_height = pixmap.physical_height();

        if p_width != self.width || p_height != self.height {
            self.resize(p_width, p_height)?;
        }

        let src_data = pixmap.data();
        let target_len = (self.width * self.height * 4) as usize;
        let copy_len = src_data.len().min(target_len);

        self.shm_buffer[..copy_len].copy_from_slice(&src_data[..copy_len]);

        #[cfg(target_os = "linux")]
        self.upload_rect(Rect::new(0, 0, self.width as i32, self.height as i32))?;

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
            if offset + copy_bytes <= self.shm_buffer.len() && offset + copy_bytes <= src_data.len()
            {
                self.shm_buffer[offset..offset + copy_bytes]
                    .copy_from_slice(&src_data[offset..offset + copy_bytes]);
            }
        }

        #[cfg(target_os = "linux")]
        self.upload_rect(clipped)?;

        Ok(())
    }
}
