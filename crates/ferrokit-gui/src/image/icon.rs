// SPDX-License-Identifier: LGPL-3.0-only
//! Multi-resolution, multi-state icon system matching Qt 6 `QIcon`.

use crate::geometry::primitives::Size;
use crate::image::image::{AspectRatioMode, Image, TransformationMode};
use crate::paint::pixmap::Pixmap;

/// Display mode for an icon matching `QIcon::Mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IconMode {
    /// Default display mode when widget is enabled and inactive.
    #[default]
    Normal,
    /// Display mode when widget is disabled (grayed out).
    Disabled,
    /// Display mode when widget is active or hovered.
    Active,
    /// Display mode when widget or item is selected.
    Selected,
}

/// State of an icon matching `QIcon::State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum IconState {
    /// Off / unchecked state.
    #[default]
    Off,
    /// On / checked state.
    On,
}

/// Single representation stored within an `Icon`.
#[derive(Clone, Debug)]
struct IconEntry {
    image: Image,
    mode: IconMode,
    state: IconState,
}

/// Scalable, multi-resolution, multi-state icon matching `QIcon`.
#[derive(Clone, Debug, Default)]
pub struct Icon {
    entries: Vec<IconEntry>,
}

impl Icon {
    /// Creates a null (empty) icon.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Creates an icon from a single image for `Normal` mode and `Off` state.
    pub fn from_image(image: Image) -> Self {
        let mut icon = Self::new();
        icon.add_image(image, IconMode::Normal, IconState::Off);
        icon
    }

    /// Creates an icon from a single pixmap.
    pub fn from_pixmap(pixmap: &Pixmap) -> Self {
        Self::from_image(pixmap.to_image())
    }

    /// Creates an icon from the frames of a Windows ICO file, one representation per frame
    /// (`QIcon` of an `.ico` exposes every stored size through `available_sizes`).
    pub fn from_ico_data(bytes: &[u8]) -> Result<Self, String> {
        let mut icon = Self::new();
        for image in crate::image::io::ImageReader::read_ico_frames(bytes)? {
            icon.add_image(image, IconMode::Normal, IconState::Off);
        }
        Ok(icon)
    }

    /// Returns `true` if this icon has no representations.
    #[inline]
    pub fn is_null(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds a representation with the specified image, mode, and state.
    pub fn add_image(&mut self, image: Image, mode: IconMode, state: IconState) {
        if image.is_null() {
            return;
        }
        self.entries.push(IconEntry { image, mode, state });
    }

    /// Adds a representation with the specified pixmap, mode, and state.
    pub fn add_pixmap(&mut self, pixmap: &Pixmap, mode: IconMode, state: IconState) {
        self.add_image(pixmap.to_image(), mode, state);
    }

    /// Returns all available sizes for the specified mode and state.
    pub fn available_sizes(&self, mode: IconMode, state: IconState) -> Vec<Size> {
        let mut sizes: Vec<Size> = self
            .entries
            .iter()
            .filter(|e| e.mode == mode && e.state == state)
            .map(|e| e.image.size())
            .collect();

        // If none found, fallback to Normal + Off
        if sizes.is_empty() && (mode != IconMode::Normal || state != IconState::Off) {
            sizes = self
                .entries
                .iter()
                .filter(|e| e.mode == IconMode::Normal && e.state == IconState::Off)
                .map(|e| e.image.size())
                .collect();
        }

        sizes.sort_by_key(|s| area(*s));
        sizes.dedup();
        sizes
    }

    /// Returns the best-matching actual size for the requested target size.
    pub fn actual_size(&self, target: Size, mode: IconMode, state: IconState) -> Size {
        let sizes = self.available_sizes(mode, state);
        if sizes.is_empty() {
            return Size::new(0, 0);
        }

        // Find exact match or smallest size that is >= target
        let target_area = area(target);
        for s in &sizes {
            if area(*s) >= target_area {
                return *s;
            }
        }

        // Otherwise return the largest available
        *sizes.last().unwrap()
    }

    /// Returns a `Pixmap` of the requested size, generating fallbacks if necessary.
    pub fn pixmap(&self, target_size: Size, mode: IconMode, state: IconState) -> Pixmap {
        if self.is_null() || target_size.width <= 0 || target_size.height <= 0 {
            let w = target_size.width.max(1) as u32;
            let h = target_size.height.max(1) as u32;
            return Pixmap::new(w, h).unwrap_or_else(|| Pixmap::new(1, 1).unwrap());
        }

        let tw = target_size.width as u32;
        let th = target_size.height as u32;

        // 1. Try finding exact (mode, state)
        let best_image = self.find_best_image(target_size, mode, state);

        let img = match best_image {
            Some(i) => i,
            None => {
                // Fallback to Normal + Off
                match self.find_best_image(target_size, IconMode::Normal, IconState::Off) {
                    Some(base) => {
                        if mode == IconMode::Disabled {
                            // Automatically generate disabled effect (grayed out + semi-translucent)
                            let mut disabled_img = base
                                .converted_to(crate::image::image_format::ImageFormat::Grayscale8);
                            disabled_img = disabled_img
                                .converted_to(crate::image::image_format::ImageFormat::Rgba8888);
                            // Reduce alpha
                            for y in 0..disabled_img.height() {
                                for x in 0..disabled_img.width() {
                                    if let Some(c) = disabled_img.pixel_color(x, y) {
                                        let u = c.to_color_u8();
                                        let new_alpha = ((u.alpha() as u32 * 128) / 255) as u8;
                                        let new_color = tiny_skia::Color::from_rgba8(
                                            u.red(),
                                            u.green(),
                                            u.blue(),
                                            new_alpha,
                                        );
                                        disabled_img.set_pixel_color(x, y, new_color);
                                    }
                                }
                            }
                            disabled_img
                        } else {
                            base
                        }
                    }
                    None => return Pixmap::new(tw, th).unwrap(),
                }
            }
        };

        // Scale to exact target size if needed
        let final_image = if img.width() == tw && img.height() == th {
            img
        } else {
            img.scaled(
                tw,
                th,
                AspectRatioMode::KeepAspectRatio,
                TransformationMode::SmoothTransformation,
            )
        };

        Pixmap::from_image(&final_image).unwrap_or_else(|| Pixmap::new(tw, th).unwrap())
    }

    fn find_best_image(
        &self,
        target_size: Size,
        mode: IconMode,
        state: IconState,
    ) -> Option<Image> {
        let matching: Vec<&IconEntry> = self
            .entries
            .iter()
            .filter(|e| e.mode == mode && e.state == state)
            .collect();

        if matching.is_empty() {
            return None;
        }

        // Find exact size or closest >= target
        let target_area = area(target_size);
        let mut best = matching[0];
        let mut best_diff = area(best.image.size()).abs_diff(target_area);

        for e in &matching[1..] {
            let diff = area(e.image.size()).abs_diff(target_area);
            if diff < best_diff {
                best = e;
                best_diff = diff;
            }
        }

        Some(best.image.clone())
    }
}

/// Pixel area in `i64`; negative extents count as zero.
fn area(s: Size) -> i64 {
    i64::from(s.width.max(0)) * i64::from(s.height.max(0))
}
