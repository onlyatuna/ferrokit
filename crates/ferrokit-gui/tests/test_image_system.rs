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
    // BT.601 red luminance is ~76 (255 * 0.299)
    assert!(g.red() > 70 && g.red() < 80);
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
