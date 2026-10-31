// SPDX-License-Identifier: LGPL-3.0-only
//! Meta Object System (`QMetaObject` & `QMetaType` equivalent).
//!
//! Provides compile-time metadata, runtime type reflection, dynamic method invocation,
//! typed properties inspection, enumerators reflection, and class hierarchy inspection.

use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use crate::variant::Variant;

// -----------------------------------------------------------------------------
// MetaType: QMetaType equivalent
// -----------------------------------------------------------------------------

/// Identifies a registered meta-type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MetaTypeId(pub u32);

impl MetaTypeId {
    pub const UNKNOWN: Self = Self(0);
    pub const BOOL: Self = Self(1);
    pub const INT: Self = Self(2);
    pub const UINT: Self = Self(3);
    pub const LONG_LONG: Self = Self(4);
    pub const ULONG_LONG: Self = Self(5);
    pub const DOUBLE: Self = Self(6);
    pub const QVARIANT_MAP: Self = Self(8);
    pub const QVARIANT_LIST: Self = Self(9);
    pub const QSTRING: Self = Self(10);
    pub const QBYTE_ARRAY: Self = Self(12);
    pub const QDATE: Self = Self(14);
    pub const QTIME: Self = Self(15);
    pub const QDATETIME: Self = Self(16);
    pub const QURL: Self = Self(17);
    pub const QLOCALE: Self = Self(18);
    pub const QRECT: Self = Self(19);
    pub const QRECT_F: Self = Self(20);
    pub const QSIZE: Self = Self(21);
    pub const QSIZE_F: Self = Self(22);
    pub const QLINE: Self = Self(23);
    pub const QLINE_F: Self = Self(24);
    pub const QPOINT: Self = Self(25);
    pub const QPOINT_F: Self = Self(26);
    pub const QVARIANT_HASH: Self = Self(28);
    pub const QEASING_CURVE: Self = Self(29);
    pub const QUUID: Self = Self(30);
    pub const FLOAT: Self = Self(38);
    pub const QOBJECT_STAR: Self = Self(39);
    pub const QVARIANT: Self = Self(41);
    pub const VOID: Self = Self(43);
    pub const QREGULAR_EXPRESSION: Self = Self(44);
    pub const QFONT: Self = Self(64);
    pub const QPIXMAP: Self = Self(65);
    pub const QBRUSH: Self = Self(66);
    pub const QCOLOR: Self = Self(67);
    pub const QICON: Self = Self(69);
    pub const QIMAGE: Self = Self(70);
    pub const QPEN: Self = Self(76);
    pub const QTRANSFORM: Self = Self(80);
    pub const QMARGINS: Self = Self(88);
    pub const QMARGINS_F: Self = Self(89);
    pub const USER: Self = Self(65536);
}

/// Metadata description of a type in the meta object system (`QMetaType`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaType {
    id: MetaTypeId,
    name: &'static str,
    size: usize,
    type_id: Option<TypeId>,
}

impl MetaType {
    pub const fn new(id: MetaTypeId, name: &'static str, size: usize) -> Self {
        Self {
            id,
            name,
            size,
            type_id: None,
        }
    }

    pub const fn with_type_id(
        id: MetaTypeId,
        name: &'static str,
        size: usize,
        type_id: TypeId,
    ) -> Self {
        Self {
            id,
            name,
            size,
            type_id: Some(type_id),
        }
    }

