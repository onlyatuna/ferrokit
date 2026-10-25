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
            // Qt's ICO plugin loads the largest frame when no size is requested.
            ImageFileFormat::Ico => Self::read_ico_frames(bytes)?
                .into_iter()
                .reduce(|best, f| {
                    if u64::from(f.width()) * u64::from(f.height())
                        > u64::from(best.width()) * u64::from(best.height())
                    {
                        f
                    } else {
                        best
                    }
                })
                .ok_or_else(|| "ICO file contains no frames".to_string()),
            _ => Err("Unsupported or unknown image file format".to_string()),
        }
    }

    /// Decodes every frame of a Windows ICO file, in directory order (what `QIcon` exposes as
    /// its sizes). Frames are PNG-compressed or BMP-style DIBs (1/4/8/24/32 bpp, uncompressed,
    /// with the AND mask). A frame that cannot be decoded fails the whole read.
    pub fn read_ico_frames(bytes: &[u8]) -> Result<Vec<Image>, String> {
        if bytes.len() < 6 || bytes[0..4] != [0, 0, 1, 0] {
            return Err("Invalid ICO header".to_string());
        }
        let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
        if count == 0 {
            return Err("ICO file contains no frames".to_string());
        }
        let dir_end = 6 + count * 16;
        if bytes.len() < dir_end {
            return Err("ICO directory shorter than its header declares".to_string());
        }
        let mut frames = Vec::with_capacity(count);
        for i in 0..count {
            let e = &bytes[6 + i * 16..][..16];
            let size = u32::from_le_bytes([e[8], e[9], e[10], e[11]]) as usize;
            let offset = u32::from_le_bytes([e[12], e[13], e[14], e[15]]) as usize;
            let data = offset
                .checked_add(size)
                .and_then(|end| bytes.get(offset..end))
                .ok_or("ICO frame lies outside the file")?;
            let frame = if data.len() >= 8 && Self::detect_format(data) == ImageFileFormat::Png {
                Self::read_png(data)?
            } else {
                Self::read_ico_dib(data)?
            };
            frames.push(frame);
        }
        Ok(frames)
    }

    /// Decodes one BMP-style ICO frame: a `BITMAPINFOHEADER`, the palette, the XOR bitmap and
    /// the 1-bpp AND mask (the header height covers both bitmaps).
    fn read_ico_dib(data: &[u8]) -> Result<Image, String> {
        if data.len() < 40 {
            return Err("ICO DIB frame too short".to_string());
        }
        let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let header_size = u32_at(0) as usize;
        if header_size < 40 || header_size > data.len() {
            return Err("Unsupported ICO DIB header size".to_string());
        }
        let width = i32::from_le_bytes(data[4..8].try_into().unwrap());
        let height2 = i32::from_le_bytes(data[8..12].try_into().unwrap());
        let bpp = u16::from_le_bytes([data[14], data[15]]) as usize;
        if u32_at(16) != 0 {
            return Err("Compressed ICO DIB not supported".to_string());
        }
        if !matches!(bpp, 1 | 4 | 8 | 24 | 32) {
            return Err(format!("Unsupported ICO bits per pixel: {bpp}"));
        }
        // The DIB height is twice the image height (XOR + AND bitmaps).
        if width <= 0 || width > 32767 || height2 <= 1 || height2 / 2 > 32767 {
            return Err("Invalid ICO frame dimensions".to_string());
        }
        let (w, h) = (width as usize, (height2 / 2) as usize);

        let palette_len = if bpp <= 8 {
            match u32_at(32) as usize {
                0 => 1usize << bpp,
                n => n.min(1usize << bpp),
            }
        } else {
            0
        };
        let palette_off = header_size;
        let xor_off = palette_off + palette_len * 4;
        let xor_stride = (w * bpp).div_ceil(32) * 4;
        let and_off = xor_off + xor_stride * h;
        let and_stride = w.div_ceil(32) * 4;
        // Checked before allocating, so a hostile header cannot demand a huge image.
        if and_off + and_stride * h > data.len() {
            return Err("ICO frame pixel data shorter than its header declares".to_string());
        }
        let palette = &data[palette_off..xor_off];
        let xor = &data[xor_off..and_off];
        let and = &data[and_off..and_off + and_stride * h];

        // 32-bpp frames carry alpha themselves; without any non-zero alpha (old-style icons)
        // the AND mask decides transparency, as Windows does.
        let use_alpha = bpp == 32 && xor.chunks_exact(4).any(|p| p[3] != 0);

        let mut image = Image::new(w as u32, h as u32, ImageFormat::Rgba8888);
        for y in 0..h {
            // DIB rows are stored bottom-up.
            let src_y = h - 1 - y;
            let row = &xor[src_y * xor_stride..][..xor_stride];
            let mask_row = &and[src_y * and_stride..][..and_stride];
            for x in 0..w {
                let (r, g, b) = match bpp {
                    32 => {
                        let p = &row[x * 4..][..4];
                        (p[2], p[1], p[0])
                    }
                    24 => {
                        let p = &row[x * 3..][..3];
                        (p[2], p[1], p[0])
                    }
                    _ => {
                        let bit = x * bpp;
                        let mask = ((1u16 << bpp) - 1) as u8;
                        let idx = ((row[bit / 8] >> (8 - bpp - bit % 8)) & mask) as usize;
                        match palette.get(idx * 4..idx * 4 + 3) {
                            Some(c) => (c[2], c[1], c[0]),
                            None => (0, 0, 0),
                        }
                    }
                };
                let a = if use_alpha {
                    row[x * 4 + 3]
                } else if mask_row[x / 8] & (0x80 >> (x % 8)) != 0 {
                    0
                } else {
                    255
                };
                image.set_pixel_color(x as u32, y as u32, Color::from_rgba8(r, g, b, a));
            }
        }
        Ok(image)
    }

    fn read_png(bytes: &[u8]) -> Result<Image, String> {
        let cursor = Cursor::new(bytes);
        let mut decoder = png::Decoder::new(cursor);
        // Expand palettes, sub-byte depths and tRNS, and reduce 16-bit samples to 8, so the
        // frame is always Gray/GrayAlpha/Rgb/Rgba at 8 bits (as Qt's PNG plugin decodes).
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder
            .read_info()
            .map_err(|e| format!("PNG read_info error: {e}"))?;
        let mut buf = vec![0u8; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| format!("PNG next_frame error: {e}"))?;

        let width = info.width;
        let height = info.height;
        let channels = match info.color_type {
            png::ColorType::Rgba => 4,
            png::ColorType::Rgb => 3,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Grayscale => 1,
            other => return Err(format!("Unsupported PNG color type: {other:?}")),
        };
        if info.bit_depth != png::BitDepth::Eight {
            return Err(format!("Unsupported PNG bit depth: {:?}", info.bit_depth));
        }

        let mut image = Image::new(width, height, ImageFormat::Rgba8888);
        let row_bytes = width as usize * channels;
        for y in 0..height {
            let start = y as usize * info.line_size;
            let row = buf
                .get(start..start + row_bytes)
                .ok_or("PNG frame shorter than its header declares")?;
            for x in 0..width {
                let p = &row[x as usize * channels..][..channels];
                let color = match channels {
                    4 => Color::from_rgba8(p[0], p[1], p[2], p[3]),
                    3 => Color::from_rgba8(p[0], p[1], p[2], 255),
                    2 => Color::from_rgba8(p[0], p[0], p[0], p[1]),
                    _ => Color::from_rgba8(p[0], p[0], p[0], 255),
                };
                image.set_pixel_color(x, y, color);
            }
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

        let bytes_per_pixel: u64 = match bpp {
            24 => 3,
            32 => 4,
            _ => return Err(format!("Unsupported BMP bits per pixel: {bpp}")),
        };
        let row_bytes = (abs_width as u64 * bytes_per_pixel).div_ceil(4) * 4;
        let needed = row_bytes * abs_height as u64;
        if (pixel_offset as u64).saturating_add(needed) > bytes.len() as u64 {
            return Err("BMP pixel data shorter than its header declares".to_string());
        }
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
        // Qt's read_pbm_header rejects `w <= 0 || w > 32767 || h <= 0 || h > 32767 || mcc == 0`;
        // without the zero check a 0 x 4294967295 header spins through billions of empty rows.
        if !(1..=32767).contains(&width) || !(1..=32767).contains(&height) {
            return Err("PPM dimensions must be in 1..=32767".to_string());
        }
        if !(1..=65535).contains(&max_val) {
            return Err("PPM max value must be in 1..=65535".to_string());
        }

        // Each sample needs at least two bytes ("0 "), so the header cannot promise more
        // pixels than the text can hold; checked before allocating.
        let samples = (width as u64)
            .checked_mul(height as u64)
            .and_then(|p| p.checked_mul(3))
            .ok_or("PPM dimensions overflow")?;
        if samples.checked_mul(2).is_none_or(|n| n > bytes.len() as u64 + 1) {
            return Err("PPM data shorter than its header declares".to_string());
        }
        let mut image = Image::new(width, height, ImageFormat::Rgba8888);
        let max = max_val as u64;
        let mut sample = |name: &str| -> Result<u8, String> {
            let v: u64 = tokens
                .next()
                .ok_or_else(|| format!("Missing {name}"))?
                .parse()
                .map_err(|e| format!("{e}"))?;
            if v > max {
                return Err(format!("PPM {name} sample {v} exceeds max value {max}"));
            }
            Ok((v * 255 / max) as u8)
        };
        for y in 0..height {
            for x in 0..width {
                let r_norm = sample("red")?;
                let g_norm = sample("green")?;
                let b_norm = sample("blue")?;
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

#[cfg(test)]
mod codec_tests {
    use super::*;

    fn encode(
        w: u32,
        h: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        palette: Option<Vec<u8>>,
        trns: Option<Vec<u8>>,
        data: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(color);
            enc.set_depth(depth);
            if let Some(p) = palette {
                enc.set_palette(p);
            }
            if let Some(t) = trns {
                enc.set_trns(t);
            }
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(data).unwrap();
        }
        out
    }

    fn rgba(img: &Image, x: u32, y: u32) -> [u8; 4] {
        let u = img.pixel_color(x, y).unwrap().to_color_u8();
        [u.red(), u.green(), u.blue(), u.alpha()]
    }

    #[test]
    fn png_16_bit_rgb_keeps_high_bytes() {
        // 2x1: (0x1234, 0x5678, 0x9ABC) and (0xFFFF, 0, 0x8000).
        let data = [0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xFF, 0xFF, 0, 0, 0x80, 0];
        let png = encode(2, 1, png::ColorType::Rgb, png::BitDepth::Sixteen, None, None, &data);
        let img = ImageReader::read_from_memory(&png).unwrap();
        assert_eq!((img.width(), img.height()), (2, 1));
        assert_eq!(rgba(&img, 0, 0), [0x12, 0x56, 0x9A, 255]);
        assert_eq!(rgba(&img, 1, 0), [255, 0, 0x80, 255]);
    }

    #[test]
    fn png_2_bit_gray_is_scaled_to_8_bit() {
        // One row of four pixels: values 0,1,2,3 packed MSB first.
        let png = encode(4, 1, png::ColorType::Grayscale, png::BitDepth::Two, None, None, &[0b00_01_10_11]);
        let img = ImageReader::read_from_memory(&png).unwrap();
        let got: Vec<u8> = (0..4).map(|x| rgba(&img, x, 0)[0]).collect();
        assert_eq!(got, vec![0, 85, 170, 255]);
    }

    #[test]
    fn png_indexed_with_transparency_decodes() {
        let pal = vec![255, 0, 0, 0, 255, 0];
        let png = encode(2, 1, png::ColorType::Indexed, png::BitDepth::Eight, Some(pal), Some(vec![128]), &[0, 1]);
        let img = ImageReader::read_from_memory(&png).unwrap();
        assert_eq!(rgba(&img, 0, 0)[..3], [255, 0, 0]);
        assert!((rgba(&img, 0, 0)[3] as i32 - 128).abs() <= 1);
        assert_eq!(rgba(&img, 1, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn png_gray_alpha_decodes() {
        let png = encode(1, 1, png::ColorType::GrayscaleAlpha, png::BitDepth::Eight, None, None, &[100, 200]);
        let img = ImageReader::read_from_memory(&png).unwrap();
        let p = rgba(&img, 0, 0);
        assert_eq!(p[..3], [100, 100, 100]);
        assert!((p[3] as i32 - 200).abs() <= 1);
    }

    #[test]
    fn ppm_rejects_zero_and_oversized_max_value() {
        let e = ImageReader::read_from_memory(b"P3\n1 1 0 0 0 0").unwrap_err();
        assert!(e.contains("max value"), "{e}");
        let e = ImageReader::read_from_memory(b"P3\n1 1 70000 0 0 0").unwrap_err();
        assert!(e.contains("max value"), "{e}");
    }

    #[test]
    fn ppm_rejects_samples_above_max_and_scales_others() {
        let e = ImageReader::read_from_memory(b"P3\n1 1 100 0 101 0").unwrap_err();
        assert!(e.contains("exceeds"), "{e}");
        let img = ImageReader::read_from_memory(b"P3\n1 1 100 100 50 0").unwrap();
        assert_eq!(rgba(&img, 0, 0), [255, 127, 0, 255]);
    }

    #[test]
    fn ppm_huge_header_does_not_allocate() {
        // Qt's PPM reader caps each dimension at 32767; a header past it is rejected up front.
        let e = ImageReader::read_from_memory(b"P3\n4000000000 4000000000 255 0 0 0").unwrap_err();
        assert!(e.contains("32767"), "{e}");
        let e = ImageReader::read_from_memory(b"P3\n32767 32767 255 0 0 0").unwrap_err();
        assert!(e.contains("shorter") || e.contains("overflow"), "{e}");
    }

    #[test]
    fn bmp_huge_header_does_not_allocate() {
        let mut bmp = vec![0u8; 54];
        bmp[0..2].copy_from_slice(b"BM");
        bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
        bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
        bmp[18..22].copy_from_slice(&i32::MAX.to_le_bytes());
        bmp[22..26].copy_from_slice(&i32::MAX.to_le_bytes());
        bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
        assert!(ImageReader::read_from_memory(&bmp).is_err());
    }
}
