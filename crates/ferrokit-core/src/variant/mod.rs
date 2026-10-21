// SPDX-License-Identifier: LGPL-3.0-only
//! QVariant dynamic type system (`QVariant` equivalent).
//!
//! Provides an extensible, union-like dynamic container that can store primitive values,
//! strings, byte arrays, geometries, temporal types, identifiers, colors, lists,
//! associative maps, QObject pointers, and arbitrary user-defined custom types via
//! [`CustomVariant`], [`std::any::TypeId`], and [`crate::meta::MetaType`].

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::json::{JsonArray, JsonDocument, JsonFormat, JsonObject, JsonParseError, JsonValue};
use crate::meta::{MetaType, MetaTypeId};
use crate::object::ObjectId;
use crate::serialize::{DataSerializable, DataStream, DataStreamReader, DataStreamWriter};
use crate::types::{
    ByteArray, Date, DateTime, Line, LineF, Locale, Margins, MarginsF, RegularExpression, Size,
    SizeF, StringList, Time, Url, Uuid,
};

/// Dynamically typed custom value container supporting any `Send + Sync + 'static` type.
///
/// Enables complete extensibility for user types, GUI primitives (Font, Brush, Pen,
/// Pixmap, Image, Transform), and foreign object pointers without hardcoding them in core.
#[derive(Clone)]
pub struct CustomVariant {
    inner: Arc<dyn Any + Send + Sync>,
    type_id: TypeId,
    type_name: &'static str,
    meta_type_id: Option<MetaTypeId>,
}

impl CustomVariant {
    /// Creates a new custom variant wrapping any `Send + Sync + 'static` value.
    pub fn new<T: Any + Send + Sync + 'static>(value: T) -> Self {
        let tid = TypeId::of::<T>();
        let meta = MetaType::from_type::<T>();
        Self {
            inner: Arc::new(value),
            type_id: tid,
            type_name: meta.name(),
            meta_type_id: Some(meta.id()),
        }
    }

    /// Creates a custom variant from an existing `Arc<T>`.
    pub fn from_arc<T: Any + Send + Sync + 'static>(value: Arc<T>) -> Self {
        let tid = TypeId::of::<T>();
        let meta = MetaType::from_type::<T>();
        Self {
            inner: value,
            type_id: tid,
            type_name: meta.name(),
            meta_type_id: Some(meta.id()),
        }
    }

    /// Creates a custom variant from a boxed trait object `Box<dyn Any + Send + Sync>`.
    pub fn from_box(boxed: Box<dyn Any + Send + Sync>) -> Self {
        let tid = (*boxed).type_id();
        let meta = MetaType::from_type_id(tid);
        let (name, mid) = match meta {
            Some(m) => (m.name(), Some(m.id())),
            None => ("Custom", None),
        };
        Self {
            inner: Arc::from(boxed),
            type_id: tid,
            type_name: name,
            meta_type_id: mid,
        }
    }

    /// Creates a custom variant from an `Arc<dyn Any + Send + Sync>` and explicit type info.
    pub fn from_arc_any(
        arc: Arc<dyn Any + Send + Sync>,
        type_id: TypeId,
        type_name: &'static str,
    ) -> Self {
        let meta = MetaType::from_type_id(type_id);
        let mid = meta.map(|m| m.id());
        Self {
            inner: arc,
            type_id,
            type_name,
            meta_type_id: mid,
        }
    }

    /// Returns a shared reference to the inner value if type matches `T`.
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        self.inner.downcast_ref::<T>()
    }

    /// Returns an Arc clone of the custom value if type matches `T`.
    pub fn downcast_arc<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        if self.type_id == TypeId::of::<T>() {
            self.inner.clone().downcast::<T>().ok()
        } else {
            None
        }
    }

    /// Returns `true` if the stored custom value is of type `T`.
    pub fn is<T: 'static>(&self) -> bool {
        self.type_id == TypeId::of::<T>()
    }

    /// Returns the compile-time `TypeId` of the stored custom value.
    pub fn type_id(&self) -> TypeId {
        self.type_id
    }

    /// Returns the type name.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// Returns the optional `MetaTypeId`.
    pub fn meta_type_id(&self) -> Option<MetaTypeId> {
        self.meta_type_id
    }

    /// Resolves the corresponding `MetaType` metadata.
    pub fn meta_type(&self) -> MetaType {
        if let Some(mt) = MetaType::from_type_id(self.type_id) {
            mt
        } else if let Some(mid) = self.meta_type_id {
            MetaType::new(mid, self.type_name, 0)
        } else {
            MetaType::with_type_id(MetaTypeId::USER, self.type_name, 0, self.type_id)
        }
    }

    /// Returns a reference to the underlying Arc trait object.
    pub fn inner(&self) -> &Arc<dyn Any + Send + Sync> {
        &self.inner
    }
}

impl std::ops::Deref for CustomVariant {
    type Target = dyn Any + Send + Sync;

    fn deref(&self) -> &Self::Target {
        &*self.inner
    }
}

impl fmt::Debug for CustomVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomVariant")
            .field("type_name", &self.type_name)
            .field("type_id", &self.type_id)
            .finish()
    }
}

impl PartialEq for CustomVariant {
    fn eq(&self, other: &Self) -> bool {
        if self.type_id != other.type_id {
            return false;
        }
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl From<Box<dyn Any + Send + Sync>> for CustomVariant {
    fn from(b: Box<dyn Any + Send + Sync>) -> Self {
        Self::from_box(b)
    }
}

impl From<Arc<dyn Any + Send + Sync>> for CustomVariant {
    fn from(a: Arc<dyn Any + Send + Sync>) -> Self {
        let tid = (*a).type_id();
        let meta = MetaType::from_type_id(tid);
        let name = meta.as_ref().map(|m| m.name()).unwrap_or("Custom");
        Self::from_arc_any(a, tid, name)
    }
}

/// Dynamic value container matching `QVariant`.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Variant {
    #[default]
    Invalid,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    ByteArray(Vec<u8>),
    // Geometry primitives (Qt QPoint, QPointF, QSize, QSizeF, QRect, QRectF, QLine, QLineF, QMargins, QMarginsF)
    Point(i32, i32),
    PointF(f32, f32),
    Size(i32, i32),
    SizeF(f32, f32),
    Rect(i32, i32, i32, i32),
    RectF(f32, f32, f32, f32),
    Line(i32, i32, i32, i32),
    LineF(f32, f32, f32, f32),
    Margins(i32, i32, i32, i32),
    MarginsF(f32, f32, f32, f32),
    // Graphic / Appearance
    Color(u8, u8, u8, u8),
    // Temporal (Qt QDate, QTime, QDateTime)
    Date(i32, u32, u32),
    Time(u32, u32, u32, u32),
    DateTime(i64),
    // Identifiers & Web & Text (Qt QUrl, QUuid, QRegularExpression, QLocale)
    Url(String),
    Uuid([u8; 16]),
    RegularExpression(String, bool),
    Locale(String),
    // Safe QObject Reference
    ObjectId(ObjectId),
    // Collections
    List(Vec<Variant>),
    Map(HashMap<String, Variant>),
    // Universal Extensibility for User Types, GUI types (Font, Brush, Pen, Pixmap, etc.)
    Custom(CustomVariant),
}

impl Variant {
    /// Helper constructor for signed 64-bit integer.
    #[inline]
    pub fn int(i: i64) -> Self {
        Variant::I64(i)
    }

    /// Helper constructor for unsigned 64-bit integer.
    #[inline]
    pub fn uint(u: u64) -> Self {
        Variant::U64(u)
    }

    /// Helper constructor for 64-bit float.
    #[inline]
    pub fn float(f: f64) -> Self {
        Variant::F64(f)
    }

    /// Creates a custom variant holding any `Send + Sync + 'static` value.
    pub fn from_custom<T: Any + Send + Sync + 'static>(value: T) -> Self {
        Variant::Custom(CustomVariant::new(value))
    }

