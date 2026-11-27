// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

/// Core polymorphic object trait: `QObject`.
pub trait QObject: std::any::Any {
    /// Core object metadata: ObjectData (`QObjectData` equivalent).
    fn object_data(&self) -> &ObjectData;
    fn object_data_mut(&mut self) -> ObjectDataMut<'_>;
    /// Returns the static MetaObject reflection structure for this class (`QObject::metaObject`).
    fn meta_object(&self) -> &'static crate::meta::MetaObject {
        &crate::meta::QOBJECT_META_OBJECT
    }

    /// Returns true if this object inherits the given class name (`QObject::inherits`).
    fn inherits(&self, class_name: &str) -> bool {
        self.meta_object().inherits_name(class_name)
    }
    /// Optional downcast to Any for hierarchy reflection.
    fn as_qobject_any(&self) -> Option<&dyn std::any::Any> {
        None
    }

    /// Optional mutable downcast to Any for hierarchy reflection.
    fn as_qobject_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Returns object name (`QObject::objectName`).
    fn object_name(&self) -> Option<&str> {
        self.object_data().object_name()
    }

    /// Sets object name (`QObject::setObjectName`).
    fn set_object_name(&mut self, name: &str) {
        self.object_data_mut().set_object_name(name);
    }

    /// Signal emitted when object name changes (`QObject::objectNameChanged`).
    fn object_name_changed(&self) -> &Signal<String> {
        &self.object_data().object_name_changed
    }

    /// Signal emitted once when the object is destroyed (`QObject::destroyed`). See
    /// [`ObjectData::destroyed`].
    fn destroyed(&self) -> &Signal<ObjectId> {
        self.object_data().destroyed()
    }

    /// Returns parent object ID (`QObject::parent`).
    fn parent(&self) -> Option<ObjectId> {
        self.object_data().parent()
    }

    /// Returns direct children object IDs (`QObject::children`).
    ///
    /// Named `children_ids` to avoid name collision with GUI/Widget tree `Widget::children()`.
    fn children_ids(&self) -> Vec<ObjectId> {
        self.object_data().children().to_vec()
    }
    /// Returns the thread affinity of this object (`QObject::thread`).
    fn thread(&self) -> ThreadId {
        self.object_data().thread()
    }

    /// Blocks or unblocks signals emitted by this object (`QObject::blockSignals`).
    /// Returns previous blocked state.
    fn block_signals(&mut self, block: bool) -> bool {
        self.object_data()
            .signals_blocked
            .swap(block, Ordering::AcqRel)
    }

    fn signals_blocked(&self) -> bool {
        self.object_data().signals_blocked.load(Ordering::Acquire)
    }

    /// Searches the children this object owns first, then the ones attached without ownership
    /// (`set_parent_by_id`): `findChild` covers every child (Qt `qt_qFindChild_helper`), so the
    /// order differs from `children()` when both kinds are mixed.
    ///
    /// Calls `visit` with the first child matching type ID and optional name, depth first after
    /// the direct children (`QObject::findChild`). Returns whether one was found. A child whose
    /// callback is running is skipped.
    ///
    /// The child is lent for the call only, so no reference to it outlives the borrow that keeps
    /// its callbacks away.
    fn find_child_any(
        &self,
        name: &str,
        type_id: std::any::TypeId,
        visit: &mut dyn FnMut(&dyn std::any::Any),
    ) -> bool {
        // 1. Direct children first (qt_qFindChild_helper)
        for child in &self.object_data().owned_children {
            let hit = child.with(|c| {
                let name_matches = name.is_empty() || c.object_name() == Some(name);
                match c.as_qobject_any() {
                    Some(any) if name_matches && any.type_id() == type_id => {
                        visit(any);
                        true
                    }
                    _ => false,
                }
            });
            if hit == Some(true) {
                return true;
            }
        }
        // 2. Descendants if no direct child matched
        for child in &self.object_data().owned_children {
            if child.with(|c| c.find_child_any(name, type_id, visit)) == Some(true) {
                return true;
            }
        }
        // 3. Children attached without ownership (`set_parent_by_id`), after the owned ones.
        let unowned = unowned_child_ids(self.object_data());
        for &child_id in &unowned {
            let hit = with_object(child_id, |c| {
                let name_matches = name.is_empty() || c.object_name() == Some(name);
                match c.as_qobject_any() {
                    Some(any) if name_matches && any.type_id() == type_id => {
                        visit(any);
                        true
                    }
                    _ => false,
                }
            });
            if hit == Some(true) {
                return true;
            }
        }
        for &child_id in &unowned {
            if with_object(child_id, |c| c.find_child_any(name, type_id, visit)) == Some(true) {
                return true;
            }
        }
        false
    }

    /// Queries the ObjectId of a child object matching type ID and optional name.
    fn find_child_id(&self, name: &str, type_id: std::any::TypeId) -> Option<ObjectId> {
        // 1. Direct children first (qt_qFindChild_helper)
        for child in &self.object_data().owned_children {
            let hit = child.with(|c| {
                let name_matches = name.is_empty() || c.object_name() == Some(name);
                name_matches && c.as_qobject_any().is_some_and(|a| a.type_id() == type_id)
            });
            if hit == Some(true) {
                return Some(OwnedObject::object_id(child));
            }
        }
        // 2. Descendants if no direct child matched
        for child in &self.object_data().owned_children {
            if let Some(Some(found_id)) = child.with(|c| c.find_child_id(name, type_id)) {
                return Some(found_id);
            }
        }
        let unowned = unowned_child_ids(self.object_data());
        for &child_id in &unowned {
            let hit = with_object(child_id, |c| {
                let name_matches = name.is_empty() || c.object_name() == Some(name);
                name_matches && c.as_qobject_any().is_some_and(|a| a.type_id() == type_id)
            });
            if hit == Some(true) {
                return Some(child_id);
            }
        }
        for &child_id in &unowned {
            if let Some(Some(found_id)) = with_object(child_id, |c| c.find_child_id(name, type_id))
            {
                return Some(found_id);
            }
        }
        None
    }

    /// As [`QObject::find_child_any`], lending the child mutably.
    fn find_child_any_mut(
        &mut self,
        name: &str,
        type_id: std::any::TypeId,
        visit: &mut dyn FnMut(&mut dyn std::any::Any),
    ) -> bool {
        // 1. Direct children first (qt_qFindChild_helper)
        for child in self.object_data_mut().into_owned_children_mut() {
            let hit = child.with_mut(|c| {
                let name_matches = name.is_empty() || c.object_name() == Some(name);
                if !name_matches || !c.as_qobject_any().is_some_and(|a| a.type_id() == type_id) {
                    return false;
                }
                match c.as_qobject_any_mut() {
                    Some(any) => {
                        visit(any);
                        true
                    }
                    None => false,
                }
            });
            if hit == Some(true) {
                return true;
            }
        }
        // 2. Descendants if no direct child matched
        for child in self.object_data_mut().into_owned_children_mut() {
            if child.with_mut(|c| c.find_child_any_mut(name, type_id, visit)) == Some(true) {
                return true;
            }
        }
        let unowned = unowned_child_ids(self.object_data());
        for &child_id in &unowned {
            let hit = with_object_mut(child_id, |c| {
                let name_matches = name.is_empty() || c.object_name() == Some(name);
                if !name_matches || !c.as_qobject_any().is_some_and(|a| a.type_id() == type_id) {
                    return false;
                }
                match c.as_qobject_any_mut() {
                    Some(any) => {
                        visit(any);
                        true
                    }
                    None => false,
                }
            });
            if hit == Some(true) {
                return true;
            }
        }
        for &child_id in &unowned {
            if with_object_mut(child_id, |c| c.find_child_any_mut(name, type_id, visit))
                == Some(true)
            {
                return true;
            }
        }
        false
    }

    /// Calls `visit` with every descendant matching type ID and optional name
    /// (`QObject::findChildren`).
    fn find_children_any(
        &self,
        name: Option<&str>,
        type_id: std::any::TypeId,
        visit: &mut dyn FnMut(&dyn std::any::Any),
    ) {
        let target_name = name.unwrap_or("");
        for child in &self.object_data().owned_children {
            child.with(|c| {
                let name_matches = target_name.is_empty() || c.object_name() == Some(target_name);
                if name_matches
                    && let Some(any) = c.as_qobject_any()
                    && any.type_id() == type_id
                {
                    visit(any);
                }
                c.find_children_any(name, type_id, visit);
            });
        }
        for child_id in unowned_child_ids(self.object_data()) {
            with_object(child_id, |c| {
                let name_matches = target_name.is_empty() || c.object_name() == Some(target_name);
                if name_matches
                    && let Some(any) = c.as_qobject_any()
                    && any.type_id() == type_id
                {
                    visit(any);
                }
                c.find_children_any(name, type_id, visit);
            });
        }
    }

    /// Main event entry point (`QObject::event`).
    fn event(&mut self, event: &mut Event) -> bool {
        match &event.kind {
            EventKind::Timer { timer_id } => {
                self.timer_event(*timer_id);
                true
            }
            EventKind::DynamicPropertyChange { property_name } => {
                let name = property_name.clone();
                self.dynamic_property_change(&name);
                true
            }
            EventKind::ChildAdded { .. } | EventKind::ChildRemoved { .. } => {
                self.child_event(event);
                true
            }
            EventKind::User(_) => {
                self.custom_event(event);
                true
            }
            _ => false,
        }
    }

    /// Timer event handler (`QObject::timerEvent`).
    fn timer_event(&mut self, _timer_id: u64) {}

    /// Child added / removed event handler (`QObject::childEvent`).
    fn child_event(&mut self, _event: &mut Event) {}

    /// Custom event handler (`QObject::customEvent`).
    fn custom_event(&mut self, _event: &mut Event) {}

    /// Dynamic property change handler (`QObject::event` handling `QDynamicPropertyChangeEvent`).
    fn dynamic_property_change(&mut self, _property_name: &str) {}

    /// Retrieves property value, prioritizing static MetaProperty then dynamic property (`QObject::property`).
    fn property(&self, name: &str) -> Option<Variant> {
        default_property(self, name)
    }

    /// Sets property value, prioritizing static MetaProperty then fallback to dynamic property (`QObject::setProperty`).
    ///
    /// Qt semantic:
    /// - Static declared property: returns `true` if writable and write succeeds, `false` if read-only or write fails.
    /// - Dynamic property (not declared in meta-object): returns `false` (both on add/change and removal).
    ///   A `DynamicPropertyChange` event is dispatched if the dynamic property was added, modified, or removed.
    fn set_property(&mut self, name: &str, value: Variant) -> bool {
        default_set_property(self, name, value)
    }

    /// Dynamically invokes a method by signature or name (meta-object dispatch).
    fn invoke_method(
        &mut self,
        member: &str,
        args: &[Variant],
    ) -> Result<Variant, crate::meta::InvokeError> {
        let mo = self.meta_object();
        if let Some(any_mut) = self.as_qobject_any_mut() {
            mo.invoke_method(any_mut, member, args)
        } else {
            Err(crate::meta::InvokeError::TargetBorrowFailed)
        }
    }

    /// Invokes a method with an explicit connection type (`the meta-object(obj, name,
    /// Qt::ConnectionType,...)`, `invokeMethodImpl`).
    ///
    /// `Direct` invokes in the caller. `Auto` is `Direct` when the object lives on the calling thread
    /// (or is unregistered), otherwise it is queued. `Queued` and `BlockingQueued` are delegated to
    /// [`invoke_method_on_thread`], which needs no `&mut` borrow of the object.
    fn invoke_method_with_type(
        &mut self,
        member: &str,
        args: &[Variant],
        conn_type: crate::signal::ConnectionType,
    ) -> Result<Variant, crate::meta::InvokeError> {
        use crate::signal::ConnectionType;
        let id = self.object_data().id();
        let on_owner_thread =
            registration_thread_of(id).is_none_or(|thread| thread == ThreadId::current());
        let direct = match conn_type {
            ConnectionType::Direct => true,
            ConnectionType::Auto => on_owner_thread,
            ConnectionType::Queued | ConnectionType::BlockingQueued => false,
        };
        if direct {
            return self.invoke_method(member, args);
        }
        let meta = self.meta_object();
        invoke_method_on_thread(id, meta, member, args, conn_type)
    }

    /// Returns all registered dynamic property names (`QObject::dynamicPropertyNames`).
    fn dynamic_property_names(&self) -> Vec<String> {
        self.object_data().dynamic_property_names()
    }

    /// Event filter handler (`QObject::eventFilter`).
    fn event_filter(&mut self, _watched: ObjectId, _event: &mut Event) -> bool {
        false
    }

    /// Starts a timer and returns a TimerId (`QObject::startTimer`).
    ///
    /// Rejected from a thread other than the object's: a diagnostic is printed, no timer is
    /// registered and [`crate::timer::TimerId::INVALID`] is returned (Qt: "Timers cannot be
    /// started from another thread").
    fn start_timer(
        &mut self,
        interval_ms: u64,
        timer_type: crate::timer::TimerType,
    ) -> crate::timer::TimerId {
        let data = self.object_data();
        if data.thread_id() != ThreadId::current() {
            eprintln!("QObject::start_timer: timers cannot be started from another thread");
            return crate::timer::TimerId::INVALID;
        }
        let receiver = data.id();
        let id = crate::timer::start_object_timer(receiver, interval_ms, timer_type);
        if id.is_valid() {
            self.object_data_mut().add_timer(id);
        }
        id
    }

    /// Kills the timer with the specified timer ID (`QObject::killTimer`). A timer this object did
    /// not start is refused and `false` is returned, as in Qt. An id returned by
    /// [`start_timer`](QObject::start_timer) stays valid when a thread move gave the timer a fresh
    /// id because the target thread already used the old one.
    fn kill_timer(&mut self, id: crate::timer::TimerId) -> bool {
        let receiver = self.object_data().id();
        let id = if self.object_data_mut().remove_timer(id) {
            id
        } else {
            match crate::timer::reidentified_as(receiver, id) {
                Some(fresh) if self.object_data_mut().remove_timer(fresh) => fresh,
                _ => return false,
            }
        };
        crate::timer::kill_receiver_timer(receiver, id)
    }

    /// Schedules this object for deferred deletion (`QObject::deleteLater`).
    fn delete_later(&self) {
        self.object_data().delete_later()
    }
}

