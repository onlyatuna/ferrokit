// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::event::*;
use ferrokit_core::object::{register_owned_qobject, ObjectData, ObjectId, OwnedObject, QObject};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;

struct EventSpyWidget {
    data: ObjectData,
    last_event_type: Option<EventType>,
    mouse_dbl_click_count: Arc<AtomicI32>,
    context_menu_received: Arc<AtomicBool>,
    pointer_received: Arc<AtomicBool>,
    touch_received: Arc<AtomicBool>,
    tablet_received: Arc<AtomicBool>,
    gesture_received: Arc<AtomicBool>,
    hover_received: Arc<AtomicBool>,
}

impl EventSpyWidget {
    #[allow(clippy::too_many_arguments)]
    fn new(
        id: ObjectId,
        mouse_dbl_click_count: Arc<AtomicI32>,
        context_menu_received: Arc<AtomicBool>,
        pointer_received: Arc<AtomicBool>,
        touch_received: Arc<AtomicBool>,
        tablet_received: Arc<AtomicBool>,
        gesture_received: Arc<AtomicBool>,
        hover_received: Arc<AtomicBool>,
    ) -> Self {
        Self {
            data: ObjectData::new(id),
            last_event_type: None,
            mouse_dbl_click_count,
            context_menu_received,
            pointer_received,
            touch_received,
            tablet_received,
            gesture_received,
            hover_received,
        }
    }
}

impl QObject for EventSpyWidget {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn event(&mut self, event: &mut Event) -> bool {
        self.last_event_type = Some(event.event_type());
        match &event.kind {
            EventKind::MouseButtonDblClick { .. } => {
                self.mouse_dbl_click_count.fetch_add(1, Ordering::SeqCst);
                true
            }
            EventKind::ContextMenu { .. } => {
                self.context_menu_received.store(true, Ordering::SeqCst);
                true
            }
            EventKind::Pointer { .. } => {
                self.pointer_received.store(true, Ordering::SeqCst);
                true
            }
            EventKind::TouchBegin { .. }
            | EventKind::TouchUpdate { .. }
            | EventKind::TouchEnd { .. } => {
                self.touch_received.store(true, Ordering::SeqCst);
                true
            }
            EventKind::TabletPress { .. }
            | EventKind::TabletMove { .. }
            | EventKind::TabletRelease { .. } => {
                self.tablet_received.store(true, Ordering::SeqCst);
                true
            }
            EventKind::Gesture { .. } | EventKind::NativeGesture { .. } => {
                self.gesture_received.store(true, Ordering::SeqCst);
                true
            }
            EventKind::HoverEnter { .. }
            | EventKind::HoverMove { .. }
            | EventKind::HoverLeave { .. } => {
                self.hover_received.store(true, Ordering::SeqCst);
                true
            }
            _ => true,
        }
    }
}

#[test]
fn test_pointing_device_and_event_point_model() {
    let mouse = PointingDevice::primary_mouse();
    assert_eq!(mouse.pointer_type, PointerType::Mouse);
    assert!(mouse.capabilities.contains(DeviceCapabilities::HOVER));

    let touch = PointingDevice::primary_touch();
    assert_eq!(touch.pointer_type, PointerType::TouchScreen);
    assert_eq!(touch.maximum_touch_points, 10);

    let tablet = PointingDevice::primary_tablet();
    assert_eq!(tablet.pointer_type, PointerType::Stylus);
    assert!(tablet.capabilities.contains(DeviceCapabilities::PRESSURE));

    // Test EventPoint construction and modifications
    let pt = EventPoint::new(
        1,
        EventPointPos::new(100.5, 200.5),
        EventPointPos::new(300.0, 400.0),
    )
    .with_state(PointState::Pressed)
    .with_pressure(0.85)
    .with_rotation(45.0)
    .with_timestamp(123456);

    assert_eq!(pt.id, 1);
    assert_eq!(pt.state, PointState::Pressed);
    assert_eq!(pt.position.x, 100.5);
    assert_eq!(pt.position.y, 200.5);
    assert_eq!(pt.pressure, 0.85);
    assert_eq!(pt.rotation, 45.0);
    assert_eq!(pt.timestamp, 123456);
}

