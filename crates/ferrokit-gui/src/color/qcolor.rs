// SPDX-License-Identifier: LGPL-3.0-only
//! `QColor`: an RGB / HSV / HSL / CMYK colour with Qt 6 semantics.
//!
//! Mirrors `qtbase/src/gui/painting/qcolor.cpp` and `qcolor.h` at tag **v6.11.2** (the Qt that
//! PySide6 6.11.2 ships). Like Qt, every component is stored as 16 bits (`[alpha, c1, c2, c3, c4]`
//! next to the colour specification) and the 8-bit accessors round with `qt_div_257`
//! (`qdrawhelper_p.h:886-887`). Conversions between specifications (`toRgb`, `toHsv`, `toHsl`,
//! `toCmyk`) use the same `float` arithmetic and `qRound` as Qt, so `getHslF()` followed by
//! `fromHslF(..).name()` reproduces Qt's result for every `#rrggbb`.
//!
//! Differences from Qt:
//! * `QColor::ExtendedRgb` (colour components outside `0.0..=1.0`, stored as `qfloat16`) is not
//!   modelled. A float RGB component outside the range is clamped to it, which is exactly what
//!   `rgba()`, `name()` and `toRgb()` of an `ExtendedRgb` colour return; a `NaN` component or an
//!   alpha outside `0.0..=1.0` makes the colour invalid, as in Qt.
//! * Qt prints a warning when an argument is out of range; this type just returns an invalid colour.
//! * Colours are `Color` (aliased `QColor`); conversion to the painter's `tiny_skia::Color`
//!   goes through the 8-bit `rgba()` value.

use std::fmt;

/// `QColor::Spec` (without `ExtendedRgb`, see the module documentation).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorSpec {
    Invalid,
    Rgb,
    Hsv,
    Cmyk,
    Hsl,
}

/// `QColor::NameFormat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameFormat {
    /// `#RRGGBB`.
    HexRgb,
    /// `#AARRGGBB`.
    HexArgb,
}

/// `Qt::GlobalColor`, in Qt's enumeration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum GlobalColor {
    Color0,
    Color1,
    Black,
    White,
    DarkGray,
    Gray,
    LightGray,
    Red,
    Green,
    Blue,
    Cyan,
    Magenta,
    Yellow,
    DarkRed,
    DarkGreen,
    DarkBlue,
    DarkCyan,
    DarkMagenta,
    DarkYellow,
    Transparent,
}

/// The error of `TryFrom<Color> for tiny_skia::Color`: the colour is invalid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidColor;

impl fmt::Display for InvalidColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("invalid color")
    }
}

impl std::error::Error for InvalidColor {}

const USHRT_MAX: u16 = u16::MAX;
const USHRT_MAX_F: f32 = 65535.0;

/// `qRound(float)`: round half away from zero, truncating to `int`.
fn q_round(x: f32) -> i32 {
    if x >= 0.0 {
        (x + 0.5) as i32
    } else {
        (x - 0.5) as i32
    }
}

/// `qRound` stored into a `ushort` (`int` to `ushort` conversion wraps, as in C++).
fn round16(x: f32) -> u16 {
    q_round(x) as u16
}

/// `qt_div_257`: a 16-bit component to 8 bits, rounded.
fn div_257(x: u16) -> u8 {
    let x = x as u32 + 128;
    ((x - (x >> 8)) >> 8) as u8
}

fn int_in_range(v: i32) -> bool {
    (0..256).contains(&v)
}

/// `qColorComponentIsInFloatRange` (false for NaN).
fn float_in_range(v: f32) -> bool {
    (0.0..=1.0).contains(&v)
}

/// `qColorHueIsInFloatRange`.
fn hue_f_in_range(h: f32) -> bool {
    float_in_range(h) || h == -1.0
}

fn hue_in_range(h: i32, degrees_limit: bool) -> bool {
    h >= -1 && (!degrees_limit || h < 360)
}

