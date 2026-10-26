// SPDX-License-Identifier: LGPL-3.0-only
//! `ferrokit_gui::color::Color` (`QColor`) against Qt 6.11.2.
//!
//! `tests/data/qcolor_oracle.txt` holds measurements of PySide6 6.11.2's real `QColor`
//! (regenerate it with `tests/data/gen_qcolor_oracle.py`): every HSL round trip of the HUD gauge
//! (`h, s, l, _ = QColor(c).getHslF(); QColor.fromHslF(max(0.0, h), s, l).name()`) for all
//! 16 777 216 `#rrggbb` colours as per-red digests plus 1 200 spot rows, the other colour
//! specifications, the `lighter` / `darker` of HSL colours, and colour-name parsing.
//! `reference` below is a second, independent transcription of `QColor::toHsl()` and the HSL branch
//! of `QColor::toRgb()` from `qcolor.cpp` (v6.11.2, lines 2293-2339 and 2422-2470).

use ferrokit_gui::color::{darker, lighter, Color, ColorSpec, GlobalColor, NameFormat, QColor};

const ORACLE: &str = include_str!("data/qcolor_oracle.txt");

fn lines(kind: &'static str) -> impl Iterator<Item = Vec<&'static str>> {
    ORACLE
        .lines()
        .filter(move |l| l.split(' ').next() == Some(kind))
        .map(|l| l.split(' ').skip(1).collect())
}

fn bits(s: &str) -> f32 {
    f32::from_bits(u32::from_str_radix(s, 16).unwrap())
}

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s, 16).unwrap()
}

fn int(s: &str) -> i32 {
    i32::from_str_radix(s, 16).unwrap()
}

/// What the HUD gauge does to a `#rrggbb` colour: `(h, s, l)` of `getHslF()` and the `rgb()` of
/// `fromHslF(max(0.0, h), s, l)`.
fn gauge_round_trip(rgb: u32) -> ([f32; 3], u32) {
    let c = Color::from_rgb_u32(rgb);
    let (h, s, l, _) = c.get_hsl_f();
    let out = Color::from_hsl_f(h.max(0.0), s, l, 1.0);
    ([h, s, l], out.rgb() & 0xff_ffff)
}

/// An independent transcription of `QColor::toHsl` / `QColor::toRgb` (HSL), `qcolor.cpp` v6.11.2.
mod reference {
    fn q_round(x: f32) -> u16 {
        (x + 0.5) as i32 as u16
    }

    /// `toHsl` of the RGB colour `rgb`: `(hue, saturation, lightness)` in Qt's 16-bit storage.
    pub fn to_hsl(rgb: u32) -> (u16, u16, u16) {
        let r = ((rgb >> 16) & 255) as u16 * 0x101;
        let g = ((rgb >> 8) & 255) as u16 * 0x101;
        let b = (rgb & 255) as u16 * 0x101;
        let (min, max) = (r.min(g).min(b), r.max(g).max(b));
        if min == max {
            return (u16::MAX, 0, max);
        }
        let chroma = (max - min) as f32;
        let lightness = 0.5f32 * (max as u32 + min as u32) as f32;
        let saturation = 0.5f32 * chroma / lightness.min(65535.0f32 - lightness);
        let mut hue = if max == r {
            0.0f32 + (g as i32 - b as i32) as f32 / chroma
        } else if max == g {
            2.0f32 + (b as i32 - r as i32) as f32 / chroma
        } else {
            4.0f32 + (r as i32 - g as i32) as f32 / chroma
        };
        if max == r && hue < 0.0 {
            hue += 6.0;
        }
        (
            q_round(hue * (60.0 * 100.0)),
            q_round(saturation * 65535.0),
            q_round(lightness),
        )
    }

