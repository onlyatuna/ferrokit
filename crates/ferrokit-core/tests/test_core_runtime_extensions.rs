// SPDX-License-Identifier: LGPL-3.0-only
use std::fs::{self, File};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ferrokit_core::animation::{
    AnimationDirection, AnimationState, EasingCurve, EasingType, PropertyAnimation,
    VariantAnimation,
};
use ferrokit_core::fs::FileSystemWatcher;
use ferrokit_core::object::{
    ObjectData, ObjectId, OwnedObject, QObject, ThreadId, move_to_thread_by_id,
    query_object_thread, register_owned_qobject,
};
use ferrokit_core::settings::Settings;
use ferrokit_core::signal::Signal;
use ferrokit_core::variant::Variant;

/// An `ObjectData` that lets `move_to_thread_by_id` move its object.
fn movable_data(id: ObjectId) -> ObjectData {
    // SAFETY: `DynamicMockObject` holds an `ObjectData` and two `Arc<Mutex<_>>` handles, which
    // are `Send`; no `Rc`, `Cell`, raw pointer or thread-local state (see the struct definition),
    // so using and dropping it on another thread after `move_to_thread_by_id` is sound.
    unsafe { ObjectData::new(id).with_thread_moves_allowed() }
}

// -----------------------------------------------------------------------------
// Test Object for Dynamic Properties and Events
// -----------------------------------------------------------------------------

struct DynamicMockObject {
    data: ObjectData,
    last_changed_property: Arc<Mutex<Option<String>>>,
    change_count: Arc<Mutex<u32>>,
}

impl DynamicMockObject {
    fn new(id: ObjectId, last_prop: Arc<Mutex<Option<String>>>, count: Arc<Mutex<u32>>) -> Self {
        Self {
            data: movable_data(id),
            last_changed_property: last_prop,
            change_count: count,
        }
    }
}

impl QObject for DynamicMockObject {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }
    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }
    fn dynamic_property_change(&mut self, property_name: &str) {
        *self.last_changed_property.lock().unwrap() = Some(property_name.to_string());
        *self.change_count.lock().unwrap() += 1;
    }
}

// -----------------------------------------------------------------------------
// 1. Test QVariant & Dynamic Properties
// -----------------------------------------------------------------------------

#[test]
fn test_variant_types_and_interpolation() {
    let v_int = Variant::I64(42);
    assert_eq!(v_int.to_int(), Some(42));
    assert_eq!(v_int.to_float(), Some(42.0));
    assert_eq!(v_int.type_name(), "qlonglong");

    let v_str = Variant::String("123".to_string());
    assert_eq!(v_str.to_int(), Some(123));

    let v_bool = Variant::String("true".to_string());
    assert_eq!(v_bool.to_bool(), Some(true));

    // Linear numeric interpolation
    let start_f = Variant::F64(10.0);
    let end_f = Variant::F64(20.0);
    let mid_f = start_f.interpolate(&end_f, 0.5).unwrap();
    assert_eq!(mid_f, Variant::F64(15.0));

    // Color interpolation
    let red = Variant::Color(255, 0, 0, 255);
    let blue = Variant::Color(0, 0, 255, 255);
    let purple = red.interpolate(&blue, 0.5).unwrap();
    assert_eq!(purple, Variant::Color(128, 0, 128, 255));

    // Point and Rect interpolation
    let pt1 = Variant::Point(0, 0);
    let pt2 = Variant::Point(100, 200);
    let pt_mid = pt1.interpolate(&pt2, 0.5).unwrap();
    assert_eq!(pt_mid, Variant::Point(50, 100));
}