/// Generic extension trait for QObject reflection and hierarchy queries (`QObject::findChild<T>`).
///
/// A child is lent to a closure, and its borrow is held until the closure returns, so a callback
/// of the child cannot run alongside the reference.
pub trait QObjectExt {
    /// Calls `f` with the first descendant of type `T` and optional `name`
    /// (`QObject::findChild<T>`); `None` when there is none.
    fn with_child<T: 'static, R>(&self, name: &str, f: impl FnOnce(&T) -> R) -> Option<R>;

    /// As [`QObjectExt::with_child`], lending the descendant mutably.
    fn with_child_mut<T: 'static, R>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut T) -> R,
    ) -> Option<R>;

    /// Calls `f` with every descendant of type `T` and optional `name` (`QObject::findChildren<T>`).
    fn for_each_child<T: 'static>(&self, name: Option<&str>, f: impl FnMut(&T));
}

impl<O: QObject + ?Sized> QObjectExt for O {
    fn with_child<T: 'static, R>(&self, name: &str, f: impl FnOnce(&T) -> R) -> Option<R> {
        let mut f = Some(f);
        let mut out = None;
        self.find_child_any(name, std::any::TypeId::of::<T>(), &mut |any| {
            if let (Some(t), Some(f)) = (any.downcast_ref::<T>(), f.take()) {
                out = Some(f(t));
            }
        });
        out
    }

    fn with_child_mut<T: 'static, R>(
        &mut self,
        name: &str,
        f: impl FnOnce(&mut T) -> R,
    ) -> Option<R> {
        let mut f = Some(f);
        let mut out = None;
        self.find_child_any_mut(name, std::any::TypeId::of::<T>(), &mut |any| {
            if let (Some(t), Some(f)) = (any.downcast_mut::<T>(), f.take()) {
                out = Some(f(t));
            }
        });
        out
    }

    fn for_each_child<T: 'static>(&self, name: Option<&str>, mut f: impl FnMut(&T)) {
        self.find_children_any(name, std::any::TypeId::of::<T>(), &mut |any| {
            if let Some(t) = any.downcast_ref::<T>() {
                f(t);
            }
        });
    }
}

