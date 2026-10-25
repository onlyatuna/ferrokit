// SPDX-License-Identifier: LGPL-3.0-only
//! Animation subsystem modeled after Qt Animation Framework.
//!
//! Provides `QEasingCurve`, `QVariantAnimation`, and `QPropertyAnimation`
//! for stateful property tweens, timeline keyframing, and UI transition effects.

pub mod easing;
pub use easing::*;

use crate::object::{ObjectId, QObject};
use crate::signal::Signal;
use crate::variant::Variant;

/// Animation state matching Qt `QAbstractAnimation::State`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationState {
    #[default]
    Stopped,
    Paused,
    Running,
}

/// Animation direction matching Qt `QAbstractAnimation::Direction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnimationDirection {
    #[default]
    Forward,
    Backward,
}

/// Dynamic value tween animation matching Qt `QVariantAnimation`.
pub struct VariantAnimation {
    duration_ms: u64,
    current_time_ms: u64,
    start_value: Option<Variant>,
    end_value: Option<Variant>,
    key_values: Vec<(f32, Variant)>,
    easing_curve: EasingCurve,
    loop_count: i32,
    current_loop: i32,
    state: AnimationState,
    direction: AnimationDirection,

    pub value_changed: Signal<Variant>,
    pub state_changed: Signal<(AnimationState, AnimationState)>,
    pub finished: Signal<()>,
}

impl Default for VariantAnimation {
    fn default() -> Self {
        Self::new()
    }
}

impl VariantAnimation {
    /// Creates a new variant animation with default parameters (duration: 250ms).
    pub fn new() -> Self {
        Self {
            duration_ms: 250,
            current_time_ms: 0,
            start_value: None,
            end_value: None,
            key_values: Vec::new(),
            easing_curve: EasingCurve::default(),
            loop_count: 1,
            current_loop: 0,
            state: AnimationState::Stopped,
            direction: AnimationDirection::Forward,
            value_changed: Signal::new(),
            state_changed: Signal::new(),
            finished: Signal::new(),
        }
    }

    /// Sets the total animation duration in milliseconds.
    pub fn set_duration(&mut self, ms: u64) {
        self.duration_ms = ms;
    }

    /// Returns the total animation duration in milliseconds.
    pub fn duration(&self) -> u64 {
        self.duration_ms
    }

    /// Sets the starting value.
    pub fn set_start_value(&mut self, val: impl Into<Variant>) {
        self.start_value = Some(val.into());
    }

    /// Sets the ending value.
    pub fn set_end_value(&mut self, val: impl Into<Variant>) {
        self.end_value = Some(val.into());
    }

    /// Sets a keyframe value at relative step `[0.0, 1.0]`.
    pub fn set_key_value_at(&mut self, step: f32, val: impl Into<Variant>) {
        let step = step.clamp(0.0, 1.0);
        self.key_values.retain(|(s, _)| (*s - step).abs() > 1e-4);
        self.key_values.push((step, val.into()));
        self.key_values
            .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    }

    /// Sets the easing curve.
    pub fn set_easing_curve(&mut self, curve: EasingCurve) {
        self.easing_curve = curve;
    }

    /// Returns the current easing curve.
    pub fn easing_curve(&self) -> &EasingCurve {
        &self.easing_curve
    }

    /// Sets the loop count. 1 for single run, -1 for infinite loops.
    pub fn set_loop_count(&mut self, count: i32) {
        self.loop_count = count;
    }

    /// Returns the current animation state.
    pub fn state(&self) -> AnimationState {
        self.state
    }

    /// Sets the animation playback direction.
    pub fn set_direction(&mut self, direction: AnimationDirection) {
        self.direction = direction;
    }

    /// Returns the current playback direction.
    pub fn direction(&self) -> AnimationDirection {
        self.direction
    }

    /// Starts or restarts the animation.
    pub fn start(&mut self) {
        let old = self.state;
        self.state = AnimationState::Running;
        self.current_loop = 0;
        self.current_time_ms = 0;
        if old != self.state {
            self.state_changed.emit(&(old, self.state));
        }
        let cur = self.current_value();
        self.value_changed.emit(&cur);
    }

    /// Pauses the animation.
    pub fn pause(&mut self) {
        if self.state == AnimationState::Running {
            let old = self.state;
            self.state = AnimationState::Paused;
            self.state_changed.emit(&(old, self.state));
        }
    }

    /// Resumes a paused animation.
    pub fn resume(&mut self) {
        if self.state == AnimationState::Paused {
            let old = self.state;
            self.state = AnimationState::Running;
            self.state_changed.emit(&(old, self.state));
        }
    }

    /// Stops the animation and resets to initial time.
    pub fn stop(&mut self) {
        if self.state != AnimationState::Stopped {
            let old = self.state;
            self.state = AnimationState::Stopped;
            self.state_changed.emit(&(old, self.state));
        }
    }

    /// Sets the current progress time in milliseconds.
    pub fn set_current_time(&mut self, ms: u64) {
        self.current_time_ms = ms.min(self.duration_ms);
        let cur = self.current_value();
        self.value_changed.emit(&cur);
    }