#[test]
fn test_qobject_dynamic_properties_and_events() {
    let last_prop = Arc::new(Mutex::new(None));
    let count = Arc::new(Mutex::new(0));
    let mut obj =
        DynamicMockObject::new(ObjectId::next(), Arc::clone(&last_prop), Arc::clone(&count));

    assert!(obj.property("custom_color").is_none());

    // Setting an undeclared property creates it, emits DynamicPropertyChange, and returns false (qobject.cpp:4574)
    let changed = obj.set_property("custom_color", Variant::Color(255, 128, 0, 255));
    assert!(
        !changed,
        "Setting undeclared property must return false (qobject.cpp:4574)"
    );
    assert_eq!(
        obj.property("custom_color"),
        Some(Variant::Color(255, 128, 0, 255))
    );
    assert_eq!(*last_prop.lock().unwrap(), Some("custom_color".to_string()));
    assert_eq!(*count.lock().unwrap(), 1);

    // Setting the same property value returns false and does not trigger event
    let changed_again = obj.set_property("custom_color", Variant::Color(255, 128, 0, 255));
    assert!(!changed_again);
    assert_eq!(*count.lock().unwrap(), 1);

    // Dynamic property names listing
    obj.set_property("opacity", Variant::F64(0.8));
    let names = obj.dynamic_property_names();
    assert!(names.contains(&"custom_color".to_string()));
    assert!(names.contains(&"opacity".to_string()));

    // Setting Variant::Invalid on absent dynamic property returns false, does not emit event, does not create property
    let r_absent = obj.set_property("absent_prop", Variant::Invalid);
    assert!(!r_absent);
    assert_eq!(obj.property("absent_prop"), None);
    assert!(
        !obj.dynamic_property_names()
            .contains(&"absent_prop".to_string())
    );
    assert_eq!(*count.lock().unwrap(), 2);

    // Setting Variant::Invalid on existing dynamic property removes it, emits DynamicPropertyChange, and returns false (qobject.cpp:4554-4574)
    let r_remove = obj.set_property("custom_color", Variant::Invalid);
    assert!(
        !r_remove,
        "Removing dynamic property with Variant::Invalid must return false (qobject.cpp:4574)"
    );
    assert_eq!(
        obj.property("custom_color"),
        None,
        "Dynamic property must be removed after setting Variant::Invalid (qobject.cpp:4557)"
    );
    assert!(
        !obj.dynamic_property_names()
            .contains(&"custom_color".to_string()),
        "Dynamic property name must be removed from list"
    );
    assert_eq!(*last_prop.lock().unwrap(), Some("custom_color".to_string()));
    assert_eq!(
        *count.lock().unwrap(),
        3,
        "Event must be emitted on removal"
    );
}
// -----------------------------------------------------------------------------
// 2. Test Signal connect_to and Object Thread Affinity
// -----------------------------------------------------------------------------

#[test]
fn test_signal_auto_thread_affinity() {
    let last_prop = Arc::new(Mutex::new(None));
    let count = Arc::new(Mutex::new(0));
    // SAFETY: `register_owned_qobject` requires that nothing outside a callback accesses the
    // object during one; the test uses it only through `obj.with` (which refuses during a
    // callback), and this object defines no callbacks beyond `dynamic_property_change`. The owner
    // `obj` stays in this local. At the end of the test `obj` is dropped on this thread after
    // `move_to_thread_by_id` moved the registration to another thread, which is not the
    // registration thread; `OwnedObject::drop` handles that by posting the destruction to the
    // registration thread (documented on `destroy_owned`), so no memory is freed from here.
    let obj = unsafe {
        register_owned_qobject(DynamicMockObject::new(ObjectId::next(), last_prop, count))
    };
    let id = OwnedObject::object_id(&obj);

    // Verify initial registered thread matches current
    assert_eq!(query_object_thread(id), Some(ThreadId::current()));

    let signal: Signal<i32> = Signal::new();
    let received = Arc::new(AtomicU32::new(0));
    let rec_clone = Arc::clone(&received);

    // Connect automatically tracking target QObject
    let _conn = obj
        .with(|o| {
            signal.connect_to(o, move |val: &i32| {
                rec_clone.store(*val as u32, Ordering::SeqCst);
            })
        })
        .unwrap();

    // Emitting on the same thread executes slot directly
    signal.emit(&999);
    assert_eq!(received.load(Ordering::SeqCst), 999);

    // Simulate moving object to another thread
    let foreign_thread = ThreadId(
        std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap(),
    );
    let res = move_to_thread_by_id(id, foreign_thread, ThreadId::current());
    assert!(res.is_ok());

    // Query global registry to verify new thread affinity
    assert_eq!(query_object_thread(id), Some(foreign_thread));
}

// -----------------------------------------------------------------------------
// 3. Test Animation Framework (QEasingCurve, VariantAnimation, PropertyAnimation)
// -----------------------------------------------------------------------------

#[test]
fn test_easing_curves_mathematical_properties() {
    let linear = EasingCurve::new(EasingType::Linear);
    assert_eq!(linear.value_for_progress(0.0), 0.0);
    assert_eq!(linear.value_for_progress(0.5), 0.5);
    assert_eq!(linear.value_for_progress(1.0), 1.0);

    let in_quad = EasingCurve::new(EasingType::InQuad);
    assert_eq!(in_quad.value_for_progress(0.5), 0.25);

    let out_quad = EasingCurve::new(EasingType::OutQuad);
    assert_eq!(out_quad.value_for_progress(0.5), 0.75);

    let out_bounce = EasingCurve::new(EasingType::OutBounce);
    assert_eq!(out_bounce.value_for_progress(0.0), 0.0);
    assert_eq!(out_bounce.value_for_progress(1.0), 1.0);

    let in_elastic = EasingCurve::new(EasingType::InElastic);
    assert_eq!(in_elastic.value_for_progress(0.0), 0.0);
    assert_eq!(in_elastic.value_for_progress(1.0), 1.0);
}