/// The CSS / SVG 1.0 colour keywords plus `transparent` (`rgbTbl`, `qcolor.cpp:129-281`), as
/// `(name, 0xAARRGGBB)` sorted by name.
static NAMED_COLORS: &[(&str, u32)] = &[
    ("aliceblue", 0xfff0f8ff),
    ("antiquewhite", 0xfffaebd7),
    ("aqua", 0xff00ffff),
    ("aquamarine", 0xff7fffd4),
    ("azure", 0xfff0ffff),
    ("beige", 0xfff5f5dc),
    ("bisque", 0xffffe4c4),
    ("black", 0xff000000),
    ("blanchedalmond", 0xffffebcd),
    ("blue", 0xff0000ff),
    ("blueviolet", 0xff8a2be2),
    ("brown", 0xffa52a2a),
    ("burlywood", 0xffdeb887),
    ("cadetblue", 0xff5f9ea0),
    ("chartreuse", 0xff7fff00),
    ("chocolate", 0xffd2691e),
    ("coral", 0xffff7f50),
    ("cornflowerblue", 0xff6495ed),
    ("cornsilk", 0xfffff8dc),
    ("crimson", 0xffdc143c),
    ("cyan", 0xff00ffff),
    ("darkblue", 0xff00008b),
    ("darkcyan", 0xff008b8b),
    ("darkgoldenrod", 0xffb8860b),
    ("darkgray", 0xffa9a9a9),
    ("darkgreen", 0xff006400),
    ("darkgrey", 0xffa9a9a9),
    ("darkkhaki", 0xffbdb76b),
    ("darkmagenta", 0xff8b008b),
    ("darkolivegreen", 0xff556b2f),
    ("darkorange", 0xffff8c00),
    ("darkorchid", 0xff9932cc),
    ("darkred", 0xff8b0000),
    ("darksalmon", 0xffe9967a),
    ("darkseagreen", 0xff8fbc8f),
    ("darkslateblue", 0xff483d8b),
    ("darkslategray", 0xff2f4f4f),
    ("darkslategrey", 0xff2f4f4f),
    ("darkturquoise", 0xff00ced1),
    ("darkviolet", 0xff9400d3),
    ("deeppink", 0xffff1493),
    ("deepskyblue", 0xff00bfff),
    ("dimgray", 0xff696969),
    ("dimgrey", 0xff696969),
    ("dodgerblue", 0xff1e90ff),
    ("firebrick", 0xffb22222),
    ("floralwhite", 0xfffffaf0),
    ("forestgreen", 0xff228b22),
    ("fuchsia", 0xffff00ff),
    ("gainsboro", 0xffdcdcdc),
    ("ghostwhite", 0xfff8f8ff),
    ("gold", 0xffffd700),
    ("goldenrod", 0xffdaa520),
    ("gray", 0xff808080),
    ("green", 0xff008000),
    ("greenyellow", 0xffadff2f),
    ("grey", 0xff808080),
    ("honeydew", 0xfff0fff0),
    ("hotpink", 0xffff69b4),
    ("indianred", 0xffcd5c5c),
    ("indigo", 0xff4b0082),
    ("ivory", 0xfffffff0),
    ("khaki", 0xfff0e68c),
    ("lavender", 0xffe6e6fa),
    ("lavenderblush", 0xfffff0f5),
    ("lawngreen", 0xff7cfc00),
    ("lemonchiffon", 0xfffffacd),
    ("lightblue", 0xffadd8e6),
    ("lightcoral", 0xfff08080),
    ("lightcyan", 0xffe0ffff),
    ("lightgoldenrodyellow", 0xfffafad2),
    ("lightgray", 0xffd3d3d3),
    ("lightgreen", 0xff90ee90),
    ("lightgrey", 0xffd3d3d3),
    ("lightpink", 0xffffb6c1),
    ("lightsalmon", 0xffffa07a),
    ("lightseagreen", 0xff20b2aa),
    ("lightskyblue", 0xff87cefa),
    ("lightslategray", 0xff778899),
    ("lightslategrey", 0xff778899),
    ("lightsteelblue", 0xffb0c4de),
    ("lightyellow", 0xffffffe0),
    ("lime", 0xff00ff00),
    ("limegreen", 0xff32cd32),
    ("linen", 0xfffaf0e6),
    ("magenta", 0xffff00ff),
    ("maroon", 0xff800000),
    ("mediumaquamarine", 0xff66cdaa),
    ("mediumblue", 0xff0000cd),
    ("mediumorchid", 0xffba55d3),
    ("mediumpurple", 0xff9370db),
    ("mediumseagreen", 0xff3cb371),
    ("mediumslateblue", 0xff7b68ee),
    ("mediumspringgreen", 0xff00fa9a),
    ("mediumturquoise", 0xff48d1cc),
    ("mediumvioletred", 0xffc71585),
    ("midnightblue", 0xff191970),
    ("mintcream", 0xfff5fffa),
    ("mistyrose", 0xffffe4e1),
    ("moccasin", 0xffffe4b5),
    ("navajowhite", 0xffffdead),
    ("navy", 0xff000080),
    ("oldlace", 0xfffdf5e6),
    ("olive", 0xff808000),
    ("olivedrab", 0xff6b8e23),
    ("orange", 0xffffa500),
    ("orangered", 0xffff4500),
    ("orchid", 0xffda70d6),
    ("palegoldenrod", 0xffeee8aa),
    ("palegreen", 0xff98fb98),
    ("paleturquoise", 0xffafeeee),
    ("palevioletred", 0xffdb7093),
    ("papayawhip", 0xffffefd5),
    ("peachpuff", 0xffffdab9),
    ("peru", 0xffcd853f),
    ("pink", 0xffffc0cb),
    ("plum", 0xffdda0dd),
    ("powderblue", 0xffb0e0e6),
    ("purple", 0xff800080),
    ("red", 0xffff0000),
    ("rosybrown", 0xffbc8f8f),
    ("royalblue", 0xff4169e1),
    ("saddlebrown", 0xff8b4513),
    ("salmon", 0xfffa8072),
    ("sandybrown", 0xfff4a460),
    ("seagreen", 0xff2e8b57),
    ("seashell", 0xfffff5ee),
    ("sienna", 0xffa0522d),
    ("silver", 0xffc0c0c0),
    ("skyblue", 0xff87ceeb),
    ("slateblue", 0xff6a5acd),
    ("slategray", 0xff708090),
    ("slategrey", 0xff708090),
    ("snow", 0xfffffafa),
    ("springgreen", 0xff00ff7f),
    ("steelblue", 0xff4682b4),
    ("tan", 0xffd2b48c),
    ("teal", 0xff008080),
    ("thistle", 0xffd8bfd8),
    ("tomato", 0xffff6347),
    ("transparent", 0x00000000),
    ("turquoise", 0xff40e0d0),
    ("violet", 0xffee82ee),
    ("wheat", 0xfff5deb3),
    ("white", 0xffffffff),
    ("whitesmoke", 0xfff5f5f5),
    ("yellow", 0xffffff00),
    ("yellowgreen", 0xff9acd32),
];

/// `QColor::GlobalColor` constructor table (`qcolor.cpp:841-865`), as `0xAARRGGBB`.
fn global_rgba(color: GlobalColor) -> u32 {
    match color {
        GlobalColor::Color0 | GlobalColor::White => 0xffff_ffff,
        GlobalColor::Color1 | GlobalColor::Black => 0xff00_0000,
        GlobalColor::DarkGray => 0xff80_8080,
        GlobalColor::Gray => 0xffa0_a0a4,
        GlobalColor::LightGray => 0xffc0_c0c0,
        GlobalColor::Red => 0xffff_0000,
        GlobalColor::Green => 0xff00_ff00,
        GlobalColor::Blue => 0xff00_00ff,
        GlobalColor::Cyan => 0xff00_ffff,
        GlobalColor::Magenta => 0xffff_00ff,
        GlobalColor::Yellow => 0xffff_ff00,
        GlobalColor::DarkRed => 0xff80_0000,
        GlobalColor::DarkGreen => 0xff00_8000,
        GlobalColor::DarkBlue => 0xff00_0080,
        GlobalColor::DarkCyan => 0xff00_8080,
        GlobalColor::DarkMagenta => 0xff80_0080,
        GlobalColor::DarkYellow => 0xff80_8000,
        GlobalColor::Transparent => 0,
    }
}

/// `hex2int`: the value of `s` as hexadecimal, or -1.
fn hex2int(s: &[u8]) -> i32 {
    let mut result = 0i32;
    for &c in s {
        let h = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return -1,
        };
        result = result * 16 + h as i32;
    }
    result
}

