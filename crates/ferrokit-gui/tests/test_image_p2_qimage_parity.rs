//! P2 QImage parity regressions: size caps, stride validation, padded stride, copy, scaling, unpremultiply.
use ferrokit_gui::geometry::primitives::Rect;
use ferrokit_gui::image::*;
use ferrokit_gui::paint::pixmap::Pixmap;
use tiny_skia::Color;

#[test]
fn new_overflowing_size_is_null() {
    assert!(Image::new(u32::MAX, u32::MAX, ImageFormat::Rgba8888).is_null());
    assert!(Image::new(1 << 30, 2, ImageFormat::Rgba8888).is_null());
    assert!(Image::new(i32::MAX as u32 + 1, 1, ImageFormat::Mono).is_null());
}

#[test]
fn from_data_and_view_reject_short_stride() {
    let data = vec![0u8; 64];
    assert!(Image::from_data(data.clone(), 4, 2, 8, ImageFormat::Rgba8888).is_none());
    assert!(ImageView::new(&data, 4, 2, 8, ImageFormat::Rgba8888).is_none());
    assert!(Image::from_data(data.clone(), 4, 2, 16, ImageFormat::Rgba8888).is_some());
}

#[test]
fn pixmap_from_padded_stride_image() {
    // 1x2 RGBA, stride 8 (4 bytes padding).
    let data = vec![1, 2, 3, 255, 0, 0, 0, 0, 4, 5, 6, 255, 0, 0, 0, 0];
    let img = Image::from_data(data, 1, 2, 8, ImageFormat::Rgba8888Premultiplied).unwrap();
    let pm = Pixmap::from_image(&img).unwrap();
    assert_eq!(&pm.data()[..4], &[1, 2, 3, 255]);
    assert_eq!(&pm.data()[4..8], &[4, 5, 6, 255]);
}

#[test]
fn copy_rect_pads_outside_with_zero_and_keeps_size() {
    let mut img = Image::new(2, 2, ImageFormat::Argb32);
    img.fill(Color::from_rgba8(255, 0, 0, 255));
    let c = img.copy_rect(Rect::new(-1, -1, 3, 3));
    assert_eq!((c.width(), c.height()), (3, 3));
    assert_eq!(c.pixel_color(0, 0).unwrap().alpha(), 0.0);
    assert_eq!(c.pixel_color(1, 1).unwrap().to_color_u8().red(), 255);
}

#[test]
fn scaled_smooth_differs_from_fast() {
    let mut img = Image::new(2, 1, ImageFormat::Rgba8888);
    img.set_pixel_color(0, 0, Color::BLACK);
    img.set_pixel_color(1, 0, Color::WHITE);
    let fast = img.scaled(4, 1, AspectRatioMode::IgnoreAspectRatio, TransformationMode::FastTransformation);
    let smooth = img.scaled(4, 1, AspectRatioMode::IgnoreAspectRatio, TransformationMode::SmoothTransformation);
    assert_eq!(fast.pixel_color(1, 0).unwrap().to_color_u8().red(), 0);
    let mid = smooth.pixel_color(1, 0).unwrap().to_color_u8().red();
    assert!(mid > 0 && mid < 255, "got {mid}");
}

#[test]
fn unpremultiply_rounds_like_qt() {
    // c=1, a=3 -> round(255/3)=85 ; c=1,a=200 -> round(1.275)=1 ; c=2,a=3 -> round(170)=170
    let data = vec![1, 1, 1, 200, 2, 2, 2, 3];
    let img = Image::from_data(data, 2, 1, 8, ImageFormat::Rgba8888Premultiplied).unwrap();
    assert_eq!(img.pixel_color(0, 0).unwrap().to_color_u8().red(), 1);
    // 1*255/200 = 1.275 -> floor 1, round 1; use value distinguishing floor vs round:
    let data = vec![3, 0, 0, 4];
    let img = Image::from_data(data, 1, 1, 4, ImageFormat::Rgba8888Premultiplied).unwrap();
    // 3*255/4 = 191.25 -> 191 both; use 2/3 -> 170 exactly; use 5/7 -> 182.14; pick 2/7 -> 72.86: floor 72, round 73
    let _ = img;
    let data = vec![2, 0, 0, 7];
    let img = Image::from_data(data, 1, 1, 4, ImageFormat::Rgba8888Premultiplied).unwrap();
    assert_eq!(img.pixel_color(0, 0).unwrap().to_color_u8().red(), 73);
}