#[test]
fn test_variant_animation_timeline_and_keyframes() {
    let mut anim = VariantAnimation::new();
    anim.set_duration(1000);
    anim.set_start_value(0.0f32);
    anim.set_end_value(100.0f32);

    let values_recorded = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rec_clone = Arc::clone(&values_recorded);
    let _conn = anim.value_changed.connect(move |v: &Variant| {
        if let Some(f) = v.to_float() {
            rec_clone.lock().unwrap().push(f);
        }
    });

    let finished_flag = Arc::new(AtomicBool::new(false));
    let fin_clone = Arc::clone(&finished_flag);
    let _fin_conn = anim.finished.connect(move |()| {
        fin_clone.store(true, Ordering::SeqCst);
    });

    anim.start();
    assert_eq!(anim.state(), AnimationState::Running);

    // Step 500ms (50% progress)
    let active = anim.step(500);
    assert!(active);
    assert_eq!(anim.current_value(), Variant::F64(50.0));

    // Step another 500ms (100% progress -> finished)
    let active_end = anim.step(500);
    assert!(!active_end);
    assert_eq!(anim.current_value(), Variant::F64(100.0));
    assert_eq!(anim.state(), AnimationState::Stopped);
    assert!(finished_flag.load(Ordering::SeqCst));

    // Test backward animation
    anim.set_direction(AnimationDirection::Backward);
    anim.start();
    anim.step(250); // 25% backward from end -> 75.0
    assert_eq!(anim.current_value(), Variant::F64(75.0));
}

#[test]
fn test_property_animation_does_not_retain_borrowed_object() {
    let last_prop = Arc::new(Mutex::new(None));
    let count = Arc::new(Mutex::new(0));
    let obj = DynamicMockObject::new(ObjectId::next(), last_prop, count);

    let mut prop_anim = PropertyAnimation::new("token_count");
    prop_anim.set_target(&obj);
    prop_anim.animation_mut().set_duration(500);
    prop_anim.animation_mut().set_start_value(1000);
    prop_anim.animation_mut().set_end_value(2000);

    prop_anim.animation_mut().start();
    prop_anim.step(250);

    assert_eq!(obj.property("token_count"), None);
}

// -----------------------------------------------------------------------------
// 4. Test QSettings INI Persistence and Hierarchy
// -----------------------------------------------------------------------------

#[test]
fn test_qsettings_hierarchy_and_atomic_sync() {
    let tmp_dir =
        std::env::temp_dir().join(format!("ferrokit_test_settings_{}", ObjectId::next().0));
    let ini_path = tmp_dir.join("app_settings.ini");

    {
        let mut settings = Settings::new(&ini_path);
        settings.set_value("version", 1);
        settings.set_value("debug_mode", true);

        settings.begin_group("HUD");
        settings.set_value("opacity", 0.85);
        settings.set_value("stay_on_top", true);
        settings.set_value("theme_color", Variant::Color(0, 200, 255, 255));

        settings.begin_group("ArcDial");
        settings.set_value("radius", 60);
        settings.end_group(); // End ArcDial

        settings.end_group(); // End HUD

        assert_eq!(settings.value_or("version", 0), Variant::I64(1));
        assert!(settings.contains("version"));

        // Commit changes to disk atomically
        settings.sync().expect("Settings sync should succeed");
    }

    // Verify written file exists
    assert!(ini_path.exists());

    // Reload in a fresh Settings instance
    {
        let mut reloaded = Settings::new(&ini_path);
        assert_eq!(reloaded.value("version"), Some(Variant::I64(1)));
        assert_eq!(reloaded.value("debug_mode"), Some(Variant::Bool(true)));

        reloaded.begin_group("HUD");
        assert_eq!(reloaded.value("opacity"), Some(Variant::F64(0.85)));
        assert_eq!(reloaded.value("stay_on_top"), Some(Variant::Bool(true)));
        assert_eq!(
            reloaded.value("theme_color"),
            Some(Variant::Color(0, 200, 255, 255))
        );

        let child_keys = reloaded.child_keys();
        assert!(child_keys.contains(&"opacity".to_string()));
        assert!(child_keys.contains(&"stay_on_top".to_string()));

        let child_groups = reloaded.child_groups();
        assert!(child_groups.contains(&"ArcDial".to_string()));

        reloaded.end_group();
    }

    // Cleanup
    let _ = fs::remove_dir_all(&tmp_dir);
}
#[test]
fn test_qsettings_round_trips_typed_and_multiline_values() {
    let path = std::env::temp_dir().join(format!(
        "ferrokit_settings_roundtrip_{}.ini",
        ObjectId::next().0
    ));
    let expected = [
        ("numeric_text", Variant::String("123".to_string())),
        (
            "multiline",
            Variant::String("first\nsecond=evil".to_string()),
        ),
        ("float", Variant::F64(1.0)),
        ("large_unsigned", Variant::U64(u64::MAX)),
    ];
    {
        let mut settings = Settings::new(&path);
        for (key, value) in &expected {
            settings.set_value(key, value.clone());
        }
        settings.sync().unwrap();
    }
    let reloaded = Settings::new(&path);
    for (key, value) in &expected {
        assert_eq!(reloaded.value(key), Some(value.clone()));
    }
    let _ = fs::remove_file(path);
}
#[test]
fn test_qsettings_refuses_to_overwrite_after_load_error() {
    let path = std::env::temp_dir().join(format!(
        "ferrokit_settings_invalid_{}.ini",
        ObjectId::next().0
    ));
    fs::write(&path, [0xff, b'=', b'x', b'\n']).unwrap();
    let mut settings = Settings::new(&path);
    settings.set_value("new_key", "new value");
    assert!(settings.sync().is_err());
    assert_eq!(fs::read(&path).unwrap(), [0xff, b'=', b'x', b'\n']);
    let _ = fs::remove_file(path);
}