    /// Creates a custom variant holding a shared `Arc<T>`.
    pub fn from_custom_arc<T: Any + Send + Sync + 'static>(value: Arc<T>) -> Self {
        Variant::Custom(CustomVariant::from_arc(value))
    }

    /// Creates a custom variant holding a boxed trait object `Box<dyn Any + Send + Sync>`.
    pub fn from_custom_box(boxed: Box<dyn Any + Send + Sync>) -> Self {
        Variant::Custom(CustomVariant::from_box(boxed))
    }

    /// Returns `true` if the variant holds a valid value.
    pub fn is_valid(&self) -> bool {
        !matches!(self, Variant::Invalid)
    }

    /// Returns the compile-time `TypeId` of the stored value.
    pub fn type_id(&self) -> TypeId {
        match self {
            Variant::Invalid => TypeId::of::<()>(),
            Variant::Bool(_) => TypeId::of::<bool>(),
            Variant::I64(_) => TypeId::of::<i64>(),
            Variant::U64(_) => TypeId::of::<u64>(),
            Variant::F64(_) => TypeId::of::<f64>(),
            Variant::String(_) => TypeId::of::<String>(),
            Variant::ByteArray(_) => TypeId::of::<Vec<u8>>(),
            Variant::Point(..) => TypeId::of::<(i32, i32)>(),
            Variant::PointF(..) => TypeId::of::<(f32, f32)>(),
            Variant::Size(..) => TypeId::of::<Size>(),
            Variant::SizeF(..) => TypeId::of::<SizeF>(),
            Variant::Rect(..) => TypeId::of::<(i32, i32, i32, i32)>(),
            Variant::RectF(..) => TypeId::of::<(f32, f32, f32, f32)>(),
            Variant::Line(..) => TypeId::of::<Line>(),
            Variant::LineF(..) => TypeId::of::<LineF>(),
            Variant::Margins(..) => TypeId::of::<Margins>(),
            Variant::MarginsF(..) => TypeId::of::<MarginsF>(),
            Variant::Color(..) => TypeId::of::<(u8, u8, u8, u8)>(),
            Variant::Date(..) => TypeId::of::<Date>(),
            Variant::Time(..) => TypeId::of::<Time>(),
            Variant::DateTime(_) => TypeId::of::<DateTime>(),
            Variant::Url(_) => TypeId::of::<Url>(),
            Variant::Uuid(_) => TypeId::of::<Uuid>(),
            Variant::RegularExpression(..) => TypeId::of::<RegularExpression>(),
            Variant::Locale(_) => TypeId::of::<Locale>(),
            Variant::ObjectId(_) => TypeId::of::<ObjectId>(),
            Variant::List(_) => TypeId::of::<Vec<Variant>>(),
            Variant::Map(_) => TypeId::of::<HashMap<String, Variant>>(),
            Variant::Custom(c) => c.type_id(),
        }
    }

    /// Returns the `MetaType` metadata matching Qt's `QMetaType`.
    pub fn meta_type(&self) -> MetaType {
        match self {
            Variant::Invalid => MetaType::new(MetaTypeId::UNKNOWN, "Invalid", 0),
            Variant::Bool(_) => MetaType::new(MetaTypeId::BOOL, "bool", 1),
            Variant::I64(_) => MetaType::new(MetaTypeId::LONG_LONG, "qlonglong", 8),
            Variant::U64(_) => MetaType::new(MetaTypeId::ULONG_LONG, "qulonglong", 8),
            Variant::F64(_) => MetaType::new(MetaTypeId::DOUBLE, "double", 8),
            Variant::String(_) => MetaType::new(
                MetaTypeId::QSTRING,
                "QString",
                std::mem::size_of::<String>(),
            ),
            Variant::ByteArray(_) => MetaType::new(
                MetaTypeId::QBYTE_ARRAY,
                "QByteArray",
                std::mem::size_of::<Vec<u8>>(),
            ),
            Variant::Point(..) => MetaType::new(MetaTypeId::QPOINT, "QPoint", 8),
            Variant::PointF(..) => MetaType::new(MetaTypeId::QPOINT_F, "QPointF", 8),
            Variant::Size(..) => MetaType::new(MetaTypeId::QSIZE, "QSize", 8),
            Variant::SizeF(..) => MetaType::new(MetaTypeId::QSIZE_F, "QSizeF", 8),
            Variant::Rect(..) => MetaType::new(MetaTypeId::QRECT, "QRect", 16),
            Variant::RectF(..) => MetaType::new(MetaTypeId::QRECT_F, "QRectF", 16),
            Variant::Line(..) => MetaType::new(MetaTypeId::QLINE, "QLine", 16),
            Variant::LineF(..) => MetaType::new(MetaTypeId::QLINE_F, "QLineF", 16),
            Variant::Margins(..) => MetaType::new(MetaTypeId::QMARGINS, "QMargins", 16),
            Variant::MarginsF(..) => MetaType::new(MetaTypeId::QMARGINS_F, "QMarginsF", 16),
            Variant::Color(..) => MetaType::new(MetaTypeId::QCOLOR, "QColor", 4),
            Variant::Date(..) => MetaType::new(MetaTypeId::QDATE, "QDate", 12),
            Variant::Time(..) => MetaType::new(MetaTypeId::QTIME, "QTime", 16),
            Variant::DateTime(_) => MetaType::new(MetaTypeId::QDATETIME, "QDateTime", 8),
            Variant::Url(_) => {
                MetaType::new(MetaTypeId::QURL, "QUrl", std::mem::size_of::<String>())
            }
            Variant::Uuid(_) => MetaType::new(MetaTypeId::QUUID, "QUuid", 16),
            Variant::RegularExpression(..) => MetaType::new(
                MetaTypeId::QREGULAR_EXPRESSION,
                "QRegularExpression",
                std::mem::size_of::<String>(),
            ),
            Variant::Locale(_) => MetaType::new(
                MetaTypeId::QLOCALE,
                "QLocale",
                std::mem::size_of::<String>(),
            ),
            Variant::ObjectId(_) => MetaType::new(
                MetaTypeId::QOBJECT_STAR,
                "QObject*",
                std::mem::size_of::<ObjectId>(),
            ),
            Variant::List(_) => MetaType::new(
                MetaTypeId::QVARIANT_LIST,
                "QVariantList",
                std::mem::size_of::<Vec<Variant>>(),
            ),
            Variant::Map(_) => MetaType::new(
                MetaTypeId::QVARIANT_MAP,
                "QVariantMap",
                std::mem::size_of::<HashMap<String, Variant>>(),
            ),
            Variant::Custom(c) => c.meta_type(),
        }
    }

    /// Returns the human-readable type name matching Qt type names.
    pub fn type_name(&self) -> &'static str {
        self.meta_type().name()
    }

    /// Returns `true` if the variant holds a value of type `T`.
    pub fn is<T: 'static>(&self) -> bool {
        self.type_id() == TypeId::of::<T>()
    }

    /// Returns a shared reference to the inner value if type matches `T`.
    ///
    /// Supports both custom types stored via `Variant::Custom` and built-in variants.
    pub fn downcast_ref<T: 'static>(&self) -> Option<&T> {
        let tid = TypeId::of::<T>();
        if tid == TypeId::of::<bool>() {
            if let Variant::Bool(b) = self {
                return (b as &dyn Any).downcast_ref::<T>();
            }
        } else if tid == TypeId::of::<i64>() {
            if let Variant::I64(i) = self {
                return (i as &dyn Any).downcast_ref::<T>();
            }
        } else if tid == TypeId::of::<u64>() {
            if let Variant::U64(u) = self {
                return (u as &dyn Any).downcast_ref::<T>();
            }
        } else if tid == TypeId::of::<f64>() {
            if let Variant::F64(f) = self {
                return (f as &dyn Any).downcast_ref::<T>();
            }
        } else if tid == TypeId::of::<String>() {
            if let Variant::String(s) = self {
                return (s as &dyn Any).downcast_ref::<T>();
            }
        } else if tid == TypeId::of::<Vec<u8>>() {
            if let Variant::ByteArray(b) = self {
                return (b as &dyn Any).downcast_ref::<T>();
            }
        } else if tid == TypeId::of::<ObjectId>() {
            if let Variant::ObjectId(id) = self {
                return (id as &dyn Any).downcast_ref::<T>();
            }
        } else if let Variant::Custom(c) = self {
            return c.downcast_ref::<T>();
        }
        None
    }

    /// Returns a cloned copy of the inner value if type matches `T`.
    pub fn downcast<T: Clone + 'static>(&self) -> Option<T> {
        self.downcast_ref::<T>().cloned()
    }

    /// Returns a shared Arc reference if the variant is `Custom` and holds type `T`.
    pub fn downcast_arc<T: Any + Send + Sync + 'static>(&self) -> Option<Arc<T>> {
        match self {
            Variant::Custom(c) => c.downcast_arc::<T>(),
            _ => None,
        }
    }

    /// Converts to a boolean value.
    pub fn to_bool(&self) -> Option<bool> {
        match self {
            Variant::Bool(b) => Some(*b),
            Variant::I64(i) => Some(*i != 0),
            Variant::U64(u) => Some(*u != 0),
            Variant::F64(f) => Some(*f != 0.0),
            Variant::String(s) => match s.to_lowercase().as_str() {
                "true" | "1" | "yes" | "on" => Some(true),
                "false" | "0" | "no" | "off" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }

    /// Converts to a signed 64-bit integer.
    ///
    /// A `U64` above `i64::MAX` yields `None` instead of wrapping to a negative
    /// number (Qt's `toLongLong` wraps; this deliberately differs).
    pub fn to_int(&self) -> Option<i64> {
        match self {
            Variant::I64(i) => Some(*i),
            Variant::U64(u) => i64::try_from(*u).ok(),
            Variant::F64(f) => Some(*f as i64),
            Variant::Bool(b) => Some(if *b { 1 } else { 0 }),
            Variant::String(s) => s.trim().parse::<i64>().ok(),
            _ => None,
        }
    }

    /// Converts to a signed 64-bit integer (`to_int` alias).
    pub fn to_i64(&self) -> Option<i64> {
        self.to_int()
    }

    /// Converts to an unsigned 64-bit integer.
    pub fn to_uint(&self) -> Option<u64> {
        match self {
            Variant::U64(u) => Some(*u),
            Variant::I64(i) if *i >= 0 => Some(*i as u64),
            Variant::F64(f) if *f >= 0.0 => Some(*f as u64),
            Variant::Bool(b) => Some(if *b { 1 } else { 0 }),
            Variant::String(s) => s.trim().parse::<u64>().ok(),
            _ => None,
        }
    }

    /// Converts to an unsigned 64-bit integer (`to_uint` alias).
    pub fn to_u64(&self) -> Option<u64> {
        self.to_uint()
    }

    /// Converts to a 64-bit floating point number.
    pub fn to_float(&self) -> Option<f64> {
        match self {
            Variant::F64(f) => Some(*f),
            Variant::I64(i) => Some(*i as f64),
            Variant::U64(u) => Some(*u as f64),
            Variant::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            Variant::String(s) => s.trim().parse::<f64>().ok(),
            _ => None,
        }
    }

    /// Converts to a 64-bit floating point number (`to_float` alias).
    pub fn to_f64(&self) -> Option<f64> {
        self.to_float()
    }

    /// Converts to a string representation.
    pub fn to_string_lossy(&self) -> String {
        match self {
            Variant::Invalid => String::new(),
            Variant::Bool(b) => b.to_string(),
            Variant::I64(i) => i.to_string(),
            Variant::U64(u) => u.to_string(),
            Variant::F64(f) => f.to_string(),
            Variant::String(s) => s.clone(),
            Variant::ByteArray(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Variant::Point(x, y) => format!("QPoint({}, {})", x, y),
            Variant::PointF(x, y) => format!("QPointF({}, {})", x, y),
            Variant::Size(w, h) => format!("QSize({}, {})", w, h),
            Variant::SizeF(w, h) => format!("QSizeF({}, {})", w, h),
            Variant::Rect(x, y, w, h) => format!("QRect({}, {}, {}, {})", x, y, w, h),
            Variant::RectF(x, y, w, h) => format!("QRectF({}, {}, {}, {})", x, y, w, h),
            Variant::Line(x1, y1, x2, y2) => format!("QLine({}, {}, {}, {})", x1, y1, x2, y2),
            Variant::LineF(x1, y1, x2, y2) => format!("QLineF({}, {}, {}, {})", x1, y1, x2, y2),
            Variant::Margins(l, t, r, b) => format!("QMargins({}, {}, {}, {})", l, t, r, b),
            Variant::MarginsF(l, t, r, b) => format!("QMarginsF({}, {}, {}, {})", l, t, r, b),
            Variant::Color(r, g, b, a) => format!("QColor({}, {}, {}, {})", r, g, b, a),
            Variant::Date(y, m, d) => format!("{:04}-{:02}-{:02}", y, m, d),
            Variant::Time(h, m, s, ms) => format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms),
            Variant::DateTime(ts) => format!("QDateTime({})", ts),
            Variant::Url(u) => u.clone(),
            Variant::Uuid(b) => Uuid::from_bytes(*b).to_string_canonical(),
            Variant::RegularExpression(r, _) => format!("QRegularExpression({})", r),
            Variant::Locale(l) => format!("QLocale({})", l),
            Variant::ObjectId(id) => format!("QObject({:?})", id),
            Variant::List(l) => format!("[{} items]", l.len()),
            Variant::Map(m) => format!("{{{} entries}}", m.len()),
            Variant::Custom(c) => format!("Custom({})", c.type_name()),
        }
    }

    /// Returns point `(x, y)` if variant is `Point`.
    pub fn to_point(&self) -> Option<(i32, i32)> {
        match self {
            Variant::Point(x, y) => Some((*x, *y)),
            Variant::PointF(x, y) => Some((*x as i32, *y as i32)),
            _ => None,
        }
    }

    /// Returns floating point `(x, y)` if variant is `PointF` or `Point`.
    pub fn to_point_f(&self) -> Option<(f32, f32)> {
        match self {
            Variant::PointF(x, y) => Some((*x, *y)),
            Variant::Point(x, y) => Some((*x as f32, *y as f32)),
            _ => None,
        }
    }

    /// Returns size `(width, height)` if variant is `Size`.
    pub fn to_size(&self) -> Option<Size> {
        match self {
            Variant::Size(w, h) => Some(Size::new(*w, *h)),
            Variant::SizeF(w, h) => Some(Size::new(*w as i32, *h as i32)),
            _ => None,
        }
    }

    /// Returns floating point size if variant is `SizeF` or `Size`.
    pub fn to_size_f(&self) -> Option<SizeF> {
        match self {
            Variant::SizeF(w, h) => Some(SizeF::new(*w, *h)),
            Variant::Size(w, h) => Some(SizeF::new(*w as f32, *h as f32)),
            _ => None,
        }
    }

    /// Returns rect `(x, y, w, h)` if variant is `Rect`.
    pub fn to_rect(&self) -> Option<(i32, i32, i32, i32)> {
        match self {
            Variant::Rect(x, y, w, h) => Some((*x, *y, *w, *h)),
            Variant::RectF(x, y, w, h) => Some((*x as i32, *y as i32, *w as i32, *h as i32)),
            _ => None,
        }
    }

    /// Returns rect `(x, y, w, h)` if variant is `RectF` or `Rect`.
    pub fn to_rect_f(&self) -> Option<(f32, f32, f32, f32)> {
        match self {
            Variant::RectF(x, y, w, h) => Some((*x, *y, *w, *h)),
            Variant::Rect(x, y, w, h) => Some((*x as f32, *y as f32, *w as f32, *h as f32)),
            _ => None,
        }
    }

    /// Returns line `(x1, y1, x2, y2)` if variant is `Line`.
    pub fn to_line(&self) -> Option<Line> {
        match self {
            Variant::Line(x1, y1, x2, y2) => Some(Line::new(*x1, *y1, *x2, *y2)),
            Variant::LineF(x1, y1, x2, y2) => {
                Some(Line::new(*x1 as i32, *y1 as i32, *x2 as i32, *y2 as i32))
            }
            _ => None,
        }
    }

    /// Returns line `(x1, y1, x2, y2)` if variant is `LineF` or `Line`.
    pub fn to_line_f(&self) -> Option<LineF> {
        match self {
            Variant::LineF(x1, y1, x2, y2) => Some(LineF::new(*x1, *y1, *x2, *y2)),
            Variant::Line(x1, y1, x2, y2) => {
                Some(LineF::new(*x1 as f32, *y1 as f32, *x2 as f32, *y2 as f32))
            }
            _ => None,
        }
    }

    /// Returns margins `(left, top, right, bottom)` if variant is `Margins`.
    pub fn to_margins(&self) -> Option<Margins> {
        match self {
            Variant::Margins(l, t, r, b) => Some(Margins::new(*l, *t, *r, *b)),
            Variant::MarginsF(l, t, r, b) => {
                Some(Margins::new(*l as i32, *t as i32, *r as i32, *b as i32))
            }
            _ => None,
        }
    }

    /// Returns margins `(left, top, right, bottom)` if variant is `MarginsF` or `Margins`.
    pub fn to_margins_f(&self) -> Option<MarginsF> {
        match self {
            Variant::MarginsF(l, t, r, b) => Some(MarginsF::new(*l, *t, *r, *b)),
            Variant::Margins(l, t, r, b) => {
                Some(MarginsF::new(*l as f32, *t as f32, *r as f32, *b as f32))
            }
            _ => None,
        }
    }

    /// Returns color `(r, g, b, a)` if variant is `Color`.
    pub fn to_color(&self) -> Option<(u8, u8, u8, u8)> {
        match self {
            Variant::Color(r, g, b, a) => Some((*r, *g, *b, *a)),
            _ => None,
        }
    }

    /// Returns `Date` if variant is `Date`.
    pub fn to_date(&self) -> Option<Date> {
        match self {
            Variant::Date(y, m, d) => Some(Date::new(*y, *m, *d)),
            Variant::DateTime(ts) => Some(DateTime::from_timestamp_ms(*ts).date),
            _ => None,
        }
    }

    /// Returns `Time` if variant is `Time`.
    pub fn to_time(&self) -> Option<Time> {
        match self {
            Variant::Time(h, m, s, ms) => Some(Time::new(*h, *m, *s, *ms)),
            Variant::DateTime(ts) => Some(DateTime::from_timestamp_ms(*ts).time),
            _ => None,
        }
    }

    /// Returns `DateTime` if variant is `DateTime`.
    pub fn to_date_time(&self) -> Option<DateTime> {
        match self {
            Variant::DateTime(ts) => Some(DateTime::from_timestamp_ms(*ts)),
            Variant::Date(y, m, d) => Some(DateTime::new(Date::new(*y, *m, *d), Time::default())),
            _ => None,
        }
    }

    /// Returns `Url` if variant is `Url` or `String`.
    pub fn to_url(&self) -> Option<Url> {
        match self {
            Variant::Url(u) => Some(Url::new(u.clone())),
            Variant::String(s) => Some(Url::new(s.clone())),
            _ => None,
        }
    }

    /// Returns `Uuid` if variant is `Uuid`.
    pub fn to_uuid(&self) -> Option<Uuid> {
        match self {
            Variant::Uuid(bytes) => Some(Uuid::from_bytes(*bytes)),
            Variant::String(s) => Uuid::from_string(s),
            _ => None,
        }
    }

    /// Returns `RegularExpression` if variant is `RegularExpression` or `String`.
    pub fn to_regular_expression(&self) -> Option<RegularExpression> {
        match self {
            Variant::RegularExpression(p, ci) => {
                Some(RegularExpression::new(p.clone()).with_case_insensitive(*ci))
            }
            Variant::String(s) => Some(RegularExpression::new(s.clone())),
            _ => None,
        }
    }

    /// Returns `Locale` if variant is `Locale` or `String`.
    pub fn to_locale(&self) -> Option<Locale> {
        match self {
            Variant::Locale(l) => Some(Locale::new(l.clone())),
            Variant::String(s) => Some(Locale::new(s.clone())),
            _ => None,
        }
    }

    /// Returns `ObjectId` if variant is `ObjectId`.
    pub fn to_object_id(&self) -> Option<ObjectId> {
        match self {
            Variant::ObjectId(id) => Some(*id),
            _ => None,
        }
    }

    /// Converts to a [`ByteArray`] if variant is `ByteArray` or `String`.
    pub fn to_byte_array(&self) -> Option<ByteArray> {
        match self {
            Variant::ByteArray(b) => Some(ByteArray(b.clone())),
            Variant::String(s) => Some(ByteArray::from(s.as_str())),
            _ => None,
        }
    }

    /// Converts to a [`StringList`] if variant is `List` of strings, or a single `String`.
    pub fn to_string_list(&self) -> Option<StringList> {
        match self {
            Variant::List(items) => {
                let mut list = Vec::with_capacity(items.len());
                for item in items {
                    if let Variant::String(s) = item {
                        list.push(s.clone());
                    } else {
                        return None;
                    }
                }
                Some(StringList(list))
            }
            Variant::String(s) => Some(StringList::from(vec![s.clone()])),
            _ => None,
        }
    }

    /// Linearly interpolates between `self` and `target` with a progress ratio `[0.0, 1.0]`.
    ///
    /// Modeled after Qt `QVariantAnimation::interpolated`.
    pub fn interpolate(&self, target: &Variant, progress: f32) -> Option<Variant> {
        let p = progress.clamp(0.0, 1.0);
        let p64 = p as f64;

        match (self, target) {
            (Variant::F64(start), Variant::F64(end)) => {
                Some(Variant::F64(start + (end - start) * p64))
            }
            (Variant::I64(start), Variant::I64(end)) => {
                let v = *start as f64 + (*end - *start) as f64 * p64;
                Some(Variant::I64(v.round() as i64))
            }
            (Variant::U64(start), Variant::U64(end)) => {
                let v = *start as f64 + (*end as f64 - *start as f64) * p64;
                Some(Variant::U64(v.round().max(0.0) as u64))
            }
            // Mixed numeric interpolation
            (Variant::F64(start), Variant::I64(end)) => {
                Some(Variant::F64(start + (*end as f64 - start) * p64))
            }
            (Variant::I64(start), Variant::F64(end)) => {
                Some(Variant::F64(*start as f64 + (end - *start as f64) * p64))
            }
            (Variant::Point(x1, y1), Variant::Point(x2, y2)) => {
                let x = *x1 as f64 + (*x2 - *x1) as f64 * p64;
                let y = *y1 as f64 + (*y2 - *y1) as f64 * p64;
                Some(Variant::Point(x.round() as i32, y.round() as i32))
            }
            (Variant::PointF(x1, y1), Variant::PointF(x2, y2)) => {
                let x = *x1 + (*x2 - *x1) * p;
                let y = *y1 + (*y2 - *y1) * p;
                Some(Variant::PointF(x, y))
            }
            (Variant::Size(w1, h1), Variant::Size(w2, h2)) => {
                let w = *w1 as f64 + (*w2 - *w1) as f64 * p64;
                let h = *h1 as f64 + (*h2 - *h1) as f64 * p64;
                Some(Variant::Size(w.round() as i32, h.round() as i32))
            }
            (Variant::SizeF(w1, h1), Variant::SizeF(w2, h2)) => {
                let w = *w1 + (*w2 - *w1) * p;
                let h = *h1 + (*h2 - *h1) * p;
                Some(Variant::SizeF(w, h))
            }
            (Variant::Rect(x1, y1, w1, h1), Variant::Rect(x2, y2, w2, h2)) => {
                let x = *x1 as f64 + (*x2 - *x1) as f64 * p64;
                let y = *y1 as f64 + (*y2 - *y1) as f64 * p64;
                let w = *w1 as f64 + (*w2 - *w1) as f64 * p64;
                let h = *h1 as f64 + (*h2 - *h1) as f64 * p64;
                Some(Variant::Rect(
                    x.round() as i32,
                    y.round() as i32,
                    w.round() as i32,
                    h.round() as i32,
                ))
            }
            (Variant::RectF(x1, y1, w1, h1), Variant::RectF(x2, y2, w2, h2)) => {
                let x = *x1 + (*x2 - *x1) * p;
                let y = *y1 + (*y2 - *y1) * p;
                let w = *w1 + (*w2 - *w1) * p;
                let h = *h1 + (*h2 - *h1) * p;
                Some(Variant::RectF(x, y, w, h))
            }
            (Variant::Line(x1, y1, x2, y2), Variant::Line(tx1, ty1, tx2, ty2)) => {
                let lx1 = *x1 as f64 + (*tx1 - *x1) as f64 * p64;
                let ly1 = *y1 as f64 + (*ty1 - *y1) as f64 * p64;
                let lx2 = *x2 as f64 + (*tx2 - *x2) as f64 * p64;
                let ly2 = *y2 as f64 + (*ty2 - *y2) as f64 * p64;
                Some(Variant::Line(
                    lx1.round() as i32,
                    ly1.round() as i32,
                    lx2.round() as i32,
                    ly2.round() as i32,
                ))
            }
            (Variant::LineF(x1, y1, x2, y2), Variant::LineF(tx1, ty1, tx2, ty2)) => {
                let lx1 = *x1 + (*tx1 - *x1) * p;
                let ly1 = *y1 + (*ty1 - *y1) * p;
                let lx2 = *x2 + (*tx2 - *x2) * p;
                let ly2 = *y2 + (*ty2 - *y2) * p;
                Some(Variant::LineF(lx1, ly1, lx2, ly2))
            }
            (Variant::Margins(l1, t1, r1, b1), Variant::Margins(l2, t2, r2, b2)) => {
                let l = *l1 as f64 + (*l2 - *l1) as f64 * p64;
                let t = *t1 as f64 + (*t2 - *t1) as f64 * p64;
                let r = *r1 as f64 + (*r2 - *r1) as f64 * p64;
                let b = *b1 as f64 + (*b2 - *b1) as f64 * p64;
                Some(Variant::Margins(
                    l.round() as i32,
                    t.round() as i32,
                    r.round() as i32,
                    b.round() as i32,
                ))
            }
            (Variant::Color(r1, g1, b1, a1), Variant::Color(r2, g2, b2, a2)) => {
                let lerp_u8 = |c1: u8, c2: u8| -> u8 {
                    let v = c1 as f32 + (c2 as f32 - c1 as f32) * p;
                    v.round().clamp(0.0, 255.0) as u8
                };
                Some(Variant::Color(
                    lerp_u8(*r1, *r2),
                    lerp_u8(*g1, *g2),
                    lerp_u8(*b1, *b2),
                    lerp_u8(*a1, *a2),
                ))
            }
            (Variant::DateTime(ts1), Variant::DateTime(ts2)) => {
                let ts = *ts1 as f64 + (*ts2 - *ts1) as f64 * p64;
                Some(Variant::DateTime(ts.round() as i64))
            }
            _ => None,
        }
    }

    // =========================================================================
    // JSON Integration (matching QVariant <-> QJson* conversions)
    // =========================================================================

    /// Converts this variant into a `JsonValue`.
    pub fn to_json_value(&self) -> JsonValue {
        match self {
            Self::Invalid => JsonValue::Null,
            Self::Bool(b) => JsonValue::Bool(*b),
            Self::I64(i) => JsonValue::Number(*i as f64),
            Self::U64(u) => JsonValue::Number(*u as f64),
            Self::F64(f) => JsonValue::Number(*f),
            Self::String(s) => JsonValue::String(s.clone()),
            Self::ByteArray(b) => {
                JsonValue::String(ByteArray::from(b.clone()).to_base64().to_string())
            }
            Self::Point(x, y) => {
                let mut obj = JsonObject::new();
                obj.insert("x", *x);
                obj.insert("y", *y);
                JsonValue::Object(obj)
            }
            Self::PointF(x, y) => {
                let mut obj = JsonObject::new();
                obj.insert("x", *x);
                obj.insert("y", *y);
                JsonValue::Object(obj)
            }
            Self::Size(w, h) => {
                let mut obj = JsonObject::new();
                obj.insert("width", *w);
                obj.insert("height", *h);
                JsonValue::Object(obj)
            }
            Self::SizeF(w, h) => {
                let mut obj = JsonObject::new();
                obj.insert("width", *w);
                obj.insert("height", *h);
                JsonValue::Object(obj)
            }
            Self::Rect(x, y, w, h) => {
                let mut obj = JsonObject::new();
                obj.insert("x", *x);
                obj.insert("y", *y);
                obj.insert("width", *w);
                obj.insert("height", *h);
                JsonValue::Object(obj)
            }
            Self::RectF(x, y, w, h) => {
                let mut obj = JsonObject::new();
                obj.insert("x", *x);
                obj.insert("y", *y);
                obj.insert("width", *w);
                obj.insert("height", *h);
                JsonValue::Object(obj)
            }
            Self::Line(x1, y1, x2, y2) => {
                let mut obj = JsonObject::new();
                obj.insert("x1", *x1);
                obj.insert("y1", *y1);
                obj.insert("x2", *x2);
                obj.insert("y2", *y2);
                JsonValue::Object(obj)
            }
            Self::LineF(x1, y1, x2, y2) => {
                let mut obj = JsonObject::new();
                obj.insert("x1", *x1);
                obj.insert("y1", *y1);
                obj.insert("x2", *x2);
                obj.insert("y2", *y2);
                JsonValue::Object(obj)
            }
            Self::Margins(l, t, r, b) => {
                let mut obj = JsonObject::new();
                obj.insert("left", *l);
                obj.insert("top", *t);
                obj.insert("right", *r);
                obj.insert("bottom", *b);
                JsonValue::Object(obj)
            }
            Self::MarginsF(l, t, r, b) => {
                let mut obj = JsonObject::new();
                obj.insert("left", *l);
                obj.insert("top", *t);
                obj.insert("right", *r);
                obj.insert("bottom", *b);
                JsonValue::Object(obj)
            }
            Self::Color(r, g, b, a) => JsonValue::String(format!("#{r:02X}{g:02X}{b:02X}{a:02X}")),
            Self::Date(y, m, d) => JsonValue::String(format!("{y:04}-{m:02}-{d:02}")),
            Self::Time(h, m, s, ms) => JsonValue::String(format!("{h:02}:{m:02}:{s:02}.{ms:03}")),
            Self::DateTime(ts) => JsonValue::Number(*ts as f64),
            Self::Url(u) => JsonValue::String(u.clone()),
            Self::Uuid(bytes) => JsonValue::String(Uuid::from_bytes(*bytes).to_string()),
            Self::RegularExpression(pat, case) => {
                let mut obj = JsonObject::new();
                obj.insert("pattern", pat.clone());
                obj.insert("case_sensitive", *case);
                JsonValue::Object(obj)
            }
            Self::Locale(l) => JsonValue::String(l.clone()),
            Self::ObjectId(id) => JsonValue::Number(id.0 as f64),
            Self::List(list) => {
                let arr: Vec<JsonValue> = list.iter().map(|item| item.to_json_value()).collect();
                JsonValue::Array(JsonArray::from_iter(arr))
            }
            Self::Map(map) => {
                let mut obj = JsonObject::new();
                for (k, v) in map.iter() {
                    obj.insert(k.clone(), v.to_json_value());
                }
                JsonValue::Object(obj)
            }
            Self::Custom(_) => JsonValue::Null,
        }
    }

    /// Reconstructs a `Variant` from a `JsonValue`.
    pub fn from_json_value(val: &JsonValue) -> Self {
        match val {
            JsonValue::Null | JsonValue::Undefined => Variant::Invalid,
            JsonValue::Bool(b) => Variant::Bool(*b),
            JsonValue::Number(n) => {
                if n.fract() == 0.0 && *n >= (i64::MIN as f64) && *n < crate::json::I64_EXCLUSIVE_UPPER_F64 {
                    Variant::I64(*n as i64)
                } else {
                    Variant::F64(*n)
                }
            }
            JsonValue::String(s) => Variant::String(s.clone()),
            JsonValue::Array(arr) => {
                let list: Vec<Variant> = arr.iter().map(Variant::from_json_value).collect();
                Variant::List(list)
            }
            JsonValue::Object(obj) => {
                let mut map = HashMap::new();
                for (k, v) in obj.iter() {
                    map.insert(k.clone(), Variant::from_json_value(v));
                }
                Variant::Map(map)
            }
        }
    }

    /// Serializes this variant to a formatted JSON string.
    pub fn to_json_string(&self, format: JsonFormat) -> String {
        crate::json::writer::to_string(&self.to_json_value(), format)
    }

    /// Serializes this variant to a formatted JSON `ByteArray` (`QJsonDocument::toJson`).
    pub fn to_json(&self, format: JsonFormat) -> ByteArray {
        ByteArray::from(self.to_json_string(format).into_bytes())
    }

    /// Deserializes a `Variant` from JSON bytes.
    pub fn from_json(json: &[u8]) -> Result<Self, JsonParseError> {
        let val = crate::json::from_slice(json)?;
        Ok(Self::from_json_value(&val))
    }

    /// Deserializes a `Variant` from a JSON string slice.
    pub fn from_json_str(json: &str) -> Result<Self, JsonParseError> {
        Self::from_json(json.as_bytes())
    }

    // =========================================================================
    // Binary Serialization (matching QVariant <-> QDataStream)
    // =========================================================================

    /// Serializes this variant to binary bytes using `DataStream`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut ds = DataStream::new();
        let mut writer = ds.writer();
        let _ = self.serialize(&mut writer);
        ds.into_bytes()
    }

    /// Deserializes a `Variant` from binary bytes using `DataStream`.
    pub fn from_bytes(bytes: &[u8]) -> std::io::Result<Self> {
        let mut reader = DataStream::reader(bytes);
        Self::deserialize(&mut reader)
    }
}

