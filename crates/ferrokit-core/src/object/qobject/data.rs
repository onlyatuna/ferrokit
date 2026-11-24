// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(pub u64);

impl ObjectId {
    /// Unique object identifier: ObjectId (`QObject*` equivalent).
    pub fn next() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        ObjectId(NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }
}

/// Core object metadata: ObjectData (`QObjectData` equivalent).
/// Core object metadata: ObjectData (`QObjectData` equivalent).
/// Unique generational object identifier for stale-reference detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GenerationalId {
    pub id: ObjectId,
    pub generation: u32,
}

impl GenerationalId {
    pub fn new(id: ObjectId, generation: u32) -> Self {
        Self { id, generation }
    }
}

/// Core object metadata: ObjectData (`QObjectData` equivalent).
pub struct ObjectData {
    pub(super) id: ObjectId,
    pub(super) parent: Option<ObjectId>,
    pub(super) children: Vec<ObjectId>,

    /// Thread affinity.
    pub(super) thread_id: ThreadId,

    pub(crate) delete_later_called: AtomicBool,

    pub(crate) signals_blocked: Arc<AtomicBool>,

    /// Installed event filters chain.
    pub(crate) event_filters: EventFilterChain,

    /// Dynamic properties map (`QObject::dynamicPropertyNames`).
    pub(crate) dynamic_properties: HashMap<String, Variant>,

    /// Object name for hierarchy queries (`QObject::objectName`).
    pub(crate) object_name: Option<String>,

    /// Emitted when object name changes (`QObject::objectNameChanged`).
    pub(crate) object_name_changed: Signal<String>,

    /// Shared liveness token for guarded weak references (`QPointer`).
    pub(crate) liveness: Arc<AtomicBool>,

    /// Object lifecycle generation counter.
    pub(crate) generation: u32,

    /// Directly owned child objects for cascading destruction. Each owner shares its pointer with
    /// the registry (see [`OwnedObject`]), and knows the child's id without reading the child.
    pub(super) owned_children: Vec<OwnedObject<dyn QObject>>,
    pub(crate) timers: Vec<crate::timer::TimerId>,

    /// True while this object is held in its parent's `owned_children`. Such an object's
    /// lifetime belongs to the parent, so `set_parent` (which only sees `&mut ObjectData`, a
    /// borrow *into* the owned object) must not detach it; [`reparent_owned_object`] moves its owner.
    pub(crate) owned_by_parent: bool,

    /// Whether [`move_to_thread_by_id`](crate::object::move_to_thread_by_id) may change this
    /// object's thread. `false` unless the creator opted in with
    /// [`ObjectData::with_thread_moves_allowed`].
    pub(super) movable_across_threads: bool,

    /// The parent's id and liveness token, captured when a registered parent was set. It lets
    /// [`ObjectData::parent`] see the parent die when the parent's drop cannot write to this
    /// object: a running callback borrows it, it was never registered, or it is registered on
    /// another thread. A token whose id differs from `parent` is stale and ignored, because
    /// `parent` is a private field managed through `set_parent`.
    pub(super) parent_liveness: Option<(ObjectId, Arc<AtomicBool>)>,
    pub(crate) borrow_state: Arc<ObjectBorrowState>,
}

impl fmt::Debug for ObjectData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectData")
            .field("id", &self.id)
            .field("object_name", &self.object_name)
            .field("parent", &self.parent)
            .field("children", &self.children)
            .field("thread_id", &self.thread_id)
            .field("delete_later_called", &self.delete_later_called)
            .field(
                "signals_blocked",
                &self.signals_blocked.load(Ordering::Relaxed),
            )
            .field("generation", &self.generation)
            .field("is_alive", &self.liveness.load(Ordering::Relaxed))
            .field("owned_children_count", &self.owned_children.len())
            .finish()
    }
}
impl Default for ObjectData {
    fn default() -> Self {
        Self::with_auto_id()
    }
}

impl ObjectData {
    /// Core object metadata: ObjectData (`QObjectData` equivalent).
    pub fn new(id: ObjectId) -> Self {
        let thread_id = ThreadId::current();
        register_object_thread(id, thread_id);
        let signals_blocked = Arc::new(AtomicBool::new(false));
        let liveness = Arc::new(AtomicBool::new(true));
        let object_name_changed =
            Signal::with_owner_parts(id, Arc::clone(&signals_blocked), Arc::clone(&liveness));
        let borrow_state = Arc::new(ObjectBorrowState::new(
            Signal::with_owner_parts(id, Arc::clone(&signals_blocked), Arc::clone(&liveness)),
            Arc::clone(&liveness),
        ));
        Self {
            id,
            parent: None,
            children: Vec::new(),
            thread_id,
            delete_later_called: AtomicBool::new(false),
            signals_blocked,
            object_name_changed,
            event_filters: EventFilterChain::new(),
            dynamic_properties: HashMap::new(),
            object_name: None,
            liveness,
            generation: 1,
            owned_children: Vec::new(),
            timers: Vec::new(),
            owned_by_parent: false,
            parent_liveness: None,
            borrow_state,
            movable_across_threads: false,
        }
    }