#[test]
fn test_event_type_mapping() {
    let ev1 = Event::new(EventKind::MouseButtonDblClick {
        x: 10,
        y: 20,
        button: 1,
    });
    assert_eq!(ev1.event_type(), EventType::MouseButtonDblClick);

    let ev2 = Event::new(EventKind::ContextMenu {
        x: 15,
        y: 25,
        global_x: 100,
        global_y: 200,
        reason: ContextMenuReason::Mouse,
    });
    assert_eq!(ev2.event_type(), EventType::ContextMenu);

    let ev3 = Event::new(EventKind::Pointer {
        device_id: PointerDeviceId::PRIMARY,
        points: vec![],
        buttons: MouseButtons::LEFT,
        modifiers: KeyboardModifiers::NO_MODIFIER,
        timestamp: 0,
    });
    assert_eq!(ev3.event_type(), EventType::Pointer);
    assert_eq!(
        EventType::Pointer as u32,
        218,
        "EventType::Pointer must match Qt QEvent::Pointer value (qcoreevent.h:285)"
    );
    assert_eq!(
        EventType::DpiChanged as u32,
        222,
        "EventType::DpiChanged must match Qt QEvent::DevicePixelRatioChange value (qcoreevent.h:292)"
    );

    let ev4 = Event::new(EventKind::HoverEnter {
        pos: EventPointPos::new(5.0, 5.0),
        old_pos: EventPointPos::new(0.0, 0.0),
        modifiers: KeyboardModifiers::NO_MODIFIER,
    });
    assert_eq!(ev4.event_type(), EventType::HoverEnter);

    let ev5 = Event::new(EventKind::Shortcut {
        key: 0x43,
        modifiers: 0x04000000,
        shortcut_id: 1,
        ambiguous: false,
    });
    assert_eq!(ev5.event_type(), EventType::Shortcut);

    let ev6 = Event::new(EventKind::ToolTip {
        x: 50,
        y: 50,
        global_x: 150,
        global_y: 250,
    });
    assert_eq!(ev6.event_type(), EventType::ToolTip);
}

#[test]
fn test_advanced_event_dispatch_to_qobject() {
    let dbl_click = Arc::new(AtomicI32::new(0));
    let context_menu = Arc::new(AtomicBool::new(false));
    let pointer = Arc::new(AtomicBool::new(false));
    let touch = Arc::new(AtomicBool::new(false));
    let tablet = Arc::new(AtomicBool::new(false));
    let gesture = Arc::new(AtomicBool::new(false));
    let hover = Arc::new(AtomicBool::new(false));

    let spy = EventSpyWidget::new(
        ObjectId::next(),
        Arc::clone(&dbl_click),
        Arc::clone(&context_menu),
        Arc::clone(&pointer),
        Arc::clone(&touch),
        Arc::clone(&tablet),
        Arc::clone(&gesture),
        Arc::clone(&hover),
    );

    // SAFETY: the owner stays on this thread; no callback of the spy runs in this test.
    let mut pinned = unsafe { register_owned_qobject(spy) };
    let spy_id = OwnedObject::object_id(&pinned);

    // 1. Mouse Button Double Click
    let mut ev_dbl = Event::new(EventKind::MouseButtonDblClick {
        x: 50,
        y: 50,
        button: 1,
    });
    pinned.event(&mut ev_dbl);
    assert_eq!(dbl_click.load(Ordering::SeqCst), 1);
    assert_eq!(pinned.last_event_type, Some(EventType::MouseButtonDblClick));

    // 2. Context Menu
    let mut ev_ctx = Event::new(EventKind::ContextMenu {
        x: 10,
        y: 10,
        global_x: 200,
        global_y: 300,
        reason: ContextMenuReason::Mouse,
    });
    pinned.event(&mut ev_ctx);
    assert!(context_menu.load(Ordering::SeqCst));
    assert_eq!(pinned.last_event_type, Some(EventType::ContextMenu));

    // 3. Pointer event with multiple points
    let p1 = EventPoint::new(
        1,
        EventPointPos::new(10.0, 10.0),
        EventPointPos::new(100.0, 100.0),
    );
    let p2 = EventPoint::new(
        2,
        EventPointPos::new(20.0, 20.0),
        EventPointPos::new(110.0, 110.0),
    );
    let mut ev_ptr = Event::new(EventKind::Pointer {
        device_id: PointerDeviceId(1),
        points: vec![p1, p2],
        buttons: MouseButtons::LEFT.union(MouseButtons::RIGHT),
        modifiers: KeyboardModifiers::SHIFT,
        timestamp: 1000,
    });
    pinned.event(&mut ev_ptr);
    assert!(pointer.load(Ordering::SeqCst));
    assert_eq!(pinned.last_event_type, Some(EventType::Pointer));

    // 4. Touch event
    let mut ev_touch = Event::new(EventKind::TouchBegin {
        device_id: PointerDeviceId(1),
        points: vec![],
        modifiers: KeyboardModifiers::NO_MODIFIER,
    });
    pinned.event(&mut ev_touch);
    assert!(touch.load(Ordering::SeqCst));
    assert_eq!(pinned.last_event_type, Some(EventType::TouchBegin));

    // 5. Tablet event
    let mut ev_tablet = Event::new(EventKind::TabletPress {
        device: TabletDevice::Stylus,
        pointer_type: TabletPointerType::Pen,
        pos: EventPointPos::new(50.0, 50.0),
        global_pos: EventPointPos::new(200.0, 200.0),
        pressure: 0.95,
        x_tilt: 10.0,
        y_tilt: -5.0,
        rotation: 0.0,
        buttons: MouseButtons::LEFT,
        modifiers: KeyboardModifiers::NO_MODIFIER,
    });
    pinned.event(&mut ev_tablet);
    assert!(tablet.load(Ordering::SeqCst));
    assert_eq!(pinned.last_event_type, Some(EventType::TabletPress));

    // 6. Gesture event
    let mut ev_gesture = Event::new(EventKind::Gesture {
        state: GestureState::GestureStarted,
        gesture: GestureType::Tap,
    });
    pinned.event(&mut ev_gesture);
    assert!(gesture.load(Ordering::SeqCst));
    assert_eq!(pinned.last_event_type, Some(EventType::Gesture));

    // 7. Hover event
    let mut ev_hover = Event::new(EventKind::HoverMove {
        pos: EventPointPos::new(30.0, 40.0),
        old_pos: EventPointPos::new(25.0, 35.0),
        modifiers: KeyboardModifiers::NO_MODIFIER,
    });
    pinned.event(&mut ev_hover);
    assert!(hover.load(Ordering::SeqCst));
    assert_eq!(pinned.last_event_type, Some(EventType::HoverMove));

    // SAFETY: the test event callbacks have returned on the registration thread.
    unsafe { ferrokit_core::object::unregister_qobject(spy_id) };
}

