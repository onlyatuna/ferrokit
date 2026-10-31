// SPDX-License-Identifier: LGPL-3.0-only
//! The cursors Qt does not take from the system on Windows and draws itself: the split cursors,
//! the open and closed hand, and the three drag cursors (`QWindowsCursor::customCursor`,
//! qwindowscursor.cpp:190-405, the variant without PNG resources). Pure pixel data and scaling, so
//! it is the same on every platform and testable without a window system.

use crate::cursor::CursorShape;

/// A cursor image: premultiplied BGRA pixels, rows top to bottom, with its hot spot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorImage {
    pub width: usize,
    pub height: usize,
    pub hot_x: i32,
    pub hot_y: i32,
    pub bgra: Vec<u8>,
}

impl CursorImage {
    /// The pixel at `(x, y)` as `[b, g, r, a]` (premultiplied).
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        [
            self.bgra[i],
            self.bgra[i + 1],
            self.bgra[i + 2],
            self.bgra[i + 3],
        ]
    }
}

/// `QBitmap::fromData(QSize(size, size), bits)` (`Format_MonoLSB`: bit 0 of a byte is the leftmost
/// pixel) with `maskBits` as the mask: a set mask bit is opaque; a set data bit is `Qt::color1`
/// (black), a clear one `Qt::color0` (white). The hot spot is the centre
/// (`createPixmapCursorFromData`, qwindowscursor.cpp:190-209).
fn from_bitmap_data(size: usize, bits: &[u8], mask: &[u8]) -> CursorImage {
    let stride = size.div_ceil(8);
    let mut bgra = vec![0u8; size * size * 4];
    for y in 0..size {
        for x in 0..size {
            let at = y * stride + x / 8;
            let bit = 1u8 << (x % 8);
            if mask[at] & bit == 0 {
                continue;
            }
            let v = if bits[at] & bit != 0 { 0 } else { 255 };
            let i = (y * size + x) * 4;
            bgra[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    CursorImage {
        width: size,
        height: size,
        hot_x: (size / 2) as i32,
        hot_y: (size / 2) as i32,
        bgra,
    }
}

/// Parses a one-character-per-pixel XPM (`QPixmap(const char *const[])`) with hot spot `(0, 0)`:
/// `"w h colors 1"`, the colour lines (`"k c #RRGGBB"` or `"k c None"`), then the rows.
fn from_xpm(rows: &[&str]) -> CursorImage {
    let mut header = rows[0]
        .split_whitespace()
        .map(|t| t.parse::<usize>().unwrap_or(0));
    let (width, height, colors) = (
        header.next().unwrap_or(0),
        header.next().unwrap_or(0),
        header.next().unwrap_or(0),
    );
    let mut palette: Vec<(u8, Option<[u8; 3]>)> = Vec::new();
    for line in &rows[1..=colors] {
        let key = line.as_bytes()[0];
        let value = line.split_whitespace().last().unwrap_or("None");
        let rgb = value.strip_prefix('#').and_then(|hex| {
            let n = u32::from_str_radix(hex, 16).ok()?;
            Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
        });
        palette.push((key, rgb));
    }
    let mut bgra = vec![0u8; width * height * 4];
    for (y, row) in rows[1 + colors..].iter().take(height).enumerate() {
        for (x, key) in row.bytes().take(width).enumerate() {
            if let Some((_, Some([r, g, b]))) = palette.iter().find(|(k, _)| *k == key) {
                let i = (y * width + x) * 4;
                bgra[i..i + 4].copy_from_slice(&[*b, *g, *r, 255]);
            }
        }
    }
    CursorImage {
        width,
        height,
        hot_x: 0,
        hot_y: 0,
        bgra,
    }
}

/// `QPixmap::transformed(QTransform::fromScale(factor, factor), Qt::SmoothTransformation)`:
/// bilinear resampling of the premultiplied pixels, target size rounded. The hot spot moves with
/// the centre rule of [`custom_cursor_image`] (`rawImage.width() / 2`).
fn scale_smooth(image: &CursorImage, factor: f64) -> CursorImage {
    let width = ((image.width as f64 * factor).round() as usize).max(1);
    let height = ((image.height as f64 * factor).round() as usize).max(1);
    let mut bgra = vec![0u8; width * height * 4];
    let at = |x: usize, y: usize, c: usize| image.bgra[(y * image.width + x) * 4 + c] as f64;
    for y in 0..height {
        let fy = ((y as f64 + 0.5) / factor - 0.5).clamp(0.0, (image.height - 1) as f64);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        let y1 = (y0 + 1).min(image.height - 1);
        for x in 0..width {
            let fx = ((x as f64 + 0.5) / factor - 0.5).clamp(0.0, (image.width - 1) as f64);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let x1 = (x0 + 1).min(image.width - 1);
            for c in 0..4 {
                let top = at(x0, y0, c) * (1.0 - tx) + at(x1, y0, c) * tx;
                let bottom = at(x0, y1, c) * (1.0 - tx) + at(x1, y1, c) * tx;
                bgra[(y * width + x) * 4 + c] = (top * (1.0 - ty) + bottom * ty).round() as u8;
            }
        }
    }
    CursorImage {
        width,
        height,
        hot_x: (width / 2) as i32,
        hot_y: (height / 2) as i32,
        bgra,
    }
}

/// The image Qt's Windows plugin draws for `shape`, or `None` for a shape the system provides.
/// `screen_cursor_width` is the system cursor width (`SM_CXCURSOR`, 32 at 100%): the bitmap
/// cursors are made for a 32 pixel cursor and are scaled once it differs by more than 0.4 of that
/// (`createPixmapCursorFromData`); the drag cursors are never scaled.
pub fn custom_cursor_image(shape: CursorShape, screen_cursor_width: i32) -> Option<CursorImage> {
    let scaled = |image: CursorImage| {
        let factor = screen_cursor_width as f64 / 32.0;
        if (factor - 1.0).abs() > 0.4 {
            scale_smooth(&image, factor)
        } else {
            image
        }
    };
    match shape {
        CursorShape::SplitV => Some(scaled(from_bitmap_data(32, &VSPLIT_BITS, &VSPLITM_BITS))),
        CursorShape::SplitH => Some(scaled(from_bitmap_data(32, &HSPLIT_BITS, &HSPLITM_BITS))),
        CursorShape::OpenHand => Some(scaled(from_bitmap_data(
            16,
            &OPENHAND_BITS,
            &OPENHANDM_BITS,
        ))),
        CursorShape::ClosedHand => Some(scaled(from_bitmap_data(
            16,
            &CLOSEDHAND_BITS,
            &CLOSEDHANDM_BITS,
        ))),
        CursorShape::DragCopy => Some(from_xpm(&DRAG_COPY_XPM)),
        CursorShape::DragMove => Some(from_xpm(&DRAG_MOVE_XPM)),
        CursorShape::DragLink => Some(from_xpm(&DRAG_LINK_XPM)),
        _ => None,
    }
}

const VSPLIT_BITS: [u8; 128] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0xc0, 0x01, 0x00,
    0x00, 0xe0, 0x03, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00,
    0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0xff, 0x7f, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0xff, 0x7f, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00,
    0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0xe0, 0x03, 0x00,
    0x00, 0xc0, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const VSPLITM_BITS: [u8; 128] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0xc0, 0x01, 0x00, 0x00, 0xe0, 0x03, 0x00,
    0x00, 0xf0, 0x07, 0x00, 0x00, 0xf8, 0x0f, 0x00, 0x00, 0xc0, 0x01, 0x00, 0x00, 0xc0, 0x01, 0x00,
    0x00, 0xc0, 0x01, 0x00, 0x80, 0xff, 0xff, 0x00, 0x80, 0xff, 0xff, 0x00, 0x80, 0xff, 0xff, 0x00,
    0x80, 0xff, 0xff, 0x00, 0x80, 0xff, 0xff, 0x00, 0x80, 0xff, 0xff, 0x00, 0x00, 0xc0, 0x01, 0x00,
    0x00, 0xc0, 0x01, 0x00, 0x00, 0xc0, 0x01, 0x00, 0x00, 0xf8, 0x0f, 0x00, 0x00, 0xf0, 0x07, 0x00,
    0x00, 0xe0, 0x03, 0x00, 0x00, 0xc0, 0x01, 0x00, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const HSPLIT_BITS: [u8; 128] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x40, 0x02, 0x00,
    0x00, 0x40, 0x02, 0x00, 0x00, 0x40, 0x02, 0x00, 0x00, 0x40, 0x02, 0x00, 0x00, 0x40, 0x02, 0x00,
    0x00, 0x41, 0x82, 0x00, 0x80, 0x41, 0x82, 0x01, 0xc0, 0x7f, 0xfe, 0x03, 0x80, 0x41, 0x82, 0x01,
    0x00, 0x41, 0x82, 0x00, 0x00, 0x40, 0x02, 0x00, 0x00, 0x40, 0x02, 0x00, 0x00, 0x40, 0x02, 0x00,
    0x00, 0x40, 0x02, 0x00, 0x00, 0x40, 0x02, 0x00, 0x00, 0x40, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const HSPLITM_BITS: [u8; 128] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xe0, 0x07, 0x00, 0x00, 0xe0, 0x07, 0x00,
    0x00, 0xe0, 0x07, 0x00, 0x00, 0xe0, 0x07, 0x00, 0x00, 0xe2, 0x47, 0x00, 0x00, 0xe3, 0xc7, 0x00,
    0x80, 0xe3, 0xc7, 0x01, 0xc0, 0xff, 0xff, 0x03, 0xe0, 0xff, 0xff, 0x07, 0xc0, 0xff, 0xff, 0x03,
    0x80, 0xe3, 0xc7, 0x01, 0x00, 0xe3, 0xc7, 0x00, 0x00, 0xe2, 0x47, 0x00, 0x00, 0xe0, 0x07, 0x00,
    0x00, 0xe0, 0x07, 0x00, 0x00, 0xe0, 0x07, 0x00, 0x00, 0xe0, 0x07, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

const OPENHAND_BITS: [u8; 32] = [
    0x80, 0x01, 0x58, 0x0e, 0x64, 0x12, 0x64, 0x52, 0x48, 0xb2, 0x48, 0x92, 0x16, 0x90, 0x19, 0x80,
    0x11, 0x40, 0x02, 0x40, 0x04, 0x40, 0x04, 0x20, 0x08, 0x20, 0x10, 0x10, 0x20, 0x10, 0x00, 0x00,
];

const OPENHANDM_BITS: [u8; 32] = [
    0x80, 0x01, 0xd8, 0x0f, 0xfc, 0x1f, 0xfc, 0x5f, 0xf8, 0xff, 0xf8, 0xff, 0xf6, 0xff, 0xff, 0xff,
    0xff, 0x7f, 0xfe, 0x7f, 0xfc, 0x7f, 0xfc, 0x3f, 0xf8, 0x3f, 0xf0, 0x1f, 0xe0, 0x1f, 0x00, 0x00,
];

const CLOSEDHAND_BITS: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xb0, 0x0d, 0x48, 0x32, 0x08, 0x50, 0x10, 0x40, 0x18, 0x40,
    0x04, 0x40, 0x04, 0x20, 0x08, 0x20, 0x10, 0x10, 0x20, 0x10, 0x20, 0x10, 0x00, 0x00, 0x00, 0x00,
];

const CLOSEDHANDM_BITS: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xb0, 0x0d, 0xf8, 0x3f, 0xf8, 0x7f, 0xf0, 0x7f, 0xf8, 0x7f,
    0xfc, 0x7f, 0xfc, 0x3f, 0xf8, 0x3f, 0xf0, 0x1f, 0xe0, 0x1f, 0xe0, 0x1f, 0x00, 0x00, 0x00, 0x00,
];

const DRAG_MOVE_XPM: [&str; 24] = [
    "11 20 3 1",
    ".        c None",
    "a        c #FFFFFF",
    "X        c #000000",
    "aa.........",
    "aXa........",
    "aXXa.......",
    "aXXXa......",
    "aXXXXa.....",
    "aXXXXXa....",
    "aXXXXXXa...",
    "aXXXXXXXa..",
    "aXXXXXXXXa.",
    "aXXXXXXXXXa",
    "aXXXXXXaaaa",
    "aXXXaXXa...",
    "aXXaaXXa...",
    "aXa..aXXa..",
    "aa...aXXa..",
    "a.....aXXa.",
    "......aXXa.",
    ".......aXXa",
    ".......aXXa",
    "........aa.",
];

const DRAG_COPY_XPM: [&str; 34] = [
    "24 30 3 1",
    ".        c None",
    "a        c #000000",
    "X        c #FFFFFF",
    "XX......................",
    "XaX.....................",
    "XaaX....................",
    "XaaaX...................",
    "XaaaaX..................",
    "XaaaaaX.................",
    "XaaaaaaX................",
    "XaaaaaaaX...............",
    "XaaaaaaaaX..............",
    "XaaaaaaaaaX.............",
    "XaaaaaaXXXX.............",
    "XaaaXaaX................",
    "XaaXXaaX................",
    "XaX..XaaX...............",
    "XX...XaaX...............",
    "X.....XaaX..............",
    "......XaaX..............",
    ".......XaaX.............",
    ".......XaaX.............",
    "........XX...aaaaaaaaaaa",
    ".............aXXXXXXXXXa",
    ".............aXXXXXXXXXa",
    ".............aXXXXaXXXXa",
    ".............aXXXXaXXXXa",
    ".............aXXaaaaaXXa",
    ".............aXXXXaXXXXa",
    ".............aXXXXaXXXXa",
    ".............aXXXXXXXXXa",
    ".............aXXXXXXXXXa",
    ".............aaaaaaaaaaa",
];

const DRAG_LINK_XPM: [&str; 34] = [
    "24 30 3 1",
    ".        c None",
    "a        c #000000",
    "X        c #FFFFFF",
    "XX......................",
    "XaX.....................",
    "XaaX....................",
    "XaaaX...................",
    "XaaaaX..................",
    "XaaaaaX.................",
    "XaaaaaaX................",
    "XaaaaaaaX...............",
    "XaaaaaaaaX..............",
    "XaaaaaaaaaX.............",
    "XaaaaaaXXXX.............",
    "XaaaXaaX................",
    "XaaXXaaX................",
    "XaX..XaaX...............",
    "XX...XaaX...............",
    "X.....XaaX..............",
    "......XaaX..............",
    ".......XaaX.............",
    ".......XaaX.............",
    "........XX...aaaaaaaaaaa",
    ".............aXXXXXXXXXa",
    ".............aXXXaaaaXXa",
    ".............aXXXXaaaXXa",
    ".............aXXXaaaaXXa",
    ".............aXXaaaXaXXa",
    ".............aXXaaXXXXXa",
    ".............aXXaXXXXXXa",
    ".............aXXXaXXXXXa",
    ".............aXXXXXXXXXa",
    ".............aaaaaaaaaaa",
];
