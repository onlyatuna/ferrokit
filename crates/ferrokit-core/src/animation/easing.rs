// SPDX-License-Identifier: LGPL-3.0-only
//! QEasingCurve easing functions (`QEasingCurve` equivalent).
//!
//! Provides 40+ easing curve algorithms matching Qt's `QEasingCurve::Type`
//! for smooth timeline interpolation, spring dynamics, bounce effects, and custom curves.
//!
//! The equations are Robert Penner's Easing Equations (BSD-3-Clause), see NOTICE.

use std::f32::consts::{FRAC_PI_2, PI};

/// Easing curve algorithm types matching Qt `QEasingCurve::Type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EasingType {
    #[default]
    Linear,
    InQuad,
    OutQuad,
    InOutQuad,
    OutInQuad,
    InCubic,
    OutCubic,
    InOutCubic,
    OutInCubic,
    InQuart,
    OutQuart,
    InOutQuart,
    OutInQuart,
    InQuint,
    OutQuint,
    InOutQuint,
    OutInQuint,
    InSine,
    OutSine,
    InOutSine,
    OutInSine,
    InExpo,
    OutExpo,
    InOutExpo,
    OutInExpo,
    InCirc,
    OutCirc,
    InOutCirc,
    OutInCirc,
    InElastic,
    OutElastic,
    InOutElastic,
    OutInElastic,
    InBack,
    OutBack,
    InOutBack,
    OutInBack,
    InBounce,
    OutBounce,
    InOutBounce,
    OutInBounce,
}

/// Easing curve container matching Qt `QEasingCurve`.
#[derive(Clone)]
pub struct EasingCurve {
    curve_type: EasingType,
    amplitude: f32,
    period: f32,
    overshoot: f32,
    custom_func: Option<fn(f32) -> f32>,
}

impl Default for EasingCurve {
    fn default() -> Self {
        Self::new(EasingType::Linear)
    }
}

impl std::fmt::Debug for EasingCurve {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EasingCurve")
            .field("type", &self.curve_type)
            .field("amplitude", &self.amplitude)
            .field("period", &self.period)
            .field("overshoot", &self.overshoot)
            .finish()
    }
}

impl EasingCurve {
    /// Creates an easing curve of the specified type with default parameters.
    pub fn new(curve_type: EasingType) -> Self {
        Self {
            curve_type,
            amplitude: 1.0,
            period: 0.3,
            overshoot: 1.70158,
            custom_func: None,
        }
    }

    /// Creates an easing curve with a custom calculation function.
    pub fn custom(func: fn(f32) -> f32) -> Self {
        Self {
            curve_type: EasingType::Linear,
            amplitude: 1.0,
            period: 0.3,
            overshoot: 1.70158,
            custom_func: Some(func),
        }
    }

    /// Returns the easing curve type.
    pub fn curve_type(&self) -> EasingType {
        self.curve_type
    }

    /// Sets the easing curve type.
    pub fn set_type(&mut self, curve_type: EasingType) {
        self.curve_type = curve_type;
        self.custom_func = None;
    }

    /// Sets overshoot parameter for Back curves. Default is 1.70158.
    pub fn set_overshoot(&mut self, overshoot: f32) {
        self.overshoot = overshoot;
    }

    /// Returns the overshoot parameter.
    pub fn overshoot(&self) -> f32 {
        self.overshoot
    }

    /// Sets amplitude parameter for Elastic curves.
    pub fn set_amplitude(&mut self, amplitude: f32) {
        self.amplitude = amplitude;
    }

    /// Returns the amplitude parameter.
    pub fn amplitude(&self) -> f32 {
        self.amplitude
    }

    /// Sets period parameter for Elastic curves.
    pub fn set_period(&mut self, period: f32) {
        self.period = period;
    }

    /// Returns the period parameter.
    pub fn period(&self) -> f32 {
        self.period
    }

