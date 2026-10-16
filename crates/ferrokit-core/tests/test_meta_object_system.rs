// SPDX-License-Identifier: LGPL-3.0-only
use ferrokit_core::meta::{
    register_meta_type, Access, MetaClassInfo, MetaEnum, MetaEnumItem, MetaMethod, MetaObject,
    MetaProperty, MetaType, MetaTypeId, MethodType,
};
use ferrokit_core::object::ObjectData;
use ferrokit_core::signal::Signal;
use ferrokit_core::variant::Variant;
use ferrokit_core::QObject;
use std::any::Any;

// -----------------------------------------------------------------------------
// Manual MetaObject setup for Deep Testing
// -----------------------------------------------------------------------------

struct CustomWidget {
    data: ObjectData,
    pub title: String,
    pub width: i32,
    #[allow(dead_code)]
    pub enabled: bool,
}

impl CustomWidget {
    fn new(title: &str, width: i32) -> Self {
        Self {
            data: ObjectData::with_auto_id(),
            title: title.to_string(),
            width,
            enabled: true,
        }
    }

    fn reset_title(&mut self) {
        self.title = "Default Title".to_string();
    }
}

// Accessors for CustomWidget properties
fn custom_widget_get_title(obj: &dyn Any) -> Variant {
    obj.downcast_ref::<CustomWidget>()
        .map(|w| Variant::from(w.title.clone()))
        .unwrap_or(Variant::Invalid)
}

fn custom_widget_set_title(
    obj: &mut dyn Any,
    val: Variant,
) -> Result<(), ferrokit_core::meta::InvokeError> {
    if let Some(w) = obj.downcast_mut::<CustomWidget>() {
        if let Some(s) = val.to_value::<String>() {
            w.title = s;
            Ok(())
        } else {
            Err(ferrokit_core::meta::InvokeError::TypeMismatch {
                index: 0,
                expected: "String",
            })
        }
    } else {
        Err(ferrokit_core::meta::InvokeError::TargetBorrowFailed)
    }
}

fn custom_widget_get_width(obj: &dyn Any) -> Variant {
    obj.downcast_ref::<CustomWidget>()
        .map(|w| Variant::from(w.width))
        .unwrap_or(Variant::Invalid)
}

fn custom_widget_set_width(
    obj: &mut dyn Any,
    val: Variant,
) -> Result<(), ferrokit_core::meta::InvokeError> {
    if let Some(w) = obj.downcast_mut::<CustomWidget>() {
        if let Some(v) = val.to_value::<i32>() {
            w.width = v;
            Ok(())
        } else {
            Err(ferrokit_core::meta::InvokeError::TypeMismatch {
                index: 0,
                expected: "i32",
            })
        }
    } else {
        Err(ferrokit_core::meta::InvokeError::TargetBorrowFailed)
    }
}

// Invokers for CustomWidget methods
fn custom_widget_invoke_reset_title(
    obj: &mut dyn Any,
    _args: &[Variant],
) -> Result<Variant, ferrokit_core::meta::InvokeError> {
    if let Some(w) = obj.downcast_mut::<CustomWidget>() {
        w.reset_title();
        Ok(Variant::Bool(true))
    } else {
        Err(ferrokit_core::meta::InvokeError::TargetBorrowFailed)
    }
}

fn custom_widget_invoke_add_width(
    obj: &mut dyn Any,
    args: &[Variant],
) -> Result<Variant, ferrokit_core::meta::InvokeError> {
    if let Some(w) = obj.downcast_mut::<CustomWidget>() {
        if let Some(delta) = args.first().and_then(|v| v.to_value::<i32>()) {
            w.width += delta;
            Ok(Variant::from(w.width))
        } else {
            Err(ferrokit_core::meta::InvokeError::TypeMismatch {
                index: 0,
                expected: "i32",
            })
        }
    } else {
        Err(ferrokit_core::meta::InvokeError::TargetBorrowFailed)
    }
}

static CUSTOM_WIDGET_PROPERTIES: &[MetaProperty] = &[
    MetaProperty::new(
        "title",
        "QString",
        true,
        true,
        true,
        false,
        Some("titleChanged"),
        Some(custom_widget_get_title),
        Some(custom_widget_set_title),
    ),
    MetaProperty::new(
        "width",
        "int",
        true,
        true,
        false,
        false,
        None,
        Some(custom_widget_get_width),
        Some(custom_widget_set_width),
    ),
];

static CUSTOM_WIDGET_METHODS: &[MetaMethod] = &[
    MetaMethod::new(
        "resetTitle",
        "resetTitle()",
        "bool",
        &[],
        &[],
        MethodType::Slot,
        Access::Public,
        Some(custom_widget_invoke_reset_title),
    ),
    MetaMethod::new(
        "addWidth",
        "addWidth(int)",
        "int",
        &["int"],
        &["delta"],
        MethodType::Method,
        Access::Public,
        Some(custom_widget_invoke_add_width),
    ),
];

