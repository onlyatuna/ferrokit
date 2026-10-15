// SPDX-License-Identifier: LGPL-3.0-only
use std::any::{Any, TypeId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use ferrokit_core::meta::{register_meta_type, MetaType, MetaTypeId};
use ferrokit_core::object::ObjectId;
use ferrokit_core::signal::Signal;
use ferrokit_core::types::*;
use ferrokit_core::variant::Variant;

// =============================================================================
// 1. User Custom Types Extensibility & Downcasting
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
struct UserProfile {
    pub username: String,
    pub age: u32,
    pub active: bool,
}

#[test]
fn test_custom_user_type_storage_and_downcast() {
    let profile = UserProfile {
        username: "Alice".to_string(),
        age: 30,
        active: true,
    };

    // 1. Storage via from_custom
    let v = Variant::from_custom(profile.clone());
    assert!(v.is_valid());
    assert!(v.is::<UserProfile>());
    assert_eq!(v.type_id(), TypeId::of::<UserProfile>());

    // 2. Downcasting by reference
    let borrowed: &UserProfile = v.downcast_ref::<UserProfile>().expect("downcast ref");
    assert_eq!(borrowed, &profile);

    // 3. Downcasting to cloned value
    let cloned: UserProfile = v.downcast::<UserProfile>().expect("downcast cloned");
    assert_eq!(cloned, profile);

    // 4. Downcasting to Arc
    let arc_ref: Arc<UserProfile> = v.downcast_arc::<UserProfile>().expect("downcast arc");
    assert_eq!(*arc_ref, profile);

    // 5. Downcasting to unrelated type returns None
    assert!(v.downcast_ref::<String>().is_none());
    assert!(v.downcast_ref::<i32>().is_none());
}

#[test]
fn test_custom_type_from_box_and_arc() {
    let profile = UserProfile {
        username: "Bob".to_string(),
        age: 25,
        active: false,
    };

    // Storing Box<dyn Any + Send + Sync> directly
    let boxed: Box<dyn Any + Send + Sync> = Box::new(profile.clone());
    let v_box: Variant = Variant::from(boxed);
    assert!(v_box.is::<UserProfile>());
    assert_eq!(v_box.downcast_ref::<UserProfile>(), Some(&profile));

    // Storing Arc<dyn Any + Send + Sync>
    let arc_any: Arc<dyn Any + Send + Sync> = Arc::new(profile.clone());
    let v_arc: Variant = Variant::from(arc_any);
    assert!(v_arc.is::<UserProfile>());
    assert_eq!(v_arc.downcast_ref::<UserProfile>(), Some(&profile));
}

// =============================================================================
// 2. MetaType Integration & Custom Registration (qRegisterMetaType)
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
struct CustomWidgetSettings {
    pub border_width: i32,
    pub antialias: bool,
}

#[test]
fn test_metatype_registration_and_reflection() {
    // Register custom type into meta object system
    let registered_id = register_meta_type::<CustomWidgetSettings>("CustomWidgetSettings");
    assert!(registered_id.0 >= MetaTypeId::USER.0);

    let settings = CustomWidgetSettings {
        border_width: 2,
        antialias: true,
    };

    let v = Variant::from_custom(settings.clone());

    // Reflection checks
    assert_eq!(v.type_name(), "CustomWidgetSettings");
    assert_eq!(v.meta_type().name(), "CustomWidgetSettings");
    assert_eq!(v.meta_type().id(), registered_id);
    assert_eq!(v.type_id(), TypeId::of::<CustomWidgetSettings>());

    // MetaType lookup checks
    let mt_by_id = MetaType::from_type_id(TypeId::of::<CustomWidgetSettings>()).unwrap();
    assert_eq!(mt_by_id.name(), "CustomWidgetSettings");

    let mt_by_name = MetaType::from_name("CustomWidgetSettings").unwrap();
    assert_eq!(mt_by_name.id(), registered_id);

    // Downcasting works seamlessly
    assert_eq!(v.downcast_ref::<CustomWidgetSettings>(), Some(&settings));
}

// =============================================================================
// 3. GUI / Subsystem Types Simulation (Font, Brush, Pen, Pixmap, Transform)
// =============================================================================

#[derive(Debug, Clone, PartialEq)]
struct MockFont {
    pub family: String,
    pub point_size: f32,
}

#[derive(Debug, Clone, PartialEq)]
struct MockBrush {
    pub color: (u8, u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq)]
struct MockTransform {
    pub m11: f32,
    pub m22: f32,
}

#[test]
fn test_gui_types_stored_in_core_variant() {
    // Register Qt GUI type names
    register_meta_type::<MockFont>("QFont");
    register_meta_type::<MockBrush>("QBrush");
    register_meta_type::<MockTransform>("QTransform");

    let font = MockFont {
        family: "Segoe UI".to_string(),
        point_size: 10.5,
    };
    let brush = MockBrush {
        color: (255, 128, 0, 255),
    };
    let transform = MockTransform { m11: 1.0, m22: 1.0 };

    let v_font = Variant::from_custom(font.clone());
    let v_brush = Variant::from_custom(brush.clone());
    let v_transform = Variant::from_custom(transform.clone());

    assert_eq!(v_font.type_name(), "QFont");
    assert_eq!(v_brush.type_name(), "QBrush");
    assert_eq!(v_transform.type_name(), "QTransform");

    assert_eq!(v_font.downcast_ref::<MockFont>(), Some(&font));
    assert_eq!(v_brush.downcast_ref::<MockBrush>(), Some(&brush));
    assert_eq!(
        v_transform.downcast_ref::<MockTransform>(),
        Some(&transform)
    );
}

// =============================================================================
// 4. Safe QObject Pointer in Variant (ObjectId)
// =============================================================================

#[test]
fn test_qobject_pointer_in_variant() {
    let obj_id = ObjectId(9999);
    let v = Variant::from(obj_id);

    assert_eq!(v.type_name(), "QObject*");
    assert_eq!(v.meta_type().id(), MetaTypeId::QOBJECT_STAR);
    assert_eq!(v.to_object_id(), Some(obj_id));
    assert_eq!(v.downcast_ref::<ObjectId>(), Some(&obj_id));
    assert_eq!(v.to_string_lossy(), "QObject(ObjectId(9999))");
}

// =============================================================================
// 5. Geometry Types (Size, Line, Margins) & Interpolation
// =============================================================================

#[test]
fn test_geometry_types_and_interpolation() {
    // Size & SizeF
    let sz = Size::new(100, 200);
    let v_sz = Variant::from(sz);
    assert_eq!(v_sz.type_name(), "QSize");
    assert_eq!(v_sz.to_size(), Some(sz));

    let sz_target = Size::new(200, 400);
    let sz_mid = v_sz.interpolate(&Variant::from(sz_target), 0.5).unwrap();
    assert_eq!(sz_mid.to_size(), Some(Size::new(150, 300)));

    // Line & LineF
    let line1 = Line::new(0, 0, 10, 20);
    let line2 = Line::new(100, 100, 110, 120);
    let v_line = Variant::from(line1);
    assert_eq!(v_line.type_name(), "QLine");
    assert_eq!(v_line.to_line(), Some(line1));

    let line_mid = v_line.interpolate(&Variant::from(line2), 0.5).unwrap();
    assert_eq!(line_mid.to_line(), Some(Line::new(50, 50, 60, 70)));

    // Margins
    let m1 = Margins::new(10, 20, 30, 40);
    let m2 = Margins::new(20, 40, 60, 80);
    let v_m = Variant::from(m1);
    assert_eq!(v_m.type_name(), "QMargins");
    assert_eq!(v_m.to_margins(), Some(m1));

    let m_mid = v_m.interpolate(&Variant::from(m2), 0.5).unwrap();
    assert_eq!(m_mid.to_margins(), Some(Margins::new(15, 30, 45, 60)));
}

// =============================================================================
// 6. Temporal & Identifiers (Date, Time, DateTime, Url, Uuid, RegularExpression, Locale)
// =============================================================================

#[test]
fn test_temporal_and_identifier_types() {
    // Date
    let d = Date::new(2026, 9, 26);
    let v_d = Variant::from(d);
    assert_eq!(v_d.type_name(), "QDate");
    assert_eq!(v_d.to_date(), Some(d));
    assert_eq!(v_d.to_string_lossy(), "2026-09-26");

    // Time
    let t = Time::new(14, 30, 45, 123);
    let v_t = Variant::from(t);
    assert_eq!(v_t.type_name(), "QTime");
    assert_eq!(v_t.to_time(), Some(t));
    assert_eq!(v_t.to_string_lossy(), "14:30:45.123");

    // DateTime
    let dt = DateTime::new(d, t);
    let v_dt = Variant::from(dt);
    assert_eq!(v_dt.type_name(), "QDateTime");
    let retrieved_dt = v_dt.to_date_time().unwrap();
    assert_eq!(retrieved_dt.date, d);
    assert_eq!(retrieved_dt.time, t);

    // Url
    let u = Url::new("https://qt.io:8080/download?os=rust#intro");
    let v_u = Variant::from(u.clone());
    assert_eq!(v_u.type_name(), "QUrl");
    let retrieved_u = v_u.to_url().unwrap();
    assert_eq!(retrieved_u.scheme(), Some("https"));
    assert_eq!(retrieved_u.host(), Some("qt.io"));
    assert_eq!(retrieved_u.port(), Some(8080));
    assert_eq!(retrieved_u.path(), "/download");

    // Uuid
    let uuid_str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
    let uuid = Uuid::from_string(uuid_str).expect("valid uuid");
    let v_uuid = Variant::from(uuid);
    assert_eq!(v_uuid.type_name(), "QUuid");
    assert_eq!(v_uuid.to_uuid(), Some(uuid));
    assert_eq!(v_uuid.to_string_lossy(), uuid_str);

    // RegularExpression
    let regex = RegularExpression::new(r"^[A-Z][a-z0-9_]+$").with_case_insensitive(true);
    let v_re = Variant::from(regex.clone());
    assert_eq!(v_re.type_name(), "QRegularExpression");
    assert_eq!(v_re.to_regular_expression(), Some(regex));

    // Locale
    let loc = Locale::new("zh_TW");
    let v_loc = Variant::from(loc.clone());
    assert_eq!(v_loc.type_name(), "QLocale");
    assert_eq!(v_loc.to_locale(), Some(loc));
}

// =============================================================================
// 7. Signal Transmission with Custom Variant
// =============================================================================

#[test]
fn test_signal_emission_with_custom_variant() {
    let sig: Signal<Variant> = Signal::new();
    let received = Arc::new(AtomicBool::new(false));
    let received_clone = Arc::clone(&received);

    sig.connect(move |val| {
        if let Some(user) = val.downcast_ref::<UserProfile>() {
            if user.username == "SignalUser" && user.age == 42 {
                received_clone.store(true, Ordering::SeqCst);
            }
        }
    });

    let profile = UserProfile {
        username: "SignalUser".to_string(),
        age: 42,
        active: true,
    };
    sig.emit(&Variant::from_custom(profile));

    assert!(received.load(Ordering::SeqCst));
}