impl fmt::Display for Variant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_string_lossy())
    }
}

static INVALID_VARIANT: Variant = Variant::Invalid;

impl std::ops::Index<&str> for Variant {
    type Output = Variant;

    fn index(&self, key: &str) -> &Self::Output {
        match self {
            Self::Map(map) => map.get(key).unwrap_or(&INVALID_VARIANT),
            _ => &INVALID_VARIANT,
        }
    }
}

impl std::ops::Index<usize> for Variant {
    type Output = Variant;

    fn index(&self, index: usize) -> &Self::Output {
        match self {
            Self::List(list) => list.get(index).unwrap_or(&INVALID_VARIANT),
            _ => &INVALID_VARIANT,
        }
    }
}

impl From<bool> for Variant {
    fn from(v: bool) -> Self {
        Variant::Bool(v)
    }
}

impl From<i32> for Variant {
    fn from(v: i32) -> Self {
        Variant::I64(v as i64)
    }
}

impl From<i64> for Variant {
    fn from(v: i64) -> Self {
        Variant::I64(v)
    }
}

impl From<u32> for Variant {
    fn from(v: u32) -> Self {
        Variant::U64(v as u64)
    }
}

impl From<u64> for Variant {
    fn from(v: u64) -> Self {
        Variant::U64(v)
    }
}

