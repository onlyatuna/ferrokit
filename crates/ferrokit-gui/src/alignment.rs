// SPDX-License-Identifier: LGPL-3.0-only
//! `Qt::Alignment`: the one alignment type of layout items, labels, progress bars and text blocks.

use crate::application::LayoutDirection;

/// `Qt::Alignment` with Qt's bit values (`qnamespace.h:151-170`).
///
/// Qt has a single flag type for every place that aligns something: a layout item or a layout
/// (`QLayoutItem::setAlignment`, `QLayout::setAlignment`), a `QLabel`'s text, a `QProgressBar`'s
/// text and a text block (`QTextBlockFormat::setAlignment`). A horizontal and a vertical flag
/// combine; an axis with no flag is unaligned and each user gives that its own meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Alignment(u32);

impl Alignment {
    pub const NONE: Self = Self(0);
    pub const LEFT: Self = Self(0x1);
    pub const RIGHT: Self = Self(0x2);
    pub const H_CENTER: Self = Self(0x4);
    pub const JUSTIFY: Self = Self(0x8);
    pub const ABSOLUTE: Self = Self(0x10);
    pub const TOP: Self = Self(0x20);
    pub const BOTTOM: Self = Self(0x40);
    pub const V_CENTER: Self = Self(0x80);
    pub const BASELINE: Self = Self(0x100);
    pub const CENTER: Self = Self(0x4 | 0x80);
    /// `Qt::AlignHorizontal_Mask`.
    const HORIZONTAL_MASK: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10;
    /// `Qt::AlignVertical_Mask`.
    const VERTICAL_MASK: u32 = 0x20 | 0x40 | 0x80 | 0x100;

    /// From Qt's numeric flag value; bits that are not alignment flags are dropped.
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits & (Self::HORIZONTAL_MASK | Self::VERTICAL_MASK))
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Every flag of `other` is set.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Some horizontal flag is set (`align & Qt::AlignHorizontal_Mask`).
    pub const fn horizontal(self) -> bool {
        self.0 & Self::HORIZONTAL_MASK != 0
    }

    /// Some vertical flag is set (`align & Qt::AlignVertical_Mask`).
    pub const fn vertical(self) -> bool {
        self.0 & Self::VERTICAL_MASK != 0
    }

    /// `QStyle::visualAlignment` / `QGuiApplicationPrivate::visualAlignment`
    /// (qguiapplication_p.h:176-186): without a horizontal flag `AlignLeft` is assumed; a Left or
    /// Right flag without `AlignAbsolute` is mirrored under right-to-left and then made absolute.
    pub const fn visual(self, direction: LayoutDirection) -> Self {
        let mut bits = self.0;
        if bits & Self::HORIZONTAL_MASK == 0 {
            bits |= Self::LEFT.0;
        }
        let left_right = Self::LEFT.0 | Self::RIGHT.0;
        if bits & Self::ABSOLUTE.0 == 0 && bits & left_right != 0 {
            if matches!(direction, LayoutDirection::RightToLeft) {
                bits ^= left_right;
            }
            bits |= Self::ABSOLUTE.0;
        }
        Self(bits)
    }
}

impl std::ops::BitOr for Alignment {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