    pub fn id(&self) -> MetaTypeId {
        self.id
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn size_of(&self) -> usize {
        self.size
    }

    pub fn type_id(&self) -> Option<TypeId> {
        self.type_id
    }

    pub fn is_valid(&self) -> bool {
        self.id != MetaTypeId::UNKNOWN
    }

    /// Resolves `MetaType` from a compile-time static type `T`.
    pub fn from_type<T: 'static>() -> Self {
        let tid = TypeId::of::<T>();
        if let Some(mt) = lookup_metatype_by_type_id(tid) {
            return mt;
        }

        // Built-in standard types lookup
        if tid == TypeId::of::<()>() {
            Self::new(MetaTypeId::VOID, "void", 0)
        } else if tid == TypeId::of::<bool>() {
            Self::new(MetaTypeId::BOOL, "bool", std::mem::size_of::<bool>())
        } else if tid == TypeId::of::<i32>() {
            Self::new(MetaTypeId::INT, "int", std::mem::size_of::<i32>())
        } else if tid == TypeId::of::<u32>() {
            Self::new(MetaTypeId::UINT, "uint", std::mem::size_of::<u32>())
        } else if tid == TypeId::of::<i64>() {
            Self::new(
                MetaTypeId::LONG_LONG,
                "qlonglong",
                std::mem::size_of::<i64>(),
            )
        } else if tid == TypeId::of::<u64>() {
            Self::new(
                MetaTypeId::ULONG_LONG,
                "qulonglong",
                std::mem::size_of::<u64>(),
            )
        } else if tid == TypeId::of::<f64>() {
            Self::new(MetaTypeId::DOUBLE, "double", std::mem::size_of::<f64>())
        } else if tid == TypeId::of::<f32>() {
            Self::new(MetaTypeId(38), "float", std::mem::size_of::<f32>())
        } else if tid == TypeId::of::<String>() {
            Self::new(
                MetaTypeId::QSTRING,
                "QString",
                std::mem::size_of::<String>(),
            )
        } else if tid == TypeId::of::<Vec<u8>>() {
            Self::new(
                MetaTypeId::QBYTE_ARRAY,
                "QByteArray",
                std::mem::size_of::<Vec<u8>>(),
            )
        } else if tid == TypeId::of::<Variant>() {
            Self::new(
                MetaTypeId::QVARIANT,
                "QVariant",
                std::mem::size_of::<Variant>(),
            )
        } else {
            Self::new(
                MetaTypeId::UNKNOWN,
                std::any::type_name::<T>(),
                std::mem::size_of::<T>(),
            )
        }
    }

    /// Resolves `MetaType` from a registered `TypeId`.
    pub fn from_type_id(tid: TypeId) -> Option<Self> {
        lookup_metatype_by_type_id(tid)
    }