impl From<f32> for Variant {
    fn from(v: f32) -> Self {
        Variant::F64(v as f64)
    }
}

impl From<f64> for Variant {
    fn from(v: f64) -> Self {
        Variant::F64(v)
    }
}

impl From<&str> for Variant {
    fn from(v: &str) -> Self {
        Variant::String(v.to_string())
    }
}

impl From<String> for Variant {
    fn from(v: String) -> Self {
        Variant::String(v)
    }
}

impl From<Vec<u8>> for Variant {
    fn from(v: Vec<u8>) -> Self {
        Variant::ByteArray(v)
    }
}

impl From<ByteArray> for Variant {
    fn from(ba: ByteArray) -> Self {
        Variant::ByteArray(ba.0)
    }
}

impl From<&ByteArray> for Variant {
    fn from(ba: &ByteArray) -> Self {
        Variant::ByteArray(ba.0.clone())
    }
}

impl From<StringList> for Variant {
    fn from(sl: StringList) -> Self {
        Variant::List(sl.0.into_iter().map(Variant::String).collect())
    }
}

impl From<&StringList> for Variant {
    fn from(sl: &StringList) -> Self {
        Variant::List(sl.0.iter().cloned().map(Variant::String).collect())
    }
}

impl From<Size> for Variant {
    fn from(s: Size) -> Self {
        Variant::Size(s.width, s.height)
    }
}

