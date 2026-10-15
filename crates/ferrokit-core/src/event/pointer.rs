// SPDX-License-Identifier: LGPL-3.0-only
/// Unique identifier for an input pointer device (`QPointingDeviceUniqueId`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct PointerDeviceId(pub i64);

impl PointerDeviceId {
    pub const PRIMARY: Self = Self(0);
}

/// Type of pointing device (`QPointingDevice::PointerType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PointerType {
    #[default]
    Unknown = 0,
    Generic = 1,
    Mouse = 2,
    TouchScreen = 3,
    TouchPad = 4,
    Puck = 5,
    Stylus = 6,
    Airbrush = 7,
    Eraser = 8,
    BarCodeReader = 9,
}

/// Device capabilities / flags (`QInputDevice::Capabilities`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct DeviceCapabilities(pub u32);

impl DeviceCapabilities {
    pub const NONE: Self = Self(0);
    pub const POSITION: Self = Self(1 << 0);
    pub const AREA: Self = Self(1 << 1);
    pub const PRESSURE: Self = Self(1 << 2);
    pub const VELOCITY: Self = Self(1 << 3);
    pub const SCROLL: Self = Self(1 << 4);
    pub const HOVER: Self = Self(1 << 5);
    pub const ROTATION: Self = Self(1 << 6);
    pub const Z_POSITION: Self = Self(1 << 7);
    pub const ALL: Self = Self(0xFFFFFFFF);

    #[inline]
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Pointing device metadata (`QPointingDevice`).
#[derive(Debug, Clone, PartialEq)]
pub struct PointingDevice {
    pub id: PointerDeviceId,
    pub name: String,
    pub pointer_type: PointerType,
    pub capabilities: DeviceCapabilities,
    pub maximum_touch_points: usize,
}

impl PointingDevice {
    pub fn primary_mouse() -> Self {
        Self {
            id: PointerDeviceId::PRIMARY,
            name: "Core Pointer".to_string(),
            pointer_type: PointerType::Mouse,
            capabilities: DeviceCapabilities(
                DeviceCapabilities::POSITION.0
                    | DeviceCapabilities::HOVER.0
                    | DeviceCapabilities::SCROLL.0,
            ),
            maximum_touch_points: 1,
        }
    }

    pub fn primary_touch() -> Self {
        Self {
            id: PointerDeviceId(1),
            name: "Primary Touchscreen".to_string(),
            pointer_type: PointerType::TouchScreen,
            capabilities: DeviceCapabilities(
                DeviceCapabilities::POSITION.0
                    | DeviceCapabilities::AREA.0
                    | DeviceCapabilities::PRESSURE.0,
            ),
            maximum_touch_points: 10,
        }
    }

    pub fn primary_tablet() -> Self {
        Self {
            id: PointerDeviceId(2),
            name: "Graphics Tablet".to_string(),
            pointer_type: PointerType::Stylus,
            capabilities: DeviceCapabilities(
                DeviceCapabilities::POSITION.0
                    | DeviceCapabilities::PRESSURE.0
                    | DeviceCapabilities::ROTATION.0
                    | DeviceCapabilities::HOVER.0,
            ),
            maximum_touch_points: 1,
        }
    }
}

/// State of an individual contact point (`QEventPoint::State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PointState {
    #[default]
    Unknown = 0,
    Stationary = 1,
    Pressed = 2,
    Updated = 3,
    Released = 4,
}

/// 2D Floating point position for high-precision event coordinates (`QPointF`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EventPointPos {
    pub x: f32,
    pub y: f32,
}

impl EventPointPos {
    #[inline]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    #[inline]
    pub fn to_i32_tuple(self) -> (i32, i32) {
        (self.x.round() as i32, self.y.round() as i32)
    }
}

impl From<(f32, f32)> for EventPointPos {
    #[inline]
    fn from((x, y): (f32, f32)) -> Self {
        Self { x, y }
    }
}

impl From<(i32, i32)> for EventPointPos {
    #[inline]
    fn from((x, y): (i32, i32)) -> Self {
        Self {
            x: x as f32,
            y: y as f32,
        }
    }
}

