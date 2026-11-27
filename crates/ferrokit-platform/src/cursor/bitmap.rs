// SPDX-License-Identifier: LGPL-3.0-only
//! Software cursor bitmaps for shapes not provided by the host platform cursor API.
//!
//! Pure pixel data and bilinear scaling, shared across platforms and testable without a window
//! system.

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

/// Renders a monochrome silhouette (`'#'` = black interior, `'.'` = background) with an automatic
/// 1-pixel 8-connected white outline (`[255, 255, 255, 255]`) around every black pixel
/// (`[0, 0, 0, 255]`).
fn from_silhouette(
    width: usize,
    height: usize,
    hot_x: i32,
    hot_y: i32,
    rows: &[&str],
) -> CursorImage {
    debug_assert_eq!(rows.len(), height);
    let is_black = |x: isize, y: isize| -> bool {
        if x < 0 || y < 0 || x >= width as isize || y >= height as isize {
            return false;
        }
        rows[y as usize].as_bytes().get(x as usize) == Some(&b'#')
    };

    let mut bgra = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            if is_black(x as isize, y as isize) {
                bgra[i..i + 4].copy_from_slice(&[0, 0, 0, 255]);
            } else {
                let mut touches_black = false;
                'neighbors: for dy in -1isize..=1 {
                    for dx in -1isize..=1 {
                        if is_black(x as isize + dx, y as isize + dy) {
                            touches_black = true;
                            break 'neighbors;
                        }
                    }
                }
                if touches_black {
                    bgra[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
    }

    CursorImage {
        width,
        height,
        hot_x,
        hot_y,
        bgra,
    }
}

/// Bilinearly resamples premultiplied BGRA pixels by `factor`, rounding target dimensions.
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

fn flip_horizontal(image: &CursorImage) -> CursorImage {
    let mut bgra = vec![0u8; image.bgra.len()];
    for y in 0..image.height {
        for x in 0..image.width {
            let src = (y * image.width + x) * 4;
            let dst = (y * image.width + (image.width - 1 - x)) * 4;
            bgra[dst..dst + 4].copy_from_slice(&image.bgra[src..src + 4]);
        }
    }
    let hot_x = if image.width == 0 {
        0
    } else {
        (image.width as i32 - 1 - image.hot_x).clamp(0, image.width as i32 - 1)
    };
    CursorImage {
        width: image.width,
        height: image.height,
        hot_x,
        hot_y: image.hot_y,
        bgra,
    }
}

/// Converts premultiplied BGRA8 pixel bytes (from [`CursorImage::bgra`]) into premultiplied
/// RGBA8 bytes in top-to-bottom row order, matching AppKit's `NSBitmapImageRep`.
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity(bgra.len());
    for &[b, g, r, a] in bgra.as_chunks::<4>().0 {
        rgba.extend_from_slice(&[r, g, b, a]);
    }
    rgba
}

/// Custom cursor image for shapes the Windows system cursor set does not provide, or `None` for
/// system-provided shapes. `screen_cursor_width` is the system cursor width (`SM_CXCURSOR`, 32 at
/// 100%); bitmap cursors are authored at 32px base scale and resampled when the system cursor
/// width differs by more than 40%, while drag cursors stay unscaled.
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
        CursorShape::SplitV => Some(scaled(from_silhouette(32, 32, 16, 16, &SPLIT_V_ROWS))),
        CursorShape::SplitH => Some(scaled(from_silhouette(32, 32, 16, 16, &SPLIT_H_ROWS))),
        CursorShape::OpenHand => Some(scaled(from_silhouette(16, 16, 8, 8, &OPEN_HAND_ROWS))),
        CursorShape::ClosedHand => Some(scaled(from_silhouette(16, 16, 8, 8, &CLOSED_HAND_ROWS))),
        CursorShape::DragMove => Some(from_silhouette(11, 20, 0, 0, &DRAG_MOVE_ROWS)),
        CursorShape::DragCopy => Some(from_silhouette(24, 30, 0, 0, &DRAG_COPY_ROWS)),
        CursorShape::DragLink => Some(from_silhouette(24, 30, 0, 0, &DRAG_LINK_ROWS)),
        _ => None,
    }
}