impl From<SizeF> for Variant {
    fn from(s: SizeF) -> Self {
        Variant::SizeF(s.width, s.height)
    }
}

impl From<Line> for Variant {
    fn from(l: Line) -> Self {
        Variant::Line(l.x1, l.y1, l.x2, l.y2)
    }
}

impl From<LineF> for Variant {
    fn from(l: LineF) -> Self {
        Variant::LineF(l.x1, l.y1, l.x2, l.y2)
    }
}

impl From<Margins> for Variant {
    fn from(m: Margins) -> Self {
        Variant::Margins(m.left, m.top, m.right, m.bottom)
    }
}

impl From<MarginsF> for Variant {
    fn from(m: MarginsF) -> Self {
        Variant::MarginsF(m.left, m.top, m.right, m.bottom)
    }
}

impl From<Date> for Variant {
    fn from(d: Date) -> Self {
        Variant::Date(d.year, d.month, d.day)
    }
}

impl From<Time> for Variant {
    fn from(t: Time) -> Self {
        Variant::Time(t.hour, t.minute, t.second, t.millisecond)
    }
}

impl From<DateTime> for Variant {
    fn from(dt: DateTime) -> Self {
        Variant::DateTime(dt.timestamp_ms)
    }
}

impl From<Url> for Variant {
    fn from(u: Url) -> Self {
        Variant::Url(u.as_str().to_string())
    }
}