/// Represents an individual touch, tablet, or mouse point in an input event (`QEventPoint`).
#[derive(Debug, Clone, PartialEq)]
pub struct EventPoint {
    pub id: i32,
    pub state: PointState,
    pub position: EventPointPos,
    pub scene_position: EventPointPos,
    pub global_position: EventPointPos,
    pub press_position: EventPointPos,
    pub last_position: EventPointPos,
    pub timestamp: u64,
    pub press_timestamp: u64,
    pub pressure: f32,
    pub rotation: f32,
    pub ellipse_diameters: (f32, f32),
    pub velocity: (f32, f32),
    pub accepted: bool,
}

impl EventPoint {
    pub fn new(id: i32, pos: EventPointPos, global_pos: EventPointPos) -> Self {
        Self {
            id,
            state: PointState::Stationary,
            position: pos,
            scene_position: pos,
            global_position: global_pos,
            press_position: pos,
            last_position: pos,
            timestamp: 0,
            press_timestamp: 0,
            pressure: 0.0,
            rotation: 0.0,
            ellipse_diameters: (0.0, 0.0),
            velocity: (0.0, 0.0),
            accepted: false,
        }
    }

    pub fn with_state(mut self, state: PointState) -> Self {
        self.state = state;
        self
    }

    pub fn with_pressure(mut self, pressure: f32) -> Self {
        self.pressure = pressure;
        self
    }

    pub fn with_rotation(mut self, rotation: f32) -> Self {
        self.rotation = rotation;
        self
    }

    pub fn with_timestamp(mut self, timestamp: u64) -> Self {
        self.timestamp = timestamp;
        self
    }
}

/// Qt Mouse Buttons bitflag (`Qt::MouseButtons`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MouseButtons(pub u32);

impl MouseButtons {
    pub const NO_BUTTON: Self = Self(0);
    pub const LEFT: Self = Self(1 << 0);
    pub const RIGHT: Self = Self(1 << 1);
    pub const MIDDLE: Self = Self(1 << 2);
    pub const BACK: Self = Self(1 << 3);
    pub const FORWARD: Self = Self(1 << 4);
    pub const TASK: Self = Self(1 << 5);

    #[inline]
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline]
    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// Keyboard modifiers bitflag (`Qt::KeyboardModifiers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyboardModifiers(pub u32);

impl KeyboardModifiers {
    pub const NO_MODIFIER: Self = Self(0);
    pub const SHIFT: Self = Self(1 << 0);
    pub const CONTROL: Self = Self(1 << 1);
    pub const ALT: Self = Self(1 << 2);
    pub const META: Self = Self(1 << 3);
    pub const KEYPAD: Self = Self(1 << 4);

    #[inline]
    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

/// Gesture state enumeration (`Qt::GestureState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GestureState {
    #[default]
    NoGesture = 0,
    GestureStarted = 1,
    GestureUpdated = 2,
    GestureFinished = 3,
    GestureCanceled = 4,
}

/// Gesture type enumeration (`Qt::GestureType`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureType {
    Tap,
    TapAndHold,
    Pan {
        delta: (f32, f32),
        acceleration: (f32, f32),
    },
    Pinch {
        total_scale_factor: f32,
        last_scale_factor: f32,
        rotation_angle: f32,
    },
    Swipe {
        horizontal_direction: f32,
        vertical_direction: f32,
    },
    Custom(u32),
}

/// Native gesture type enumeration (`Qt::NativeGestureType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeGestureType {
    Begin,
    End,
    Pan,
    Zoom,
    SmartZoom,
    Rotate,
    Swipe,
}

/// Tablet device type enumeration (`QTabletEvent::TabletDevice`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TabletDevice {
    #[default]
    NoDevice = 0,
    Puck = 1,
    Stylus = 2,
    Airbrush = 3,
    FourDMouse = 4,
    RotationStylus = 5,
}

/// Tablet pointer type (`QPointingDevice::PointerType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TabletPointerType {
    #[default]
    Unknown = 0,
    Pen = 1,
    Cursor = 2,
    Eraser = 3,
}

/// Context menu trigger reason (`QContextMenuEvent::Reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ContextMenuReason {
    #[default]
    Mouse,
    Keyboard,
    Other,
}