    /// Opts this object in to [`move_to_thread_by_id`](crate::object::move_to_thread_by_id)
    /// (`QObject::moveToThread`). Without it the object keeps the thread it was registered on:
    /// a move of it, or of an ancestor, is refused with
    /// [`MoveByIdError::NotMovable`](crate::object::MoveByIdError::NotMovable) before an event is
    /// sent or any state changes. [`Timer`](crate::timer::Timer) objects never opt in, because
    /// their `!Send` handle shares state with them that only the creating thread may use.
    ///
    /// # Safety
    ///
    /// `move_to_thread_by_id` is a safe function, and after a move the registry runs the
    /// object's `&mut` callbacks, and its owner drops it, on the target thread. The caller
    /// promises that the whole object is safe to use and drop on another thread, as a `Send`
    /// type is: no `Rc`, no `Cell` or `RefCell` shared with a handle that stays behind, no raw
    /// pointer or `!Send` closure that is still reached from the creating thread, and no
    /// thread-local state. The promise holds for the object's lifetime, because a move may
    /// happen at any time while it is registered.
    #[must_use]
    pub unsafe fn with_thread_moves_allowed(mut self) -> Self {
        self.movable_across_threads = true;
        self
    }

    /// Whether this object may change thread: it was opted in with
    /// [`ObjectData::with_thread_moves_allowed`]. A move takes the whole subtree, so a
    /// descendant that answers `false` refuses the move of every ancestor.
    #[inline]
    pub fn can_move_to_thread(&self) -> bool {
        self.movable_across_threads
    }

    /// Creates a new ObjectData with a unique auto-generated ObjectId.
    #[allow(clippy::new_without_default)]
    pub fn with_auto_id() -> Self {
        Self::new(ObjectId::next())
    }
    /// Returns the unique object identifier (`QObject*` equivalent).
    #[inline]
    pub fn id(&self) -> ObjectId {
        self.id
    }

    /// Returns the thread affinity of this object (`QObject::thread`).
    #[inline]
    pub fn thread_id(&self) -> ThreadId {
        self.thread_id
    }

    /// Returns a slice of directly owned child objects.
    #[inline]
    pub fn owned_children(&self) -> &[OwnedObject<dyn QObject>] {
        &self.owned_children
    }
    /// Emitted when object name changes (`QObject::objectNameChanged`).
    #[inline]
    pub fn object_name_changed(&self) -> &Signal<String> {
        &self.object_name_changed
    }

    /// Emitted once when this object is destroyed (`QObject::destroyed`), with the object's id.
    ///
    /// The object counts as destroyed from its unregistration (what `deleteLater` delivery does)
    /// or from the start of its drop, whichever comes first; `destroyed` is emitted at that point,
    /// before the object's connections are cut and before its children are deleted, even when
    /// signals of the object are blocked. By then the object is dead: `QPointer`s to it are null
    /// and the registry no longer reaches it, so receivers get the id and nothing else. A slot
    /// must not assume the object's memory is still there.
    ///
    /// A direct receiver runs on the thread that destroys the object. A queued or cross-thread
    /// receiver is posted to its own thread, and gets the id after the object is gone.
    #[inline]
    pub fn destroyed(&self) -> &Signal<ObjectId> {
        &self.borrow_state.destroyed
    }

    /// Installed event filters chain.
    #[inline]
    pub fn event_filters(&self) -> &EventFilterChain {
        &self.event_filters
    }

    /// Active timer identifiers registered on this object.
    #[inline]
    pub fn timers(&self) -> &[crate::timer::TimerId] {
        &self.timers
    }
    /// True while signals of this object are blocked (`QObject::signalsBlocked`).
    #[inline]
    pub fn signals_blocked(&self) -> bool {
        self.signals_blocked.load(Ordering::Acquire)
    }

    /// Shared signal-blocker token.
    #[inline]
    pub fn signals_blocked_token(&self) -> &Arc<AtomicBool> {
        &self.signals_blocked
    }

    pub(crate) fn set_thread_id(&mut self, thread_id: ThreadId) {
        self.thread_id = thread_id;
    }

    pub(crate) fn owned_children_mut(&mut self) -> &mut Vec<OwnedObject<dyn QObject>> {
        &mut self.owned_children
    }

    /// Core object metadata: ObjectData (`QObjectData` equivalent).
    pub fn with_thread(id: ObjectId, thread_id: ThreadId) -> Self {
        register_object_thread(id, thread_id);
        let signals_blocked = Arc::new(AtomicBool::new(false));
        let liveness = Arc::new(AtomicBool::new(true));
        let object_name_changed =
            Signal::with_owner_parts(id, Arc::clone(&signals_blocked), Arc::clone(&liveness));
        let borrow_state = Arc::new(ObjectBorrowState::new(
            Signal::with_owner_parts(id, Arc::clone(&signals_blocked), Arc::clone(&liveness)),
            Arc::clone(&liveness),
        ));
        Self {
            id,
            parent: None,
            children: Vec::new(),
            thread_id,
            delete_later_called: AtomicBool::new(false),
            signals_blocked,
            object_name_changed,
            event_filters: EventFilterChain::new(),
            dynamic_properties: HashMap::new(),
            object_name: None,
            liveness,
            generation: 1,
            owned_children: Vec::new(),
            timers: Vec::new(),
            owned_by_parent: false,
            parent_liveness: None,
            borrow_state,
            movable_across_threads: false,
        }
    }