// -----------------------------------------------------------------------------
// 5. Test QFileSystemWatcher File and Directory Monitoring
// -----------------------------------------------------------------------------

#[test]
fn test_filesystem_watcher_notifications() {
    let tmp_dir = std::env::temp_dir().join(format!("ferrokit_test_fs_{}", ObjectId::next().0));
    fs::create_dir_all(&tmp_dir).expect("Create test dir");

    let test_file = tmp_dir.join("config.json");
    {
        let mut f = File::create(&test_file).expect("Create test file");
        writeln!(f, "{{\"version\": 1}}").unwrap();
    }

    let mut watcher = FileSystemWatcher::new();
    assert!(watcher.add_path(&test_file));
    assert!(watcher.add_path(&tmp_dir));

    assert_eq!(watcher.files(), vec![test_file.clone()]);
    assert_eq!(watcher.directories(), vec![tmp_dir.clone()]);

    let file_changed_flag = Arc::new(AtomicBool::new(false));
    let fc_clone = Arc::clone(&file_changed_flag);
    let _fc_conn = watcher.file_changed.connect(move |_path| {
        fc_clone.store(true, Ordering::SeqCst);
    });

    let dir_changed_flag = Arc::new(AtomicBool::new(false));
    let dc_clone = Arc::clone(&dir_changed_flag);
    let _dc_conn = watcher.directory_changed.connect(move |_path| {
        dc_clone.store(true, Ordering::SeqCst);
    });

    // Initial poll: no changes
    watcher.poll_changes();
    assert!(!file_changed_flag.load(Ordering::SeqCst));
    assert!(!dir_changed_flag.load(Ordering::SeqCst));

    // Modify file
    std::thread::sleep(std::time::Duration::from_millis(15));
    {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(&test_file)
            .unwrap();
        writeln!(f, "{{\"version\": 2}}").unwrap();
        f.flush().unwrap();
    }

    // Create a new file in directory
    let new_child = tmp_dir.join("extra.txt");
    File::create(&new_child).unwrap();

    // Poll changes
    watcher.poll_changes();
    assert!(
        file_changed_flag.load(Ordering::SeqCst),
        "file_changed signal should be fired"
    );
    assert!(
        dir_changed_flag.load(Ordering::SeqCst),
        "directory_changed signal should be fired"
    );

    // Remove paths
    assert!(watcher.remove_path(&test_file));
    assert!(watcher.remove_path(&tmp_dir));
    assert!(watcher.files().is_empty());
    assert!(watcher.directories().is_empty());

    // Cleanup
    let _ = fs::remove_dir_all(&tmp_dir);
}

#[test]
fn test_animation_step_with_huge_delta_does_not_overflow() {
    let mut anim = VariantAnimation::new();
    anim.set_duration(1000);
    anim.set_loop_count(-1);
    anim.set_start_value(0.0f32);
    anim.set_end_value(1.0f32);
    anim.start();
    assert!(anim.step(1));
    // A saturating advance finishes the loop instead of overflowing the elapsed time.
    assert!(anim.step(u64::MAX));
}
