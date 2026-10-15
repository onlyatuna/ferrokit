// SPDX-License-Identifier: LGPL-3.0-only
//! Integration tests for Modern Color and Pixel Format System (`ferrokit-gui::color`).

use ferrokit_gui::color::*;

#[test]
fn test_pixel_format_presets() {
    // RGBA8888
    let rgba = PixelFormat::rgba8888();
    assert_eq!(rgba.color_model(), ColorModel::Rgb);
    assert_eq!(rgba.red_size(), 8);
    assert_eq!(rgba.green_size(), 8);
    assert_eq!(rgba.blue_size(), 8);
    assert_eq!(rgba.alpha_size(), 8);
    assert_eq!(rgba.bits_per_pixel(), 32);
    assert_eq!(rgba.bytes_per_pixel(), 4);
    assert_eq!(rgba.channel_count(), 4);
    assert!(rgba.has_alpha());
    assert!(!rgba.is_premultiplied());
    assert_eq!(rgba.alpha_position(), AlphaPosition::AtEnd);

    // Premultiplied RGBA8888
    let premul = PixelFormat::rgba8888_premultiplied();
    assert!(premul.is_premultiplied());

    // BGRA8888
    let bgra = PixelFormat::bgra8888();
    assert_eq!(bgra.color_model(), ColorModel::Bgr);
    assert_eq!(bgra.red_size(), 8);
    assert_eq!(bgra.blue_size(), 8);
    assert_eq!(bgra.bits_per_pixel(), 32);

    // RGB888
    let rgb = PixelFormat::rgb888();
    assert_eq!(rgb.color_model(), ColorModel::Rgb);
    assert_eq!(rgb.bits_per_pixel(), 24);
    assert_eq!(rgb.bytes_per_pixel(), 3);
    assert_eq!(rgb.channel_count(), 3);
    assert!(!rgb.has_alpha());

    // Grayscale8
    let gray = PixelFormat::grayscale8();
    assert_eq!(gray.color_model(), ColorModel::Grayscale);
    assert_eq!(gray.first_size(), 8);
    assert_eq!(gray.bits_per_pixel(), 8);
    assert_eq!(gray.bytes_per_pixel(), 1);

    // HDR RGBA32F
    let hdr = PixelFormat::rgba32f();
    assert_eq!(hdr.bits_per_pixel(), 128);
    assert_eq!(hdr.bytes_per_pixel(), 16);
    assert_eq!(hdr.type_interpretation(), TypeInterpretation::FloatingPoint);
}

#[test]
fn test_color_space_transfer_functions() {
    let srgb = ColorSpace::srgb();
    assert!(!srgb.is_linear());

    // sRGB transfer roundtrip
    for val in [0.0f32, 0.01, 0.04, 0.1, 0.5, 0.8, 1.0] {
        let linear = srgb.to_linear(val);
        let back = srgb.to_encoded(linear);
        assert!(
            (val - back).abs() < 1e-4,
            "sRGB roundtrip mismatch for {}",
            val
        );
    }

    // Linear sRGB
    let linear_cs = ColorSpace::linear_srgb();
    assert!(linear_cs.is_linear());
    assert_eq!(linear_cs.to_linear(0.75), 0.75);
    assert_eq!(linear_cs.to_encoded(0.75), 0.75);

    // Gamma 2.2
    let adobe = ColorSpace::adobe_rgb();
    let val = 0.5f32;
    let lin = adobe.to_linear(val);
    assert!((val - adobe.to_encoded(lin)).abs() < 1e-4);

    // BT.2100 PQ (HDR)
    let pq = ColorSpace::bt2100_pq();
    let hdr_val = 0.6f32;
    let pq_lin = pq.to_linear(hdr_val);
    let pq_back = pq.to_encoded(pq_lin);
    assert!((hdr_val - pq_back).abs() < 1e-3);

    // BT.2100 HLG (HDR)
    let hlg = ColorSpace::bt2100_hlg();
    let hlg_val = 0.6f32;
    let hlg_lin = hlg.to_linear(hlg_val);
    let hlg_back = hlg.to_encoded(hlg_lin);
    assert!((hlg_val - hlg_back).abs() < 1e-3);
}

