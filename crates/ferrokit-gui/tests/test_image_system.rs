// SPDX-License-Identifier: LGPL-3.0-only
//! Comprehensive integration tests for `ferrokit-gui` Image System.
//!
//! Validates:
//! 1. `ImageFormat` metadata and scanline stride calculation.
//! 2. `Image` CPU buffer allocation, pixel manipulation, and fill.
//! 3. Format conversion (`converted_to`) across RGBA8888, BGRA8888, ARGB32, RGB888, Grayscale8.
//! 4. Transformations: `scaled`, `mirrored`, `copy_rect`.
//! 5. Zero-copy `ImageView` and `ImageViewMut` sub-view extraction.
//! 6. `Pixmap` platform display representation decoupling (`from_image` & `to_image`).
//! 7. `Bitmap` 1-bit monochrome mask and bitwise operations (`&`, `|`, `^`, `!`).
//! 8. `Icon` multi-resolution, multi-state selection and auto-disabled generation.
//! 9. `ImageReader` and `ImageWriter` format detection and codecs (PNG, BMP, PPM).
//! 10. Canonical Qt 6 type aliases (`QImage`, `QPixmap`, `QBitmap`, `QIcon`, etc.).

use ferrokit_gui::geometry::primitives::{Rect, Size};
use ferrokit_gui::image::*;
use tiny_skia::Color;

// =============================================================================
// 1. ImageFormat Metadata Tests
// =============================================================================

#[test]
fn test_image_format_properties() {
    assert_eq!(ImageFormat::Mono.bits_per_pixel(), 1);
    assert_eq!(ImageFormat::Grayscale8.bits_per_pixel(), 8);
    assert_eq!(ImageFormat::Rgb888.bits_per_pixel(), 24);
    assert_eq!(ImageFormat::Rgba8888.bits_per_pixel(), 32);
    assert_eq!(ImageFormat::Bgra8888.bits_per_pixel(), 32);

    assert!(ImageFormat::Rgba8888.has_alpha_channel());
    assert!(!ImageFormat::Rgb888.has_alpha_channel());
    assert!(!ImageFormat::Grayscale8.has_alpha_channel());

    // 4-byte stride alignment check
    // Width 10 in Rgb888 = 30 bytes -> aligned to 4 bytes is 32 bytes
    assert_eq!(ImageFormat::Rgb888.bytes_per_line(10, 4), 32);
    // Width 10 in Rgba8888 = 40 bytes -> already aligned
    assert_eq!(ImageFormat::Rgba8888.bytes_per_line(10, 4), 40);
}

// =============================================================================
// 2. Image Allocation, Pixel Read/Write Tests
// =============================================================================

#[test]
fn test_image_pixels_and_fill() {
    let mut img = Image::new(4, 4, ImageFormat::Rgba8888);
    assert_eq!(img.width(), 4);
    assert_eq!(img.height(), 4);
    assert!(!img.is_null());

    let red = Color::from_rgba8(255, 0, 0, 255);
    let green = Color::from_rgba8(0, 255, 0, 255);

    img.set_pixel_color(1, 1, red);
    img.set_pixel_color(2, 2, green);

    let p1 = img.pixel_color(1, 1).expect("p1");
    let p2 = img.pixel_color(2, 2).expect("p2");
    let p0 = img.pixel_color(0, 0).expect("p0");

    assert_eq!(p1.to_color_u8().red(), 255);
    assert_eq!(p1.to_color_u8().green(), 0);
    assert_eq!(p2.to_color_u8().green(), 255);
    assert_eq!(p0.to_color_u8().alpha(), 0); // Unset is 0

    let blue = Color::from_rgba8(0, 0, 255, 255);
    img.fill(blue);
    assert_eq!(img.pixel_color(0, 0).unwrap().to_color_u8().blue(), 255);
    assert_eq!(img.pixel_color(3, 3).unwrap().to_color_u8().blue(), 255);
}

// =============================================================================
// 3. Format Conversion Tests
// =============================================================================