/// Custom cursor image for shapes AppKit has no public cross-version `NSCursor` class method for,
/// or `None` when a public `NSCursor` class selector provides the shape natively.
pub fn cocoa_custom_cursor_image(shape: CursorShape) -> Option<CursorImage> {
    match shape {
        CursorShape::SizeFDiag => Some(from_silhouette(17, 17, 8, 8, &COCOA_SIZE_FDIAG_ROWS)),
        CursorShape::SizeBDiag => Some(flip_horizontal(&from_silhouette(
            17,
            17,
            8,
            8,
            &COCOA_SIZE_FDIAG_ROWS,
        ))),
        CursorShape::SizeAll => Some(from_silhouette(17, 17, 8, 8, &COCOA_SIZE_ALL_ROWS)),
        CursorShape::UpArrow => Some(from_silhouette(13, 17, 6, 1, &COCOA_UP_ARROW_ROWS)),
        CursorShape::Wait => Some(from_silhouette(15, 19, 7, 9, &COCOA_WAIT_ROWS)),
        CursorShape::Busy => Some(from_silhouette(24, 24, 1, 1, &COCOA_BUSY_ROWS)),
        CursorShape::WhatsThis => Some(from_silhouette(24, 24, 1, 1, &COCOA_WHATS_THIS_ROWS)),
        CursorShape::Blank => Some(CursorImage {
            width: 1,
            height: 1,
            hot_x: 0,
            hot_y: 0,
            bgra: vec![0, 0, 0, 0],
        }),
        _ => None,
    }
}

#[rustfmt::skip]
const SPLIT_V_ROWS: [&str; 32] = [
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "...............##...............",
    "..............####..............",
    ".............######.............",
    "............########............",
    "...............##...............",
    "...............##...............",
    "...............##...............",
    "...............##...............",
    "........################........",
    "................................",
    "................................",
    "........################........",
    "...............##...............",
    "...............##...............",
    "...............##...............",
    "...............##...............",
    "............########............",
    ".............######.............",
    "..............####..............",
    "...............##...............",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
];

#[rustfmt::skip]
const SPLIT_H_ROWS: [&str; 32] = [
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "..............#..#..............",
    "..............#..#..............",
    "..............#..#..............",
    "..............#..#..............",
    ".........#....#..#....#.........",
    "........##....#..#....##........",
    ".......###....#..#....###.......",
    "......######..#..#..######......",
    "......######..#..#..######......",
    ".......###....#..#....###.......",
    "........##....#..#....##........",
    ".........#....#..#....#.........",
    "..............#..#..............",
    "..............#..#..............",
    "..............#..#..............",
    "..............#..#..............",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
    "................................",
];

#[rustfmt::skip]
const OPEN_HAND_ROWS: [&str; 16] = [
    "................",
    ".......##.......",
    "....##.##.##....",
    "....##.##.##....",
    "....##.##.##.##.",
    "....##.##.##.##.",
    ".##.###########.",
    ".##.###########.",
    "..#############.",
    "...############.",
    "...############.",
    "....##########..",
    "....##########..",
    ".....########...",
    ".....########...",
    "................",
];

#[rustfmt::skip]
const CLOSED_HAND_ROWS: [&str; 16] = [
    "................",
    "................",
    "................",
    "................",
    ".....##.##.##...",
    "....##########..",
    "...###########..",
    "...###########..",
    "...###########..",
    "....##########..",
    "....##########..",
    ".....########...",
    ".....########...",
    ".....########...",
    "................",
    "................",
];

#[rustfmt::skip]
const DRAG_MOVE_ROWS: [&str; 20] = [
    "...........",
    ".#.........",
    ".##........",
    ".###.......",
    ".####......",
    ".#####.....",
    ".######....",
    ".#######...",
    ".########..",
    ".#########.",
    ".######....",
    ".###.##....",
    ".##..###...",
    ".#...###...",
    "......###..",
    "......###..",
    ".......##..",
    "...........",
    "...........",
    "...........",
];