/// `get_hex_rgb`: `#RGB`, `#RRGGBB`, `#AARRGGBB`, `#RRRGGGBBB` and `#RRRRGGGGBBBB` as 16-bit
/// `[r, g, b, a]`.
fn hex_rgba64(name: &[u8]) -> Option<[u16; 4]> {
    if name.first() != Some(&b'#') {
        return None;
    }
    let d = &name[1..];
    let mut a = 65535;
    let (r, g, b);
    match d.len() {
        12 => {
            r = hex2int(&d[0..4]);
            g = hex2int(&d[4..8]);
            b = hex2int(&d[8..12]);
        }
        9 => {
            let (r9, g9, b9) = (hex2int(&d[0..3]), hex2int(&d[3..6]), hex2int(&d[6..9]));
            if r9 == -1 || g9 == -1 || b9 == -1 {
                return None;
            }
            r = (r9 << 4) | (r9 >> 8);
            g = (g9 << 4) | (g9 >> 8);
            b = (b9 << 4) | (b9 >> 8);
        }
        8 => {
            a = hex2int(&d[0..2]) * 0x101;
            r = hex2int(&d[2..4]) * 0x101;
            g = hex2int(&d[4..6]) * 0x101;
            b = hex2int(&d[6..8]) * 0x101;
        }
        6 => {
            r = hex2int(&d[0..2]) * 0x101;
            g = hex2int(&d[2..4]) * 0x101;
            b = hex2int(&d[4..6]) * 0x101;
        }
        3 => {
            r = hex2int(&d[0..1]) * 0x1111;
            g = hex2int(&d[1..2]) * 0x1111;
            b = hex2int(&d[2..3]) * 0x1111;
        }
        _ => return None,
    }
    if [r, g, b, a].iter().any(|v| !(0..=65535).contains(v)) {
        return None;
    }
    Some([r as u16, g as u16, b as u16, a as u16])
}

/// `get_named_rgb`: blanks and tabs are ignored, the match is ASCII case-insensitive. Qt converts
/// the string with `QChar::toLatin1`, which maps every non-Latin-1 character to NUL and so ends the
/// C string there; that is reproduced.
fn named_rgba(name: &str) -> Option<u32> {
    if name.encode_utf16().count() > 255 {
        return None;
    }
    let mut key = Vec::with_capacity(name.len());
    for c in name.chars() {
        let byte = u8::try_from(u32::from(c)).unwrap_or_default();
        if byte == 0 {
            break;
        }
        if c != '\t' && c != ' ' {
            key.push(byte.to_ascii_lowercase());
        }
    }
    NAMED_COLORS
        .binary_search_by(|(n, _)| n.as_bytes().cmp(key.as_slice()))
        .ok()
        .map(|i| NAMED_COLORS[i].1)
}

/// A colour in the RGB, HSV, HSL or CMYK specification, or invalid (`QColor`).
///
/// `==` is `QColor::operator==`: the specifications and all 16-bit components must match (hues
/// compared modulo 36000 for HSV / HSL), so `Color::from_rgb(..)` is never equal to the same colour
/// in HSV.
#[derive(Clone, Copy)]
pub struct Color {
    spec: ColorSpec,
    /// `[alpha, red | hue | cyan, green | saturation | magenta, blue | value | lightness | yellow,
    /// pad | black]`, all 16-bit.
    ct: [u16; 5],
}

pub type QColor = Color;

impl Default for Color {
    /// `QColor()`: an invalid colour.
    fn default() -> Self {
        Self {
            spec: ColorSpec::Invalid,
            ct: [USHRT_MAX, 0, 0, 0, 0],
        }
    }
}

impl fmt::Debug for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Color({:?}, {:?})", self.spec, self.ct)
    }
}

impl PartialEq for Color {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (&self.ct, &other.ct);
        self.spec == other.spec
            && a[0] == b[0]
            && ((matches!(self.spec, ColorSpec::Hsv | ColorSpec::Hsl)
                && a[1] % 36000 == b[1] % 36000)
                || a[1] == b[1])
            && a[2] == b[2]
            && a[3] == b[3]
            && a[4] == b[4]
    }
}

impl Eq for Color {}

impl Color {
    /// `QColor()`: an invalid colour.
    pub const fn invalid() -> Self {
        Self {
            spec: ColorSpec::Invalid,
            ct: [USHRT_MAX, 0, 0, 0, 0],
        }
    }

    /// `QColor(int r, int g, int b, int a = 255)`. Each argument must be in `0..=255`, otherwise the
    /// colour is invalid (with alpha 0, unlike [`Color::invalid`]; this is Qt's constructor).
    pub const fn new(r: i32, g: i32, b: i32, a: i32) -> Self {
        if (r | g | b | a) >= 0 && r < 256 && g < 256 && b < 256 && a < 256 {
            Self {
                spec: ColorSpec::Rgb,
                ct: [
                    (a * 0x101) as u16,
                    (r * 0x101) as u16,
                    (g * 0x101) as u16,
                    (b * 0x101) as u16,
                    0,
                ],
            }
        } else {
            Self {
                spec: ColorSpec::Invalid,
                ct: [0; 5],
            }
        }
    }

    /// `QColor::fromRgb(int r, int g, int b, int a = 255)`: invalid unless every argument is in
    /// `0..=255`.
    pub fn from_rgb(r: i32, g: i32, b: i32, a: i32) -> Self {
        if ![r, g, b, a].into_iter().all(int_in_range) {
            return Self::invalid();
        }
        Self::new(r, g, b, a)
    }

    /// `QColor::fromRgb(QRgb)`: `0xAARRGGBB`, the alpha byte is ignored (the colour is opaque).
    pub fn from_rgb_u32(rgb: u32) -> Self {
        Self::new(
            ((rgb >> 16) & 0xff) as i32,
            ((rgb >> 8) & 0xff) as i32,
            (rgb & 0xff) as i32,
            255,
        )
    }

    /// `QColor::fromRgba(QRgb)`: `0xAARRGGBB`.
    pub fn from_rgba(argb: u32) -> Self {
        Self::new(
            ((argb >> 16) & 0xff) as i32,
            ((argb >> 8) & 0xff) as i32,
            (argb & 0xff) as i32,
            (argb >> 24) as i32,
        )
    }

    /// `QColor::fromRgba64`.
    pub fn from_rgba64(r: u16, g: u16, b: u16, a: u16) -> Self {
        Self {
            spec: ColorSpec::Rgb,
            ct: [a, r, g, b, 0],
        }
    }

    /// `QColor::fromRgbF`; see the module documentation for out-of-range components.
    pub fn from_rgb_f(r: f32, g: f32, b: f32, a: f32) -> Self {
        if !float_in_range(a) || r.is_nan() || g.is_nan() || b.is_nan() {
            return Self::invalid();
        }
        let c = |v: f32| round16(v.clamp(0.0, 1.0) * USHRT_MAX_F);
        Self {
            spec: ColorSpec::Rgb,
            ct: [round16(a * USHRT_MAX_F), c(r), c(g), c(b), 0],
        }
    }