#[test]
fn test_image_format_conversions() {
    let mut src = Image::new(2, 2, ImageFormat::Rgba8888);
    src.set_pixel_color(0, 0, Color::from_rgba8(255, 0, 0, 255)); // Red
    src.set_pixel_color(1, 1, Color::from_rgba8(0, 255, 0, 255)); // Green

    // Convert to BGRA8888
    let bgra = src.converted_to(ImageFormat::Bgra8888);
    assert_eq!(bgra.format(), ImageFormat::Bgra8888);
    let c = bgra.pixel_color(0, 0).unwrap().to_color_u8();
    assert_eq!(c.red(), 255);
    assert_eq!(c.green(), 0);

    // Convert to Grayscale8
    let gray = src.converted_to(ImageFormat::Grayscale8);
    assert_eq!(gray.format(), ImageFormat::Grayscale8);
    let g = gray.pixel_color(0, 0).unwrap().to_color_u8();
    // Qt's qGray(255, 0, 0) = 255 * 11 / 32 = 87
    assert_eq!(g.red(), 87);
    assert_eq!(g.red(), g.green());
    assert_eq!(g.green(), g.blue());

    // Convert to RGB888
    let rgb = src.converted_to(ImageFormat::Rgb888);
    assert_eq!(rgb.format(), ImageFormat::Rgb888);
    let r = rgb.pixel_color(0, 0).unwrap().to_color_u8();
    assert_eq!(r.red(), 255);
    assert_eq!(r.green(), 0);
}

// =============================================================================
// 4. Transformations: Mirrored, CopyRect, Scaled
// =============================================================================

#[test]
fn test_image_transformations() {
    let mut img = Image::new(4, 4, ImageFormat::Rgba8888);
    img.set_pixel_color(0, 0, Color::from_rgba8(255, 0, 0, 255)); // Top-left is red

    // Mirrored horizontally: red should be at (3, 0)
    let mirrored_h = img.mirrored(true, false);
    assert_eq!(
        mirrored_h.pixel_color(3, 0).unwrap().to_color_u8().red(),
        255
    );
    assert_eq!(mirrored_h.pixel_color(0, 0).unwrap().to_color_u8().red(), 0);

    // Mirrored vertically: red should be at (0, 3)
    let mirrored_v = img.mirrored(false, true);
    assert_eq!(
        mirrored_v.pixel_color(0, 3).unwrap().to_color_u8().red(),
        255
    );

    // Copy rect (2x2 from top-left)
    let sub = img.copy_rect(Rect::new(0, 0, 2, 2));
    assert_eq!(sub.width(), 2);
    assert_eq!(sub.height(), 2);
    assert_eq!(sub.pixel_color(0, 0).unwrap().to_color_u8().red(), 255);

    // Scaled (4x4 -> 8x8)
    let scaled = img.scaled(
        8,
        8,
        AspectRatioMode::IgnoreAspectRatio,
        TransformationMode::FastTransformation,
    );
    assert_eq!(scaled.width(), 8);
    assert_eq!(scaled.height(), 8);
    assert_eq!(scaled.pixel_color(0, 0).unwrap().to_color_u8().red(), 255);
    assert_eq!(scaled.pixel_color(1, 1).unwrap().to_color_u8().red(), 255);
}

// =============================================================================
// 5. Zero-Copy ImageView and ImageViewMut Tests
// =============================================================================

#[test]
fn test_image_view_zero_copy() {
    let mut img = Image::new(10, 10, ImageFormat::Rgba8888);
    img.set_pixel_color(5, 5, Color::from_rgba8(123, 200, 50, 255));

    let view = img.as_view();
    assert_eq!(view.width(), 10);
    assert_eq!(view.height(), 10);
    assert_eq!(view.format(), ImageFormat::Rgba8888);

    // Sub-view
    let sub = view.sub_view(Rect::new(4, 4, 3, 3)).expect("sub view");
    assert_eq!(sub.width(), 3);
    assert_eq!(sub.height(), 3);
}

#[test]
fn test_image_view_sub_view_one_bit_formats() {
    for format in [ImageFormat::Mono, ImageFormat::MonoLsb] {
        let img = Image::new(16, 1, format);
        let view = img.as_view();
        // Column 10 is not byte aligned: a view cannot start mid-byte, and must not panic.
        assert!(view.sub_view(Rect::new(10, 0, 6, 1)).is_none());
        // Column 8 starts at byte 1.
        let sub = view.sub_view(Rect::new(8, 0, 8, 1)).expect("aligned sub view");
        assert_eq!(sub.scan_line(0).map(<[u8]>::len), Some(1));
    }
}

// =============================================================================
// 6. Pixmap Decoupling and Image Roundtrip Tests
// =============================================================================

