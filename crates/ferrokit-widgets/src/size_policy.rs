// SPDX-License-Identifier: LGPL-3.0-only
/// Size policy behavior for a single dimension (`QSizePolicy::Policy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Policy {
    /// The sizeHint is the only acceptable size. Cannot grow or shrink.
    Fixed,
    /// The sizeHint is minimal. Can grow freely, cannot shrink smaller than sizeHint.
    Minimum,
    /// The sizeHint is maximal. Can shrink freely, cannot grow larger than sizeHint.
    Maximum,
    /// The sizeHint is preferred. Can grow or shrink.
    #[default]
    Preferred,
    /// The sizeHint is a reasonable size, but the widget benefits from more room and should be given as much as is available.
    Expanding,
    /// The sizeHint is minimal, but the widget makes good use of extra space and expands as much as possible.
    MinimumExpanding,
    /// The sizeHint is disregarded; the widget takes as much space as it can get.
    Ignored,
}

impl Policy {
    /// Returns true if the policy permits shrinking below the size hint.
    pub fn can_shrink(&self) -> bool {
        matches!(
            self,
            Self::Maximum | Self::Preferred | Self::Expanding | Self::Ignored
        )
    }

    /// Returns true if the policy permits growing beyond the size hint.
    pub fn can_grow(&self) -> bool {
        matches!(
            self,
            Self::Minimum
                | Self::Preferred
                | Self::Expanding
                | Self::MinimumExpanding
                | Self::Ignored
        )
    }

    /// Returns true if the widget wants to expand aggressively into extra space.
    pub fn is_expanding(&self) -> bool {
        matches!(
            self,
            Self::Expanding | Self::MinimumExpanding | Self::Ignored
        )
    }
}

/// 2D size policy for horizontal and vertical dimensions (`QSizePolicy`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QSizePolicy {
    pub horizontal: Policy,
    pub vertical: Policy,
    pub horizontal_stretch: u32,
    pub vertical_stretch: u32,
    /// `QSizePolicy::heightForWidth` (`hfw` bit): the widget's height depends on its width.
    pub height_for_width: bool,
    /// `QSizePolicy::retainSizeWhenHidden`: a hidden widget keeps its layout slot.
    pub retain_size_when_hidden: bool,
}

impl Default for QSizePolicy {
    fn default() -> Self {
        Self {
            horizontal: Policy::Preferred,
            vertical: Policy::Preferred,
            horizontal_stretch: 0,
            vertical_stretch: 0,
            height_for_width: false,
            retain_size_when_hidden: false,
        }
    }
}

impl QSizePolicy {
    /// Creates a size policy with specified horizontal and vertical policies.
    pub fn new(horizontal: Policy, vertical: Policy) -> Self {
        Self {
            horizontal,
            vertical,
            horizontal_stretch: 0,
            vertical_stretch: 0,
            height_for_width: false,
            retain_size_when_hidden: false,
        }
    }

    /// Creates a fixed size policy for both dimensions.
    pub fn fixed() -> Self {
        Self::new(Policy::Fixed, Policy::Fixed)
    }

    /// Creates a preferred size policy for both dimensions.
    pub fn preferred() -> Self {
        Self::new(Policy::Preferred, Policy::Preferred)
    }

    /// Creates an expanding size policy for both dimensions.
    pub fn expanding() -> Self {
        Self::new(Policy::Expanding, Policy::Expanding)
    }

    /// Configures stretch factors for horizontal and vertical directions.
    pub fn with_stretch(mut self, h_stretch: u32, v_stretch: u32) -> Self {
        self.horizontal_stretch = h_stretch;
        self.vertical_stretch = v_stretch;
        self
    }

    /// Sets `retainSizeWhenHidden` (`QSizePolicy::setRetainSizeWhenHidden`): a hidden widget keeps
    /// its layout slot (`QWidgetItem::isEmpty`, qlayoutitem.cpp:691-693).
    pub fn with_retain_size_when_hidden(mut self, retain: bool) -> Self {
        self.retain_size_when_hidden = retain;
        self
    }

    /// Transposes the horizontal and vertical policies and stretch factors.
    pub fn transposed(&self) -> Self {
        Self {
            horizontal: self.vertical,
            vertical: self.horizontal,
            horizontal_stretch: self.vertical_stretch,
            vertical_stretch: self.horizontal_stretch,
            height_for_width: self.height_for_width,
            retain_size_when_hidden: self.retain_size_when_hidden,
        }
    }
}

pub(crate) fn transposed_policy(policy: QSizePolicy) -> QSizePolicy {
    policy.transposed()
}