    /// Schedules deferred deletion of this object (`QObject::deleteLater`).
    pub fn delete_later(&self) {
        delete_later(self)
    }

    pub fn install_event_filter(&mut self, filter: ObjectId) {
        install_event_filter(self, filter);
    }

    pub fn remove_event_filter(&mut self, filter: ObjectId) {
        remove_event_filter(self, filter);
    }

    /// See [`set_parent`].
    ///
    /// If the old parent's `ChildRemoved` handler panics, the change is finished and the panic is
    /// rethrown without `ChildAdded`; see [`set_parent_by_id`] (`# Panics`).
    pub fn set_parent(&mut self, new_parent: Option<ObjectId>) -> Result<(), ReparentError> {
        set_parent(self, new_parent)
    }

    /// True while a parent owns this object (see [`reparent_owned_object`]).
    pub fn is_owned_by_parent(&self) -> bool {
        self.owned_by_parent
    }

    pub(crate) fn add_child(&mut self, child_id: ObjectId) {
        if !self.children.contains(&child_id) {
            self.children.push(child_id);
        }
    }

    pub(crate) fn remove_child(&mut self, child_id: ObjectId) {
        self.children.retain(|&id| id != child_id);
    }

    /// Returns the parent object ID if any (`QObject::parent`).
    ///
    /// `None` once the parent has been dropped, as long as it was registered when it was set,
    /// even if this object could not be updated at that moment: the link this object keeps can
    /// still name the dead parent then, and this method does not report it.
    pub fn parent(&self) -> Option<ObjectId> {
        let id = self.parent?;
        match &self.parent_liveness {
            Some((token_id, alive)) if *token_id == id && !alive.load(Ordering::Acquire) => None,
            _ => Some(id),
        }
    }

    /// Forgets the parent, and the token that tracked it.
    pub(super) fn clear_parent(&mut self) {
        self.parent = None;
        self.parent_liveness = None;
    }

    /// Returns a slice of direct children object IDs (`QObject::children`).
    ///
    /// A change to the children made while a callback of this object is running (for example
    /// `set_parent` called from its `event`) shows here once that callback has returned.
    pub fn children(&self) -> &[ObjectId] {
        &self.children
    }

    /// Returns the thread affinity of this object (`QObject::thread`).
    pub fn thread(&self) -> ThreadId {
        self.thread_id
    }
    /// Sets object name for reflection and hierarchy queries (`QObject::setObjectName`).
    pub fn set_object_name(&mut self, name: impl Into<String>) {
        let name = name.into();
        let prev = self.object_name.as_deref().unwrap_or("");
        if prev != name {
            self.object_name = Some(name.clone());
            self.object_name_changed.emit(&name);
        }
    }

    /// Returns object name (`QObject::objectName`).
    pub fn object_name(&self) -> Option<&str> {
        self.object_name.as_deref()
    }