    /// `fromHslF(h, s, l)` followed by `rgb()`, with `h`, `s`, `l` as `getHslF()` reports them.
    pub fn from_hsl_f_to_rgb(h: f32, s: f32, l: f32) -> u32 {
        let hue = if h == -1.0 {
            u16::MAX
        } else {
            match q_round(h * 36000.0) {
                36000 => 0,
                v => v,
            }
        };
        let sat = q_round(s * 65535.0);
        let light = q_round(l * 65535.0);
        let rgb16: [u16; 3] = if sat == 0 || hue == u16::MAX {
            [light; 3]
        } else if light == 0 {
            [0; 3]
        } else {
            let h = if hue == 36000 {
                0.0
            } else {
                hue as f32 / 36000.0
            };
            let s = sat as f32 / 65535.0;
            let l = light as f32 / 65535.0;
            let temp2 = if l < 0.5 {
                l * (1.0 + s)
            } else {
                l + s - l * s
            };
            let temp1 = 2.0 * l - temp2;
            let third = 1.0f32 / 3.0f32;
            let mut out = [0u16; 3];
            for (i, mut t) in [h + third, h, h - third].into_iter().enumerate() {
                if t < 0.0 {
                    t += 1.0;
                } else if t > 1.0 {
                    t -= 1.0;
                }
                let v = if t * 6.0 < 1.0 {
                    temp1 + (temp2 - temp1) * (t * 6.0)
                } else if t * 2.0 < 1.0 {
                    temp2
                } else if t * 3.0 < 2.0 {
                    temp1 + (temp2 - temp1) * (2.0 / 3.0 - t) * 6.0
                } else {
                    temp1
                };
                out[i] = q_round(v * 65535.0);
                if out[i] == 1 {
                    out[i] = 0;
                }
            }
            out
        };
        // qt_div_257 of every component.
        let div = |x: u16| {
            let x = x as u32 + 128;
            (x - (x >> 8)) >> 8
        };
        div(rgb16[0]) << 16 | div(rgb16[1]) << 8 | div(rgb16[2])
    }

    /// The HUD gauge round trip: `(h, s, l)` as `getHslF()` returns them, and the result.
    pub fn gauge_round_trip(rgb: u32) -> ([f32; 3], u32) {
        let (hue, sat, light) = to_hsl(rgb);
        let h = if hue == u16::MAX {
            -1.0
        } else {
            hue as f32 / 36000.0
        };
        let s = sat as f32 / 65535.0;
        let l = light as f32 / 65535.0;
        ([h, s, l], from_hsl_f_to_rgb(h.max(0.0), s, l))
    }
}

#[test]
fn gauge_hsl_round_trip_matches_qt_on_spot_colours() {
    let mut rows = 0;
    for f in lines("hsl") {
        let rgb = hex(f[0]);
        let (hsl, out) = gauge_round_trip(rgb);
        let want = [bits(f[1]), bits(f[2]), bits(f[3])];
        assert_eq!(
            hsl.map(f32::to_bits),
            want.map(f32::to_bits),
            "getHslF of #{rgb:06x}"
        );
        assert_eq!(out, hex(f[4]), "round trip of #{rgb:06x}");
        // `.name()` is the `#rrggbb` of that value.
        let (h, s, l, _) = Color::from_string(&format!("#{rgb:06x}")).get_hsl_f();
        assert_eq!(
            Color::from_hsl_f(h.max(0.0), s, l, 1.0).name(),
            format!("#{:06x}", hex(f[4]))
        );
        rows += 1;
    }
    assert!(rows >= 1000, "oracle rows: {rows}");
}

fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Every one of the 16 777 216 `#rrggbb` colours (Qt's digests are per red value, see the oracle
/// generator for the definition); under Miri a stride through the space checked against Qt's
/// algorithm transcribed in `reference`.
#[test]
fn gauge_hsl_round_trip_matches_qt_for_every_rgb_colour() {
    if cfg!(miri) {
        for rgb in (0..1u32 << 24).step_by(98_317) {
            assert_eq!(
                gauge_round_trip(rgb),
                reference::gauge_round_trip(rgb),
                "#{rgb:06x}"
            );
        }
        return;
    }
    let mut chunks = 0;
    for f in lines("sweep") {
        let r = hex(f[0]);
        let mut digest = 0u64;
        for idx in 0..65536u32 {
            let rgb = r << 16 | idx;
            let ([h, s, l], out) = gauge_round_trip(rgb);
            let mut e = mix(u64::from(idx));
            for col in [h.to_bits(), s.to_bits(), l.to_bits(), out] {
                e = mix(e ^ u64::from(col));
            }
            digest = digest.wrapping_add(e);
        }
        assert_eq!(
            digest,
            u64::from_str_radix(f[1], 16).unwrap(),
            "digest of the colours #{r:02x}0000..#{r:02x}ffff"
        );
        chunks += 1;
    }
    assert_eq!(chunks, 256);
}

#[test]
fn gauge_hsl_round_trip_agrees_with_the_transcribed_algorithm() {
    let stride = if cfg!(miri) { 400_009 } else { 1009 };
    for rgb in (0..1u32 << 24).step_by(stride) {
        assert_eq!(
            gauge_round_trip(rgb),
            reference::gauge_round_trip(rgb),
            "#{rgb:06x}"
        );
    }
    // Every grey and every colour with at most two distinct channel values (all the edge cases of
    // the integer conversion: hue 0/60/.../300, saturation 100 %, lightness 0 and 100 %).
    for rgb in (0..256u32)
        .map(|v| v * 0x01_0101)
        .chain((0..64u32).flat_map(|m| {
            let pick = |bit: u32| if m >> bit & 1 == 1 { 255u32 } else { 0 };
            let pick2 = |bit: u32| if m >> bit & 1 == 1 { 128u32 } else { 1 };
            [
                pick(0) << 16 | pick(1) << 8 | pick(2),
                pick2(3) << 16 | pick2(4) << 8 | pick2(5),
            ]
        }))
    {
        assert_eq!(
            gauge_round_trip(rgb),
            reference::gauge_round_trip(rgb),
            "#{rgb:06x}"
        );
    }
}

#[test]
fn hsl_of_known_colours_is_what_qt_reports() {
    // PySide6 6.11.2: QColor("#abcdef").getHslF() and QColor("#808080").getHslF().
    let (h, s, l, a) = Color::from_string("#abcdef").get_hsl_f();
    assert_eq!(
        (h as f64, s as f64, l as f64, a),
        (
            0.5833333134651184,
            0.6800030469894409,
            0.8039215803146362,
            1.0
        )
    );
    let grey = Color::from_string("#808080");
    assert_eq!(grey.get_hsl_f(), (-1.0, 0.0, 0.501_960_8, 1.0));
    assert_eq!(grey.hsl_hue(), -1);
    assert_eq!(grey.hsl_hue_f(), -1.0);
    assert_eq!(grey.lightness(), 128);
    assert_eq!(grey.hsl_saturation(), 0);
    assert_eq!(
        (
            Color::from_string("#abcdef").hsl_hue(),
            Color::from_string("#abcdef").lightness()
        ),
        (210, 205)
    );
    assert_eq!(Color::from_string("#abcdef").hsv_hue(), 210);
    assert_eq!(Color::from_string("#abcdef").value(), 239);
    assert_eq!(Color::from_string("#abcdef").hsv_saturation(), 73);
}

#[test]
fn other_specifications_match_qt() {
    let mut rows = 0;
    for f in lines("conv") {
        let argb = hex(f[0]);
        let c = Color::new(
            ((argb >> 16) & 255) as i32,
            ((argb >> 8) & 255) as i32,
            (argb & 255) as i32,
            (argb >> 24) as i32,
        );
        let what = format!("{argb:08x}");
        let (h, s, v, a) = c.get_hsv_f();
        assert_eq!(
            [h, s, v].map(f32::to_bits),
            [bits(f[1]), bits(f[2]), bits(f[3])].map(f32::to_bits),
            "getHsvF {what}"
        );
        assert_eq!(a, c.alpha_f());
        let hsl = c.get_hsl();
        assert_eq!(
            (hsl.0, hsl.1, hsl.2),
            (int(f[4]), int(f[5]), int(f[6])),
            "getHsl {what}"
        );
        let cmyk = c.get_cmyk();
        assert_eq!(
            (cmyk.0, cmyk.1, cmyk.2, cmyk.3),
            (int(f[7]), int(f[8]), int(f[9]), int(f[10])),
            "getCmyk {what}"
        );
        let cmyk_f = c.get_cmyk_f();
        assert_eq!(
            [cmyk_f.0, cmyk_f.1, cmyk_f.2, cmyk_f.3].map(f32::to_bits),
            [bits(f[11]), bits(f[12]), bits(f[13]), bits(f[14])].map(f32::to_bits),
            "getCmykF {what}"
        );
        let hsv_f = c.get_hsv_f();
        let (ch, cs, cv, ca) = c.get_hsv();
        let round_trips = [
            Color::from_hsv_f(hsv_f.0, hsv_f.1, hsv_f.2, hsv_f.3).rgba(),
            Color::from_cmyk_f(cmyk_f.0, cmyk_f.1, cmyk_f.2, cmyk_f.3, cmyk_f.4).rgba(),
            Color::from_hsl(hsl.0, hsl.1, hsl.2, hsl.3).rgba(),
            Color::from_hsv(ch, cs, cv, ca).rgba(),
        ];
        for (i, got) in round_trips.into_iter().enumerate() {
            assert_eq!(got, hex(f[15 + i]), "round trip #{i} of {what}");
        }
        let (hh, ss, ll, aa) = c.get_hsl_f();
        let base = Color::from_hsl_f(hh, ss, ll, aa);
        let variants = [
            base.lighter(150),
            base.darker(150),
            base.darker(250),
            base.lighter(50),
        ];
        for (i, v) in variants.into_iter().enumerate() {
            let w = &f[19 + i * 5..24 + i * 5];
            assert_eq!(v.spec() as i32, int(w[0]), "spec of variant #{i} of {what}");
            let g = v.get_hsl_f();
            assert_eq!(
                [g.0, g.1, g.2].map(f32::to_bits),
                [bits(w[1]), bits(w[2]), bits(w[3])].map(f32::to_bits),
                "getHslF of variant #{i} of {what}"
            );
            assert_eq!(v.rgba(), hex(w[4]), "rgba of variant #{i} of {what}");
        }
        rows += 1;
    }
    assert!(rows >= 400, "oracle rows: {rows}");
}

#[test]
fn colours_built_from_float_components_match_qt() {
    let mut rows = 0;
    for f in lines("from") {
        let c: Vec<f32> = f[1..5].iter().map(|s| bits(s)).collect();
        let alpha = bits(f[5]);
        let color = match f[0] {
            "hsv" => Color::from_hsv_f(c[0], c[1], c[2], alpha),
            "hsl" => Color::from_hsl_f(c[0], c[1], c[2], alpha),
            "cmyk" => Color::from_cmyk_f(c[0], c[1], c[2], c[3], alpha),
            "rgb" => Color::from_rgb_f(c[0], c[1], c[2], alpha),
            other => panic!("unknown kind {other}"),
        };
        let what = format!("{} {:?} {}", f[0], c, alpha);
        assert_eq!(color.spec() as i32, int(f[6]), "spec of {what}");
        assert_eq!(color.rgba(), hex(f[7]), "rgba of {what}");
        let (h, s, l, _) = color.get_hsl_f();
        assert_eq!(
            [h, s, l].map(f32::to_bits),
            [bits(f[8]), bits(f[9]), bits(f[10])].map(f32::to_bits),
            "getHslF of {what}"
        );
        let (h, s, v, _) = color.get_hsv_f();
        assert_eq!(
            [h, s, v].map(f32::to_bits),
            [bits(f[11]), bits(f[12]), bits(f[13])].map(f32::to_bits),
            "getHsvF of {what}"
        );
        let (cy, m, y, k, _) = color.get_cmyk_f();
        assert_eq!(
            [cy, m, y, k].map(f32::to_bits),
            [bits(f[14]), bits(f[15]), bits(f[16]), bits(f[17])].map(f32::to_bits),
            "getCmykF of {what}"
        );
        rows += 1;
    }
    assert_eq!(rows, 1400);
}

#[test]
fn colour_names_parse_like_qt() {
    let mut rows = 0;
    for f in lines("name") {
        let name = if f[0] == "-" {
            String::new()
        } else {
            let bytes: Vec<u8> = (0..f[0].len() / 2)
                .map(|i| u8::from_str_radix(&f[0][i * 2..i * 2 + 2], 16).unwrap())
                .collect();
            String::from_utf8(bytes).unwrap()
        };
        let c = Color::from_string(&name);
        assert_eq!(c.is_valid(), f[1] == "1", "isValid of {name:?}");
        assert_eq!(c.rgba(), hex(f[2]), "rgba() of {name:?}");
        assert_eq!(Color::from(name.as_str()), c);
        assert_eq!(Color::is_valid_color_name(&name), c.is_valid());
        rows += 1;
    }
    assert!(rows >= 60, "oracle rows: {rows}");
}

#[test]
fn invalid_names_are_invalid_and_not_black() {
    for bad in [
        "",
        "#",
        "#12",
        "#12345",
        "#gggggg",
        "nosuchcolor",
        "#fff ",
        "rgb(1,2,3)",
    ] {
        let c = Color::from_string(bad);
        assert!(!c.is_valid(), "{bad:?}");
        assert_eq!(c.spec(), ColorSpec::Invalid);
        assert_ne!(c, Color::from_string("black"), "{bad:?}");
        assert_eq!(c.to_tiny_skia(), None);
        assert!(tiny_skia::Color::try_from(c).is_err());
    }
    assert_eq!(Color::from_string("black").name(), "#000000");
    assert!(Color::from_string("black").is_valid());
    assert!(!Color::default().is_valid());
    assert_eq!(Color::default(), Color::invalid());
}

#[test]
fn names_and_name_formats() {
    let c = Color::from_string("#80aabbcc");
    assert_eq!(c.name(), "#aabbcc");
    assert_eq!(c.name_with(NameFormat::HexRgb), "#aabbcc");
    assert_eq!(c.name_with(NameFormat::HexArgb), "#80aabbcc");
    assert_eq!(
        (c.alpha(), c.red(), c.green(), c.blue()),
        (0x80, 0xaa, 0xbb, 0xcc)
    );
    assert_eq!(Color::from_string("#abc").name(), "#aabbcc");
    assert_eq!(Color::from_string("#ABC").name(), "#aabbcc");
    assert_eq!(
        Color::from_string("#fff").name_with(NameFormat::HexArgb),
        "#ffffffff"
    );
    assert_eq!(
        Color::from_string("transparent").name_with(NameFormat::HexArgb),
        "#00000000"
    );
    assert_eq!(Color::from_string(" Alice Blue\t").name(), "#f0f8ff");
    assert_eq!(
        Color::from_string("#123456789").rgba64(),
        (0x1231, 0x4564, 0x7897, 0xffff)
    );
    assert_eq!(
        Color::from_string("#123456789abc").rgba64(),
        (0x1234, 0x5678, 0x9abc, 0xffff)
    );
    // Qt formats an invalid colour like opaque black.
    assert_eq!(Color::invalid().name(), "#000000");
    assert_eq!(Color::invalid().name_with(NameFormat::HexArgb), "#ff000000");
    // Qt: `QColor("red\u4e00")` is valid because the non-Latin-1 character ends the C string.
    assert!(Color::from_string("red\u{4e00}").is_valid());
    assert!(!Color::from_string("re\u{4e00}d").is_valid());
}

#[test]
fn the_name_table_is_qts() {
    let names = Color::color_names();
    assert_eq!(names.len(), 148);
    assert!(names.windows(2).all(|w| w[0] < w[1]), "sorted and unique");
    for n in &names {
        assert!(Color::from_string(n).is_valid(), "{n}");
        assert!(Color::from_string(&n.to_uppercase()).is_valid(), "{n}");
    }
    // CSS Color 4 keyword that Qt's SVG 1.0 table does not have.
    assert!(!Color::from_string("rebeccapurple").is_valid());
    assert_eq!(Color::from_string("yellowgreen").name(), "#9acd32");
    assert_eq!(
        Color::from_string("darkgrey"),
        Color::from_string("darkgray")
    );
}

#[test]
fn constructors_validate_their_arguments_like_qt() {
    let c = Color::new(10, 20, 30, 40);
    assert_eq!(c.rgba64(), (0x0a0a, 0x1414, 0x1e1e, 0x2828));
    assert_eq!(c.rgba(), 0x280a_141e);
    assert_eq!(c.rgb(), 0xff0a_141e);
    assert_eq!(
        Color::new(10, 20, 30, 255),
        Color::from_rgb(10, 20, 30, 255)
    );
    assert_eq!(Color::from_rgba(0x280a_141e), c);
    assert_eq!(Color::from_rgb_u32(0x280a_141e).alpha(), 255);
    // Out of range: invalid. The constructor leaves alpha 0, `fromRgb` returns `QColor()`.
    assert!(!Color::new(256, 0, 0, 255).is_valid());
    assert!(!Color::new(-1, 0, 0, 255).is_valid());
    assert!(!Color::from_rgb(0, 0, 0, 256).is_valid());
    assert_ne!(Color::new(300, 0, 0, 255), Color::invalid());
    assert_eq!(Color::from_rgb(300, 0, 0, 255), Color::invalid());
    assert_eq!(Color::new(300, 0, 0, 255).rgba(), 0);
    assert!(!Color::from_hsv(360, 0, 0, 255).is_valid());
    assert!(Color::from_hsv(-1, 255, 255, 255).is_valid());
    assert!(!Color::from_hsl_f(1.5, 0.0, 0.0, 1.0).is_valid());
    assert!(!Color::from_hsl_f(f32::NAN, 0.0, 0.0, 1.0).is_valid());
    assert!(!Color::from_rgb_f(0.5, 0.5, 0.5, 1.5).is_valid());
    assert!(!Color::from_rgb_f(f32::NAN, 0.5, 0.5, 1.0).is_valid());
    assert!(!Color::from_cmyk(0, 0, 0, 0, 256).is_valid());
    // Float RGB outside 0..=1 reads back clamped (Qt: ExtendedRgb).
    assert_eq!(Color::from_rgb_f(1.5, -0.5, 0.5, 1.0).rgba(), 0xffff_0080);
    assert_eq!(
        Color::from_rgba64(0x1234, 0x5678, 0x9abc, 0xdef0).rgba(),
        0xde12_569a
    );
}

#[test]
fn global_colours_match_qt() {
    assert_eq!(Color::from(GlobalColor::DarkGray).name(), "#808080");
    assert_eq!(Color::from(GlobalColor::Gray).name(), "#a0a0a4");
    assert_eq!(Color::from(GlobalColor::LightGray).name(), "#c0c0c0");
    assert_eq!(Color::from(GlobalColor::Color0).name(), "#ffffff");
    assert_eq!(Color::from(GlobalColor::Color1).name(), "#000000");
    assert_eq!(Color::from(GlobalColor::DarkYellow).name(), "#808000");
    assert_eq!(
        Color::from(GlobalColor::Transparent).name_with(NameFormat::HexArgb),
        "#00000000"
    );
    assert_eq!(
        Color::from_global(GlobalColor::Red),
        Color::new(255, 0, 0, 255)
    );
}

#[test]
fn equality_is_qcolors() {
    let hsv = Color::from_hsv_f(0.5, 0.5, 0.5, 1.0);
    // Different specifications are never equal, even when they denote the same colour.
    assert_ne!(hsv, hsv.to_rgb());
    assert_eq!(hsv, Color::from_hsv_f(0.5, 0.5, 0.5, 1.0));
    assert_eq!(hsv.to_rgb(), hsv.to_rgb());
    // Hues compare modulo 36000.
    let mut a = Color::from_hsl(0, 20, 30, 255);
    let mut b = a;
    b.set_hsl_f(1.0, 20.0 / 255.0, 30.0 / 255.0, 1.0);
    assert_eq!(b.get_hsl_f().0, 1.0);
    a.set_hsl_f(0.0, 20.0 / 255.0, 30.0 / 255.0, 1.0);
    assert_eq!(a, b);
    assert_ne!(Color::new(1, 2, 3, 4), Color::new(1, 2, 3, 5));
    assert_eq!(
        Color::from_cmyk(1, 2, 3, 4, 5),
        Color::from_cmyk(1, 2, 3, 4, 5)
    );
    assert_ne!(
        Color::from_cmyk(1, 2, 3, 4, 5),
        Color::from_cmyk(1, 2, 3, 5, 5)
    );
}

#[test]
fn setters_follow_qt() {
    // Qt: setRed() on an invalid colour turns it into an RGB colour with opaque black elsewhere.
    let mut c = Color::default();
    c.set_red(5);
    assert_eq!((c.spec(), c.rgba()), (ColorSpec::Rgb, 0xff05_0000));
    let mut c = Color::new(10, 20, 30, 40);
    c.set_alpha(300);
    assert_eq!(c.alpha(), 255);
    c.set_alpha_f(f32::NAN);
    assert_eq!(c.alpha(), 0);
    c.set_alpha_f(0.5);
    assert_eq!(c.alpha(), 128);
    c.set_green(-4);
    assert_eq!(c.green(), 0);
    c.set_blue_f(1.5);
    assert_eq!(c.blue(), 255);
    c.set_red_f(0.5);
    assert_eq!(c.red_f(), 32768.0 / 65535.0);
    // setRed on a HSV colour converts it.
    let mut h = Color::from_hsv(0, 255, 255, 255);
    h.set_red(0);
    assert_eq!(h.spec(), ColorSpec::Rgb);
    assert_eq!(h.rgba(), 0xff00_0000);
    let mut c = Color::default();
    c.set_hsv(400, 10, 10, 40);
    assert_eq!((c.spec(), c.hsv_hue()), (ColorSpec::Hsv, 40));
    c.set_rgb(1, 2, 3, 4);
    assert_eq!(c.get_rgb(), (1, 2, 3, 4));
}

#[test]
fn lighter_and_darker_keep_the_specification() {
    let hsl = Color::from_hsl(120, 200, 100, 255);
    let l = hsl.lighter(150);
    assert_eq!(l.spec(), ColorSpec::Hsl);
    // PySide6 6.11.2: QColor.fromHsl(120, 200, 100).lighter(150).getHsl() == (120, 255, 149, 255)
    assert_eq!(l.get_hsl(), (120, 255, 149, 255));
    assert_eq!(hsl.darker(200).get_hsl(), (120, 200, 50, 255));
    assert_eq!(hsl.darker(50).get_hsl(), (120, 255, 194, 255));
    assert_eq!(
        Color::new(200, 100, 50, 255).lighter(150).rgba(),
        0xffff_9e6d
    );
    assert_eq!(
        Color::new(200, 100, 50, 255).darker(200).rgba(),
        0xff64_3219
    );
    // Non-positive factors leave the colour alone.
    assert_eq!(hsl.lighter(0), hsl);
    assert_eq!(hsl.darker(-5), hsl);
    assert_eq!(Color::default().lighter(200), Color::default());
    assert_eq!(Color::default().darker(200), Color::default());
}

#[test]
fn qcolor_ops_delegates_to_the_colour_type() {
    let skia = tiny_skia::Color::from_rgba8(200, 100, 50, 14);
    for factor in [-3, 0, 1, 50, 99, 100, 101, 150, 200, 300, 12_345, i32::MAX] {
        let got = darker(skia, factor);
        let want = Color::new(200, 100, 50, 14).darker(factor);
        assert_eq!(Color::from(got).rgba(), want.rgba(), "darker({factor})");
        let got = lighter(skia, factor);
        let want = Color::new(200, 100, 50, 14).lighter(factor);
        assert_eq!(Color::from(got).rgba(), want.rgba(), "lighter({factor})");
    }
}

#[test]
fn tiny_skia_conversions() {
    let c = Color::new(10, 20, 30, 40);
    assert_eq!(
        c.to_tiny_skia(),
        Some(tiny_skia::Color::from_rgba8(10, 20, 30, 40))
    );
    assert_eq!(
        tiny_skia::Color::try_from(c),
        Ok(tiny_skia::Color::from_rgba8(10, 20, 30, 40))
    );
    assert_eq!(Color::from(tiny_skia::Color::from_rgba8(10, 20, 30, 40)), c);
    // An HSL colour paints as the 8-bit RGB value Qt reports for it.
    let hsl = Color::from_hsl_f(0.5, 0.5, 0.5, 1.0);
    let skia: tiny_skia::Color = hsl.try_into().unwrap();
    assert_eq!(Color::from(skia).rgba(), hsl.rgba());
    // Every 8-bit colour survives the trip.
    for v in 0..=255 {
        let c = Color::new(v, 255 - v, v / 2, v);
        assert_eq!(Color::from(c.to_tiny_skia().unwrap()), c);
    }
}

#[test]
fn qcolor_alias_and_default() {
    let c: QColor = Color::from("#112233");
    assert_eq!(c.get_rgb(), (0x11, 0x22, 0x33, 255));
    assert_eq!(
        format!("{:?}", Color::default()),
        "Color(Invalid, [65535, 0, 0, 0, 0])"
    );
    assert_eq!(Color::from_hsl(-1, 0, 255, 255).to_rgb().name(), "#ffffff");
}