/// RAII guard for temporarily blocking signals emitted by a QObject (`QSignalBlocker`).
pub struct SignalBlocker {
    signals_blocked: Arc<AtomicBool>,
    previous_state: bool,
    active: bool,
}

impl SignalBlocker {
    pub fn new(obj: &mut dyn QObject) -> Self {
        Self::from_data(obj.object_data_mut().raw_data_mut())
    }

    pub fn from_data(data: &mut ObjectData) -> Self {
        let signals_blocked = Arc::clone(&data.signals_blocked);
        let previous_state = signals_blocked.swap(true, Ordering::AcqRel);
        Self {
            signals_blocked,
            previous_state,
            active: true,
        }
    }

    pub fn reblock(&mut self) {
        self.signals_blocked.store(true, Ordering::Release);
        self.active = true;
    }

    pub fn unblock(&mut self) {
        self.signals_blocked
            .store(self.previous_state, Ordering::Release);
        self.active = false;
    }
}

impl Drop for SignalBlocker {
    fn drop(&mut self) {
        if self.active {
            self.signals_blocked
                .store(self.previous_state, Ordering::Release);
        }
    }
}

/// Liveness token for a QObject identity. It does not expose references to the QObject.
/// Becomes invalid when the underlying `ObjectData` is dropped.
pub struct QPointer<T: ?Sized> {
    id: ObjectId,
    generation: u32,
    liveness: Arc<AtomicBool>,
    _marker: std::marker::PhantomData<T>,
}