    /// Shared liveness token.
    pub fn liveness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.liveness)
    }

    /// Returns true if the object has not been dropped.
    pub fn is_alive(&self) -> bool {
        self.liveness.load(Ordering::Acquire)
    }

    /// Current generation index.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Generational identifier for guarded validation.
    pub fn generational_id(&self) -> GenerationalId {
        GenerationalId::new(self.id, self.generation)
    }

    /// Recursively cascades deletion to all children matching Qt `QObjectPrivate::deleteChildren`.
    ///
    /// An owned child whose callback is running is destroyed at once as far as anything else can
    /// tell (unregistered, no longer alive, no events, timers or connections to other objects),
    /// and so is everything below it; but its memory and its `Drop` wait until that callback
    /// returns: the callback still runs on `&mut self`. Such a child is not read, not even for its
    /// id, which comes from its owner.
    pub fn delete_children(&mut self) {
        let mut idle = Vec::with_capacity(self.owned_children.len());
        for mut child in std::mem::take(&mut self.owned_children) {
            // `with_mut` finds the object borrowed when a callback of it runs.
            if child
                .with_mut(|c| c.object_data_mut().clear_parent())
                .is_some()
            {
                idle.push(child);
            } else {
                destroy_when_released(child);
            }
        }
        for child_id in std::mem::take(&mut self.children) {
            release_child(child_id, self.id);
        }
        // The registry's list is complete at once, while this object's own list lags until a
        // running callback of this object returns. Whatever is still registered here goes too.
        for child_id in registered_children_of(self.id) {
            release_child(child_id, self.id);
        }
        drop(idle);
    }

    /// Unregisters this object from dispatch and global metadata.
    ///
    /// Unregistration is the object's destruction as far as connections, timers, posted events and
    /// `QPointer` are concerned, so this emits [`ObjectData::destroyed`] (once; the emission is
    /// not repeated when the object is dropped later, and registering the object again does not
    /// re-arm it, since the object stays dead). The same holds for the children a dropped parent
    /// does not own: they are unregistered, and emit `destroyed`, when the parent goes.
    ///
    /// # Safety
    /// Follows [`unregister_qobject`] requirements; no callback may be active for this object.
    pub unsafe fn unregister(&mut self) {
        // SAFETY: the caller upholds this method's `# Safety` section, which is the contract of
        // `unregister_qobject`; `self.id` is this object's own id.
        unsafe { unregister_qobject(self.id) };
    }

    /// Sets a dynamic property value (`QObject::setProperty` dynamic fallback).
    ///
    /// If `value` is `Variant::Invalid`:
    /// - If the property exists, it is removed and `true` (changed) is returned.
    /// - If the property does not exist, `false` (unchanged) is returned.
    ///
    /// If `value` is valid:
    /// - If the property already exists with the same value, `false` (unchanged) is returned.
    /// - Otherwise (new property or changed value), it is stored and `true` (changed) is returned.
    pub fn set_property(&mut self, name: &str, value: Variant) -> bool {
        if !value.is_valid() {
            return self.dynamic_properties.remove(name).is_some();
        }
        match self.dynamic_properties.get(name) {
            Some(existing) if existing == &value => false,
            _ => {
                self.dynamic_properties.insert(name.to_string(), value);
                true
            }
        }
    }

    /// Retrieves a dynamic property by name.
    pub fn property(&self, name: &str) -> Option<&Variant> {
        self.dynamic_properties.get(name)
    }

    /// Returns all registered dynamic property names.
    pub fn dynamic_property_names(&self) -> Vec<String> {
        self.dynamic_properties.keys().cloned().collect()
    }

    /// Swaps only property and name contents with another ObjectData instance.
    /// Strictly does NOT swap hierarchy (parent/children), ownership (owned_children/owned_by_parent),
    /// identity (id), liveness, thread_id, borrow_state, event filters, signals_blocked, timers,
    /// or delete_later_called.
    pub fn swap_properties(&mut self, other: &mut Self) {
        std::mem::swap(&mut self.object_name, &mut other.object_name);
        std::mem::swap(&mut self.dynamic_properties, &mut other.dynamic_properties);
    }

    /// Pure property swap alias. Hierarchy and ownership must be transferred via
    /// transactional operations that update child parent pointers and registry state.
    #[inline]
    pub fn swap_content(&mut self, other: &mut Self) {
        self.swap_properties(other);
    }
}

/// Scoped mutable proxy for ObjectData.
///
/// Encapsulates mutable access to an object's internal metadata without giving out a raw
/// `&mut ObjectData`, preventing arbitrary `std::mem::swap` from breaking identity,
/// registry and liveness invariants.
pub struct ObjectDataMut<'a> {
    pub(crate) data: &'a mut ObjectData,
}

impl<'a> ObjectDataMut<'a> {
    #[inline]
    pub fn new(data: &'a mut ObjectData) -> Self {
        Self { data }
    }

    /// Swaps only property and name contents with another ObjectDataMut proxy.
    #[inline]
    pub fn swap_properties(&mut self, other: &mut ObjectDataMut<'_>) {
        self.data.swap_properties(other.data);
    }

    /// Pure property swap alias.
    #[inline]
    pub fn swap_content(&mut self, other: &mut ObjectDataMut<'_>) {
        self.data.swap_content(other.data);
    }

