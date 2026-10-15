// SPDX-License-Identifier: LGPL-3.0-only
//! Image input/output and format detection matching `QImageReader` and `QImageWriter`.

use std::io::Cursor;

use crate::image::image::Image;
use crate::image::image_format::ImageFormat;
use tiny_skia::Color;

/// Supported container file formats for image I/O matching Qt image plugins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFileFormat {
    Png,
    Bmp,
    Ppm,
    Ico,
    Gif,
    Unknown,
}

/// Image reader for reading and detecting image formats matching `QImageReader`.
pub struct ImageReader;

impl ImageReader {
    /// Detects the image file format by inspecting file magic bytes.
    pub fn detect_format(bytes: &[u8]) -> ImageFileFormat {
        if bytes.len() >= 8 && bytes[0..8] == [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] {
            return ImageFileFormat::Png;
        }
        if bytes.len() >= 2 && &bytes[0..2] == b"BM" {
            return ImageFileFormat::Bmp;
        }
        if bytes.len() >= 4 && bytes[0..4] == [0x00, 0x00, 0x01, 0x00] {
            return ImageFileFormat::Ico;
        }
        if bytes.len() >= 3
            && (&bytes[0..3] == b"P3\n" || &bytes[0..3] == b"P6\n" || &bytes[0..3] == b"P6 ")
        {
            return ImageFileFormat::Ppm;
        }
        if bytes.len() >= 6 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
            return ImageFileFormat::Gif;
        }
        ImageFileFormat::Unknown
    }

    /// Reads an image from an in-memory byte buffer, auto-detecting the format.
    pub fn read_from_memory(bytes: &[u8]) -> Result<Image, String> {
        let format = Self::detect_format(bytes);
        match format {
            ImageFileFormat::Png => Self::read_png(bytes),
            ImageFileFormat::Bmp => Self::read_bmp(bytes),
            ImageFileFormat::Ppm => Self::read_ppm(bytes),
            _ => Err("Unsupported or unknown image file format".to_string()),
        }
    }

    fn read_png(bytes: &[u8]) -> Result<Image, String> {
        let cursor = Cursor::new(bytes);
        let decoder = png::Decoder::new(cursor);
        let mut reader = decoder
            .read_info()
            .map_err(|e| format!("PNG read_info error: {e}"))?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| format!("PNG next_frame error: {e}"))?;

        let width = info.width;
        let height = info.height;

        let mut image = Image::new(width, height, ImageFormat::Rgba8888);
        let total_pixels = (width * height) as usize;

        match info.color_type {
            png::ColorType::Rgba => {
                let bytes_to_copy = total_pixels * 4;
                if buf.len() >= bytes_to_copy {
                    for y in 0..height {
                        let src_start = (y as usize) * (width as usize) * 4;
                        let src_end = src_start + (width as usize) * 4;
                        if let Some(dest_line) = image.scan_line_mut(y) {
                            dest_line[0..(width as usize) * 4]
                                .copy_from_slice(&buf[src_start..src_end]);
                        }
                    }
                }
            }
            png::ColorType::Rgb => {
                for y in 0..height {
                    for x in 0..width {
                        let src_off = ((y * width + x) as usize) * 3;
                        if src_off + 2 < buf.len() {
                            let color = Color::from_rgba8(
                                buf[src_off],
                                buf[src_off + 1],
                                buf[src_off + 2],
                                255,
                            );
                            image.set_pixel_color(x, y, color);
                        }
                    }
                }
            }
            png::ColorType::Grayscale => {
                for y in 0..height {
                    for x in 0..width {
                        let src_off = (y * width + x) as usize;
                        if src_off < buf.len() {
                            let v = buf[src_off];
                            image.set_pixel_color(x, y, Color::from_rgba8(v, v, v, 255));
                        }
                    }
                }
            }
            _ => return Err(format!("Unsupported PNG color type: {:?}", info.color_type)),
        }

        Ok(image)
    }

    fn read_bmp(bytes: &[u8]) -> Result<Image, String> {
        if bytes.len() < 54 {
            return Err("BMP data too short".to_string());
        }
        if &bytes[0..2] != b"BM" {
            return Err("Invalid BMP signature".to_string());
        }

        let pixel_offset = u32::from_le_bytes(bytes[10..14].try_into().unwrap()) as usize;
        let dib_header_size = u32::from_le_bytes(bytes[14..18].try_into().unwrap());
        if dib_header_size < 40 {
            return Err("Unsupported DIB header size".to_string());
        }

        let width = i32::from_le_bytes(bytes[18..22].try_into().unwrap());
        let height = i32::from_le_bytes(bytes[22..26].try_into().unwrap());
        let bpp = u16::from_le_bytes(bytes[28..30].try_into().unwrap());
        let compression = u32::from_le_bytes(bytes[30..34].try_into().unwrap());

        if compression != 0 {
            return Err("Compressed BMP not supported".to_string());
        }
        if width <= 0 || height == 0 {
            return Err("Invalid BMP dimensions".to_string());
        }

        let is_top_down = height < 0;
        let abs_height = height.unsigned_abs();
        let abs_width = width as u32;

        let mut image = Image::new(abs_width, abs_height, ImageFormat::Rgba8888);

        if bpp == 24 {
            let row_stride = (abs_width as usize * 3).div_ceil(4) * 4;
            for row in 0..abs_height {
                let src_y = if is_top_down {
                    row
                } else {
                    abs_height - 1 - row
                };
                let row_start = pixel_offset + (src_y as usize) * row_stride;
                for col in 0..abs_width {
                    let off = row_start + (col as usize) * 3;
                    if off + 2 < bytes.len() {
                        let b = bytes[off];
                        let g = bytes[off + 1];
                        let r = bytes[off + 2];
                        image.set_pixel_color(col, row, Color::from_rgba8(r, g, b, 255));
                    }
                }
            }
        } else if bpp == 32 {
            let row_stride = abs_width as usize * 4;
            for row in 0..abs_height {
                let src_y = if is_top_down {
                    row
                } else {
                    abs_height - 1 - row
                };
                let row_start = pixel_offset + (src_y as usize) * row_stride;
                for col in 0..abs_width {
                    let off = row_start + (col as usize) * 4;
                    if off + 3 < bytes.len() {
                        let b = bytes[off];
                        let g = bytes[off + 1];
                        let r = bytes[off + 2];
                        let a = bytes[off + 3];
                        image.set_pixel_color(col, row, Color::from_rgba8(r, g, b, a));
                    }
                }
            }
        } else {
            return Err(format!("Unsupported BMP bits per pixel: {bpp}"));
        }

        Ok(image)
    }

    fn read_ppm(bytes: &[u8]) -> Result<Image, String> {
        let text = std::str::from_utf8(bytes).map_err(|e| format!("PPM utf8 error: {e}"))?;
        let mut tokens = text.split_whitespace();

        let magic = tokens.next().ok_or("Empty PPM header")?;
        if magic != "P3" {
            return Err("Only P3 ASCII PPM supported in simple reader".to_string());
        }

        let width: u32 = tokens
            .next()
            .ok_or("Missing PPM width")?
            .parse()
            .map_err(|e| format!("{e}"))?;
        let height: u32 = tokens
            .next()
            .ok_or("Missing PPM height")?
            .parse()
            .map_err(|e| format!("{e}"))?;
        let max_val: u32 = tokens
            .next()
            .ok_or("Missing PPM max_val")?
            .parse()
            .map_err(|e| format!("{e}"))?;

        let mut image = Image::new(width, height, ImageFormat::Rgba8888);
        for y in 0..height {
            for x in 0..width {
                let r: u32 = tokens
                    .next()
                    .ok_or("Missing red")?
                    .parse()
                    .map_err(|e| format!("{e}"))?;
                let g: u32 = tokens
                    .next()
                    .ok_or("Missing green")?
                    .parse()
                    .map_err(|e| format!("{e}"))?;
                let b: u32 = tokens
                    .next()
                    .ok_or("Missing blue")?
                    .parse()
                    .map_err(|e| format!("{e}"))?;

                let r_norm = (r * 255 / max_val) as u8;
                let g_norm = (g * 255 / max_val) as u8;
                let b_norm = (b * 255 / max_val) as u8;
                image.set_pixel_color(x, y, Color::from_rgba8(r_norm, g_norm, b_norm, 255));
            }
        }

        Ok(image)
    }
}