impl<T: ?Sized> QPointer<T> {
    pub fn new<O: QObject>(obj: &O) -> Self {
        let data = obj.object_data();
        Self {
            id: data.id(),
            generation: data.generation,
            liveness: Arc::clone(&data.liveness),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn from_data(data: &ObjectData) -> Self {
        Self {
            id: data.id(),
            generation: data.generation,
            liveness: Arc::clone(&data.liveness),
            _marker: std::marker::PhantomData,
        }
    }

    pub fn null() -> Self {
        Self {
            id: ObjectId(0),
            generation: 0,
            liveness: Arc::new(AtomicBool::new(false)),
            _marker: std::marker::PhantomData,
        }
    }

    #[inline]
    pub fn is_null(&self) -> bool {
        !self.liveness.load(Ordering::Acquire)
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        !self.is_null()
    }

    #[inline]
    pub fn id(&self) -> Option<ObjectId> {
        if self.is_null() { None } else { Some(self.id) }
    }

    pub fn clear(&mut self) {
        self.liveness = Arc::new(AtomicBool::new(false));
    }
}

impl<T: ?Sized> Clone for QPointer<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            generation: self.generation,
            liveness: Arc::clone(&self.liveness),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<T: ?Sized> Default for QPointer<T> {
    fn default() -> Self {
        Self::null()
    }
}

impl<T: ?Sized> fmt::Debug for QPointer<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("QPointer")
            .field("id", &self.id)
            .field("generation", &self.generation)
            .field("is_null", &self.is_null())
            .finish()
    }
}

impl<T: ?Sized> PartialEq for QPointer<T> {
    fn eq(&self, other: &Self) -> bool {
        if self.is_null() && other.is_null() {
            true
        } else if self.is_null() || other.is_null() {
            false
        } else {
            self.id == other.id && self.generation == other.generation
        }
    }
}

impl<T: ?Sized> Eq for QPointer<T> {}

/// Sends an event to a receiver object synchronously (synchronous event delivery).
pub fn send_event(receiver: ObjectId, event: &mut Event) -> bool {
    crate::event_loop::notify_helper(receiver, event)
}