    /// Resolves `MetaType` from a type name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "void" => Some(Self::new(MetaTypeId::VOID, "void", 0)),
            "bool" => Some(Self::new(
                MetaTypeId::BOOL,
                "bool",
                std::mem::size_of::<bool>(),
            )),
            "int" => Some(Self::new(
                MetaTypeId::INT,
                "int",
                std::mem::size_of::<i32>(),
            )),
            "uint" => Some(Self::new(
                MetaTypeId::UINT,
                "uint",
                std::mem::size_of::<u32>(),
            )),
            "qlonglong" | "i64" => Some(Self::new(
                MetaTypeId::LONG_LONG,
                "qlonglong",
                std::mem::size_of::<i64>(),
            )),
            "qulonglong" | "u64" => Some(Self::new(
                MetaTypeId::ULONG_LONG,
                "qulonglong",
                std::mem::size_of::<u64>(),
            )),
            "double" | "f64" => Some(Self::new(
                MetaTypeId::DOUBLE,
                "double",
                std::mem::size_of::<f64>(),
            )),
            "float" | "f32" => Some(Self::new(
                MetaTypeId::FLOAT,
                "float",
                std::mem::size_of::<f32>(),
            )),
            "QString" | "String" => Some(Self::new(
                MetaTypeId::QSTRING,
                "QString",
                std::mem::size_of::<String>(),
            )),
            "QByteArray" => Some(Self::new(
                MetaTypeId::QBYTE_ARRAY,
                "QByteArray",
                std::mem::size_of::<Vec<u8>>(),
            )),
            "QDate" => Some(Self::new(MetaTypeId::QDATE, "QDate", 12)),
            "QTime" => Some(Self::new(MetaTypeId::QTIME, "QTime", 16)),
            "QDateTime" => Some(Self::new(MetaTypeId::QDATETIME, "QDateTime", 8)),
            "QUrl" => Some(Self::new(
                MetaTypeId::QURL,
                "QUrl",
                std::mem::size_of::<String>(),
            )),
            "QUuid" => Some(Self::new(MetaTypeId::QUUID, "QUuid", 16)),
            "QRegularExpression" => Some(Self::new(
                MetaTypeId::QREGULAR_EXPRESSION,
                "QRegularExpression",
                std::mem::size_of::<String>(),
            )),
            "QLocale" => Some(Self::new(
                MetaTypeId::QLOCALE,
                "QLocale",
                std::mem::size_of::<String>(),
            )),
            "QPoint" => Some(Self::new(MetaTypeId::QPOINT, "QPoint", 8)),
            "QPointF" => Some(Self::new(MetaTypeId::QPOINT_F, "QPointF", 8)),
            "QSize" => Some(Self::new(MetaTypeId::QSIZE, "QSize", 8)),
            "QSizeF" => Some(Self::new(MetaTypeId::QSIZE_F, "QSizeF", 8)),
            "QRect" => Some(Self::new(MetaTypeId::QRECT, "QRect", 16)),
            "QRectF" => Some(Self::new(MetaTypeId::QRECT_F, "QRectF", 16)),
            "QLine" => Some(Self::new(MetaTypeId::QLINE, "QLine", 16)),
            "QLineF" => Some(Self::new(MetaTypeId::QLINE_F, "QLineF", 16)),
            "QMargins" => Some(Self::new(MetaTypeId::QMARGINS, "QMargins", 16)),
            "QMarginsF" => Some(Self::new(MetaTypeId::QMARGINS_F, "QMarginsF", 16)),
            "QColor" => Some(Self::new(MetaTypeId::QCOLOR, "QColor", 4)),
            "QFont" => Some(Self::new(MetaTypeId::QFONT, "QFont", 0)),
            "QBrush" => Some(Self::new(MetaTypeId::QBRUSH, "QBrush", 0)),
            "QPen" => Some(Self::new(MetaTypeId::QPEN, "QPen", 0)),
            "QPixmap" => Some(Self::new(MetaTypeId::QPIXMAP, "QPixmap", 0)),
            "QImage" => Some(Self::new(MetaTypeId::QIMAGE, "QImage", 0)),
            "QIcon" => Some(Self::new(MetaTypeId::QICON, "QIcon", 0)),
            "QTransform" => Some(Self::new(MetaTypeId::QTRANSFORM, "QTransform", 0)),
            "QVariant" => Some(Self::new(
                MetaTypeId::QVARIANT,
                "QVariant",
                std::mem::size_of::<Variant>(),
            )),
            "QVariantList" => Some(Self::new(
                MetaTypeId::QVARIANT_LIST,
                "QVariantList",
                std::mem::size_of::<Vec<Variant>>(),
            )),
            "QVariantMap" => Some(Self::new(MetaTypeId::QVARIANT_MAP, "QVariantMap", 0)),
            "QVariantHash" => Some(Self::new(MetaTypeId::QVARIANT_HASH, "QVariantHash", 0)),
            "QObject*" | "QObject" => Some(Self::new(
                MetaTypeId::QOBJECT_STAR,
                "QObject*",
                std::mem::size_of::<crate::object::ObjectId>(),
            )),
            _ => lookup_metatype_by_name(name),
        }
    }
}

static USER_METATYPE_REGISTRY: LazyLock<RwLock<HashMap<TypeId, MetaType>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));
static USER_METATYPE_NAME_REGISTRY: LazyLock<RwLock<HashMap<&'static str, MetaType>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn lookup_metatype_by_type_id(tid: TypeId) -> Option<MetaType> {
    USER_METATYPE_REGISTRY.read().ok()?.get(&tid).cloned()
}

fn lookup_metatype_by_name(name: &str) -> Option<MetaType> {
    USER_METATYPE_NAME_REGISTRY.read().ok()?.get(name).cloned()
}