/// Image writer for serializing images matching `QImageWriter`.
pub struct ImageWriter;

impl ImageWriter {
    /// Writes an image to an in-memory byte buffer in the target format.
    pub fn write_to_memory(image: &Image, format: ImageFileFormat) -> Result<Vec<u8>, String> {
        if image.is_null() {
            return Err("Cannot write null image".to_string());
        }

        match format {
            ImageFileFormat::Png => Self::write_png(image),
            ImageFileFormat::Bmp => Self::write_bmp(image),
            ImageFileFormat::Ppm => Self::write_ppm(image),
            _ => Err("Unsupported output image format".to_string()),
        }
    }

    fn write_png(image: &Image) -> Result<Vec<u8>, String> {
        let rgba_image = if image.format() == ImageFormat::Rgba8888 {
            image.clone()
        } else {
            image.converted_to(ImageFormat::Rgba8888)
        };

        let mut buffer = Vec::new();
        {
            let mut encoder =
                png::Encoder::new(&mut buffer, rgba_image.width(), rgba_image.height());
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| format!("PNG write_header error: {e}"))?;

            // Pack pixels without row padding
            let mut packed_data =
                Vec::with_capacity((rgba_image.width() * rgba_image.height() * 4) as usize);
            for y in 0..rgba_image.height() {
                if let Some(line) = rgba_image.scan_line(y) {
                    packed_data.extend_from_slice(&line[0..(rgba_image.width() as usize) * 4]);
                }
            }

            writer
                .write_image_data(&packed_data)
                .map_err(|e| format!("PNG write_image_data error: {e}"))?;
        }