static CUSTOM_WIDGET_ENUMS: &[MetaEnum] = &[MetaEnum::new(
    "Alignment",
    "CustomWidget",
    true,
    &[
        MetaEnumItem {
            key: "AlignLeft",
            value: 0x01,
        },
        MetaEnumItem {
            key: "AlignRight",
            value: 0x02,
        },
        MetaEnumItem {
            key: "AlignCenter",
            value: 0x04,
        },
        MetaEnumItem {
            key: "AlignTop",
            value: 0x10,
        },
        MetaEnumItem {
            key: "AlignBottom",
            value: 0x20,
        },
    ],
)];

static CUSTOM_WIDGET_CLASS_INFOS: &[MetaClassInfo] = &[
    MetaClassInfo::new("Author", "FerroKit Engine Team"),
    MetaClassInfo::new("Version", "1.0.0"),
];

static CUSTOM_WIDGET_META_OBJECT: MetaObject = MetaObject::new(
    "CustomWidget",
    Some(&ferrokit_core::meta::QOBJECT_META_OBJECT),
    CUSTOM_WIDGET_METHODS,
    CUSTOM_WIDGET_PROPERTIES,
    CUSTOM_WIDGET_ENUMS,
    CUSTOM_WIDGET_CLASS_INFOS,
);

impl QObject for CustomWidget {
    fn object_data(&self) -> &ObjectData {
        &self.data
    }

    fn object_data_mut(&mut self) -> ferrokit_core::object::ObjectDataMut<'_> {
        ferrokit_core::object::ObjectDataMut::new(&mut self.data)
    }

    fn as_qobject_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn meta_object(&self) -> &'static MetaObject {
        &CUSTOM_WIDGET_META_OBJECT
    }
}

// -----------------------------------------------------------------------------
// Derive Macro Test Object
// -----------------------------------------------------------------------------

#[derive(QObject)]
#[qobject(class_name = "DerivedPanel")]
struct DerivedPanel {
    data: ObjectData,
    #[property]
    pub caption: String,
    #[property]
    pub opacity: f64,
    #[property(readonly)]
    pub version_code: i32,
    #[signal]
    #[allow(dead_code)]
    pub changed: Signal<String>,
}

impl DerivedPanel {
    fn new(caption: &str, opacity: f64, version_code: i32) -> Self {
        Self {
            data: ObjectData::with_auto_id(),
            caption: caption.to_string(),
            opacity,
            version_code,
            changed: Signal::new(),
        }
    }
}

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[test]
fn test_meta_type_system_resolution_and_registration() {
    // 1. Primitive and standard Qt types resolution
    let t_int = MetaType::from_type::<i32>();
    assert_eq!(t_int.id(), MetaTypeId::INT);
    assert_eq!(t_int.name(), "int");
    assert_eq!(t_int.size_of(), 4);

    let t_string = MetaType::from_type::<String>();
    assert_eq!(t_string.id(), MetaTypeId::QSTRING);
    assert_eq!(t_string.name(), "QString");

    let t_double = MetaType::from_name("double").expect("double exists");
    assert_eq!(t_double.id(), MetaTypeId::DOUBLE);

    // 2. Custom User Type Registration
    struct CustomUserData {
        _val: u64,
    }
    let user_id = register_meta_type::<CustomUserData>("CustomUserData");
    assert!(user_id.0 >= MetaTypeId::USER.0);

    let resolved_user = MetaType::from_name("CustomUserData").expect("registered user type exists");
    assert_eq!(resolved_user.id(), user_id);
    assert_eq!(resolved_user.name(), "CustomUserData");
}

#[test]
fn test_meta_object_hierarchy_and_inherits() {
    let widget = CustomWidget::new("Inspector", 800);

    // 1. Class name and inheritance reflection
    assert_eq!(widget.meta_object().class_name(), "CustomWidget");
    assert!(widget.inherits("CustomWidget"));
    assert!(widget.inherits("QObject"));
    assert!(!widget.inherits("QWindow"));

    let sc = widget.meta_object().super_class().expect("has superclass");
    assert_eq!(sc.class_name(), "QObject");
    assert!(sc.super_class().is_none());

    // 2. ClassInfo metadata
    assert_eq!(widget.meta_object().class_info_count(), 2);
    let author_idx = widget.meta_object().index_of_class_info("Author").unwrap();
    assert_eq!(
        widget.meta_object().class_info(author_idx).unwrap().value,
        "FerroKit Engine Team"
    );
}