impl From<Uuid> for Variant {
    fn from(u: Uuid) -> Self {
        Variant::Uuid(u.bytes)
    }
}

impl From<RegularExpression> for Variant {
    fn from(r: RegularExpression) -> Self {
        Variant::RegularExpression(r.pattern().to_string(), r.is_case_insensitive())
    }
}

impl From<Locale> for Variant {
    fn from(l: Locale) -> Self {
        Variant::Locale(l.name().to_string())
    }
}

impl From<ObjectId> for Variant {
    fn from(id: ObjectId) -> Self {
        Variant::ObjectId(id)
    }
}

impl From<CustomVariant> for Variant {
    fn from(c: CustomVariant) -> Self {
        Variant::Custom(c)
    }
}

impl From<Box<dyn Any + Send + Sync>> for Variant {
    fn from(b: Box<dyn Any + Send + Sync>) -> Self {
        Variant::Custom(CustomVariant::from_box(b))
    }
}

impl From<Arc<dyn Any + Send + Sync>> for Variant {
    fn from(a: Arc<dyn Any + Send + Sync>) -> Self {
        Variant::Custom(CustomVariant::from(a))
    }
}

impl TryFrom<Variant> for ByteArray {
    type Error = &'static str;

    fn try_from(v: Variant) -> Result<Self, Self::Error> {
        v.to_byte_array().ok_or("Variant is not a ByteArray")
    }
}

