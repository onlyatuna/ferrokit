// SPDX-License-Identifier: LGPL-3.0-only
use crate::surface::PlatformSurface;
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;
#[cfg(target_os = "linux")]
use std::fs::{File, OpenOptions};
#[cfg(target_os = "linux")]
use std::os::unix::fs::FileExt;
#[cfg(target_os = "linux")]
use std::os::unix::io::AsFd;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_shm, wl_shm::WlShm, wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
#[cfg(target_os = "linux")]
use wayland_client::{Connection, QueueHandle};

#[cfg(target_os = "linux")]
static SHM_FILE_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

#[cfg(target_os = "linux")]
struct WaylandLiveSurface {
    conn: Connection,
    wl_surface: WlSurface,
    wl_shm: WlShm,
    qh: QueueHandle<crate::window::wayland::WaylandAppState>,
    pool_file: Option<File>,
    pool: Option<WlShmPool>,
    pool_capacity: usize,
    buffer: Option<WlBuffer>,
    buf_width: u32,
    buf_height: u32,
}

#[cfg(target_os = "linux")]
impl Drop for WaylandLiveSurface {
    fn drop(&mut self) {
        if let Some(buf) = self.buffer.take() {
            buf.destroy();
        }
        if let Some(pool) = self.pool.take() {
            pool.destroy();
        }
        let _ = self.conn.flush();
    }
}

#[cfg(target_os = "linux")]
impl WaylandLiveSurface {
    fn create_unlinked_shm_file(byte_len: usize) -> Result<File, &'static str> {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let pid = std::process::id();
        let seq = SHM_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("ferrokit-wl-shm-{pid}-{seq}"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| "Failed to create Wayland SHM backing file")?;
        let _ = std::fs::remove_file(&path);
        file.set_len(byte_len as u64)
            .map_err(|_| "Failed to size Wayland SHM backing file")?;
        Ok(file)
    }

    fn ensure_buffer(&mut self, width: u32, height: u32) -> Result<(), &'static str> {
        let byte_len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4)
            .max(4);
        if self.pool_file.is_none() || self.pool.is_none() {
            let file = Self::create_unlinked_shm_file(byte_len)?;
            let pool = self
                .wl_shm
                .create_pool(file.as_fd(), byte_len as i32, &self.qh, ());
            self.pool_file = Some(file);
            self.pool = Some(pool);
            self.pool_capacity = byte_len;
        } else if byte_len > self.pool_capacity {
            if let Some(file) = self.pool_file.as_ref() {
                file.set_len(byte_len as u64)
                    .map_err(|_| "Failed to grow Wayland SHM backing file")?;
            }
            if let Some(pool) = self.pool.as_ref() {
                pool.resize(byte_len as i32);
            }
            self.pool_capacity = byte_len;
        }

        if self.buffer.is_none() || self.buf_width != width || self.buf_height != height {
            if let Some(old) = self.buffer.take() {
                old.destroy();
            }
            let pool = self.pool.as_ref().ok_or("Missing Wayland SHM pool")?;
            let stride = (width * 4) as i32;
            let buf = pool.create_buffer(
                0,
                width as i32,
                height as i32,
                stride,
                wl_shm::Format::Argb8888,
                &self.qh,
                (),
            );
            self.buffer = Some(buf);
            self.buf_width = width;
            self.buf_height = height;
        }

        Ok(())
    }

    fn upload_and_commit(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
        opacity: f32,
        damage: Rect,
    ) -> Result<(), &'static str> {
        if width == 0 || height == 0 {
            return Ok(());
        }
        self.ensure_buffer(width, height)?;

        let byte_len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        let src_len = rgba.len().min(byte_len);
        let mut argb = vec![0u8; byte_len];
        let alpha_scale = opacity.clamp(0.0, 1.0);
        let scale_alpha = alpha_scale < 0.999;

        for (src, dst) in rgba[..src_len]
            .as_chunks::<4>()
            .0
            .iter()
            .zip(argb[..src_len].as_chunks_mut::<4>().0.iter_mut())
        {
            if scale_alpha {
                dst[0] = ((src[2] as f32) * alpha_scale).round() as u8;
                dst[1] = ((src[1] as f32) * alpha_scale).round() as u8;
                dst[2] = ((src[0] as f32) * alpha_scale).round() as u8;
                dst[3] = ((src[3] as f32) * alpha_scale).round() as u8;
            } else {
                dst[0] = src[2];
                dst[1] = src[1];
                dst[2] = src[0];
                dst[3] = src[3];
            }
        }

        if let Some(file) = self.pool_file.as_ref() {
            file.write_all_at(&argb, 0)
                .map_err(|_| "Failed to write Wayland SHM buffer")?;
        }
        if let Some(buf) = self.buffer.as_ref() {
            self.wl_surface.attach(Some(buf), 0, 0);
            self.wl_surface.damage_buffer(
                damage.x,
                damage.y,
                damage.width.max(1),
                damage.height.max(1),
            );
            self.wl_surface.commit();
            let _ = self.conn.flush();
        }
        Ok(())
    }
}