/// Registers a custom user type into the global meta type system (`qRegisterMetaType<T>()`).
pub fn register_meta_type<T: 'static>(name: &'static str) -> MetaTypeId {
    static NEXT_USER_ID: std::sync::atomic::AtomicU32 =
        std::sync::atomic::AtomicU32::new(MetaTypeId::USER.0);
    let id = MetaTypeId(NEXT_USER_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    let mt = MetaType::with_type_id(id, name, std::mem::size_of::<T>(), TypeId::of::<T>());

    if let Ok(mut reg) = USER_METATYPE_REGISTRY.write() {
        reg.insert(TypeId::of::<T>(), mt.clone());
    }
    if let Ok(mut reg) = USER_METATYPE_NAME_REGISTRY.write() {
        reg.insert(name, mt);
    }
    id
}

// -----------------------------------------------------------------------------
// MetaMethod: QMetaMethod equivalent
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodType {
    Method,
    Signal,
    Slot,
    Constructor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Public,
    Protected,
    Private,
}

/// Function pointer type for invoking a method dynamically on a target object.
pub type MethodInvoker = fn(&mut dyn std::any::Any, &[Variant]) -> Result<Variant, InvokeError>;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvokeError {
    MethodNotFound,
    ParameterCountMismatch {
        expected: usize,
        actual: usize,
    },
    TypeMismatch {
        index: usize,
        expected: &'static str,
    },
    TargetBorrowFailed,
    ExecutionFailed(String),
}

/// Metadata description of a method (`QMetaMethod`).
#[derive(Clone)]
pub struct MetaMethod {
    name: &'static str,
    signature: &'static str,
    return_type: &'static str,
    parameter_types: &'static [&'static str],
    parameter_names: &'static [&'static str],
    method_type: MethodType,
    access: Access,
    invoker: Option<MethodInvoker>,
}

impl std::fmt::Debug for MetaMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaMethod")
            .field("name", &self.name)
            .field("signature", &self.signature)
            .field("return_type", &self.return_type)
            .field("parameter_types", &self.parameter_types)
            .field("parameter_names", &self.parameter_names)
            .field("method_type", &self.method_type)
            .field("access", &self.access)
            .finish()
    }
}

impl MetaMethod {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        name: &'static str,
        signature: &'static str,
        return_type: &'static str,
        parameter_types: &'static [&'static str],
        parameter_names: &'static [&'static str],
        method_type: MethodType,
        access: Access,
        invoker: Option<MethodInvoker>,
    ) -> Self {
        Self {
            name,
            signature,
            return_type,
            parameter_types,
            parameter_names,
            method_type,
            access,
            invoker,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn method_signature(&self) -> &'static str {
        self.signature
    }

    pub fn return_type(&self) -> &'static str {
        self.return_type
    }

    pub fn parameter_count(&self) -> usize {
        self.parameter_types.len()
    }

    pub fn parameter_types(&self) -> &'static [&'static str] {
        self.parameter_types
    }

    pub fn parameter_names(&self) -> &'static [&'static str] {
        self.parameter_names
    }

    pub fn method_type(&self) -> MethodType {
        self.method_type
    }

    pub fn access(&self) -> Access {
        self.access
    }

    pub fn is_invokable(&self) -> bool {
        self.invoker.is_some()
    }

    /// Invokes this method on the given target object with arguments (`QMetaMethod::invoke`).
    pub fn invoke(
        &self,
        target: &mut dyn std::any::Any,
        args: &[Variant],
    ) -> Result<Variant, InvokeError> {
        let invoker = self.invoker.ok_or(InvokeError::MethodNotFound)?;
        self.check_arguments(args)?;
        invoker(target, args)
    }

    /// Checks argument count, then each argument against its declared parameter type, before
    /// invocation (`QMetaMethod::invoke` → `invokeImpl`, qmetaobject.cpp:2770-2809).
    pub fn check_arguments(&self, args: &[Variant]) -> Result<(), InvokeError> {
        if args.len() != self.parameter_types.len() {
            return Err(InvokeError::ParameterCountMismatch {
                expected: self.parameter_types.len(),
                actual: args.len(),
            });
        }
        for (index, (arg, param)) in args.iter().zip(self.parameter_types.iter()).enumerate() {
            if !variant_accepts_parameter(arg, param) {
                return Err(InvokeError::TypeMismatch {
                    index,
                    expected: param,
                });
            }
        }
        Ok(())
    }
}