    /// `QColor::fromHsv(int h, int s, int v, int a = 255)`: `h` in `-1..360`, the rest `0..=255`.
    pub fn from_hsv(h: i32, s: i32, v: i32, a: i32) -> Self {
        Self::hsx_int(ColorSpec::Hsv, h, s, v, a, true)
    }

    /// `QColor::fromHsvF`: all arguments in `0.0..=1.0`, `h` may also be `-1.0` (achromatic).
    pub fn from_hsv_f(h: f32, s: f32, v: f32, a: f32) -> Self {
        if !(hue_f_in_range(h) && float_in_range(s) && float_in_range(v) && float_in_range(a)) {
            return Self::invalid();
        }
        Self {
            spec: ColorSpec::Hsv,
            ct: [
                round16(a * USHRT_MAX_F),
                if h == -1.0 {
                    USHRT_MAX
                } else {
                    round16(h * 36000.0)
                },
                round16(s * USHRT_MAX_F),
                round16(v * USHRT_MAX_F),
                0,
            ],
        }
    }

    /// `QColor::fromHsl(int h, int s, int l, int a = 255)`: `h` in `-1..360`, the rest `0..=255`.
    pub fn from_hsl(h: i32, s: i32, l: i32, a: i32) -> Self {
        Self::hsx_int(ColorSpec::Hsl, h, s, l, a, true)
    }

    /// `QColor::fromHslF`: all arguments in `0.0..=1.0`, `h` may also be `-1.0` (achromatic). A hue
    /// that rounds to 360 degrees is stored as 0.
    pub fn from_hsl_f(h: f32, s: f32, l: f32, a: f32) -> Self {
        let mut color = Self::set_hsl_f_impl(h, s, l, a);
        if color.ct[1] == 36000 {
            color.ct[1] = 0;
        }
        color
    }

    fn set_hsl_f_impl(h: f32, s: f32, l: f32, a: f32) -> Self {
        if !(hue_f_in_range(h) && float_in_range(s) && float_in_range(l) && float_in_range(a)) {
            return Self::invalid();
        }
        Self {
            spec: ColorSpec::Hsl,
            ct: [
                round16(a * USHRT_MAX_F),
                if h == -1.0 {
                    USHRT_MAX
                } else {
                    round16(h * 36000.0)
                },
                round16(s * USHRT_MAX_F),
                round16(l * USHRT_MAX_F),
                0,
            ],
        }
    }

    /// `QColor::fromCmyk(int c, int m, int y, int k, int a = 255)`: every argument `0..=255`.
    pub fn from_cmyk(c: i32, m: i32, y: i32, k: i32, a: i32) -> Self {
        if ![c, m, y, k, a].into_iter().all(int_in_range) {
            return Self::invalid();
        }
        Self {
            spec: ColorSpec::Cmyk,
            ct: [
                (a * 0x101) as u16,
                (c * 0x101) as u16,
                (m * 0x101) as u16,
                (y * 0x101) as u16,
                (k * 0x101) as u16,
            ],
        }
    }

    /// `QColor::fromCmykF`: every argument in `0.0..=1.0`.
    pub fn from_cmyk_f(c: f32, m: f32, y: f32, k: f32, a: f32) -> Self {
        if ![c, m, y, k, a].into_iter().all(float_in_range) {
            return Self::invalid();
        }
        Self {
            spec: ColorSpec::Cmyk,
            ct: [
                round16(a * USHRT_MAX_F),
                round16(c * USHRT_MAX_F),
                round16(m * USHRT_MAX_F),
                round16(y * USHRT_MAX_F),
                round16(k * USHRT_MAX_F),
            ],
        }
    }

    fn hsx_int(spec: ColorSpec, h: i32, s: i32, x: i32, a: i32, degrees_limit: bool) -> Self {
        if !(hue_in_range(h, degrees_limit)
            && int_in_range(s)
            && int_in_range(x)
            && int_in_range(a))
        {
            return Self::invalid();
        }
        Self {
            spec,
            ct: [
                (a * 0x101) as u16,
                if h == -1 {
                    USHRT_MAX
                } else {
                    ((h % 360) * 100) as u16
                },
                (s * 0x101) as u16,
                (x * 0x101) as u16,
                0,
            ],
        }
    }

    /// `QColor::fromString` / `QColor(const QString &)`: `#RGB`, `#RRGGBB`, `#AARRGGBB`,
    /// `#RRRGGGBBB`, `#RRRRGGGGBBBB`, an SVG colour keyword (case-insensitive, blanks ignored) or
    /// `transparent`. Anything else gives an invalid colour.
    pub fn from_string(name: &str) -> Self {
        if name.is_empty() {
            return Self::invalid();
        }
        if name.as_bytes()[0] == b'#' {
            if name.encode_utf16().count() > 13 {
                return Self::invalid();
            }
            return match hex_rgba64(name.as_bytes()) {
                Some([r, g, b, a]) => Self::from_rgba64(r, g, b, a),
                None => Self::invalid(),
            };
        }
        match named_rgba(name) {
            Some(argb) => Self::from_rgba(argb),
            None => Self::invalid(),
        }
    }

    /// `QColor::isValidColorName`.
    pub fn is_valid_color_name(name: &str) -> bool {
        Self::from_string(name).is_valid()
    }