#[test]
fn test_color_transform_pipeline() {
    let srgb = ColorSpace::srgb();
    let linear = ColorSpace::linear_srgb();

    // Identity check
    let id_transform = ColorTransform::new(&srgb, &srgb);
    assert!(id_transform.is_identity());
    let (r, g, b, a) = id_transform.map_rgba(0.5, 0.2, 0.8, 1.0);
    assert_eq!((r, g, b, a), (0.5, 0.2, 0.8, 1.0));

    // sRGB -> Linear sRGB
    let to_linear = srgb.transformation_to_color_space(&linear);
    assert!(!to_linear.is_identity());

    let (lr, lg, lb) = to_linear.map_rgb(1.0, 0.0, 0.5);
    assert!((lr - 1.0).abs() < 1e-4);
    assert!(lg.abs() < 1e-5);
    assert!(lb < 0.5); // sRGB 0.5 in linear light is ~0.214

    // Display P3 -> sRGB
    let p3 = ColorSpace::display_p3();
    let p3_to_srgb = p3.transformation_to_color_space(&srgb);
    let (sr, _sg, _sb) = p3_to_srgb.map_rgb(1.0, 0.0, 0.0); // P3 pure red is wider than sRGB red
    assert!(sr > 0.0);

    // 8-bit mapping
    let (u_r, u_g, u_b, u_a) = id_transform.map_rgba_u8(255, 128, 64, 255);
    assert_eq!((u_r, u_g, u_b, u_a), (255, 128, 64, 255));
}

#[test]
fn test_hdr_color_and_tonemapping() {
    // Normal SDR color
    let sdr = HdrColor::new(0.5, 0.5, 0.5, 1.0);
    assert!(!sdr.is_hdr());

    // HDR color with bright highlights
    let hdr = HdrColor::new(2.5, 4.0, 1.2, 1.0);
    assert!(hdr.is_hdr());
    assert!(hdr.luminance() > 1.0);

    // Alpha premultiplication
    let semi = HdrColor::new(1.0, 0.5, 0.2, 0.5);
    let premul = semi.premultiplied();
    assert_eq!(premul.r, 0.5);
    assert_eq!(premul.g, 0.25);
    assert_eq!(premul.b, 0.1);
    assert_eq!(premul.a, 0.5);

    let un_premul = premul.unpremultiplied();
    assert!((un_premul.r - 1.0).abs() < 1e-5);
    assert!((un_premul.g - 0.5).abs() < 1e-5);
    assert!((un_premul.b - 0.2).abs() < 1e-5);

    // Tone Mapping: Reinhard
    let tm_reinhard = hdr.tone_map_reinhard();
    assert!(tm_reinhard.r <= 1.0 && tm_reinhard.r > 0.0);
    assert!(tm_reinhard.g <= 1.0 && tm_reinhard.g > 0.0);
    assert!(tm_reinhard.b <= 1.0 && tm_reinhard.b > 0.0);

    // Tone Mapping: ACES
    let tm_aces = hdr.tone_map_aces();
    assert!(tm_aces.r <= 1.0 && tm_aces.r > 0.0);
    assert!(tm_aces.g <= 1.0 && tm_aces.g > 0.0);
    assert!(tm_aces.b <= 1.0 && tm_aces.b > 0.0);

    // Tone Mapping: Exposure
    let tm_exp = hdr.tone_map_exposure(0.5);
    assert!(tm_exp.r <= 1.0 && tm_exp.r > 0.0);
    assert!(tm_exp.g <= 1.0 && tm_exp.g > 0.0);
}

#[test]
fn test_icc_profile_parser() {
    let mut fake_icc = vec![0u8; 128];
    // Set size
    fake_icc[0..4].copy_from_slice(&128u32.to_be_bytes());
    // Set magic 'acsp' at offset 36..40
    fake_icc[36..40].copy_from_slice(b"acsp");
    // Set color space 'RGB ' at offset 16..20
    fake_icc[16..20].copy_from_slice(b"RGB ");

    let profile = IccProfile::from_bytes(&fake_icc).expect("valid icc header");
    assert!(profile.is_valid());
    assert_eq!(profile.color_model(), Some(ColorModel::Rgb));
    let cs = profile.to_color_space().expect("to color space");
    assert_eq!(cs.primaries(), Primaries::Srgb);

    // Invalid ICC header (wrong magic)
    fake_icc[36..40].copy_from_slice(b"xxxx");
    assert!(IccProfile::from_bytes(&fake_icc).is_none());
}

#[test]
fn test_qt_canonical_aliases() {
    let _qpf: QPixelFormat = PixelFormat::rgba8888();
    let _qcs: QColorSpace = ColorSpace::srgb();
    let _qct: QColorTransform = ColorTransform::identity();
    let _qhdr: QRgbaFloat32 = HdrColor::WHITE;
    let _qicc: QIccProfile = IccProfile::new();
}