        Ok(buffer)
    }

    fn write_bmp(image: &Image) -> Result<Vec<u8>, String> {
        let w = image.width();
        let h = image.height();
        let row_stride = (w as usize * 3).div_ceil(4) * 4;
        let image_size = row_stride * (h as usize);
        let file_size = 54 + image_size;

        let mut out = Vec::with_capacity(file_size);

        // BITMAPFILEHEADER (14 bytes)
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&(file_size as u32).to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]); // Reserved
        out.extend_from_slice(&54u32.to_le_bytes()); // Pixel data offset

        // BITMAPINFOHEADER (40 bytes)
        out.extend_from_slice(&40u32.to_le_bytes()); // Header size
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(h as i32).to_le_bytes()); // Bottom-up
        out.extend_from_slice(&1u16.to_le_bytes()); // Color planes
        out.extend_from_slice(&24u16.to_le_bytes()); // Bits per pixel
        out.extend_from_slice(&0u32.to_le_bytes()); // Compression (BI_RGB)
        out.extend_from_slice(&(image_size as u32).to_le_bytes());
        out.extend_from_slice(&2835u32.to_le_bytes()); // H-res (72 DPI)
        out.extend_from_slice(&2835u32.to_le_bytes()); // V-res (72 DPI)
        out.extend_from_slice(&0u32.to_le_bytes()); // Palette colors
        out.extend_from_slice(&0u32.to_le_bytes()); // Important colors

        // Bottom-up 24-bit BGR scanlines
        let padding_len = row_stride - (w as usize * 3);
        let padding = [0u8; 4];

        for y in (0..h).rev() {
            for x in 0..w {
                let color = image.pixel_color(x, y).unwrap_or(Color::BLACK);
                let u = color.to_color_u8();
                out.push(u.blue());
                out.push(u.green());
                out.push(u.red());
            }
            if padding_len > 0 {
                out.extend_from_slice(&padding[..padding_len]);
            }
        }

        Ok(out)
    }

    fn write_ppm(image: &Image) -> Result<Vec<u8>, String> {
        let w = image.width();
        let h = image.height();
        let mut out = format!("P3\n{w} {h}\n255\n");

        for y in 0..h {
            for x in 0..w {
                let c = image.pixel_color(x, y).unwrap_or(Color::BLACK);
                let u = c.to_color_u8();
                out.push_str(&format!("{} {} {} ", u.red(), u.green(), u.blue()));
            }
            out.push('\n');
        }

        Ok(out.into_bytes())
    }
}