    /// Transactionally swaps children hierarchy and ownership between two objects,
    /// synchronizing both owned and non-owned transferred children's local parent ID,
    /// parent liveness token, and the global object registry.
    ///
    /// # Locking
    /// - Before any lock: the registry records of both objects and of every child to move are
    ///   looked up, an [`ObjectBorrowGuard`] is taken for every child (a failed borrow returns
    ///   [`ReparentError::Busy`] at once, nothing waits for one), and the handles to the data the
    ///   commit writes are obtained from the objects' accessors (their own code, which must not run
    ///   under a lock).
    /// - `HIERARCHY_MUTATION`, then Level 0 (the registry, read). The cycle checks run against that
    ///   registry, under both, so nothing that adds a parent link can slip in before the commit.
    ///   The records looked up earlier must still be the registered ones.
    /// - Level 1: both objects' `children` write locks in ascending `ObjectId` order. The registry's
    ///   lists are compared with the objects' own lists only now, under these write locks, so a list
    ///   that changed since the lookup is refused instead of overwritten.
    /// - Level 2: the `parent` write locks of the children in ascending `ObjectId` order, each
    ///   checked to name the object it is being moved away from.
    /// - On every exit, the locks go first (Level 2, Level 1, Level 0), then `HIERARCHY_MUTATION`,
    ///   and the borrow guards last: dropping one takes the `pending_child_changes` mutex and can
    ///   free parked children, which takes the registry for writing.
    ///
    /// # Atomicity Boundary
    /// During the commit phase, all write locks and borrow guards are held simultaneously, guaranteeing
    /// that intermediate states cannot be observed via synchronized cross-object snapshot queries
    /// (such as [`query_object_parents_snapshot`]). Note that independent single-record queries
    /// (such as [`query_object_parent`]) are protected individually by single locks, but do not provide
    /// cross-record snapshot atomicity.
    ///
    /// An `Err` leaves everything as it was, and no event is sent. `InvalidParent`: one object lists
    /// the other (that is answered first), an owned child is missing from its parent's list, a
    /// listed child is not registered, a ring would form, or the lists and links the objects keep
    /// disagree with the registry's. `DifferentThread`: the thread affinities of the two objects, or
    /// of a child and the object it moves to, do not fit, or a child is registered on another
    /// thread. `UnknownObject`: either object is not registered or is dead, or a child is dead.
    /// `Busy`: a child is borrowed by a running callback, or a lock is poisoned.
    pub fn swap_hierarchy_and_ownership(
        &mut self,
        other: &mut ObjectDataMut<'_>,
    ) -> Result<(), ReparentError> {
        let self_id = self.data.id;
        let other_id = other.data.id;
        if self_id == other_id {
            return Ok(());
        }

        // A parent that lists the other object, or is listed by it, cannot be swapped: the other
        // one would end up its own child. It is answered here, also because the data handles taken
        // below must not reach the object of the other argument, which the caller borrows.
        if self.data.children.contains(&other_id) || other.data.children.contains(&self_id) {
            return Err(ReparentError::InvalidParent);
        }

        // 1. Thread affinity validation: all children must match destination parent's thread.
        let self_thread = self.data.thread_id;
        let other_thread = other.data.thread_id;
        if self_thread != other_thread {
            for child in &self.data.owned_children {
                if owned_thread(child) != Some(other_thread) {
                    return Err(ReparentError::DifferentThread);
                }
            }
            for &child_id in &self.data.children {
                if in_different_thread(other_thread, child_id) {
                    return Err(ReparentError::DifferentThread);
                }
            }
            for child in &other.data.owned_children {
                if owned_thread(child) != Some(self_thread) {
                    return Err(ReparentError::DifferentThread);
                }
            }
            for &child_id in &other.data.children {
                if in_different_thread(self_thread, child_id) {
                    return Err(ReparentError::DifferentThread);
                }
            }
        }

        // 2. Owned children, by the ids their owners kept: each must be in its parent's list.
        let self_owned_set: std::collections::HashSet<ObjectId> = self
            .data
            .owned_children
            .iter()
            .map(OwnedObject::object_id)
            .collect();
        let other_owned_set: std::collections::HashSet<ObjectId> = other
            .data
            .owned_children
            .iter()
            .map(OwnedObject::object_id)
            .collect();
        for &owned_id in &self_owned_set {
            if !self.data.children.contains(&owned_id) {
                return Err(ReparentError::InvalidParent);
            }
        }
        for &owned_id in &other_owned_set {
            if !other.data.children.contains(&owned_id) {
                return Err(ReparentError::InvalidParent);
            }
        }

        #[derive(Clone, Copy, PartialEq, Eq)]
        enum TransferTarget {
            ToOther,
            ToSelf,
        }

        struct ChildTransfer {
            child_id: ObjectId,
            target: TransferTarget,
            record: Arc<ObjectRecord>,
            is_owned: bool,
        }

        // 3. The records involved. The registry lock is let go again; what is found here is
        // checked to be still registered once the locks below are held.
        let (self_record, other_record, mut transfers) = {
            let registry = GLOBAL_OBJECT_REGISTRY
                .read()
                .map_err(|_| ReparentError::Busy)?;
            let self_record =
                Arc::clone(registry.get(&self_id).ok_or(ReparentError::UnknownObject)?);
            let other_record = Arc::clone(
                registry
                    .get(&other_id)
                    .ok_or(ReparentError::UnknownObject)?,
            );
            if !self_record.liveness.load(Ordering::Acquire)
                || !other_record.liveness.load(Ordering::Acquire)
            {
                return Err(ReparentError::UnknownObject);
            }
            let mut transfers: Vec<ChildTransfer> =
                Vec::with_capacity(self.data.children.len() + other.data.children.len());
            for (children, target, owned_set) in [
                (
                    &self.data.children,
                    TransferTarget::ToOther,
                    &self_owned_set,
                ),
                (
                    &other.data.children,
                    TransferTarget::ToSelf,
                    &other_owned_set,
                ),
            ] {
                for &child_id in children {
                    // A child that the object lists but the registry does not hold: the two disagree.
                    let record = registry
                        .get(&child_id)
                        .ok_or(ReparentError::InvalidParent)?;
                    if !record.liveness.load(Ordering::Acquire) {
                        return Err(ReparentError::UnknownObject);
                    }
                    transfers.push(ChildTransfer {
                        child_id,
                        target,
                        record: Arc::clone(record),
                        is_owned: owned_set.contains(&child_id),
                    });
                }
            }
            (self_record, other_record, transfers)
        };

        // Sort all child transfers by child_id for a lock acquisition order that is the same for everyone.
        transfers.sort_by_key(|t| t.child_id);
        if transfers.windows(2).any(|w| w[0].child_id == w[1].child_id) {
            return Err(ReparentError::InvalidParent);
        }

        // 4. Borrows and data handles, before any lock. The borrow guards are declared before the
        // registry guard and the locks below, so they are dropped after all of them, on every exit.
        let mut borrow_guards: Vec<ObjectBorrowGuard> = Vec::with_capacity(transfers.len());
        for t in &transfers {
            borrow_guards.push(borrow_record(&t.record)?);
        }
        // A non-owned child is written through its borrow guard; an owned child through its owner,
        // as the parent holds it. Each object gets one handle, so no two `&mut` reach it.
        let mut child_datas: Vec<Option<ObjectDataMut<'_>>> = borrow_guards
            .iter_mut()
            .zip(&transfers)
            .map(|(guard, t)| (!t.is_owned).then(|| guard.object_data_mut()))
            .collect();
        let mut self_owned_datas: Vec<ObjectDataMut<'_>> = self
            .data
            .owned_children
            .iter_mut()
            // SAFETY: every owned child is listed in `transfers`, and its borrow guard in
            // `borrow_guards` holds its borrow flag, so no callback of it runs and no other
            // reference to it is live.
            .map(|child| unsafe { child.as_mut_unchecked() }.object_data_mut())
            .collect();
        let mut other_owned_datas: Vec<ObjectDataMut<'_>> = other
            .data
            .owned_children
            .iter_mut()
            // SAFETY: as above.
            .map(|child| unsafe { child.as_mut_unchecked() }.object_data_mut())
            .collect();

        // 5. From here on no accessor and no handler runs.
        let _hierarchy = lock_hierarchy();
        let registry = GLOBAL_OBJECT_REGISTRY
            .read()
            .map_err(|_| ReparentError::Busy)?;
        let registered = |id: ObjectId, record: &Arc<ObjectRecord>| {
            registry
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, record))
        };
        if !registered(self_id, &self_record) || !registered(other_id, &other_record) {
            return Err(ReparentError::UnknownObject);
        }
        if transfers.iter().any(|t| !registered(t.child_id, &t.record)) {
            return Err(ReparentError::InvalidParent);
        }

        // Cycle check: neither object can be a descendant of the other, and no child being
        // transferred can create a cycle with its target parent.
        if would_create_cycle_in(&registry, self_id, other_id)?
            || would_create_cycle_in(&registry, other_id, self_id)?
        {
            return Err(ReparentError::InvalidParent);
        }
        for &child_id in &self.data.children {
            if would_create_cycle_in(&registry, child_id, other_id)? {
                return Err(ReparentError::InvalidParent);
            }
        }
        for &child_id in &other.data.children {
            if would_create_cycle_in(&registry, child_id, self_id)? {
                return Err(ReparentError::InvalidParent);
            }
        }

        #[cfg(test)]
        test_hooks::run(test_hooks::Point::AfterCycleCheck);

        // Tier 1: the parents' `children` write locks in ascending ObjectId order.
        let (mut self_children_guard, mut other_children_guard) = if self_id < other_id {
            let s = self_record
                .children
                .write()
                .map_err(|_| ReparentError::Busy)?;
            let o = other_record
                .children
                .write()
                .map_err(|_| ReparentError::Busy)?;
            (s, o)
        } else {
            let o = other_record
                .children
                .write()
                .map_err(|_| ReparentError::Busy)?;
            let s = self_record
                .children
                .write()
                .map_err(|_| ReparentError::Busy)?;
            (s, o)
        };
        // Only a writer of these lists could change them, and none can now.
        if *self_children_guard != self.data.children
            || *other_children_guard != other.data.children
        {
            return Err(ReparentError::InvalidParent);
        }

        // Tier 2: the children's `parent` write locks in ascending child_id order.
        let mut child_parent_guards: Vec<std::sync::RwLockWriteGuard<'_, Option<ObjectId>>> =
            Vec::with_capacity(transfers.len());
        for t in &transfers {
            let guard = t.record.parent.write().map_err(|_| ReparentError::Busy)?;
            let named = match t.target {
                TransferTarget::ToOther => self_id,
                TransferTarget::ToSelf => other_id,
            };
            if *guard != Some(named) {
                return Err(ReparentError::InvalidParent);
            }
            child_parent_guards.push(guard);
        }

        let self_liveness = Arc::clone(&self_record.liveness);
        let other_liveness = Arc::clone(&other_record.liveness);

        // 6. Infallible Atomic Commit:
        // All checks succeeded, all borrow guards and write locks are held.
        // No step from here can fail, and none runs code of an object.

        // Step A: Update non-owned children's local ObjectData.
        for (t, data) in transfers.iter().zip(child_datas.iter_mut()) {
            if let Some(data) = data {
                let raw = data.raw_data_mut();
                match t.target {
                    TransferTarget::ToOther => {
                        raw.parent = Some(other_id);
                        raw.parent_liveness = Some((other_id, Arc::clone(&other_liveness)));
                    }
                    TransferTarget::ToSelf => {
                        raw.parent = Some(self_id);
                        raw.parent_liveness = Some((self_id, Arc::clone(&self_liveness)));
                    }
                }
            }
        }

        // Step B: Update owned children's local ObjectData via their respective parents' owned_children.
        for data in &mut self_owned_datas {
            let raw = data.raw_data_mut();
            raw.parent = Some(other_id);
            raw.parent_liveness = Some((other_id, Arc::clone(&other_liveness)));
        }
        for data in &mut other_owned_datas {
            let raw = data.raw_data_mut();
            raw.parent = Some(self_id);
            raw.parent_liveness = Some((self_id, Arc::clone(&self_liveness)));
        }

        // Step C: Update Registry `record.parent` for all children under the held write locks.
        for (t, guard) in transfers.iter().zip(child_parent_guards.iter_mut()) {
            let new_parent = match t.target {
                TransferTarget::ToOther => other_id,
                TransferTarget::ToSelf => self_id,
            };
            store_link(&registry, &t.record, guard, Some(new_parent));
        }

        // Step D: Update Registry `record.children` for both parents under the held write locks.
        *self_children_guard = other.data.children.clone();
        *other_children_guard = self.data.children.clone();

        // Step E: Atomic local vector swap.
        std::mem::swap(&mut self.data.children, &mut other.data.children);
        std::mem::swap(
            &mut self.data.owned_children,
            &mut other.data.owned_children,
        );

        // Commit complete. When returning Ok(()), all guards drop cleanly and publish the changes atomically.
        Ok(())
    }

    #[inline]
    pub fn install_event_filter(&mut self, filter: ObjectId) {
        self.data.install_event_filter(filter);
    }

    #[inline]
    pub fn remove_event_filter(&mut self, filter: ObjectId) {
        self.data.remove_event_filter(filter);
    }

    #[inline]
    pub fn set_object_name(&mut self, name: &str) {
        self.data.set_object_name(name);
    }

    #[inline]
    pub fn set_property(&mut self, name: &str, value: Variant) -> bool {
        self.data.set_property(name, value)
    }

    #[inline]
    pub(crate) fn set_thread_id(&mut self, thread_id: ThreadId) {
        self.data.set_thread_id(thread_id);
    }

    #[inline]
    pub(crate) fn clear_parent(&mut self) {
        self.data.clear_parent();
    }

    #[inline]
    pub(crate) fn add_child(&mut self, child: ObjectId) {
        self.data.add_child(child);
    }

    #[inline]
    pub(crate) fn remove_child(&mut self, child: ObjectId) {
        self.data.remove_child(child);
    }

    /// See [`set_parent`]. Panics as [`set_parent`] documents for its `ChildRemoved` handler.
    #[inline]
    pub fn set_parent(&mut self, parent: Option<ObjectId>) -> Result<(), ReparentError> {
        self.data.set_parent(parent)
    }

    #[inline]
    pub(crate) fn add_timer(&mut self, id: crate::timer::TimerId) {
        self.data.timers.push(id);
    }

    /// Re-labels a moved timer that the target thread registered under a fresh id.
    pub(crate) fn replace_timer(&mut self, old: crate::timer::TimerId, new: crate::timer::TimerId) {
        if let Some(slot) = self.data.timers.iter_mut().find(|t| **t == old) {
            *slot = new;
        }
    }

    #[inline]
    pub(crate) fn remove_timer(&mut self, id: crate::timer::TimerId) -> bool {
        let before = self.data.timers.len();
        self.data.timers.retain(|&t| t != id);
        self.data.timers.len() != before
    }

    #[inline]
    pub(crate) fn raw_data_mut(&mut self) -> &mut ObjectData {
        self.data
    }

    #[inline]
    pub(crate) fn owned_children_mut(&mut self) -> &mut Vec<OwnedObject<dyn QObject>> {
        &mut self.data.owned_children
    }

    #[inline]
    pub(crate) fn into_owned_children_mut(self) -> &'a mut Vec<OwnedObject<dyn QObject>> {
        &mut self.data.owned_children
    }

    /// Takes the owner of the child `id` out of the list, and the id out of `children`. Nothing
    /// changes when this object does not own `id`.
    pub(crate) fn take_owned_child(&mut self, id: ObjectId) -> Option<OwnedObject<dyn QObject>> {
        let index = self
            .data
            .owned_children
            .iter()
            .position(|c| OwnedObject::object_id(c) == id)?;
        self.data.children.retain(|&c| c != id);
        Some(self.data.owned_children.remove(index))
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn timers_mut(&mut self) -> &mut Vec<crate::timer::TimerId> {
        &mut self.data.timers
    }

    #[inline]
    pub(crate) fn children_mut(&mut self) -> &mut Vec<ObjectId> {
        &mut self.data.children
    }
}