#[rustfmt::skip]
const DRAG_COPY_ROWS: [&str; 30] = [
    "........................",
    ".#......................",
    ".##.....................",
    ".###....................",
    ".####...................",
    ".#####..................",
    ".######.................",
    ".#######................",
    ".########...............",
    ".#########..............",
    ".######.................",
    ".###.##.................",
    ".##..###................",
    ".#...###................",
    "......###...............",
    "......###...............",
    ".......##...............",
    "........................",
    "........................",
    "...............##.......",
    "...............##.......",
    "...............##.......",
    "............########....",
    "............########....",
    "...............##.......",
    "...............##.......",
    "...............##.......",
    "........................",
    "........................",
    "........................",
];

#[rustfmt::skip]
const DRAG_LINK_ROWS: [&str; 30] = [
    "........................",
    ".#......................",
    ".##.....................",
    ".###....................",
    ".####...................",
    ".#####..................",
    ".######.................",
    ".#######................",
    ".########...............",
    ".#########..............",
    ".######.................",
    ".###.##.................",
    ".##..###................",
    ".#...###................",
    "......###...............",
    "......###...............",
    ".......##...............",
    "........................",
    "........................",
    "..............######....",
    "...............#####....",
    "................####....",
    "...............##.##....",
    "..............##...#....",
    ".............##.........",
    "............##..........",
    "...........##...........",
    "........................",
    "........................",
    "........................",
];

#[rustfmt::skip]
const COCOA_SIZE_FDIAG_ROWS: [&str; 17] = [
    ".................",
    ".######..........",
    ".#####...........",
    ".####............",
    ".#####...........",
    ".##.###..........",
    ".#...###.........",
    "......###........",
    ".......###.......",
    "........###......",
    ".........###...#.",
    "..........###.##.",
    "...........#####.",
    "............####.",
    "...........#####.",
    "..........######.",
    ".................",
];

#[rustfmt::skip]
const COCOA_SIZE_ALL_ROWS: [&str; 17] = [
    ".................",
    "........#........",
    ".......###.......",
    "......#####......",
    "........#........",
    "........#........",
    "...#....#....#...",
    "..##....#....##..",
    ".###############.",
    "..##....#....##..",
    "...#....#....#...",
    "........#........",
    "........#........",
    "......#####......",
    ".......###.......",
    "........#........",
    ".................",
];

#[rustfmt::skip]
const COCOA_UP_ARROW_ROWS: [&str; 17] = [
    ".............",
    "......#......",
    ".....###.....",
    "....#####....",
    "...#######...",
    "..#########..",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".....###.....",
    ".............",
];

#[rustfmt::skip]
const COCOA_WAIT_ROWS: [&str; 19] = [
    "...............",
    "..###########..",
    "..###########..",
    "...#########...",
    "....#######....",
    ".....#####.....",
    "......###......",
    ".......#.......",
    "......###......",
    ".....#####.....",
    "....#######....",
    "...#########...",
    "..###########..",
    "..###########..",
    "..###########..",
    "..###########..",
    "..###########..",
    "..###########..",
    "...............",
];

#[rustfmt::skip]
const COCOA_BUSY_ROWS: [&str; 24] = [
    "........................",
    ".#......................",
    ".##.....................",
    ".###....................",
    ".####...................",
    ".#####..................",
    ".######.................",
    ".#######................",
    ".########...............",
    ".#########..............",
    ".######.................",
    ".###.##.......#######...",
    ".##..###......#######...",
    ".#...###.......#####....",
    "......###.......###.....",
    "......###........#......",
    ".......##.......###.....",
    "...............#####....",
    "..............#######...",
    "..............#######...",
    "..............#######...",
    "..............#######...",
    "........................",
    "........................",
];

#[rustfmt::skip]
const COCOA_WHATS_THIS_ROWS: [&str; 24] = [
    "........................",
    ".#......................",
    ".##.....................",
    ".###....................",
    ".####...................",
    ".#####..................",
    ".######.................",
    ".#######................",
    ".########...............",
    ".#########..............",
    ".######.................",
    ".###.##.......#####.....",
    ".##..###.....#######....",
    ".#...###.....##...##....",
    "......###........###....",
    "......###.......###.....",
    ".......##.......##......",
    "................##......",
    "........................",
    "................##......",
    "................##......",
    "........................",
    "........................",
    "........................",
];