#[test]
fn test_meta_property_introspection_and_read_write() {
    let mut widget = CustomWidget::new("Dashboard", 1024);

    // 1. Inspection
    let mo = widget.meta_object();
    assert_eq!(mo.property_count(), 2);

    let title_idx = mo
        .index_of_property("title")
        .expect("title property exists");
    let prop_title = mo.property(title_idx).unwrap();
    assert_eq!(prop_title.name(), "title");
    assert_eq!(prop_title.type_name(), "QString");
    assert!(prop_title.is_readable());
    assert!(prop_title.is_writable());
    assert_eq!(prop_title.notify_signal_name(), Some("titleChanged"));

    // 2. Reading via QObject::property
    let title_val = widget.property("title").expect("property read");
    assert_eq!(title_val, Variant::String("Dashboard".into()));

    // 3. Writing via QObject::setProperty
    let ok = widget.set_property("title", Variant::String("New Dashboard".into()));
    assert!(ok);
    assert_eq!(widget.title, "New Dashboard");

    // 4. Width integer property
    let ok_w = widget.set_property("width", Variant::I64(1280));
    assert!(ok_w);
    assert_eq!(widget.width, 1280);

    // 5. Fallback to dynamic property when meta property doesn't exist
    assert_eq!(widget.property("dynamic_foo"), None);
    widget.set_property("dynamic_foo", Variant::I64(42));
    assert_eq!(widget.property("dynamic_foo"), Some(Variant::I64(42)));
}

#[test]
fn test_meta_method_introspection_and_invoke() {
    let mut widget = CustomWidget::new("Toolbox", 500);

    let mo = widget.meta_object();
    assert_eq!(mo.method_count(), 2);

    let reset_idx = mo
        .index_of_method("resetTitle()")
        .expect("resetTitle exists");
    let method = mo.method(reset_idx).unwrap();
    assert_eq!(method.method_type(), MethodType::Slot);
    assert_eq!(method.access(), Access::Public);

    // 1. Invoking parameterless method
    let res = widget
        .invoke_method("resetTitle()", &[])
        .expect("invoke success");
    assert_eq!(res, Variant::Bool(true));
    assert_eq!(widget.title, "Default Title");

    // 2. Invoking method with arguments
    let res2 = widget
        .invoke_method("addWidth(int)", &[Variant::I64(150)])
        .expect("invoke success");
    assert_eq!(res2, Variant::I64(650));
    assert_eq!(widget.width, 650);

    // 3. Method not found error
    let err = widget.invoke_method("nonExistent()", &[]);
    assert_eq!(err, Err(ferrokit_core::meta::InvokeError::MethodNotFound));
}

#[test]
fn test_meta_enum_key_value_and_flags_conversions() {
    let widget = CustomWidget::new("EnumTester", 100);
    let mo = widget.meta_object();

    let enum_idx = mo
        .index_of_enumerator("Alignment")
        .expect("Alignment exists");
    let menum = mo.enumerator(enum_idx).unwrap();

    assert_eq!(menum.name(), "Alignment");
    assert_eq!(menum.scope(), "CustomWidget");
    assert!(menum.is_flag());
    assert_eq!(menum.key_count(), 5);

    // 1. Key to value & Value to key
    assert_eq!(menum.key_to_value("AlignRight"), Some(0x02));
    assert_eq!(menum.value_to_key(0x04), Some("AlignCenter"));

    // 2. Bitwise flag keys combination
    let flag_val = menum
        .keys_to_value("AlignLeft | AlignTop")
        .expect("valid flags");
    assert_eq!(flag_val, 0x11);
}

#[test]
fn test_proc_macro_derive_qobject_and_property_reflection() {
    let mut panel = DerivedPanel::new("Control Panel", 0.85, 42);

    // 1. MetaObject reflection generated automatically
    let mo = panel.meta_object();
    assert_eq!(mo.class_name(), "DerivedPanel");
    assert!(panel.inherits("DerivedPanel"));
    assert!(panel.inherits("QObject"));

    let signal_index = mo
        .index_of_signal("changed(String)")
        .expect("signal metadata exists");
    let signal = mo.method(signal_index).unwrap();
    assert_eq!(signal.method_type(), MethodType::Signal);
    assert_eq!(signal.parameter_types(), &["String"]);
    assert!(signal.is_invokable());

    // 2. Generated properties inspection
    assert_eq!(mo.property_count(), 3);

    let caption_idx = mo.index_of_property("caption").expect("caption exists");
    let prop_caption = mo.property(caption_idx).unwrap();
    assert!(prop_caption.is_writable());

    let version_idx = mo
        .index_of_property("version_code")
        .expect("version_code exists");
    let prop_version = mo.property(version_idx).unwrap();
    assert!(
        !prop_version.is_writable(),
        "version_code was marked #[property(readonly)]"
    );

    // 3. Read property via QObject::property
    assert_eq!(
        panel.property("caption"),
        Some(Variant::String("Control Panel".into()))
    );
    assert_eq!(panel.property("opacity"), Some(Variant::F64(0.85)));
    assert_eq!(panel.property("version_code"), Some(Variant::I64(42)));

    // 4. Modify writable property via QObject::setProperty
    let ok = panel.set_property("caption", Variant::String("System Monitor".into()));
    assert!(ok);
    assert_eq!(panel.caption, "System Monitor");

    let ok_op = panel.set_property("opacity", Variant::F64(0.99));
    assert!(ok_op);
    assert_eq!(panel.opacity, 0.99);

    // 5. Read-only property rejects modification
    let ok_ro = panel.set_property("version_code", Variant::I64(99));
    assert!(!ok_ro, "Read-only property write must fail");
    assert_eq!(panel.version_code, 42);
}