#[test]
fn test_event_compression_with_high_frequency_events() {
    let compressor = compressor::CoreCompressor;
    let target = ObjectId::next();

    // 1. MouseMove is not compressed (QApplicationPrivate::compressEvent: qapplication.cpp:794-804)
    let mut ev_move1 = Event::new(EventKind::MouseMove { x: 10, y: 10 });
    let ev_move2 = Event::new(EventKind::MouseMove { x: 25, y: 35 });
    assert!(
        !compressor.try_compress(&mut ev_move1, &ev_move2, target),
        "MouseMove events must not be compressed"
    );

    // 2. HoverMove is not compressed (QApplicationPrivate::compressEvent: qapplication.cpp:794-804)
    let mut ev_hover1 = Event::new(EventKind::HoverMove {
        pos: EventPointPos::new(10.0, 10.0),
        old_pos: EventPointPos::new(0.0, 0.0),
        modifiers: KeyboardModifiers::NO_MODIFIER,
    });
    let ev_hover2 = Event::new(EventKind::HoverMove {
        pos: EventPointPos::new(88.0, 99.0),
        old_pos: EventPointPos::new(10.0, 10.0),
        modifiers: KeyboardModifiers::NO_MODIFIER,
    });
    assert!(
        !compressor.try_compress(&mut ev_hover1, &ev_hover2, target),
        "HoverMove events must not be compressed"
    );

    // 3. Resize compression
    let mut ev_resize1 = Event::new(EventKind::Resize {
        width: 100,
        height: 100,
        old_width: 50,
        old_height: 50,
    });
    let ev_resize2 = Event::new(EventKind::Resize {
        width: 800,
        height: 600,
        old_width: 100,
        old_height: 100,
    });
    assert!(compressor.try_compress(&mut ev_resize1, &ev_resize2, target));
    if let EventKind::Resize { width, height, .. } = ev_resize1.kind {
        assert_eq!(width, 800);
        assert_eq!(height, 600);
    } else {
        panic!("Expected Resize");
    }
}