/// Whether `arg` is accepted for a parameter declared as `param`.
///
/// Mirrors Qt `invokeImpl`'s per-argument compatibility check (qmetaobject.cpp:2774-2809): an
/// exact metatype match or a Qt-legal numeric widening. Strings are never parsed as numbers.
/// Parameter type names outside this table are not checked (the metatype table is incomplete).
fn variant_accepts_parameter(arg: &Variant, param: &str) -> bool {
    match param {
        "QVariant" | "Variant" => true,
        "bool" => matches!(arg, Variant::Bool(_)),
        "int" | "i32" => match arg {
            Variant::I64(v) => i32::try_from(*v).is_ok(),
            Variant::U64(v) => i32::try_from(*v).is_ok(),
            _ => false,
        },
        "uint" | "u32" => match arg {
            Variant::I64(v) => u32::try_from(*v).is_ok(),
            Variant::U64(v) => u32::try_from(*v).is_ok(),
            _ => false,
        },
        "qlonglong" | "i64" => match arg {
            Variant::I64(_) => true,
            Variant::U64(v) => i64::try_from(*v).is_ok(),
            _ => false,
        },
        "qulonglong" | "u64" => match arg {
            Variant::U64(_) => true,
            Variant::I64(v) => *v >= 0,
            _ => false,
        },
        "double" | "float" | "f64" | "f32" => {
            matches!(arg, Variant::F64(_) | Variant::I64(_) | Variant::U64(_))
        }
        "QString" | "String" => matches!(arg, Variant::String(_)),
        "QByteArray" | "Vec<u8>" => matches!(arg, Variant::ByteArray(_)),
        _ => true,
    }
}

// -----------------------------------------------------------------------------
// MetaProperty: QMetaProperty equivalent
// -----------------------------------------------------------------------------

pub type PropertyGetter = fn(&dyn std::any::Any) -> Variant;
pub type PropertySetter = fn(&mut dyn std::any::Any, Variant) -> Result<(), InvokeError>;

/// Metadata description of a property (`QMetaProperty`).
#[derive(Clone)]
pub struct MetaProperty {
    name: &'static str,
    type_name: &'static str,
    is_readable: bool,
    is_writable: bool,
    is_resettable: bool,
    is_constant: bool,
    notify_signal: Option<&'static str>,
    getter: Option<PropertyGetter>,
    setter: Option<PropertySetter>,
}

impl std::fmt::Debug for MetaProperty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetaProperty")
            .field("name", &self.name)
            .field("type_name", &self.type_name)
            .field("is_readable", &self.is_readable)
            .field("is_writable", &self.is_writable)
            .field("is_constant", &self.is_constant)
            .field("notify_signal", &self.notify_signal)
            .finish()
    }
}

impl MetaProperty {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        name: &'static str,
        type_name: &'static str,
        is_readable: bool,
        is_writable: bool,
        is_resettable: bool,
        is_constant: bool,
        notify_signal: Option<&'static str>,
        getter: Option<PropertyGetter>,
        setter: Option<PropertySetter>,
    ) -> Self {
        Self {
            name,
            type_name,
            is_readable,
            is_writable,
            is_resettable,
            is_constant,
            notify_signal,
            getter,
            setter,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    pub fn is_readable(&self) -> bool {
        self.is_readable
    }

    pub fn is_writable(&self) -> bool {
        self.is_writable
    }

    pub fn is_constant(&self) -> bool {
        self.is_constant
    }

    pub fn is_resettable(&self) -> bool {
        self.is_resettable
    }

    pub fn notify_signal_name(&self) -> Option<&'static str> {
        self.notify_signal
    }

    pub fn read(&self, object: &dyn std::any::Any) -> Option<Variant> {
        self.getter.map(|g| g(object))
    }

    pub fn write(&self, object: &mut dyn std::any::Any, val: Variant) -> Result<(), InvokeError> {
        if !self.is_writable {
            return Err(InvokeError::ExecutionFailed(format!(
                "Property '{}' is read-only",
                self.name
            )));
        }
        let setter = self.setter.ok_or(InvokeError::MethodNotFound)?;
        setter(object, val)
    }
}