#[test]
fn test_pixmap_image_roundtrip() {
    let mut img = Image::new(8, 8, ImageFormat::Rgba8888Premultiplied);
    img.set_pixel_color(2, 2, Color::from_rgba8(200, 100, 50, 255));

    // Upload CPU Image to platform Pixmap
    let pm = Pixmap::from_image(&img).expect("pixmap from image");
    assert_eq!(pm.physical_width(), 8);
    assert_eq!(pm.physical_height(), 8);

    // Read back platform Pixmap to CPU Image
    let back_img = pm.to_image();
    assert_eq!(back_img.width(), 8);
    assert_eq!(back_img.height(), 8);

    let c = back_img.pixel_color(2, 2).expect("pixel");
    assert_eq!(c.to_color_u8().red(), 200);
    assert_eq!(c.to_color_u8().green(), 100);
    assert_eq!(c.to_color_u8().blue(), 50);
}

// =============================================================================
// 7. Bitmap 1-bit Monochrome Mask Tests
// =============================================================================

#[test]
fn test_bitmap_operations() {
    let mut bm1 = Bitmap::new(8, 8);
    let mut bm2 = Bitmap::new(8, 8);

    bm1.set_bit(0, 0, true);
    bm1.set_bit(1, 1, true);
    assert!(bm1.bit(0, 0));
    assert!(bm1.bit(1, 1));
    assert!(!bm1.bit(2, 2));

    bm2.set_bit(1, 1, true);
    bm2.set_bit(2, 2, true);

    // Bitwise AND: only (1, 1) should remain
    let and_bm = bm1.clone() & bm2.clone();
    assert!(!and_bm.bit(0, 0));
    assert!(and_bm.bit(1, 1));
    assert!(!and_bm.bit(2, 2));

    // Bitwise OR: (0, 0), (1, 1), (2, 2)
    let or_bm = bm1.clone() | bm2.clone();
    assert!(or_bm.bit(0, 0));
    assert!(or_bm.bit(1, 1));
    assert!(or_bm.bit(2, 2));

    // Bitwise NOT
    let not_bm = !bm1;
    assert!(!not_bm.bit(0, 0));
    assert!(not_bm.bit(2, 2));
}

// =============================================================================
// 8. Icon Multi-Resolution and State Tests
// =============================================================================

#[test]
fn test_icon_multi_resolution_and_states() {
    let mut icon = Icon::new();
    assert!(icon.is_null());

    let img16 = Image::new(16, 16, ImageFormat::Rgba8888);
    let img32 = Image::new(32, 32, ImageFormat::Rgba8888);

    icon.add_image(img16, IconMode::Normal, IconState::Off);
    icon.add_image(img32, IconMode::Normal, IconState::Off);

    assert!(!icon.is_null());
    let sizes = icon.available_sizes(IconMode::Normal, IconState::Off);
    assert_eq!(sizes.len(), 2);
    assert_eq!(sizes[0], Size::new(16, 16));
    assert_eq!(sizes[1], Size::new(32, 32));

    // Actual size lookup
    assert_eq!(
        icon.actual_size(Size::new(12, 12), IconMode::Normal, IconState::Off),
        Size::new(16, 16)
    );
    assert_eq!(
        icon.actual_size(Size::new(24, 24), IconMode::Normal, IconState::Off),
        Size::new(32, 32)
    );

    // Fallback generation for Disabled mode
    let pm_disabled = icon.pixmap(Size::new(16, 16), IconMode::Disabled, IconState::Off);
    assert_eq!(pm_disabled.physical_width(), 16);
    assert_eq!(pm_disabled.physical_height(), 16);
}

// =============================================================================
// 9. ImageReader and ImageWriter Codecs Tests
// =============================================================================