impl TryFrom<Variant> for StringList {
    type Error = &'static str;

    fn try_from(v: Variant) -> Result<Self, Self::Error> {
        v.to_string_list().ok_or("Variant is not a StringList")
    }
}

impl From<(i32, i32)> for Variant {
    fn from((x, y): (i32, i32)) -> Self {
        Variant::Point(x, y)
    }
}

impl From<(f32, f32)> for Variant {
    fn from((x, y): (f32, f32)) -> Self {
        Variant::PointF(x, y)
    }
}

impl From<(i32, i32, i32, i32)> for Variant {
    fn from((x, y, w, h): (i32, i32, i32, i32)) -> Self {
        Variant::Rect(x, y, w, h)
    }
}

impl From<(f32, f32, f32, f32)> for Variant {
    fn from((x, y, w, h): (f32, f32, f32, f32)) -> Self {
        Variant::RectF(x, y, w, h)
    }
}

impl From<(u8, u8, u8, u8)> for Variant {
    fn from((r, g, b, a): (u8, u8, u8, u8)) -> Self {
        Variant::Color(r, g, b, a)
    }
}

/// Trait for extracting typed value from a Variant (`Variant::to_value<T>()`).
pub trait FromVariant: Sized {
    fn from_variant(v: &Variant) -> Option<Self>;
}

impl FromVariant for bool {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_bool()
    }
}

impl FromVariant for i32 {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_int().map(|i| i as i32)
    }
}

impl FromVariant for i64 {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_int()
    }
}

impl FromVariant for u32 {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_uint().map(|u| u as u32)
    }
}

impl FromVariant for u64 {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_uint()
    }
}

impl FromVariant for f32 {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_float().map(|f| f as f32)
    }
}

impl FromVariant for f64 {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_float()
    }
}

impl FromVariant for String {
    fn from_variant(v: &Variant) -> Option<Self> {
        match v {
            Variant::String(s) => Some(s.clone()),
            _ => None,
        }
    }
}

impl FromVariant for Vec<u8> {
    fn from_variant(v: &Variant) -> Option<Self> {
        match v {
            Variant::ByteArray(b) => Some(b.clone()),
            _ => None,
        }
    }
}

impl FromVariant for Size {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_size()
    }
}

impl FromVariant for SizeF {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_size_f()
    }
}

impl FromVariant for Line {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_line()
    }
}

impl FromVariant for LineF {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_line_f()
    }
}

impl FromVariant for Margins {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_margins()
    }
}

impl FromVariant for MarginsF {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_margins_f()
    }
}

impl FromVariant for Date {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_date()
    }
}

impl FromVariant for Time {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_time()
    }
}

impl FromVariant for DateTime {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_date_time()
    }
}

impl FromVariant for Url {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_url()
    }
}

impl FromVariant for Uuid {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_uuid()
    }
}

impl FromVariant for RegularExpression {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_regular_expression()
    }
}

impl FromVariant for Locale {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_locale()
    }
}

impl FromVariant for ObjectId {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_object_id()
    }
}

impl FromVariant for ByteArray {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_byte_array()
    }
}

impl FromVariant for StringList {
    fn from_variant(v: &Variant) -> Option<Self> {
        v.to_string_list()
    }
}

impl FromVariant for Variant {
    fn from_variant(v: &Variant) -> Option<Self> {
        Some(v.clone())
    }
}

/// Helper function used by derive macros to convert a Variant to a target type.
pub fn convert_variant_to_type<T: FromVariant>(v: &Variant) -> Option<T> {
    T::from_variant(v)
}

impl Variant {
    /// Converts the variant to a strongly-typed value `T`.
    pub fn to_value<T: FromVariant>(&self) -> Option<T> {
        T::from_variant(self)
    }
}

impl DataSerializable for Variant {
    fn serialize<W: std::io::Write>(
        &self,
        stream: &mut DataStreamWriter<W>,
    ) -> std::io::Result<()> {
        match self {
            Self::Invalid => stream.write_u8(0),
            Self::Bool(b) => {
                stream.write_u8(1)?;
                stream.write_bool(*b)
            }
            Self::I64(i) => {
                stream.write_u8(2)?;
                stream.write_i64(*i)
            }
            Self::U64(u) => {
                stream.write_u8(3)?;
                stream.write_u64(*u)
            }
            Self::F64(f) => {
                stream.write_u8(4)?;
                stream.write_f64(*f)
            }
            Self::String(s) => {
                stream.write_u8(5)?;
                stream.write_str(s)
            }
            Self::ByteArray(b) => {
                stream.write_u8(6)?;
                stream.write_bytes(b)
            }
            Self::Point(x, y) => {
                stream.write_u8(7)?;
                stream.write_i32(*x)?;
                stream.write_i32(*y)
            }
            Self::PointF(x, y) => {
                stream.write_u8(8)?;
                stream.write_f32(*x)?;
                stream.write_f32(*y)
            }
            Self::Size(w, h) => {
                stream.write_u8(9)?;
                stream.write_i32(*w)?;
                stream.write_i32(*h)
            }
            Self::SizeF(w, h) => {
                stream.write_u8(10)?;
                stream.write_f32(*w)?;
                stream.write_f32(*h)
            }
            Self::Rect(x, y, w, h) => {
                stream.write_u8(11)?;
                stream.write_i32(*x)?;
                stream.write_i32(*y)?;
                stream.write_i32(*w)?;
                stream.write_i32(*h)
            }
            Self::RectF(x, y, w, h) => {
                stream.write_u8(12)?;
                stream.write_f32(*x)?;
                stream.write_f32(*y)?;
                stream.write_f32(*w)?;
                stream.write_f32(*h)
            }
            Self::Line(x1, y1, x2, y2) => {
                stream.write_u8(13)?;
                stream.write_i32(*x1)?;
                stream.write_i32(*y1)?;
                stream.write_i32(*x2)?;
                stream.write_i32(*y2)
            }
            Self::LineF(x1, y1, x2, y2) => {
                stream.write_u8(14)?;
                stream.write_f32(*x1)?;
                stream.write_f32(*y1)?;
                stream.write_f32(*x2)?;
                stream.write_f32(*y2)
            }
            Self::Margins(l, t, r, b) => {
                stream.write_u8(15)?;
                stream.write_i32(*l)?;
                stream.write_i32(*t)?;
                stream.write_i32(*r)?;
                stream.write_i32(*b)
            }
            Self::MarginsF(l, t, r, b) => {
                stream.write_u8(16)?;
                stream.write_f32(*l)?;
                stream.write_f32(*t)?;
                stream.write_f32(*r)?;
                stream.write_f32(*b)
            }
            Self::Color(r, g, b, a) => {
                stream.write_u8(17)?;
                stream.write_u8(*r)?;
                stream.write_u8(*g)?;
                stream.write_u8(*b)?;
                stream.write_u8(*a)
            }
            Self::Date(y, m, d) => {
                stream.write_u8(18)?;
                stream.write_i32(*y)?;
                stream.write_u32(*m)?;
                stream.write_u32(*d)
            }
            Self::Time(h, m, s, ms) => {
                stream.write_u8(19)?;
                stream.write_u32(*h)?;
                stream.write_u32(*m)?;
                stream.write_u32(*s)?;
                stream.write_u32(*ms)
            }
            Self::DateTime(ts) => {
                stream.write_u8(20)?;
                stream.write_i64(*ts)
            }
            Self::Url(u) => {
                stream.write_u8(21)?;
                stream.write_str(u)
            }
            Self::Uuid(bytes) => {
                stream.write_u8(22)?;
                stream.write_raw(bytes)
            }
            Self::RegularExpression(pat, case) => {
                stream.write_u8(23)?;
                stream.write_str(pat)?;
                stream.write_bool(*case)
            }
            Self::Locale(l) => {
                stream.write_u8(24)?;
                stream.write_str(l)
            }
            Self::ObjectId(id) => {
                stream.write_u8(25)?;
                stream.write_u64(id.0)
            }
            Self::List(list) => {
                stream.write_u8(26)?;
                stream.write_u32(list.len() as u32)?;
                for item in list {
                    item.serialize(stream)?;
                }
                Ok(())
            }
            Self::Map(map) => {
                stream.write_u8(27)?;
                stream.write_u32(map.len() as u32)?;
                for (k, v) in map {
                    stream.write_str(k)?;
                    v.serialize(stream)?;
                }
                Ok(())
            }
            Self::Custom(_) => stream.write_u8(0),
        }
    }