// -----------------------------------------------------------------------------
// MetaEnum: QMetaEnum equivalent
// -----------------------------------------------------------------------------

/// Single key-value entry in an enum (`QMetaEnum::key(i)` / `value(i)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaEnumItem {
    pub key: &'static str,
    pub value: i64,
}

/// Metadata description of an enumeration (`QMetaEnum`).
#[derive(Debug, Clone)]
pub struct MetaEnum {
    name: &'static str,
    scope: &'static str,
    is_flag: bool,
    items: &'static [MetaEnumItem],
}

impl MetaEnum {
    pub const fn new(
        name: &'static str,
        scope: &'static str,
        is_flag: bool,
        items: &'static [MetaEnumItem],
    ) -> Self {
        Self {
            name,
            scope,
            is_flag,
            items,
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    pub fn scope(&self) -> &'static str {
        self.scope
    }

    pub fn is_flag(&self) -> bool {
        self.is_flag
    }

    pub fn key_count(&self) -> usize {
        self.items.len()
    }

    pub fn key(&self, index: usize) -> Option<&'static str> {
        self.items.get(index).map(|item| item.key)
    }

    pub fn value(&self, index: usize) -> Option<i64> {
        self.items.get(index).map(|item| item.value)
    }

    /// Converts key name to value (`QMetaEnum::keyToValue`).
    pub fn key_to_value(&self, key: &str) -> Option<i64> {
        for item in self.items {
            if item.key == key {
                return Some(item.value);
            }
        }
        None
    }

    /// Converts value to key name (`QMetaEnum::valueToKey`).
    pub fn value_to_key(&self, value: i64) -> Option<&'static str> {
        for item in self.items {
            if item.value == value {
                return Some(item.key);
            }
        }
        None
    }

    /// Converts bitwise keys combination like "Read | Write" to integer flags value (`QMetaEnum::keysToValue`).
    pub fn keys_to_value(&self, keys: &str) -> Option<i64> {
        let mut result = 0i64;
        for part in keys.split('|') {
            let trimmed = part.trim();
            if trimmed.is_empty() {
                continue;
            }
            {
                let val = self.key_to_value(trimmed)?;
                result |= val;
            }
        }
        Some(result)
    }
}

// -----------------------------------------------------------------------------
// MetaClassInfo: QMetaClassInfo equivalent
// -----------------------------------------------------------------------------

/// Metadata tag associated with a class via `Q_CLASSINFO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetaClassInfo {
    pub name: &'static str,
    pub value: &'static str,
}

impl MetaClassInfo {
    pub const fn new(name: &'static str, value: &'static str) -> Self {
        Self { name, value }
    }
}

// -----------------------------------------------------------------------------
// MetaObject: QMetaObject equivalent
// -----------------------------------------------------------------------------

/// The root reflection structure representing all compile-time metadata of a QObject class (`QMetaObject`).
#[derive(Debug, Clone)]
pub struct MetaObject {
    pub class_name: &'static str,
    pub super_class: Option<&'static MetaObject>,
    pub methods: &'static [MetaMethod],
    pub properties: &'static [MetaProperty],
    pub enums: &'static [MetaEnum],
    pub class_infos: &'static [MetaClassInfo],
}

impl MetaObject {
    pub const fn new(
        class_name: &'static str,
        super_class: Option<&'static MetaObject>,
        methods: &'static [MetaMethod],
        properties: &'static [MetaProperty],
        enums: &'static [MetaEnum],
        class_infos: &'static [MetaClassInfo],
    ) -> Self {
        Self {
            class_name,
            super_class,
            methods,
            properties,
            enums,
            class_infos,
        }
    }