pub struct WaylandShmSurface {
    surface_id: u32,
    width: u32,
    height: u32,
    shm_buffer: Vec<u8>,
    is_busy: AtomicBool,
    #[cfg(target_os = "linux")]
    live: Option<WaylandLiveSurface>,
}

impl WaylandShmSurface {
    pub fn new(surface_id: u32, width: u32, height: u32) -> Result<Self, &'static str> {
        if width == 0 || height == 0 {
            return Err("Width and height must be greater than 0");
        }

        let buffer_size = (width * height * 4) as usize;
        let shm_buffer = vec![0u8; buffer_size];

        Ok(Self {
            surface_id,
            width,
            height,
            shm_buffer,
            is_busy: AtomicBool::new(false),
            #[cfg(target_os = "linux")]
            live: None,
        })
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn new_with_wayland(
        surface_id: u32,
        width: u32,
        height: u32,
        conn: Connection,
        wl_surface: WlSurface,
        wl_shm: WlShm,
        qh: QueueHandle<crate::window::wayland::WaylandAppState>,
    ) -> Result<Self, &'static str> {
        let mut surface = Self::new(surface_id, width, height)?;
        surface.live = Some(WaylandLiveSurface {
            conn,
            wl_surface,
            wl_shm,
            qh,
            pool_file: None,
            pool: None,
            pool_capacity: 0,
            buffer: None,
            buf_width: 0,
            buf_height: 0,
        });
        Ok(surface)
    }

    pub fn surface_id(&self) -> u32 {
        self.surface_id
    }

    pub fn shm_buffer(&self) -> &[u8] {
        &self.shm_buffer
    }

    #[inline]
    pub fn is_busy(&self) -> bool {
        self.is_busy.load(Ordering::Acquire)
    }

    #[inline]
    pub fn on_buffer_release(&self) {
        self.is_busy.store(false, Ordering::Release);
    }

    #[inline]
    pub fn mark_busy(&self) {
        self.is_busy.store(true, Ordering::Release);
    }
}

impl PlatformSurface for WaylandShmSurface {
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

    fn present(&mut self, pixmap: &mut Pixmap, opacity: f32) -> Result<(), &'static str> {
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
        if let Some(live) = self.live.as_mut() {
            live.upload_and_commit(
                &self.shm_buffer,
                self.width,
                self.height,
                opacity,
                Rect::new(0, 0, self.width as i32, self.height as i32),
            )?;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = opacity;

        self.mark_busy();

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
        if let Some(live) = self.live.as_mut() {
            live.upload_and_commit(&self.shm_buffer, self.width, self.height, opacity, clipped)?;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = opacity;

        self.mark_busy();

        Ok(())
    }
}