#[test]
fn test_image_codecs_roundtrip() {
    let mut img = Image::new(4, 4, ImageFormat::Rgba8888);
    img.fill(Color::from_rgba8(100, 150, 200, 255));
    img.set_pixel_color(1, 1, Color::from_rgba8(255, 0, 0, 255));

    // 1. PNG codec roundtrip
    let png_bytes = ImageWriter::write_to_memory(&img, ImageFileFormat::Png).expect("PNG write");
    assert_eq!(ImageReader::detect_format(&png_bytes), ImageFileFormat::Png);
    let decoded_png = ImageReader::read_from_memory(&png_bytes).expect("PNG read");
    assert_eq!(decoded_png.width(), 4);
    assert_eq!(decoded_png.height(), 4);
    assert_eq!(
        decoded_png.pixel_color(1, 1).unwrap().to_color_u8().red(),
        255
    );

    // 2. BMP codec roundtrip
    let bmp_bytes = ImageWriter::write_to_memory(&img, ImageFileFormat::Bmp).expect("BMP write");
    assert_eq!(ImageReader::detect_format(&bmp_bytes), ImageFileFormat::Bmp);
    let decoded_bmp = ImageReader::read_from_memory(&bmp_bytes).expect("BMP read");
    assert_eq!(decoded_bmp.width(), 4);
    assert_eq!(decoded_bmp.height(), 4);
    assert_eq!(
        decoded_bmp.pixel_color(1, 1).unwrap().to_color_u8().red(),
        255
    );

    // 3. PPM codec roundtrip
    let ppm_bytes = ImageWriter::write_to_memory(&img, ImageFileFormat::Ppm).expect("PPM write");
    assert_eq!(ImageReader::detect_format(&ppm_bytes), ImageFileFormat::Ppm);
    let decoded_ppm = ImageReader::read_from_memory(&ppm_bytes).expect("PPM read");
    assert_eq!(decoded_ppm.width(), 4);
    assert_eq!(decoded_ppm.height(), 4);
    assert_eq!(
        decoded_ppm.pixel_color(1, 1).unwrap().to_color_u8().red(),
        255
    );
}

// =============================================================================
// 10. Canonical Qt 6 Type Aliases Tests
// =============================================================================

#[test]
fn test_canonical_qt_image_aliases() {
    let mut qimg: QImage = QImage::new(10, 10, QImageFormat::Rgba8888);
    qimg.fill(Color::WHITE);
    let _qpm: QPixmap = QPixmap::from_image(&qimg).expect("pixmap");
    let _qbm: QBitmap = QBitmap::from_image(&qimg);
    let _qicon: QIcon = QIcon::from_image(qimg);
}

// =============================================================================
// 11. ICO decoding
// =============================================================================

/// Builds an ICO file from `(declared width, declared height, frame bytes)` entries.
fn build_ico(frames: &[(u8, u8, Vec<u8>)]) -> Vec<u8> {
    let mut out = vec![0, 0, 1, 0];
    out.extend_from_slice(&(frames.len() as u16).to_le_bytes());
    let mut offset = 6 + frames.len() * 16;
    for (w, h, data) in frames {
        out.extend_from_slice(&[*w, *h, 0, 0, 1, 0, 32, 0]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += data.len();
    }
    for (_, _, data) in frames {
        out.extend_from_slice(data);
    }
    out
}

/// A 2x2 DIB frame header: `bpp` bits, `colors` palette entries; height covers XOR + AND.
fn dib_header(bpp: u16, colors: u32) -> Vec<u8> {
    let mut h = Vec::new();
    h.extend_from_slice(&40u32.to_le_bytes());
    h.extend_from_slice(&2i32.to_le_bytes());
    h.extend_from_slice(&4i32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes());
    h.extend_from_slice(&bpp.to_le_bytes());
    h.extend_from_slice(&[0; 4]);
    h.extend_from_slice(&[0; 12]);
    h.extend_from_slice(&colors.to_le_bytes());
    h.extend_from_slice(&[0; 4]);
    h
}

fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let u = img.pixel_color(x, y).unwrap().to_color_u8();
    [u.red(), u.green(), u.blue(), u.alpha()]
}