    /// Returns class name (`QMetaObject::className`).
    pub fn class_name(&self) -> &'static str {
        self.class_name
    }

    /// Returns superclass meta object (`QMetaObject::superClass`).
    pub fn super_class(&self) -> Option<&'static MetaObject> {
        self.super_class
    }

    /// Checks if this meta object inherits from another meta object (`QMetaObject::inherits`).
    pub fn inherits(&self, other: &MetaObject) -> bool {
        if std::ptr::eq(self, other) || self.class_name == other.class_name {
            return true;
        }
        let mut cur = self.super_class;
        while let Some(sc) = cur {
            if std::ptr::eq(sc, other) || sc.class_name == other.class_name {
                return true;
            }
            cur = sc.super_class;
        }
        false
    }

    /// Checks if this meta object inherits a class by name (`QObject::inherits`).
    pub fn inherits_name(&self, name: &str) -> bool {
        if self.class_name == name {
            return true;
        }
        let mut cur = self.super_class;
        while let Some(sc) = cur {
            if sc.class_name == name {
                return true;
            }
            cur = sc.super_class;
        }
        false
    }

    // --- Method inspection ---

    pub fn method_count(&self) -> usize {
        let parent_count = self.super_class.map_or(0, |s| s.method_count());
        parent_count + self.methods.len()
    }

    pub fn method_offset(&self) -> usize {
        self.super_class.map_or(0, |s| s.method_count())
    }

    pub fn method(&self, index: usize) -> Option<&MetaMethod> {
        let offset = self.method_offset();
        if index < offset {
            self.super_class?.method(index)
        } else {
            self.methods.get(index - offset)
        }
    }

    pub fn index_of_method(&self, signature: &str) -> Option<usize> {
        // Search local methods first
        let offset = self.method_offset();
        for (i, m) in self.methods.iter().enumerate() {
            if m.signature == signature || m.name == signature {
                return Some(offset + i);
            }
        }
        self.super_class?.index_of_method(signature)
    }

    /// Name-only lookup for `invokeMethod(obj, name, args)`. Scans methods in declaration order and
    /// takes the first invokable overload whose parameter types match (`invokeMethodImpl`,
    /// qmetaobject.cpp:1693-1708; a parameter mismatch moves on, qmetaobject_p.h:98-102).
    fn index_of_compatible_method(&self, name: &str, args: &[Variant]) -> Option<usize> {
        let offset = self.method_offset();
        for (i, m) in self.methods.iter().enumerate() {
            if m.name == name && m.is_invokable() && m.check_arguments(args).is_ok() {
                return Some(offset + i);
            }
        }
        self.super_class?.index_of_compatible_method(name, args)
    }

    pub fn index_of_signal(&self, signal: &str) -> Option<usize> {
        let offset = self.method_offset();
        for (i, m) in self.methods.iter().enumerate() {
            if m.method_type == MethodType::Signal && (m.signature == signal || m.name == signal) {
                return Some(offset + i);
            }
        }
        self.super_class?.index_of_signal(signal)
    }

    pub fn index_of_slot(&self, slot: &str) -> Option<usize> {
        let offset = self.method_offset();
        for (i, m) in self.methods.iter().enumerate() {
            if m.method_type == MethodType::Slot && (m.signature == slot || m.name == slot) {
                return Some(offset + i);
            }
        }
        self.super_class?.index_of_slot(slot)
    }

    // --- Property inspection ---

    pub fn property_count(&self) -> usize {
        let parent_count = self.super_class.map_or(0, |s| s.property_count());
        parent_count + self.properties.len()
    }

    pub fn property_offset(&self) -> usize {
        self.super_class.map_or(0, |s| s.property_count())
    }

    pub fn property(&self, index: usize) -> Option<&MetaProperty> {
        let offset = self.property_offset();
        if index < offset {
            self.super_class?.property(index)
        } else {
            self.properties.get(index - offset)
        }
    }

    pub fn index_of_property(&self, name: &str) -> Option<usize> {
        let offset = self.property_offset();
        for (i, p) in self.properties.iter().enumerate() {
            if p.name == name {
                return Some(offset + i);
            }
        }
        self.super_class?.index_of_property(name)
    }

    // --- Enumerator inspection ---

    pub fn enumerator_count(&self) -> usize {
        let parent_count = self.super_class.map_or(0, |s| s.enumerator_count());
        parent_count + self.enums.len()
    }

    pub fn enumerator(&self, index: usize) -> Option<&MetaEnum> {
        let offset = self.super_class.map_or(0, |s| s.enumerator_count());
        if index < offset {
            self.super_class?.enumerator(index)
        } else {
            self.enums.get(index - offset)
        }
    }

    pub fn index_of_enumerator(&self, name: &str) -> Option<usize> {
        let offset = self.super_class.map_or(0, |s| s.enumerator_count());
        for (i, e) in self.enums.iter().enumerate() {
            if e.name == name {
                return Some(offset + i);
            }
        }
        self.super_class?.index_of_enumerator(name)
    }

    // --- ClassInfo inspection ---

    pub fn class_info_count(&self) -> usize {
        let parent_count = self.super_class.map_or(0, |s| s.class_info_count());
        parent_count + self.class_infos.len()
    }

    pub fn class_info(&self, index: usize) -> Option<&MetaClassInfo> {
        let offset = self.super_class.map_or(0, |s| s.class_info_count());
        if index < offset {
            self.super_class?.class_info(index)
        } else {
            self.class_infos.get(index - offset)
        }
    }

    pub fn index_of_class_info(&self, name: &str) -> Option<usize> {
        let offset = self.super_class.map_or(0, |s| s.class_info_count());
        for (i, info) in self.class_infos.iter().enumerate() {
            if info.name == name {
                return Some(offset + i);
            }
        }
        self.super_class?.index_of_class_info(name)
    }

    // --- Dynamic Invocation Helper: QMetaObject::invokeMethod ---

    /// Resolves `member` to a method accepting `args`, without invoking it.
    ///
    /// A member containing `(` is a signature and must match exactly; its count and types are then
    /// checked and reported. A bare name takes the first invokable overload whose parameter types
    /// match. Queued invokes resolve here before posting, as Qt checks in `invokeImpl` before
    /// queueing (qmetaobject.cpp:2774-2809).
    pub fn resolve_method(
        &self,
        member: &str,
        args: &[Variant],
    ) -> Result<&MetaMethod, InvokeError> {
        let idx = if member.contains('(') {
            self.index_of_method(member)
        } else {
            self.index_of_compatible_method(member, args)
        }
        .ok_or(InvokeError::MethodNotFound)?;
        let method = self.method(idx).ok_or(InvokeError::MethodNotFound)?;
        if !method.is_invokable() {
            return Err(InvokeError::MethodNotFound);
        }
        method.check_arguments(args)?;
        Ok(method)
    }

    /// Invokes a method by name or signature on the target object (`QMetaObject::invokeMethod`).
    pub fn invoke_method(
        &self,
        target: &mut dyn std::any::Any,
        member: &str,
        args: &[Variant],
    ) -> Result<Variant, InvokeError> {
        self.resolve_method(member, args)?.invoke(target, args)
    }
}

/// Static root MetaObject for `QObject` base class.
pub static QOBJECT_META_OBJECT: MetaObject = MetaObject::new("QObject", None, &[], &[], &[], &[]);

/// Supplies the slot descriptors that a `#[slots]` impl block generates for a type.
///
/// The blanket impl contributes none. A `#[slots]` block emits an inherent `__ferrokit_slot_methods`,
/// which shadows this trait method during path resolution.
#[doc(hidden)]
pub trait SlotMethodsFallback {
    fn __ferrokit_slot_methods() -> Vec<MetaMethod> {
        Vec::new()
    }
}

impl<T> SlotMethodsFallback for T {}