    /// Computes the eased value for progress `t` in `[0.0, 1.0]`.
    ///
    /// Modeled after Qt `QEasingCurve::valueForProgress(qreal progress)`.
    pub fn value_for_progress(&self, progress: f32) -> f32 {
        if let Some(func) = self.custom_func {
            return func(progress);
        }

        let t = progress.clamp(0.0, 1.0);
        let s = self.overshoot;

        match self.curve_type {
            EasingType::Linear => t,
            EasingType::InQuad => t * t,
            EasingType::OutQuad => t * (2.0 - t),
            EasingType::InOutQuad => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    -1.0 + (4.0 - 2.0 * t) * t
                }
            }
            EasingType::OutInQuad => out_in(t, |p| p * p, |p| p * (2.0 - p)),
            EasingType::InCubic => t * t * t,
            EasingType::OutCubic => {
                let f = t - 1.0;
                f * f * f + 1.0
            }
            EasingType::InOutCubic => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    let f = 2.0 * t - 2.0;
                    0.5 * f * f * f + 1.0
                }
            }
            EasingType::OutInCubic => out_in(
                t,
                |p| p * p * p,
                |p| {
                    let f = p - 1.0;
                    f * f * f + 1.0
                },
            ),
            EasingType::InQuart => t * t * t * t,
            EasingType::OutQuart => 1.0 - (t - 1.0).powi(4),
            EasingType::InOutQuart => {
                if t < 0.5 {
                    8.0 * t.powi(4)
                } else {
                    1.0 - 8.0 * (t - 1.0).powi(4)
                }
            }
            EasingType::OutInQuart => out_in(t, |p| p.powi(4), |p| 1.0 - (p - 1.0).powi(4)),
            EasingType::InQuint => t.powi(5),
            EasingType::OutQuint => (t - 1.0).powi(5) + 1.0,
            EasingType::InOutQuint => {
                if t < 0.5 {
                    16.0 * t.powi(5)
                } else {
                    0.5 * (2.0 * t - 2.0).powi(5) + 1.0
                }
            }
            EasingType::OutInQuint => out_in(t, |p| p.powi(5), |p| (p - 1.0).powi(5) + 1.0),
            EasingType::InSine => 1.0 - (t * FRAC_PI_2).cos(),
            EasingType::OutSine => (t * FRAC_PI_2).sin(),
            EasingType::InOutSine => 0.5 * (1.0 - (PI * t).cos()),
            EasingType::OutInSine => out_in(
                t,
                |p| 1.0 - (p * FRAC_PI_2).cos(),
                |p| (p * FRAC_PI_2).sin(),
            ),
            EasingType::InExpo => {
                if t == 0.0 {
                    0.0
                } else {
                    2.0f32.powf(10.0 * (t - 1.0))
                }
            }
            EasingType::OutExpo => {
                if t >= 1.0 {
                    1.0
                } else {
                    1.0 - 2.0f32.powf(-10.0 * t)
                }
            }
            EasingType::InOutExpo => {
                if t == 0.0 {
                    0.0
                } else if t >= 1.0 {
                    1.0
                } else if t < 0.5 {
                    0.5 * 2.0f32.powf(20.0 * t - 10.0)
                } else {
                    1.0 - 0.5 * 2.0f32.powf(-20.0 * t + 10.0)
                }
            }
            EasingType::OutInExpo => out_in(
                t,
                |p| {
                    if p == 0.0 {
                        0.0
                    } else {
                        2.0f32.powf(10.0 * (p - 1.0))
                    }
                },
                |p| {
                    if p >= 1.0 {
                        1.0
                    } else {
                        1.0 - 2.0f32.powf(-10.0 * p)
                    }
                },
            ),
            EasingType::InCirc => 1.0 - (1.0 - t * t).max(0.0).sqrt(),
            EasingType::OutCirc => (1.0 - (t - 1.0).powi(2)).max(0.0).sqrt(),
            EasingType::InOutCirc => {
                if t < 0.5 {
                    0.5 * (1.0 - (1.0 - 4.0 * t * t).max(0.0).sqrt())
                } else {
                    0.5 * ((1.0 - (2.0 * t - 2.0).powi(2)).max(0.0).sqrt() + 1.0)
                }
            }
            EasingType::OutInCirc => out_in(
                t,
                |p| 1.0 - (1.0 - p * p).max(0.0).sqrt(),
                |p| (1.0 - (p - 1.0).powi(2)).max(0.0).sqrt(),
            ),
            EasingType::InElastic => ease_in_elastic(t, self.amplitude, self.period),
            EasingType::OutElastic => ease_out_elastic(t, self.amplitude, self.period),
            EasingType::InOutElastic => {
                if t < 0.5 {
                    0.5 * ease_in_elastic(2.0 * t, self.amplitude, self.period)
                } else {
                    0.5 + 0.5 * ease_out_elastic(2.0 * t - 1.0, self.amplitude, self.period)
                }
            }
            EasingType::OutInElastic => out_in(
                t,
                |p| ease_in_elastic(p, self.amplitude, self.period),
                |p| ease_out_elastic(p, self.amplitude, self.period),
            ),
            EasingType::InBack => t * t * ((s + 1.0) * t - s),
            EasingType::OutBack => {
                let f = t - 1.0;
                f * f * ((s + 1.0) * f + s) + 1.0
            }
            EasingType::InOutBack => {
                let s2 = s * 1.525;
                if t < 0.5 {
                    0.5 * (4.0 * t * t * ((s2 + 1.0) * 2.0 * t - s2))
                } else {
                    let f = 2.0 * t - 2.0;
                    0.5 * (f * f * ((s2 + 1.0) * f + s2) + 2.0)
                }
            }
            EasingType::OutInBack => out_in(
                t,
                |p| p * p * ((s + 1.0) * p - s),
                |p| {
                    let f = p - 1.0;
                    f * f * ((s + 1.0) * f + s) + 1.0
                },
            ),
            EasingType::InBounce => 1.0 - ease_out_bounce(1.0 - t),
            EasingType::OutBounce => ease_out_bounce(t),
            EasingType::InOutBounce => {
                if t < 0.5 {
                    0.5 * (1.0 - ease_out_bounce(1.0 - 2.0 * t))
                } else {
                    0.5 * (1.0 + ease_out_bounce(2.0 * t - 1.0))
                }
            }
            EasingType::OutInBounce => {
                out_in(t, |p| 1.0 - ease_out_bounce(1.0 - p), ease_out_bounce)
            }
        }
    }
}