impl<'a> std::ops::Deref for ObjectDataMut<'a> {
    type Target = ObjectData;
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl Drop for ObjectData {
    fn drop(&mut self) {
        // The object is gone from this first step on: the token goes false before the parent is
        // told or a child is deleted, so a signal of this object that is emitted by then reaches
        // no slot. Qt `~QObject` disconnects every receiver before `deleteChildren`
        // (qobject.cpp:1086-1178), and `QPointer` is cleared before anything else is destroyed.
        self.liveness.store(false, Ordering::Release);
        self.borrow_state
            .memory_destroyed
            .store(true, Ordering::Release);
        // `destroyed` goes out before the parent is told or a child is deleted (Qt `~QObject`,
        // qobject.cpp:1086-1105). The object is dead by now: the token is false and the memory
        // counts as destroyed, so no slot reaches it through the registry.
        self.borrow_state.emit_destroyed_once(self.id);
        // 1. If we have a parent, unlink from parent and notify it. A panic from a filter or
        // `event()` during the ChildRemoved send is held until the teardown below has run, then
        // resumed (swallowed if the thread is already unwinding, since a second panic aborts).
        let mut held_panic: Option<Box<dyn std::any::Any + Send>> = None;
        if let Some(parent_id) = self.parent() {
            if registered_and_borrowed(parent_id) {
                // The parent's borrow is held, so its child lists cannot be edited now. The
                // removal is queued and the event is posted, not dropped.
                edit_child_list(parent_id, ChildListChange::Remove(self.id));
                crate::event_loop::post_event_to_thread(
                    ThreadId::current(),
                    parent_id,
                    Event::new(EventKind::ChildRemoved { child_id: self.id }),
                );
            } else {
                with_object_mut(parent_id, |parent_obj| {
                    let mut pdata = parent_obj.object_data_mut();
                    pdata.children_mut().retain(|&id| id != self.id);
                    pdata
                        .owned_children_mut()
                        .retain(|c| OwnedObject::object_id(c) != self.id);
                });
                let mut ev = Event::new(EventKind::ChildRemoved { child_id: self.id });
                held_panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::event_loop::notify_helper(parent_id, &mut ev);
                }))
                .err();
            }
        }
        // 2. Cascade deletion to all children (deleteChildren)
        self.delete_children();

        // 3. Advance the generation for QPointer / GenerationalId safety
        self.generation = self.generation.wrapping_add(1);

        // SAFETY: `ObjectData::drop` runs only when the object's memory is being freed, and that
        // happens only after the owner won the borrow flag or the guard of the running callback
        // returned (`destroy_here`, `run_parked`), or, for an object without an `OwnedObject`, under
        // the registration contract (no callback runs while the owner drops it). So no callback
        // runs, and `unregister_qobject` is memory-neutral: it works from the record and ids.
        unsafe { unregister_qobject(self.id) };

        if let Some(payload) = held_panic
            && !std::thread::panicking()
        {
            std::panic::resume_unwind(payload);
        }
    }
}