    fn deserialize<R: std::io::Read>(stream: &mut DataStreamReader<R>) -> std::io::Result<Self> {
        Self::deserialize_at_depth(stream, 0)
    }
}

/// Maximum List/Map nesting accepted by [`Variant`] binary deserialization.
const MAX_DESERIALIZE_DEPTH: usize = 128;

/// Upper bound on elements preallocated from an untrusted count.
const MAX_PREALLOC_ELEMS: usize = 1024;

impl Variant {
    fn deserialize_at_depth<R: std::io::Read>(
        stream: &mut DataStreamReader<R>,
        depth: usize,
    ) -> std::io::Result<Self> {
        if depth > MAX_DESERIALIZE_DEPTH {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Variant nesting too deep",
            ));
        }
        let tag = stream.read_u8()?;
        match tag {
            0 => Ok(Self::Invalid),
            1 => stream.read_bool().map(Self::Bool),
            2 => stream.read_i64().map(Self::I64),
            3 => stream.read_u64().map(Self::U64),
            4 => stream.read_f64().map(Self::F64),
            5 => stream.read_string().map(Self::String),
            6 => stream.read_bytes().map(Self::ByteArray),
            7 => {
                let x = stream.read_i32()?;
                let y = stream.read_i32()?;
                Ok(Self::Point(x, y))
            }
            8 => {
                let x = stream.read_f32()?;
                let y = stream.read_f32()?;
                Ok(Self::PointF(x, y))
            }
            9 => {
                let w = stream.read_i32()?;
                let h = stream.read_i32()?;
                Ok(Self::Size(w, h))
            }
            10 => {
                let w = stream.read_f32()?;
                let h = stream.read_f32()?;
                Ok(Self::SizeF(w, h))
            }
            11 => {
                let x = stream.read_i32()?;
                let y = stream.read_i32()?;
                let w = stream.read_i32()?;
                let h = stream.read_i32()?;
                Ok(Self::Rect(x, y, w, h))
            }
            12 => {
                let x = stream.read_f32()?;
                let y = stream.read_f32()?;
                let w = stream.read_f32()?;
                let h = stream.read_f32()?;
                Ok(Self::RectF(x, y, w, h))
            }
            13 => {
                let x1 = stream.read_i32()?;
                let y1 = stream.read_i32()?;
                let x2 = stream.read_i32()?;
                let y2 = stream.read_i32()?;
                Ok(Self::Line(x1, y1, x2, y2))
            }
            14 => {
                let x1 = stream.read_f32()?;
                let y1 = stream.read_f32()?;
                let x2 = stream.read_f32()?;
                let y2 = stream.read_f32()?;
                Ok(Self::LineF(x1, y1, x2, y2))
            }
            15 => {
                let l = stream.read_i32()?;
                let t = stream.read_i32()?;
                let r = stream.read_i32()?;
                let b = stream.read_i32()?;
                Ok(Self::Margins(l, t, r, b))
            }
            16 => {
                let l = stream.read_f32()?;
                let t = stream.read_f32()?;
                let r = stream.read_f32()?;
                let b = stream.read_f32()?;
                Ok(Self::MarginsF(l, t, r, b))
            }
            17 => {
                let r = stream.read_u8()?;
                let g = stream.read_u8()?;
                let b = stream.read_u8()?;
                let a = stream.read_u8()?;
                Ok(Self::Color(r, g, b, a))
            }
            18 => {
                let y = stream.read_i32()?;
                let m = stream.read_u32()?;
                let d = stream.read_u32()?;
                Ok(Self::Date(y, m, d))
            }
            19 => {
                let h = stream.read_u32()?;
                let m = stream.read_u32()?;
                let s = stream.read_u32()?;
                let ms = stream.read_u32()?;
                Ok(Self::Time(h, m, s, ms))
            }
            20 => stream.read_i64().map(Self::DateTime),
            21 => stream.read_string().map(Self::Url),
            22 => {
                let mut bytes = [0u8; 16];
                stream.read_raw(&mut bytes)?;
                Ok(Self::Uuid(bytes))
            }
            23 => {
                let pat = stream.read_string()?;
                let case = stream.read_bool()?;
                Ok(Self::RegularExpression(pat, case))
            }
            24 => stream.read_string().map(Self::Locale),
            25 => stream.read_u64().map(|id| Self::ObjectId(ObjectId(id))),
            26 => {
                let count = stream.read_u32()? as usize;
                let mut list = Vec::with_capacity(count.min(MAX_PREALLOC_ELEMS));
                for _ in 0..count {
                    list.push(Self::deserialize_at_depth(stream, depth + 1)?);
                }
                Ok(Self::List(list))
            }
            27 => {
                let count = stream.read_u32()? as usize;
                let mut map = HashMap::with_capacity(count.min(MAX_PREALLOC_ELEMS));
                for _ in 0..count {
                    let k = stream.read_string()?;
                    let v = Self::deserialize_at_depth(stream, depth + 1)?;
                    map.insert(k, v);
                }
                Ok(Self::Map(map))
            }
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown Variant binary tag: {tag}"),
            )),
        }
    }
}

impl From<JsonValue> for Variant {
    fn from(val: JsonValue) -> Self {
        Variant::from_json_value(&val)
    }
}

impl From<JsonObject> for Variant {
    fn from(obj: JsonObject) -> Self {
        Variant::from_json_value(&JsonValue::Object(obj))
    }
}

impl From<JsonArray> for Variant {
    fn from(arr: JsonArray) -> Self {
        Variant::from_json_value(&JsonValue::Array(arr))
    }
}

impl From<JsonDocument> for Variant {
    fn from(doc: JsonDocument) -> Self {
        if doc.is_object() {
            Variant::from_json_value(&JsonValue::Object(doc.object().clone()))
        } else if doc.is_array() {
            Variant::from_json_value(&JsonValue::Array(doc.array().clone()))
        } else {
            Variant::Invalid
        }
    }
}

impl FromVariant for JsonValue {
    fn from_variant(v: &Variant) -> Option<Self> {
        Some(v.to_json_value())
    }
}

impl FromVariant for JsonObject {
    fn from_variant(v: &Variant) -> Option<Self> {
        match v.to_json_value() {
            JsonValue::Object(o) => Some(o),
            _ => None,
        }
    }
}

impl FromVariant for JsonArray {
    fn from_variant(v: &Variant) -> Option<Self> {
        match v.to_json_value() {
            JsonValue::Array(a) => Some(a),
            _ => None,
        }
    }
}

impl FromVariant for JsonDocument {
    fn from_variant(v: &Variant) -> Option<Self> {
        match v.to_json_value() {
            JsonValue::Object(o) => Some(JsonDocument::from_object(o)),
            JsonValue::Array(a) => Some(JsonDocument::from_array(a)),
            _ => None,
        }
    }
}