#[inline]
fn out_in<InF, OutF>(t: f32, in_f: InF, out_f: OutF) -> f32
where
    InF: Fn(f32) -> f32,
    OutF: Fn(f32) -> f32,
{
    if t < 0.5 {
        0.5 * out_f(2.0 * t)
    } else {
        0.5 + 0.5 * in_f(2.0 * t - 1.0)
    }
}

#[inline]
fn ease_out_bounce(t: f32) -> f32 {
    let n1 = 7.5625;
    let d1 = 2.75;

    if t < 1.0 / d1 {
        n1 * t * t
    } else if t < 2.0 / d1 {
        let t = t - 1.5 / d1;
        n1 * t * t + 0.75
    } else if t < 2.5 / d1 {
        let t = t - 2.25 / d1;
        n1 * t * t + 0.9375
    } else {
        let t = t - 2.625 / d1;
        n1 * t * t + 0.984375
    }
}

#[inline]
fn ease_in_elastic(t: f32, amplitude: f32, period: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let p = if period <= 0.0 { 0.3 } else { period };
    let a = amplitude.max(1.0);
    let s = p / (2.0 * PI) * (1.0 / a).asin();
    let t = t - 1.0;
    -(a * 2.0f32.powf(10.0 * t) * ((t - s) * (2.0 * PI) / p).sin())
}

#[inline]
fn ease_out_elastic(t: f32, amplitude: f32, period: f32) -> f32 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    let p = if period <= 0.0 { 0.3 } else { period };
    let a = amplitude.max(1.0);
    let s = p / (2.0 * PI) * (1.0 / a).asin();
    a * 2.0f32.powf(-10.0 * t) * ((t - s) * (2.0 * PI) / p).sin() + 1.0
}