/// Marks an object for deferred deletion (`QObject::deleteLater`, qobject.cpp:2511-2565).
///
/// Debounced per object: the first request posts one `DeferredDelete` and later requests are
/// ignored (qobject.cpp:2531-2534). A request from the owning thread is stamped with that
/// thread's loop and handler-scope levels; scope 0 is promoted to 1 while a loop runs
/// (qobject.cpp:2539-2559). A request from another thread is stamped (0, 0).
pub fn delete_later(obj: &ObjectData) {
    let Some(owner) = query_object_thread(obj.id) else {
        return;
    };
    if obj.delete_later_called.swap(true, Ordering::SeqCst) {
        return;
    }
    let (loop_level, scope_level) = if owner == ThreadContext::current_id() {
        let loop_level = ThreadContext::current_loop_level();
        let scope_level = ThreadContext::current_scope_level();
        if scope_level == 0 && loop_level != 0 {
            (loop_level, 1)
        } else {
            (loop_level, scope_level)
        }
    } else {
        (0, 0)
    };
    crate::event_loop::post_event_to_thread(
        owner,
        obj.id,
        Event::new(EventKind::DeferredDelete {
            loop_level,
            scope_level,
        }),
    );
}

/// Event filter callback (`QObject::eventFilter`, `qobject.cpp:2407-2423`).
pub fn install_event_filter(target: &mut ObjectData, filter: ObjectId) -> bool {
    // Cross-thread check: filter and target must be in the same thread (qobject.cpp:2412-2415)
    if let Some(filter_thread) = query_object_thread(filter)
        && filter_thread != target.thread_id
    {
        return false;
    }

    target.event_filters.install(filter);
    true
}

pub fn remove_event_filter(target: &mut ObjectData, filter: ObjectId) {
    target.event_filters.remove(filter);
}