#[test]
fn test_ico_largest_frame_and_icon_sizes() {
    // 2x2 24-bpp DIB: top row red, green; bottom row blue, white. The AND mask hides
    // the top-right pixel. Rows are stored bottom-up and padded to 4 bytes.
    let mut dib = dib_header(24, 0);
    dib.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // bottom: blue, white (BGR)
    dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // top: red, green
    dib.extend_from_slice(&[0, 0, 0, 0]); // AND bottom: opaque
    dib.extend_from_slice(&[0b0100_0000, 0, 0, 0]); // AND top: pixel 1 transparent

    let mut big = Image::new(4, 4, ImageFormat::Rgba8888);
    big.fill(Color::from_rgba8(255, 0, 0, 255));
    let png = ImageWriter::write_to_memory(&big, ImageFileFormat::Png).unwrap();

    let ico = build_ico(&[(2, 2, dib), (4, 4, png)]);
    assert_eq!(ImageReader::detect_format(&ico), ImageFileFormat::Ico);

    // Default decode picks the largest frame (the PNG one).
    let best = ImageReader::read_from_memory(&ico).unwrap();
    assert_eq!((best.width(), best.height()), (4, 4));
    assert_eq!(px(&best, 3, 3), [255, 0, 0, 255]);
    let pm = QPixmap::load_from_data(&ico).expect("pixmap");
    assert_eq!((pm.physical_width(), pm.physical_height()), (4, 4));

    // All frames are exposed; the DIB frame decodes with orientation and mask applied.
    let frames = ImageReader::read_ico_frames(&ico).unwrap();
    assert_eq!(frames.len(), 2);
    let small = &frames[0];
    assert_eq!(px(small, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(small, 1, 0)[3], 0);
    assert_eq!(px(small, 0, 1), [0, 0, 255, 255]);
    assert_eq!(px(small, 1, 1), [255, 255, 255, 255]);

    let icon = QIcon::from_ico_data(&ico).unwrap();
    assert_eq!(
        icon.available_sizes(IconMode::Normal, IconState::Off),
        vec![Size::new(2, 2), Size::new(4, 4)]
    );
}

#[test]
fn test_ico_palette_and_32bpp_alpha_frames() {
    // 4-bpp palette frame: indices bottom row (0, 1), top row (1, 0).
    let mut dib4 = dib_header(4, 16);
    let mut pal = vec![0u8; 64];
    pal[0..3].copy_from_slice(&[30, 20, 10]); // BGR of (10, 20, 30)
    pal[4..7].copy_from_slice(&[50, 100, 200]); // BGR of (200, 100, 50)
    dib4.extend_from_slice(&pal);
    dib4.extend_from_slice(&[0x01, 0, 0, 0, 0x10, 0, 0, 0]);
    dib4.extend_from_slice(&[0; 8]); // AND: fully opaque
    let ico = build_ico(&[(2, 2, dib4)]);
    let img = ImageReader::read_from_memory(&ico).unwrap();
    assert_eq!(px(&img, 0, 0), [200, 100, 50, 255]);
    assert_eq!(px(&img, 1, 0), [10, 20, 30, 255]);
    assert_eq!(px(&img, 0, 1), [10, 20, 30, 255]);
    assert_eq!(px(&img, 1, 1), [200, 100, 50, 255]);

    // 32-bpp frame: its own alpha wins over the (all-opaque) AND mask.
    let mut dib32 = dib_header(32, 0);
    dib32.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // bottom row, alpha 0
    dib32.extend_from_slice(&[0, 0, 255, 128, 0, 0, 255, 255]); // top row: red @128, red @255
    dib32.extend_from_slice(&[0; 8]);
    let ico = build_ico(&[(2, 2, dib32)]);
    let img = ImageReader::read_from_memory(&ico).unwrap();
    assert_eq!(px(&img, 0, 0)[3], 128);
    assert_eq!(px(&img, 1, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 0, 1)[3], 0);
}

#[test]
fn test_ico_rejects_malformed_data() {
    assert!(ImageReader::read_from_memory(&[0, 0, 1, 0, 0, 0]).is_err()); // zero frames
    assert!(ImageReader::read_from_memory(&[0, 0, 1, 0, 5, 0]).is_err()); // truncated directory
    // Frame pointing past the end of the file.
    let mut ico = build_ico(&[(2, 2, vec![0; 4])]);
    ico[6 + 8..6 + 12].copy_from_slice(&1000u32.to_le_bytes());
    assert!(ImageReader::read_from_memory(&ico).is_err());
    // DIB header promising far more pixels than the data holds.
    let mut dib = dib_header(32, 0);
    dib[4..8].copy_from_slice(&32767i32.to_le_bytes());
    dib[8..12].copy_from_slice(&65534i32.to_le_bytes());
    assert!(ImageReader::read_from_memory(&build_ico(&[(0, 0, dib)])).is_err());
    assert!(QPixmap::load_from_data(b"not an image").is_none());
}

/// A DIB frame header with explicit width and raw height field (no pixel data).
fn dib_header_sized(width: i32, height_field: i32, bpp: u16) -> Vec<u8> {
    let mut h = dib_header(bpp, 0);
    h[4..8].copy_from_slice(&width.to_le_bytes());
    h[8..12].copy_from_slice(&height_field.to_le_bytes());
    h
}

#[test]
fn test_ico_rejects_oversize_dib() {
    // 257x257 is past the ICO format limit; the data is complete, so only the cap rejects it.
    let (w, h) = (257usize, 257usize);
    let mut dib = dib_header_sized(w as i32, (2 * h) as i32, 24);
    dib.extend_from_slice(&vec![0u8; (w * 3).div_ceil(4) * 4 * h + w.div_ceil(32) * 4 * h]);
    assert!(ImageReader::read_from_memory(&build_ico(&[(0, 0, dib.clone())])).is_err());
    assert!(ImageReader::read_ico_frames(&build_ico(&[(0, 0, dib)])).is_err());

    // 256 is the largest accepted extent, in either direction.
    for (w, h) in [(256usize, 2usize), (2, 256)] {
        let mut dib = dib_header_sized(w as i32, (2 * h) as i32, 24);
        dib.extend_from_slice(&vec![0u8; (w * 3).div_ceil(4) * 4 * h + w.div_ceil(32) * 4 * h]);
        let img = ImageReader::read_from_memory(&build_ico(&[(0, 0, dib)])).unwrap();
        assert_eq!((img.width(), img.height()), (w as u32, h as u32));
    }
}

#[test]
fn test_ico_rejects_oversize_png_header() {
    let mut big = Image::new(4, 4, ImageFormat::Rgba8888);
    big.fill(Color::WHITE);
    let mut png = ImageWriter::write_to_memory(&big, ImageFileFormat::Png).unwrap();
    // Rewrite the IHDR width to 100000 (the CRC is not checked before the size cap).
    png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    assert!(ImageReader::read_ico_frames(&build_ico(&[(0, 0, png.clone())])).is_err());
    assert!(ImageReader::read_from_memory(&png).is_err());
}

#[test]
fn test_ico_directory_bomb_hits_pixel_budget() {
    // One 256x256 frame referenced by many directory entries: each decodes to 64K pixels,
    // so the total budget must stop the read long before all entries are decoded.
    let (w, h) = (256usize, 256usize);
    let mut dib = dib_header_sized(w as i32, (2 * h) as i32, 24);
    dib.extend_from_slice(&vec![0u8; w * 3 * h + w.div_ceil(32) * 4 * h]);
    let count = 200usize;
    let offset = 6 + count * 16;
    let mut ico = vec![0, 0, 1, 0];
    ico.extend_from_slice(&(count as u16).to_le_bytes());
    for _ in 0..count {
        ico.extend_from_slice(&[0, 0, 0, 0, 1, 0, 24, 0]);
        ico.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        ico.extend_from_slice(&(offset as u32).to_le_bytes());
    }
    ico.extend_from_slice(&dib);
    assert!(ImageReader::read_ico_frames(&ico).is_err());
    assert!(QIcon::from_ico_data(&ico).is_err());
}

#[test]
fn test_ico_32bpp_without_mask_and_undoubled_height() {
    // 32-bpp, doubled height field but no AND mask: alpha is taken from the pixels.
    let mut dib = dib_header(32, 0);
    dib.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]); // bottom row: alpha 0
    dib.extend_from_slice(&[0, 0, 255, 128, 0, 0, 255, 255]); // top row
    let img = ImageReader::read_from_memory(&build_ico(&[(2, 2, dib)])).unwrap();
    assert_eq!((img.width(), img.height()), (2, 2));
    assert_eq!(px(&img, 0, 0), [255, 0, 0, 128]);
    assert_eq!(px(&img, 1, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 0, 1)[3], 0);

    // Fully zero alpha and no mask: opaque.
    let mut dib = dib_header(32, 0);
    dib.extend_from_slice(&[0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0]);
    let img = ImageReader::read_from_memory(&build_ico(&[(2, 2, dib)])).unwrap();
    assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);

    // Undoubled height field (2 rows) with XOR-only 24-bpp data of exactly that size.
    let mut dib = dib_header_sized(2, 2, 24);
    dib.extend_from_slice(&[255, 0, 0, 255, 255, 255, 0, 0]); // bottom: blue, white
    dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // top: red, green
    let img = ImageReader::read_from_memory(&build_ico(&[(2, 2, dib)])).unwrap();
    assert_eq!((img.width(), img.height()), (2, 2));
    assert_eq!(px(&img, 0, 0), [255, 0, 0, 255]);
    assert_eq!(px(&img, 1, 0), [0, 255, 0, 255]);
    assert_eq!(px(&img, 0, 1), [0, 0, 255, 255]);
    assert_eq!(px(&img, 1, 1), [255, 255, 255, 255]);
}