    /// Computes the current interpolated value based on progress and easing.
    pub fn current_value(&self) -> Variant {
        let progress = if self.duration_ms == 0 {
            1.0
        } else {
            (self.current_time_ms as f32 / self.duration_ms as f32).clamp(0.0, 1.0)
        };

        let effective_progress = match self.direction {
            AnimationDirection::Forward => progress,
            AnimationDirection::Backward => 1.0 - progress,
        };

        let eased_progress = self.easing_curve.value_for_progress(effective_progress);

        // Interpolate across keyframes if provided
        if !self.key_values.is_empty() {
            let mut lower = (0.0f32, self.start_value.clone().unwrap_or(Variant::Invalid));
            let mut upper = (1.0f32, self.end_value.clone().unwrap_or(Variant::Invalid));

            for (step, val) in &self.key_values {
                if *step <= eased_progress && *step >= lower.0 {
                    lower = (*step, val.clone());
                }
                if *step >= eased_progress && *step <= upper.0 {
                    upper = (*step, val.clone());
                    break;
                }
            }

            if (upper.0 - lower.0).abs() < 1e-4 {
                return lower.1;
            }

            let segment_p = (eased_progress - lower.0) / (upper.0 - lower.0);
            return lower.1.interpolate(&upper.1, segment_p).unwrap_or(lower.1);
        }

        // Standard 2-point interpolation (start -> end)
        if let (Some(start), Some(end)) = (&self.start_value, &self.end_value) {
            start.interpolate(end, eased_progress).unwrap_or_else(|| {
                if eased_progress >= 1.0 {
                    end.clone()
                } else {
                    start.clone()
                }
            })
        } else if let Some(end) = &self.end_value {
            end.clone()
        } else if let Some(start) = &self.start_value {
            start.clone()
        } else {
            Variant::Invalid
        }
    }

    /// Advances the animation timeline by `delta_ms`.
    ///
    /// Returns true if the animation is still active/running.
    pub fn step(&mut self, delta_ms: u64) -> bool {
        if self.state != AnimationState::Running {
            return false;
        }

        let new_time = self.current_time_ms.saturating_add(delta_ms);
        if new_time >= self.duration_ms {
            self.current_time_ms = self.duration_ms;
            let val = self.current_value();
            self.value_changed.emit(&val);

            self.current_loop += 1;
            if self.loop_count < 0 || self.current_loop < self.loop_count {
                // Loop again
                self.current_time_ms = 0;
                true
            } else {
                // Finished
                let old = self.state;
                self.state = AnimationState::Stopped;
                self.state_changed.emit(&(old, self.state));
                self.finished.emit(&());
                false
            }
        } else {
            self.current_time_ms = new_time;
            let val = self.current_value();
            self.value_changed.emit(&val);
            true
        }
    }
}

/// Property-bound animation matching Qt `QPropertyAnimation`.
pub struct PropertyAnimation {
    animation: VariantAnimation,
    target_id: Option<ObjectId>,
    property_name: String,
}

impl PropertyAnimation {
    /// Creates a new property animation targeting a named dynamic property.
    pub fn new(property_name: impl Into<String>) -> Self {
        Self {
            animation: VariantAnimation::new(),
            target_id: None,
            property_name: property_name.into(),
        }
    }

    /// Sets the target QObject identifier.
    pub fn set_target(&mut self, target: &dyn QObject) {
        self.target_id = Some(target.object_data().id());
    }

    /// Sets target object ID directly.
    pub fn set_target_id(&mut self, id: ObjectId) {
        self.target_id = Some(id);
    }

    /// Sets target property name.
    pub fn set_property_name(&mut self, name: impl Into<String>) {
        self.property_name = name.into();
    }

    /// Returns a mutable reference to the underlying `VariantAnimation`.
    pub fn animation_mut(&mut self) -> &mut VariantAnimation {
        &mut self.animation
    }

    /// Returns a reference to the underlying `VariantAnimation`.
    pub fn animation(&self) -> &VariantAnimation {
        &self.animation
    }

    /// Advances the timeline and writes the value into `target` with `QObject::set_property`,
    /// for a target that is not in the object registry (a `Window`, or a widget the caller
    /// holds). Writes only while the animation runs, like `QPropertyAnimation::updateCurrentValue`.
    pub fn step_target(&mut self, delta_ms: u64, target: &mut dyn QObject) -> bool {
        if self.animation.state() != AnimationState::Running {
            return false;
        }
        let active = self.animation.step(delta_ms);
        target.set_property(&self.property_name, self.animation.current_value());
        active
    }

    /// Advances the timeline and updates the target object's dynamic property.
    pub fn step(&mut self, delta_ms: u64) -> bool {
        let active = self.animation.step(delta_ms);
        let val = self.animation.current_value();

        if let Some(target_id) = self.target_id {
            crate::object::with_object_mut(target_id, |obj| {
                obj.set_property(&self.property_name, val.clone());
            });
        }

        active
    }
}
