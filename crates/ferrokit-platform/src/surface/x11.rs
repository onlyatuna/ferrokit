// SPDX-License-Identifier: LGPL-3.0-only
use crate::surface::PlatformSurface;
use ferrokit_gui::geometry::Rect;
use ferrokit_gui::paint::Pixmap;

pub struct X11ShmSurface {
    window_id: u32,
    width: u32,
    height: u32,
    shm_seg: u32,
    shm_buffer: Vec<u8>,
}

unsafe impl Send for X11ShmSurface {}
unsafe impl Sync for X11ShmSurface {}

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
        })
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

        Ok(())
    }
}