    /// `QColor::colorNames`: the keywords [`Color::from_string`] knows, sorted.
    pub fn color_names() -> Vec<&'static str> {
        NAMED_COLORS.iter().map(|(n, _)| *n).collect()
    }

    /// `QColor(Qt::GlobalColor)`.
    pub fn from_global(color: GlobalColor) -> Self {
        Self::from_rgba(global_rgba(color))
    }

    /// `QColor::isValid`.
    pub fn is_valid(&self) -> bool {
        self.spec != ColorSpec::Invalid
    }

    /// `QColor::spec`.
    pub fn spec(&self) -> ColorSpec {
        self.spec
    }

    /// `QColor::name()`: `#rrggbb`. Like Qt, an invalid colour prints as `#000000`.
    pub fn name(&self) -> String {
        self.name_with(NameFormat::HexRgb)
    }

    /// `QColor::name(NameFormat)`.
    pub fn name_with(&self, format: NameFormat) -> String {
        let argb = self.rgba();
        match format {
            NameFormat::HexRgb => format!("#{:06x}", argb & 0x00ff_ffff),
            NameFormat::HexArgb => format!("#{argb:08x}"),
        }
    }

    // --- RGB -------------------------------------------------------------------------------

    /// The components as seen in `target`: `ct` itself when the colour already is in that
    /// specification (or invalid), otherwise the converted ones.
    fn comps(&self, target: ColorSpec) -> [u16; 5] {
        if self.spec != ColorSpec::Invalid && self.spec != target {
            self.convert_to(target).ct
        } else {
            self.ct
        }
    }

    /// `QColor::rgba()`: `0xAARRGGBB`.
    pub fn rgba(&self) -> u32 {
        let c = self.comps(ColorSpec::Rgb);
        u32::from(div_257(c[0])) << 24
            | u32::from(div_257(c[1])) << 16
            | u32::from(div_257(c[2])) << 8
            | u32::from(div_257(c[3]))
    }

    /// `QColor::rgb()`: `0xFFRRGGBB`.
    pub fn rgb(&self) -> u32 {
        self.rgba() | 0xff00_0000
    }

    /// `QColor::rgba64()` as `(r, g, b, a)`.
    pub fn rgba64(&self) -> (u16, u16, u16, u16) {
        let c = self.comps(ColorSpec::Rgb);
        (c[1], c[2], c[3], c[0])
    }

    /// `QColor::red()`, 0-255.
    pub fn red(&self) -> i32 {
        i32::from(div_257(self.comps(ColorSpec::Rgb)[1]))
    }

    /// `QColor::green()`, 0-255.
    pub fn green(&self) -> i32 {
        i32::from(div_257(self.comps(ColorSpec::Rgb)[2]))
    }

    /// `QColor::blue()`, 0-255.
    pub fn blue(&self) -> i32 {
        i32::from(div_257(self.comps(ColorSpec::Rgb)[3]))
    }

    /// `QColor::alpha()`, 0-255.
    pub fn alpha(&self) -> i32 {
        i32::from(div_257(self.ct[0]))
    }

    /// `QColor::redF()`.
    pub fn red_f(&self) -> f32 {
        f32::from(self.comps(ColorSpec::Rgb)[1]) / USHRT_MAX_F
    }

    /// `QColor::greenF()`.
    pub fn green_f(&self) -> f32 {
        f32::from(self.comps(ColorSpec::Rgb)[2]) / USHRT_MAX_F
    }

    /// `QColor::blueF()`.
    pub fn blue_f(&self) -> f32 {
        f32::from(self.comps(ColorSpec::Rgb)[3]) / USHRT_MAX_F
    }

    /// `QColor::alphaF()`.
    pub fn alpha_f(&self) -> f32 {
        f32::from(self.ct[0]) / USHRT_MAX_F
    }

    /// `QColor::getRgb`: `(r, g, b, a)`, 0-255.
    pub fn get_rgb(&self) -> (i32, i32, i32, i32) {
        let c = self.comps(ColorSpec::Rgb);
        let d = |x| i32::from(div_257(x));
        (d(c[1]), d(c[2]), d(c[3]), d(c[0]))
    }

    /// `QColor::getRgbF`: `(r, g, b, a)`.
    pub fn get_rgb_f(&self) -> (f32, f32, f32, f32) {
        let c = self.comps(ColorSpec::Rgb);
        let f = |x| f32::from(x) / USHRT_MAX_F;
        (f(c[1]), f(c[2]), f(c[3]), f(c[0]))
    }

    /// `QColor::setRgb(int, int, int, int)`.
    pub fn set_rgb(&mut self, r: i32, g: i32, b: i32, a: i32) {
        *self = Self::from_rgb(r, g, b, a);
    }

    /// `QColor::setRgbF`.
    pub fn set_rgb_f(&mut self, r: f32, g: f32, b: f32, a: f32) {
        *self = Self::from_rgb_f(r, g, b, a);
    }

    /// `QColor::setRgba(QRgb)`.
    pub fn set_rgba(&mut self, argb: u32) {
        *self = Self::from_rgba(argb);
    }

    /// `QColor::setRgba64`.
    pub fn set_rgba64(&mut self, r: u16, g: u16, b: u16, a: u16) {
        *self = Self::from_rgba64(r, g, b, a);
    }

    /// `QColor::setRed`: the value is clamped to 0-255 (Qt warns); a non-RGB colour becomes RGB.
    pub fn set_red(&mut self, red: i32) {
        let red = red.clamp(0, 255);
        if self.spec != ColorSpec::Rgb {
            let (g, b, a) = (self.green(), self.blue(), self.alpha());
            self.set_rgb(red, g, b, a);
        } else {
            self.ct[1] = (red * 0x101) as u16;
        }
    }

    /// `QColor::setGreen`.
    pub fn set_green(&mut self, green: i32) {
        let green = green.clamp(0, 255);
        if self.spec != ColorSpec::Rgb {
            let (r, b, a) = (self.red(), self.blue(), self.alpha());
            self.set_rgb(r, green, b, a);
        } else {
            self.ct[2] = (green * 0x101) as u16;
        }
    }

    /// `QColor::setBlue`.
    pub fn set_blue(&mut self, blue: i32) {
        let blue = blue.clamp(0, 255);
        if self.spec != ColorSpec::Rgb {
            let (r, g, a) = (self.red(), self.green(), self.alpha());
            self.set_rgb(r, g, blue, a);
        } else {
            self.ct[3] = (blue * 0x101) as u16;
        }
    }

    /// `QColor::setAlpha`: clamped to 0-255.
    pub fn set_alpha(&mut self, alpha: i32) {
        self.ct[0] = (alpha.clamp(0, 255) * 0x101) as u16;
    }

    /// `QColor::setRedF`.
    pub fn set_red_f(&mut self, red: f32) {
        if self.spec == ColorSpec::Rgb && float_in_range(red) {
            self.ct[1] = round16(red * USHRT_MAX_F);
            return;
        }
        let red = if red.is_nan() { 0.0 } else { red };
        let (g, b, a) = (self.green_f(), self.blue_f(), self.alpha_f());
        self.set_rgb_f(red, g, b, a);
    }

    /// `QColor::setGreenF`.
    pub fn set_green_f(&mut self, green: f32) {
        if self.spec == ColorSpec::Rgb && float_in_range(green) {
            self.ct[2] = round16(green * USHRT_MAX_F);
            return;
        }
        let green = if green.is_nan() { 0.0 } else { green };
        let (r, b, a) = (self.red_f(), self.blue_f(), self.alpha_f());
        self.set_rgb_f(r, green, b, a);
    }

    /// `QColor::setBlueF`.
    pub fn set_blue_f(&mut self, blue: f32) {
        if self.spec == ColorSpec::Rgb && float_in_range(blue) {
            self.ct[3] = round16(blue * USHRT_MAX_F);
            return;
        }
        let blue = if blue.is_nan() { 0.0 } else { blue };
        let (r, g, a) = (self.red_f(), self.green_f(), self.alpha_f());
        self.set_rgb_f(r, g, blue, a);
    }

    /// `QColor::setAlphaF`: clamped to `0.0..=1.0` (`NaN` is 0).
    pub fn set_alpha_f(&mut self, alpha: f32) {
        let alpha = if alpha.is_nan() {
            0.0
        } else {
            alpha.clamp(0.0, 1.0)
        };
        self.ct[0] = round16(alpha * USHRT_MAX_F);
    }

    // --- HSV -------------------------------------------------------------------------------

    /// `QColor::getHsv`: `(h, s, v, a)`, hue `-1` when achromatic.
    pub fn get_hsv(&self) -> (i32, i32, i32, i32) {
        let c = self.comps(ColorSpec::Hsv);
        let d = |x| i32::from(div_257(x));
        (
            if c[1] == USHRT_MAX {
                -1
            } else {
                i32::from(c[1] / 100)
            },
            d(c[2]),
            d(c[3]),
            d(c[0]),
        )
    }

    /// `QColor::getHsvF`: `(h, s, v, a)`, hue `-1.0` when achromatic.
    pub fn get_hsv_f(&self) -> (f32, f32, f32, f32) {
        let c = self.comps(ColorSpec::Hsv);
        let f = |x| f32::from(x) / USHRT_MAX_F;
        (hue_f(c[1]), f(c[2]), f(c[3]), f(c[0]))
    }

    /// `QColor::hsvHue` / `hue`.
    pub fn hsv_hue(&self) -> i32 {
        self.get_hsv().0
    }

    /// `QColor::hsvSaturation` / `saturation`.
    pub fn hsv_saturation(&self) -> i32 {
        self.get_hsv().1
    }

    /// `QColor::value`.
    pub fn value(&self) -> i32 {
        self.get_hsv().2
    }

    /// `QColor::hsvHueF` / `hueF`.
    pub fn hsv_hue_f(&self) -> f32 {
        self.get_hsv_f().0
    }

    /// `QColor::hsvSaturationF` / `saturationF`.
    pub fn hsv_saturation_f(&self) -> f32 {
        self.get_hsv_f().1
    }

    /// `QColor::valueF`.
    pub fn value_f(&self) -> f32 {
        self.get_hsv_f().2
    }

    /// `QColor::setHsv`.
    pub fn set_hsv(&mut self, h: i32, s: i32, v: i32, a: i32) {
        *self = Self::hsx_int(ColorSpec::Hsv, h, s, v, a, false);
    }

    /// `QColor::setHsvF`.
    pub fn set_hsv_f(&mut self, h: f32, s: f32, v: f32, a: f32) {
        *self = Self::from_hsv_f(h, s, v, a);
    }

    // --- HSL -------------------------------------------------------------------------------

    /// `QColor::getHsl`: `(h, s, l, a)`, hue `-1` when achromatic.
    pub fn get_hsl(&self) -> (i32, i32, i32, i32) {
        let c = self.comps(ColorSpec::Hsl);
        let d = |x| i32::from(div_257(x));
        (
            if c[1] == USHRT_MAX {
                -1
            } else {
                i32::from(c[1] / 100)
            },
            d(c[2]),
            d(c[3]),
            d(c[0]),
        )
    }

    /// `QColor::getHslF`: `(h, s, l, a)`, hue `-1.0` when achromatic (the value Python's
    /// `QColor.getHslF()` also reports).
    pub fn get_hsl_f(&self) -> (f32, f32, f32, f32) {
        let c = self.comps(ColorSpec::Hsl);
        let f = |x| f32::from(x) / USHRT_MAX_F;
        (hue_f(c[1]), f(c[2]), f(c[3]), f(c[0]))
    }

    /// `QColor::hslHue`.
    pub fn hsl_hue(&self) -> i32 {
        self.get_hsl().0
    }

    /// `QColor::hslSaturation`.
    pub fn hsl_saturation(&self) -> i32 {
        self.get_hsl().1
    }

    /// `QColor::lightness`.
    pub fn lightness(&self) -> i32 {
        self.get_hsl().2
    }

    /// `QColor::hslHueF`.
    pub fn hsl_hue_f(&self) -> f32 {
        self.get_hsl_f().0
    }

    /// `QColor::hslSaturationF`.
    pub fn hsl_saturation_f(&self) -> f32 {
        self.get_hsl_f().1
    }

    /// `QColor::lightnessF`.
    pub fn lightness_f(&self) -> f32 {
        self.get_hsl_f().2
    }

    /// `QColor::setHsl`.
    pub fn set_hsl(&mut self, h: i32, s: i32, l: i32, a: i32) {
        *self = Self::hsx_int(ColorSpec::Hsl, h, s, l, a, false);
    }

    /// `QColor::setHslF`. Unlike [`Color::from_hsl_f`] a hue that rounds to 360 degrees is kept.
    pub fn set_hsl_f(&mut self, h: f32, s: f32, l: f32, a: f32) {
        *self = Self::set_hsl_f_impl(h, s, l, a);
    }

    // --- CMYK ------------------------------------------------------------------------------

    /// `QColor::getCmyk`: `(c, m, y, k, a)`.
    pub fn get_cmyk(&self) -> (i32, i32, i32, i32, i32) {
        let c = self.comps(ColorSpec::Cmyk);
        let d = |x| i32::from(div_257(x));
        (d(c[1]), d(c[2]), d(c[3]), d(c[4]), d(c[0]))
    }

    /// `QColor::getCmykF`: `(c, m, y, k, a)`.
    pub fn get_cmyk_f(&self) -> (f32, f32, f32, f32, f32) {
        let c = self.comps(ColorSpec::Cmyk);
        let f = |x| f32::from(x) / USHRT_MAX_F;
        (f(c[1]), f(c[2]), f(c[3]), f(c[4]), f(c[0]))
    }

    /// `QColor::cyan`.
    pub fn cyan(&self) -> i32 {
        self.get_cmyk().0
    }

    /// `QColor::magenta`.
    pub fn magenta(&self) -> i32 {
        self.get_cmyk().1
    }

    /// `QColor::yellow`.
    pub fn yellow(&self) -> i32 {
        self.get_cmyk().2
    }

    /// `QColor::black`.
    pub fn black(&self) -> i32 {
        self.get_cmyk().3
    }

    /// `QColor::cyanF`.
    pub fn cyan_f(&self) -> f32 {
        self.get_cmyk_f().0
    }

    /// `QColor::magentaF`.
    pub fn magenta_f(&self) -> f32 {
        self.get_cmyk_f().1
    }

    /// `QColor::yellowF`.
    pub fn yellow_f(&self) -> f32 {
        self.get_cmyk_f().2
    }

    /// `QColor::blackF`.
    pub fn black_f(&self) -> f32 {
        self.get_cmyk_f().3
    }

    /// `QColor::setCmyk`.
    pub fn set_cmyk(&mut self, c: i32, m: i32, y: i32, k: i32, a: i32) {
        *self = Self::from_cmyk(c, m, y, k, a);
    }

    /// `QColor::setCmykF`.
    pub fn set_cmyk_f(&mut self, c: f32, m: f32, y: f32, k: f32, a: f32) {
        *self = Self::from_cmyk_f(c, m, y, k, a);
    }

    // --- Conversions -----------------------------------------------------------------------

    /// `QColor::toRgb`.
    pub fn to_rgb(&self) -> Self {
        if !self.is_valid() || self.spec == ColorSpec::Rgb {
            return *self;
        }
        let [alpha, c1, c2, c3, c4] = self.ct;
        // `QColor color;` starts with all components 0.
        let mut rgb = [0u16; 3];
        match self.spec {
            ColorSpec::Hsv => {
                let (hue, sat, val) = (c1, c2, c3);
                if sat == 0 || hue == USHRT_MAX {
                    rgb = [val; 3];
                } else {
                    let h = if hue == 36000 {
                        0.0
                    } else {
                        f32::from(hue) / 6000.0
                    };
                    let s = f32::from(sat) / USHRT_MAX_F;
                    let v = f32::from(val) / USHRT_MAX_F;
                    let i = h as i32;
                    let f = h - i as f32;
                    let p = v * (1.0 - s);
                    let to = |x: f32| round16(x * USHRT_MAX_F);
                    if i & 1 != 0 {
                        let q = v * (1.0 - (s * f));
                        match i {
                            1 => rgb = [to(q), to(v), to(p)],
                            3 => rgb = [to(p), to(q), to(v)],
                            5 => rgb = [to(v), to(p), to(q)],
                            _ => {}
                        }
                    } else {
                        let t = v * (1.0 - (s * (1.0 - f)));
                        match i {
                            0 => rgb = [to(v), to(t), to(p)],
                            2 => rgb = [to(p), to(v), to(t)],
                            4 => rgb = [to(t), to(p), to(v)],
                            _ => {}
                        }
                    }
                }
            }
            ColorSpec::Hsl => {
                let (hue, sat, light) = (c1, c2, c3);
                if sat == 0 || hue == USHRT_MAX {
                    rgb = [light; 3];
                } else if light == 0 {
                    rgb = [0; 3];
                } else {
                    let h = if hue == 36000 {
                        0.0
                    } else {
                        f32::from(hue) / 36000.0
                    };
                    let s = f32::from(sat) / USHRT_MAX_F;
                    let l = f32::from(light) / USHRT_MAX_F;
                    let temp2 = if l < 0.5 {
                        l * (1.0 + s)
                    } else {
                        l + s - (l * s)
                    };
                    let temp1 = (2.0 * l) - temp2;
                    let mut temp3 = [h + (1.0 / 3.0), h, h - (1.0 / 3.0)];
                    for (out, t) in rgb.iter_mut().zip(temp3.iter_mut()) {
                        if *t < 0.0 {
                            *t += 1.0;
                        } else if *t > 1.0 {
                            *t -= 1.0;
                        }
                        let sixtemp3 = *t * 6.0;
                        let x = if sixtemp3 < 1.0 {
                            temp1 + (temp2 - temp1) * sixtemp3
                        } else if (*t * 2.0) < 1.0 {
                            temp2
                        } else if (*t * 3.0) < 2.0 {
                            temp1 + (temp2 - temp1) * (2.0 / 3.0 - *t) * 6.0
                        } else {
                            temp1
                        };
                        *out = round16(x * USHRT_MAX_F);
                    }
                    for v in &mut rgb {
                        if *v == 1 {
                            *v = 0;
                        }
                    }
                }
            }
            ColorSpec::Cmyk => {
                let c = f32::from(c1) / USHRT_MAX_F;
                let m = f32::from(c2) / USHRT_MAX_F;
                let y = f32::from(c3) / USHRT_MAX_F;
                let k = f32::from(c4) / USHRT_MAX_F;
                rgb = [
                    round16((1.0 - (c * (1.0 - k) + k)) * USHRT_MAX_F),
                    round16((1.0 - (m * (1.0 - k) + k)) * USHRT_MAX_F),
                    round16((1.0 - (y * (1.0 - k) + k)) * USHRT_MAX_F),
                ];
            }
            ColorSpec::Rgb | ColorSpec::Invalid => {}
        }
        Self {
            spec: ColorSpec::Rgb,
            ct: [alpha, rgb[0], rgb[1], rgb[2], 0],
        }
    }

    /// `(min, max)` of the 16-bit channels of an RGB colour.
    fn min_max(&self) -> (u16, u16) {
        let [_, r, g, b, _] = self.ct;
        (r.min(g).min(b), r.max(g).max(b))
    }

    /// The hue of an RGB colour in 1/100 degree (`toHsv` and `toHsl` share it).
    fn rgb_hue(&self, max: u16, chroma: f32) -> u16 {
        let [_, r, g, b, _] = self.ct;
        let (r, g, b) = (f32::from(r), f32::from(g), f32::from(b));
        let hue = if max == self.ct[1] {
            let mut h = 0.0 + (g - b) / chroma;
            if h < 0.0 {
                h += 6.0;
            }
            h
        } else if max == self.ct[2] {
            2.0 + (b - r) / chroma
        } else {
            4.0 + (r - g) / chroma
        };
        round16(hue * (60.0 * 100.0))
    }

    /// `QColor::toHsv`.
    pub fn to_hsv(&self) -> Self {
        if !self.is_valid() || self.spec == ColorSpec::Hsv {
            return *self;
        }
        if self.spec != ColorSpec::Rgb {
            return self.to_rgb().to_hsv();
        }
        let (min, max) = self.min_max();
        let (hue, sat) = if max == min {
            (USHRT_MAX, 0)
        } else {
            let chroma = f32::from(max - min);
            (
                self.rgb_hue(max, chroma),
                round16((chroma / f32::from(max)) * USHRT_MAX_F),
            )
        };
        Self {
            spec: ColorSpec::Hsv,
            ct: [self.ct[0], hue, sat, max, 0],
        }
    }

    /// `QColor::toHsl`.
    pub fn to_hsl(&self) -> Self {
        if !self.is_valid() || self.spec == ColorSpec::Hsl {
            return *self;
        }
        if self.spec != ColorSpec::Rgb {
            return self.to_rgb().to_hsl();
        }
        let (min, max) = self.min_max();
        let (hue, sat, light) = if max == min {
            (USHRT_MAX, 0, max)
        } else {
            let chroma = f32::from(max - min);
            let lightness = 0.5 * (u32::from(max) + u32::from(min)) as f32;
            let saturation = 0.5 * chroma / lightness.min(USHRT_MAX_F - lightness);
            (
                self.rgb_hue(max, chroma),
                round16(saturation * USHRT_MAX_F),
                round16(lightness),
            )
        };
        Self {
            spec: ColorSpec::Hsl,
            ct: [self.ct[0], hue, sat, light, 0],
        }
    }

    /// `QColor::toCmyk`.
    pub fn to_cmyk(&self) -> Self {
        if !self.is_valid() || self.spec == ColorSpec::Cmyk {
            return *self;
        }
        if self.spec != ColorSpec::Rgb {
            return self.to_rgb().to_cmyk();
        }
        let [alpha, r, g, b, _] = self.ct;
        let (c, m, y, k) = if r == 0 && g == 0 && b == 0 {
            (0, 0, 0, USHRT_MAX)
        } else {
            let mut c = 1.0 - f32::from(r) / USHRT_MAX_F;
            let mut m = 1.0 - f32::from(g) / USHRT_MAX_F;
            let mut y = 1.0 - f32::from(b) / USHRT_MAX_F;
            let k = c.min(m.min(y));
            c = (c - k) / (1.0 - k);
            m = (m - k) / (1.0 - k);
            y = (y - k) / (1.0 - k);
            (
                round16(c * USHRT_MAX_F),
                round16(m * USHRT_MAX_F),
                round16(y * USHRT_MAX_F),
                round16(k * USHRT_MAX_F),
            )
        };
        Self {
            spec: ColorSpec::Cmyk,
            ct: [alpha, c, m, y, k],
        }
    }

    /// `QColor::convertTo`.
    pub fn convert_to(&self, spec: ColorSpec) -> Self {
        if spec == self.spec {
            return *self;
        }
        match spec {
            ColorSpec::Rgb => self.to_rgb(),
            ColorSpec::Hsv => self.to_hsv(),
            ColorSpec::Cmyk => self.to_cmyk(),
            ColorSpec::Hsl => self.to_hsl(),
            ColorSpec::Invalid => Self::invalid(),
        }
    }

    /// `QColor::lighter(factor)`: converts to HSV, multiplies the value by `factor / 100` (the
    /// saturation absorbs an overflow) and converts back to this colour's specification.
    /// `factor <= 0` returns the colour unchanged; `factor < 100` is `darker(10000 / factor)`.
    pub fn lighter(&self, factor: i32) -> Self {
        if factor <= 0 {
            return *self;
        }
        if factor < 100 {
            return self.darker(10000 / factor);
        }
        let mut hsv = self.to_hsv();
        let mut s = i32::from(hsv.ct[2]);
        // `v = (factor * v) / 100` is evaluated in `unsigned int`.
        let mut v = (factor as u32).wrapping_mul(u32::from(hsv.ct[3])) / 100;
        if v > u32::from(USHRT_MAX) {
            // overflow... adjust saturation
            s = (s as u32).wrapping_sub(v - u32::from(USHRT_MAX)) as i32;
            if s < 0 {
                s = 0;
            }
            v = u32::from(USHRT_MAX);
        }
        hsv.ct[2] = s as u16;
        hsv.ct[3] = v as u16;
        hsv.convert_to(self.spec)
    }

    /// `QColor::darker(factor)`: converts to HSV, divides the value by `factor / 100` and converts
    /// back to this colour's specification. `factor <= 0` returns the colour unchanged;
    /// `factor < 100` is `lighter(10000 / factor)`.
    pub fn darker(&self, factor: i32) -> Self {
        if factor <= 0 {
            return *self;
        }
        if factor < 100 {
            return self.lighter(10000 / factor);
        }
        let mut hsv = self.to_hsv();
        hsv.ct[3] = ((i32::from(hsv.ct[3]) * 100) / factor) as u16;
        hsv.convert_to(self.spec)
    }

    // --- tiny_skia -------------------------------------------------------------------------

    /// The colour as the painter's `tiny_skia::Color` (through the 8-bit `rgba()` value), or `None`
    /// when it is invalid.
    pub fn to_tiny_skia(&self) -> Option<tiny_skia::Color> {
        if !self.is_valid() {
            return None;
        }
        let c = self.rgba();
        Some(tiny_skia::Color::from_rgba8(
            (c >> 16) as u8,
            (c >> 8) as u8,
            c as u8,
            (c >> 24) as u8,
        ))
    }
}

/// `QColor::hsvHueF` / `hslHueF` of a stored hue.
fn hue_f(hue: u16) -> f32 {
    if hue == USHRT_MAX {
        -1.0
    } else {
        f32::from(hue) / 36000.0
    }
}

impl From<&str> for Color {
    /// `QColor(const char *)`, see [`Color::from_string`].
    fn from(name: &str) -> Self {
        Self::from_string(name)
    }
}

impl From<GlobalColor> for Color {
    fn from(color: GlobalColor) -> Self {
        Self::from_global(color)
    }
}

impl From<tiny_skia::Color> for Color {
    /// The 8-bit RGBA value of `color` (components rounded to the nearest 1/255).
    fn from(color: tiny_skia::Color) -> Self {
        let to8 = |c: f32| (c * 255.0).round() as i32;
        Self::new(
            to8(color.red()),
            to8(color.green()),
            to8(color.blue()),
            to8(color.alpha()),
        )
    }
}

impl TryFrom<Color> for tiny_skia::Color {
    type Error = InvalidColor;

    fn try_from(color: Color) -> Result<Self, InvalidColor> {
        color.to_tiny_skia().ok_or(InvalidColor)
    }
}
