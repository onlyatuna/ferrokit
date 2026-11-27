// SPDX-License-Identifier: LGPL-3.0-only
use super::*;

/// Why a parent change was refused. Nothing is changed when an error is returned, except for
/// [`ReparentError::DifferentThread`] from [`set_parent`] and [`set_parent_by_id`], where
/// the child is detached from its old parent (matching Qt `setParent(nullptr)` detach semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReparentError {
    /// [`set_parent`]: the object is held by its parent's `owned_children`. Detaching it there
    /// would destroy it (or leave the caller's `&mut` dangling); use [`reparent_owned_object`].
    OwnedByParent,
    /// [`reparent_owned_object`]: the object has no parent that owns it, or that parent is not
    /// registered on the calling thread.
    NotOwned,
    /// The object is not registered, is dead, or is registered on another thread. For
    /// [`add_owned_child`] the object is the new parent; for
    /// [`ObjectDataMut::swap_hierarchy_and_ownership`] it is either of the two objects, or one of
    /// the children that move (dead).
    UnknownObject,
    /// [`set_parent`]: the new parent is the object itself or one of its descendants, or the
    /// child is registered and the new parent is not (two unregistered objects can be linked: only
    /// the child's own link is recorded). A new parent of another thread affinity gets
    /// `DifferentThread` instead of the second case. [`reparent_owned_object`]: the new parent is the child itself, or is
    /// not registered on the calling thread, or (checked last) is a descendant of the child.
    /// [`add_owned_child`]: the child has a parent already, or the parent is the child itself or
    /// one of its descendants. [`ObjectDataMut::swap_hierarchy_and_ownership`]: one object is
    /// below the other, the lists the objects keep disagree with the registry's, or a listed child
    /// is not registered.
    InvalidParent,
    /// The object or a parent is borrowed by an active callback, or a lock that guards a link or
    /// a list of the hierarchy is poisoned. Nothing was changed.
    Busy,
    /// The new parent belongs to a different thread than the child: their thread affinities
    /// (`ObjectData::thread_id`) differ, and a parent and its children share one thread. What is
    /// left behind depends on the call:
    /// - [`set_parent`] and [`set_parent_by_id`] do what Qt's `setParent` does. The child is removed
    ///   from its old parent in both the registry and local data, the old parent receives `ChildRemoved`,
    ///   the child is left without a parent (`parent() == None`), and Qt's warning goes to stderr.
    ///   This detach behavior is an explicit, documented exception to the general "zero state mutation
    ///   on failure" rule of other reparent errors.
    /// - [`reparent_owned_object`] changes nothing and prints nothing: the child stays with its old
    ///   parent, which still owns it. Qt would detach the child.
    /// - [`add_owned_child`] attaches nothing and prints nothing, and hands the `Box` back: the
    ///   child has no parent and the parent's children are as they were. Qt's constructor leaves
    ///   the new object without a parent in the same way, but warns.
    DifferentThread,
}

/// Lets go of a child that its parent does not own: it survives the parent, loses its
/// parent link, and is unregistered. A listed object whose own link names another parent, or none,
/// is not a child of `parent_id` (the safe `reparent` edits the objects' own links and lists
/// separately, so a list can name an object that was never linked to this parent) and is left
/// alone.
pub(super) fn release_child(child_id: ObjectId, parent_id: ObjectId) {
    // The link is cleared before the unregister below, after which the child can no longer be
    // reached. A child that cannot be borrowed here (a running callback holds it, or it was never
    // registered) is not written to; its `parent()` sees the parent's liveness token instead, and
    // it is let go.
    let belongs_elsewhere = with_object_mut(child_id, |child| {
        let mut data = child.object_data_mut();
        if data.raw_data_mut().parent == Some(parent_id) {
            data.clear_parent();
            false
        } else {
            true
        }
    });
    if belongs_elsewhere == Some(true) {
        return;
    }
    if belongs_elsewhere.is_none()
        && let Some((child_record, _)) = registered_ptr(child_id)
    {
        // The child's own link cannot be read, but the registry's can: another parent there
        // means the entry is stale. (A link already cleared, as `unregister_qobject` of the
        // dying parent does before its object is freed, is this parent's child all the same.)
        let linked_to = *read_ignoring_poison(&child_record.parent);
        if linked_to.is_some_and(|linked| linked != parent_id) {
            return;
        }
        *lock_ignoring_poison(&child_record.pending_parent_sync) = Some((None, None));
        child_record
            .has_pending_parent_sync
            .store(true, Ordering::Release);
    }
    // SAFETY: `unregister_qobject` works from the registry record and ids and never reads or
    // forms a reference to the object's memory. `child_id` is registered, so its memory is valid
    // until it is unregistered (the registration contract); a callback still running in it keeps
    // its own `Arc` to the record, and whoever owns the child frees it only after that callback
    // returns (`destroy_when_released`, `run_parked`). A child shares its parent's thread
    // (`set_parent` refuses a parent of another thread affinity), and this runs on that thread.
    unsafe { unregister_qobject(child_id) };
}

/// The children the registry lists for `id` that are still registered and that the registry says
/// are `id`'s: a listed child whose own record names another parent (or none) is a stale entry
/// (the safe `reparent` edits the objects' own links only, so lists can name objects that were
/// never linked in the registry) and is not a child to destroy or move.
pub(super) fn registered_children_of(id: ObjectId) -> Vec<ObjectId> {
    // Reached from the cleanup of a destroyed object, which cannot report a failure and must not
    // skip its children: a poisoned lock is read as it is (see `read_ignoring_poison`).
    let registry = read_ignoring_poison(&GLOBAL_OBJECT_REGISTRY);
    let Some(record) = registry.get(&id) else {
        return Vec::new();
    };
    let children = read_ignoring_poison(&record.children);
    children
        .iter()
        .copied()
        .filter(|child_id| {
            registry
                .get(child_id)
                .is_some_and(|child| *read_ignoring_poison(&child.parent) == Some(id))
        })
        .collect()
}

/// Whether `parent` belongs to a different thread than `child_thread`. An object whose thread is
/// unknown (not built through `ObjectData`, or already unregistered) is not judged to differ.
pub(super) fn in_different_thread(child_thread: ThreadId, parent: ObjectId) -> bool {
    query_object_thread(parent).is_some_and(|thread| thread != child_thread)
}

/// The liveness token of a registered object, or `None` if `id` is not registered.
pub(super) fn liveness_of(id: ObjectId) -> Option<Arc<AtomicBool>> {
    let registry = GLOBAL_OBJECT_REGISTRY.read().ok()?;
    registry.get(&id).map(|record| Arc::clone(&record.liveness))
}

/// The record the registry holds for `id`. The registry read lock is taken and let go inside, so
/// the caller must not hold it.
pub(super) fn record_of(id: ObjectId) -> Result<Option<Arc<ObjectRecord>>, ReparentError> {
    let registry = GLOBAL_OBJECT_REGISTRY
        .read()
        .map_err(|_| ReparentError::Busy)?;
    Ok(registry.get(&id).cloned())
}

/// Whether making `new_parent` the parent of `child_id` would make an object its own ancestor:
/// `new_parent` is the child itself or one of its descendants, as the registry records them.
///
/// The caller holds `HIERARCHY_MUTATION` and a lock on the registry, and passes the registry it
/// holds: a second read lock taken by the same thread waits for ever when a writer has queued
/// between the two. While `HIERARCHY_MUTATION` is held no other operation adds a parent link, so
/// the chain walked here does not grow before the caller commits; links removed meanwhile only
/// shorten it.
///
/// A walk that does not end within one step per registered object means the registry already
/// holds a cycle; the answer is then `true`, so that nothing is linked into it.
pub(super) fn would_create_cycle_in(
    registry: &HashMap<ObjectId, Arc<ObjectRecord>>,
    child_id: ObjectId,
    new_parent: ObjectId,
) -> Result<bool, ReparentError> {
    let mut cursor = Some(new_parent);
    for _ in 0..=registry.len() {
        let Some(id) = cursor else { return Ok(false) };
        if id == child_id {
            return Ok(true);
        }
        let Some(record) = registry.get(&id) else {
            return Ok(false);
        };
        let parent_guard = record.parent.read().map_err(|_| ReparentError::Busy)?;
        cursor = *parent_guard;
    }
    Ok(true)
}

/// `start` and the ids above it, as the registry records them. The chain ends at an id that is
/// not registered or that was already seen. A poisoned link is read as it is.
pub(super) fn ancestors_in(
    registry: &HashMap<ObjectId, Arc<ObjectRecord>>,
    start: Option<ObjectId>,
) -> Vec<ObjectId> {
    let mut chain = Vec::new();
    let mut cursor = start;
    while let Some(id) = cursor {
        if chain.contains(&id) {
            break;
        }
        chain.push(id);
        cursor = registry
            .get(&id)
            .and_then(|record| *read_ignoring_poison(&record.parent));
    }
    chain
}

/// Sets a non-owned child's parent and updates registered relationship metadata, sending
/// `ChildRemoved`/`ChildAdded` to the registered parents.
///
/// Refuses with [`ReparentError::OwnedByParent`] when a parent owns the child; use
/// [`reparent_owned_object`] for those. Qt's `setParent_helper` never destroys
/// the child; here the borrow `child` points into the object the parent owns, so ownership has to
/// move through an id-based call.
///
/// Refuses with [`ReparentError::InvalidParent`], changing nothing, when `new_parent` is the
/// child itself or one of its descendants. Qt's release build accepts that and builds a cycle,
/// which Qt documents as undefined behavior.
///
/// Thread affinity: the child's thread and the new parent's thread (`ObjectData::thread_id`, as
/// [`move_to_thread`](crate::object::move_to_thread) changes it) must be the same, because a parent and its children share one
/// thread. Otherwise, as in Qt (line numbers differ between Qt versions,
/// these were checked against Qt 6.11.2's), the child is removed from its old parent,
/// which receives `ChildRemoved`, and is left without a parent; a warning goes to stderr, and this
/// returns [`ReparentError::DifferentThread`]. Only the two affinities are compared. The calling
/// thread is not checked, but a registered child must still be used on the thread it is registered
/// on (see the thread rules in [`crate::object`]). [`reparent_owned_object`] and [`add_owned_child`] refuse the same
/// pairs without detaching anything and without the warning; see [`ReparentError::DifferentThread`].
///
/// The parent does not own the child: whoever does keeps the child alive and frees it.
/// Dropping the parent does not destroy such a child (Qt deletes it). The parent unregisters its
/// children (see [`unregister_qobject`] for what that ends), including a child attached while a
/// running callback borrowed the parent. [`ObjectData::parent`] of the child returns `None` once
/// the parent has been dropped or unregistered, provided the parent was registered when this was
/// called. That holds for a child that a running callback borrows, or that was never registered,
/// too.
///
/// Limits of that guarantee:
/// - In the cases just named, where the parent's drop cannot write to the child, the child's own
///   link can keep the old id. Read the parent through `parent()`, which ignores a link whose
///   parent is dead.
/// - While a running callback holds the parent's borrow, the parent's own `children()` still
///   shows the list as it was; the change is applied when that callback returns. The registry
///   has it at once, so dropping the parent reaches the child either way.
///
/// `new_parent` need not be registered when the child is not either: the id is accepted and only
/// the child's own link is recorded, and the parent's child list, `ChildAdded` and the death
/// tracking above, which need a registered parent, do not happen. A registered child is refused
/// with [`ReparentError::InvalidParent`], changing nothing: `set_parent` does not link a
/// registered object to an id the registry does not hold. (An object that was linked to an
/// unregistered parent while it was unregistered keeps that link when it is registered; the
/// registry then has it on the object's record only, and not in the parent's list.) A new parent
/// of another thread affinity gets [`ReparentError::DifferentThread`] instead, registered or not.
/// Qt has no such case (a `QObject*` is always a live object), so nothing more is specified for
/// it. [`reparent_owned_object`] and [`add_owned_child`] refuse an unregistered parent.
///
/// # Events and what they see
/// The registry is changed completely before the first event. From `ChildRemoved` on,
/// [`query_object_parent`], [`query_object_children`] and the registry's lists show the finished
/// change. What the objects keep themselves follows Qt's order (`setParent_helper` removes the
/// child from the old parent's list, sends `ChildRemoved`, and only then changes the child's
/// parent and the new parent's list): the old parent's own `children()` has lost the child when it
/// gets `ChildRemoved`; the child's own `parent()` and the new parent's own `children()` change
/// after that handler has returned, and `ChildAdded` sees all of it.
///
/// An object whose borrow is held by a running callback has its list change applied when that
/// callback returns, and its event is posted to the thread's event queue: the next event
/// processing delivers it, not this call. An event that is delivered at once (to the other
/// parent, when its borrow is not held) can therefore come before a posted one. The events go to
/// the receiver directly, so its object event filters do not see them, unlike those of
/// [`reparent_owned_object`] and [`add_owned_child`].
///
/// If the `ChildRemoved` handler panics, the registry hierarchy lists and child parent link in
/// the registry are repaired prior to `resume_unwind`, and `ChildAdded` is suppressed. If the
/// child's borrow was not retained across the panic, its local `ObjectData` parent is also
/// synchronized prior to `resume_unwind`; however, if an `ObjectBorrowGuard` on the child remains
/// alive across the panic, local parent data synchronization is deferred until that guard drops,
/// rather than being applied prior to `resume_unwind`.
pub(super) struct ReparentGuard<'a>(&'a AtomicBool);
impl<'a> Drop for ReparentGuard<'a> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(super) struct ShortTermBorrowGuard<'a>(&'a AtomicBool);
impl<'a> Drop for ShortTermBorrowGuard<'a> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
/// Status of post-commit synchronization of local parent data with the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostCommitSyncStatus {
    /// Synchronized directly to local ObjectData via with_object_mut.
    AppliedDirectly,
    /// Direct borrow could not be acquired (held by active callback); queued in ObjectRecord
    /// for deterministic application upon release of that borrow.
    DeferredToPending,
    /// Object is unregistered or dead; no local synchronization needed.
    DeadOrUnregistered,
}

pub(crate) fn sync_child_local_parent(
    _child_id: ObjectId,
    child_record: &Arc<ObjectRecord>,
    current_reg_parent: Option<ObjectId>,
    target_liveness: Option<(ObjectId, Arc<AtomicBool>)>,
) -> PostCommitSyncStatus {
    if child_record
        .borrow_state
        .memory_destroyed
        .load(Ordering::Acquire)
    {
        return PostCommitSyncStatus::DeadOrUnregistered;
    }
    if child_record
        .borrow_state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let _release = ReleaseBorrowFlag(&child_record.borrow_state);
        // SAFETY: the CAS above made the borrow flag ours (`_release` frees it), so no callback of
        // the child runs and none can start. `memory_destroyed` was false, and the memory is
        // freed only by `destroy_here`, which runs on the registration thread (this thread:
        // `set_parent_by_id` got the record from `registered_ptr`, which checks it) and frees
        // only after it wins the flag we hold. The owner makes no direct use of the object
        // between callbacks (registration contract), so the `&mut` borrowed here has no alias.
        let mut data = unsafe { (*child_record.ptr.0).object_data_mut() };
        let raw = data.raw_data_mut();
        raw.parent = current_reg_parent;
        raw.parent_liveness = target_liveness;
        *lock_ignoring_poison(&child_record.pending_parent_sync) = None;
        child_record
            .has_pending_parent_sync
            .store(false, Ordering::Release);
        PostCommitSyncStatus::AppliedDirectly
    } else {
        *lock_ignoring_poison(&child_record.pending_parent_sync) =
            Some((current_reg_parent, target_liveness));
        child_record
            .has_pending_parent_sync
            .store(true, Ordering::Release);
        PostCommitSyncStatus::DeferredToPending
    }
}

/// Returns whether a registered object has a pending post-commit parent sync queued.
pub fn query_pending_parent_sync(child_id: ObjectId) -> Option<bool> {
    registered_ptr(child_id).map(|(rec, _)| rec.has_pending_parent_sync.load(Ordering::Acquire))
}

/// Sets a registered object's parent by its [`ObjectId`].
///
/// This id-based interface eliminates `&mut ObjectData` aliasing across callbacks,
/// and rejects conflicting nested hierarchy mutations on the same object with [`ReparentError::Busy`].
///
/// # Thread affinity and DifferentThread detach exception
///
/// When `new_parent` belongs to a different thread affinity than `child_id`, `set_parent_by_id`
/// follows Qt `setParent` semantics: the child is detached from its old parent in both registry
/// and local data (the old parent receives `ChildRemoved`), left parentless (`parent() == None`),
/// a warning is printed to stderr, and `Err(ReparentError::DifferentThread)` is returned.
/// This detach behavior is an explicit, documented exception to the general "zero state mutation
/// on failure" guarantee of other reparent errors.
///
/// # Post-commit synchronization guarantee
///
/// Once registry and hierarchy write locks are committed, `set_parent_by_id` synchronizes local
/// child and parent data without post-commit `Busy` aborts. If the child is actively borrowed by
/// an ongoing callback, the parent update is queued via a pending sync record and applied
/// deterministically upon release of that borrow.
///
/// If a `ChildRemoved` handler panics while retaining an `ObjectBorrowGuard` on the child,
/// the global registry and hierarchy lists are repaired before `resume_unwind`, but local
/// parent data synchronization remains pending and is deferred until that `ObjectBorrowGuard`
/// drops.
///
/// # Panics
///
/// If a `ChildRemoved` handler on the old parent panics, the panic is caught and rethrown with
/// `std::panic::resume_unwind` and the original payload. Before the rethrow, the registry and the
/// hierarchy lists are repaired so that the child is in its new parent's list and no longer in the
/// old one. `ChildAdded` is **not** sent to the new parent in this case; this is the public contract
/// (see issue #21). The new parent receives no event for this call.
///
/// If an `ObjectBorrowGuard` on the child is still alive across the panic, the local `ObjectData`
/// parent sync stays pending and is applied when that guard drops.
///
/// A panic from a custom `QObject::object_data_mut()` during the pending-sync application in
/// `ObjectBorrowGuard::drop` loses the queued items; this is a known limitation.
pub fn set_parent_by_id(
    child_id: ObjectId,
    new_parent: Option<ObjectId>,
) -> Result<(), ReparentError> {
    let (child_record, _) = registered_ptr(child_id).ok_or(ReparentError::UnknownObject)?;
    if child_record
        .reparenting
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(ReparentError::Busy);
    }
    let _reparent_guard = ReparentGuard(&child_record.reparenting);

    if child_record
        .borrow_state
        .borrow_flag
        .load(Ordering::Acquire)
    {
        return Err(ReparentError::Busy);
    }

    let old_parent = *child_record
        .parent
        .read()
        .map_err(|_| ReparentError::Busy)?;
    if old_parent == new_parent {
        return Ok(());
    }

    let child_owned = with_object(child_id, |c| c.object_data().is_owned_by_parent())
        .ok_or(ReparentError::Busy)?;
    if child_owned {
        return Err(ReparentError::OwnedByParent);
    }

    if let Some(parent_id) = old_parent {
        // A parent whose callback is running cannot be read; its list of owned children is not
        // needed then: an owned child was refused above by its own flag, and the edit of the
        // parent's `children` list is queued until that callback returns, as for `set_parent`.
        let parent_owns = match with_object(parent_id, |p| {
            p.object_data()
                .owned_children
                .iter()
                .any(|c| OwnedObject::object_id(c) == child_id)
        }) {
            Some(owns) => owns,
            None if registered_and_borrowed(parent_id) => false,
            None => return Err(ReparentError::Busy),
        };
        if parent_owns {
            return Err(ReparentError::OwnedByParent);
        }
    }

    child_record
        .borrow_state
        .borrow_flag
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| ReparentError::Busy)?;
    let short_term_borrow = ShortTermBorrowGuard(&child_record.borrow_state.borrow_flag);

    let child_thread_id = *child_record
        .thread_id
        .read()
        .map_err(|_| ReparentError::Busy)?;
    let requested_parent = new_parent;
    let refused_thread =
        requested_parent.is_some_and(|np| in_different_thread(child_thread_id, np));
    let new_parent = if refused_thread {
        None
    } else {
        requested_parent
    };

    let hierarchy = lock_hierarchy();
    let registry = GLOBAL_OBJECT_REGISTRY
        .read()
        .map_err(|_| ReparentError::Busy)?;
    if let Some(np) = requested_parent
        && would_create_cycle_in(&registry, child_id, np)?
    {
        return Err(ReparentError::InvalidParent);
    }
    #[cfg(test)]
    test_hooks::run(test_hooks::Point::AfterCycleCheck);

    let old_rec = old_parent.and_then(|id| registry.get(&id));
    let new_rec = new_parent.and_then(|id| registry.get(&id));

    if new_parent.is_some() && new_rec.is_none() {
        return Err(ReparentError::InvalidParent);
    }

    enum ParentLockPair<'a> {
        None,
        SingleOld(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        SingleNew(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        Both {
            old_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
            new_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
        },
    }

    let parent_locks = match (old_parent, new_parent) {
        (Some(old_id), Some(new_id)) if old_id != new_id => {
            let old_r = old_rec.ok_or(ReparentError::InvalidParent);
            let new_r = new_rec.ok_or(ReparentError::InvalidParent);
            if let (Ok(o_rec), Ok(n_rec)) = (old_r, new_r) {
                if old_id < new_id {
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both {
                        old_guard: o_g,
                        new_guard: n_g,
                    }
                } else {
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both {
                        old_guard: o_g,
                        new_guard: n_g,
                    }
                }
            } else if let Ok(o_rec) = old_r {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else if let Ok(n_rec) = new_r {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (Some(_), _) => {
            if let Some(o_rec) = old_rec {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else {
                ParentLockPair::None
            }
        }
        (_, Some(_)) => {
            if let Some(n_rec) = new_rec {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (None, None) => ParentLockPair::None,
    };

    let child_parent_guard = child_record
        .parent
        .write()
        .map_err(|_| ReparentError::Busy)?;
    {
        let parent_locks = parent_locks;
        let mut child_parent_guard = child_parent_guard;
        match parent_locks {
            ParentLockPair::Both {
                mut old_guard,
                mut new_guard,
            } => {
                old_guard.retain(|&id| id != child_id);
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::SingleOld(mut old_guard) => {
                old_guard.retain(|&id| id != child_id);
            }
            ParentLockPair::SingleNew(mut new_guard) => {
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::None => {}
        }

        store_link(
            &registry,
            &child_record,
            &mut child_parent_guard,
            new_parent,
        );
    }

    let new_liveness =
        new_parent.and_then(|id| registry.get(&id).map(|rec| (id, Arc::clone(&rec.liveness))));
    drop(registry);
    drop(hierarchy);
    drop(short_term_borrow);
    let removed_panic = old_parent.and_then(|old_parent_id| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            edit_child_list(old_parent_id, ChildListChange::Remove(child_id));
            let removed = Event::new(EventKind::ChildRemoved { child_id });
            if let Err(mut removed) = defer_if_borrowed(old_parent_id, removed) {
                dispatch_to_object(old_parent_id, &mut removed);
            }
        }))
        .err()
    });

    let current_reg_parent = query_object_parent(child_id).flatten();
    let target_liveness = if current_reg_parent == new_parent {
        new_liveness
    } else {
        current_reg_parent.and_then(|id| liveness_of(id).map(|alive| (id, alive)))
    };
    let _sync_status =
        sync_child_local_parent(child_id, &child_record, current_reg_parent, target_liveness);
    if current_reg_parent != new_parent
        && let Some(np) = new_parent
        && let Ok(reg) = GLOBAL_OBJECT_REGISTRY.read()
        && let Some(rec) = reg.get(&np)
        && let Ok(mut ch) = rec.children.write()
    {
        ch.retain(|&id| id != child_id);
    }

    if let Some(target_parent_id) = current_reg_parent {
        edit_child_list(target_parent_id, ChildListChange::Add(child_id));
        if removed_panic.is_none() {
            let added = Event::new(EventKind::ChildAdded { child_id });
            if let Err(mut added) = defer_if_borrowed(target_parent_id, added) {
                dispatch_to_object(target_parent_id, &mut added);
            }
        }
    }
    if let Some(payload) = removed_panic {
        std::panic::resume_unwind(payload);
    }
    if refused_thread {
        eprintln!("QObject::setParent: Cannot set parent, new parent is in a different thread");
        return Err(ReparentError::DifferentThread);
    }
    Ok(())
}

/// Sets the parent of `child` in the local data and, through the registry, in the hierarchy.
///
/// # Panics
///
/// If the old parent's `ChildRemoved` handler panics, the change is finished and the panic is
/// rethrown without `ChildAdded`, as for [`set_parent_by_id`] (`# Panics`).
pub fn set_parent(
    child: &mut ObjectData,
    new_parent: Option<ObjectId>,
) -> Result<(), ReparentError> {
    let old_parent = child.parent();
    if old_parent == new_parent {
        return Ok(());
    }
    if child.owned_by_parent {
        return Err(ReparentError::OwnedByParent);
    }
    let child_id = child.id;
    let requested_parent = new_parent;
    let refused_thread =
        requested_parent.is_some_and(|np| in_different_thread(child.thread_id, np));
    let new_parent = if refused_thread {
        None
    } else {
        requested_parent
    };

    let child_rec = registered_ptr(child_id).map(|(rec, _)| rec);
    let _reparent_guard = if let Some(ref rec) = child_rec {
        if rec
            .reparenting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ReparentError::Busy);
        }
        Some(ReparentGuard(&rec.reparenting))
    } else {
        None
    };

    let short_term_borrow = if let Some(ref rec) = child_rec {
        rec.borrow_state
            .borrow_flag
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| ReparentError::Busy)?;
        Some(ShortTermBorrowGuard(&rec.borrow_state.borrow_flag))
    } else {
        None
    };

    // `HIERARCHY_MUTATION`, then the registry (L0). The cycle check and the commit below both
    // happen under the two, so no other operation can add a parent link between them.
    let hierarchy = lock_hierarchy();
    let registry = GLOBAL_OBJECT_REGISTRY
        .read()
        .map_err(|_| ReparentError::Busy)?;
    if let Some(np) = requested_parent
        && would_create_cycle_in(&registry, child_id, np)?
    {
        return Err(ReparentError::InvalidParent);
    }
    #[cfg(test)]
    test_hooks::run(test_hooks::Point::AfterCycleCheck);

    let child_is_registered = registry.contains_key(&child_id);
    let old_rec = old_parent.and_then(|id| registry.get(&id));
    let new_rec = new_parent.and_then(|id| registry.get(&id));

    if child_is_registered && new_parent.is_some() && new_rec.is_none() {
        return Err(ReparentError::InvalidParent);
    }

    // Tier 1: Acquire parents' `children` write lock(s) in ascending ObjectId order.
    enum ParentLockPair<'a> {
        None,
        SingleOld(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        SingleNew(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        Both {
            old_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
            new_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
        },
    }

    let parent_locks = match (old_parent, new_parent) {
        (Some(old_id), Some(new_id)) if old_id != new_id => {
            let old_r = old_rec.ok_or(ReparentError::InvalidParent);
            let new_r = new_rec.ok_or(ReparentError::InvalidParent);
            if let (Ok(o_rec), Ok(n_rec)) = (old_r, new_r) {
                if old_id < new_id {
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both {
                        old_guard: o_g,
                        new_guard: n_g,
                    }
                } else {
                    let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both {
                        old_guard: o_g,
                        new_guard: n_g,
                    }
                }
            } else if let Ok(o_rec) = old_r {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else if let Ok(n_rec) = new_r {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (Some(_), _) => {
            if let Some(o_rec) = old_rec {
                let o_g = o_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o_g)
            } else {
                ParentLockPair::None
            }
        }
        (_, Some(_)) => {
            if let Some(n_rec) = new_rec {
                let n_g = n_rec.children.write().map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleNew(n_g)
            } else {
                ParentLockPair::None
            }
        }
        (None, None) => ParentLockPair::None,
    };

    // Tier 2: Acquire child's `record.parent` write lock if child is registered.
    let child_parent_guard = if child_is_registered {
        Some(
            registry
                .get(&child_id)
                .unwrap()
                .parent
                .write()
                .map_err(|_| ReparentError::Busy)?,
        )
    } else {
        None
    };

    // Infallible Commit: all locks held, no failures possible from here. The guards are moved
    // into a block of their own, so that L1 and L2 are gone before L0 is let go below.
    {
        let parent_locks = parent_locks;
        let child_parent_guard = child_parent_guard;
        match parent_locks {
            ParentLockPair::Both {
                mut old_guard,
                mut new_guard,
            } => {
                old_guard.retain(|&id| id != child_id);
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::SingleOld(mut old_guard) => {
                old_guard.retain(|&id| id != child_id);
            }
            ParentLockPair::SingleNew(mut new_guard) => {
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::None => {}
        }

        if let Some(mut p_guard) = child_parent_guard
            && let Some(record) = registry.get(&child_id)
        {
            store_link(&registry, record, &mut p_guard, new_parent);
        }
    }

    // The registry lock (L0) and `HIERARCHY_MUTATION` end here. Below, `edit_child_list`,
    // `defer_if_borrowed` and `dispatch_to_object` take the registry again for themselves, deliver
    // events to handlers that may register or drop objects, and take the `pending_child_changes`
    // mutex, which is only taken with none of L0, L1 and L2 held. What the child keeps of its new
    // parent is copied out first.
    let new_liveness =
        new_parent.and_then(|id| registry.get(&id).map(|rec| (id, Arc::clone(&rec.liveness))));
    drop(registry);
    drop(hierarchy);
    // Update local children lists and send ChildRemoved/ChildAdded via registered callbacks. A
    // panic of the old parent's handler is held back until the child and the new parent's list
    // agree with the registry, and then continues.
    let removed_panic = old_parent.and_then(|old_parent_id| {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            edit_child_list(old_parent_id, ChildListChange::Remove(child_id));
            let removed = Event::new(EventKind::ChildRemoved { child_id });
            if let Err(mut removed) = defer_if_borrowed(old_parent_id, removed) {
                dispatch_to_object(old_parent_id, &mut removed);
            }
        }))
        .err()
    });

    // Following Qt's setParent_helper ordering: child.parent is updated after ChildRemoved is
    // dispatched, and reconciled on panic before resuming unwind.
    let pending_sync = if let Some(ref rec) = child_rec {
        if rec.has_pending_parent_sync.swap(false, Ordering::AcqRel) {
            lock_ignoring_poison(&rec.pending_parent_sync).take()
        } else {
            None
        }
    } else {
        None
    };

    let (effective_parent, effective_liveness) = if let Some((synced_parent, synced_liveness)) =
        pending_sync
    {
        (synced_parent, synced_liveness)
    } else if child_rec.is_some() && query_object_parent(child_id).flatten() != new_parent {
        let current_reg = query_object_parent(child_id).flatten();
        let target_liveness = current_reg.and_then(|id| liveness_of(id).map(|alive| (id, alive)));
        (current_reg, target_liveness)
    } else {
        (new_parent, new_liveness.clone())
    };

    child.parent = effective_parent;
    child.parent_liveness = effective_liveness;

    let current_reg_parent = effective_parent;
    if let Some(target_parent_id) = current_reg_parent {
        edit_child_list(target_parent_id, ChildListChange::Add(child_id));
        if removed_panic.is_none() {
            let added = Event::new(EventKind::ChildAdded { child_id });
            if let Err(mut added) = defer_if_borrowed(target_parent_id, added) {
                dispatch_to_object(target_parent_id, &mut added);
            }
        }
    }
    drop(short_term_borrow);
    if let Some(payload) = removed_panic {
        std::panic::resume_unwind(payload);
    }
    if refused_thread {
        eprintln!("QObject::setParent: Cannot set parent, new parent is in a different thread");
        return Err(ReparentError::DifferentThread);
    }
    Ok(())
}

/// Moves an owned child to `new_parent`, or releases it to the caller when `new_parent` is
/// `None` (`Ok(Some(owner))`; the child stays alive and registered, with no parent).
///
/// Unlike Qt's `setParent`, ownership is explicit: the parent owns the child, so detaching hands
/// the child to the caller instead of dropping it. `ChildRemoved`/`ChildAdded` go through
/// `notify_helper`, so object and application event filters see them. Validation and borrows are
/// taken before anything is changed, so an `Err` leaves the hierarchy exactly as it was.
///
/// Refuses with [`ReparentError::DifferentThread`] when the new parent's thread differs from the
/// child's. Unlike [`set_parent`] it changes nothing: the child keeps its old parent and that
/// parent keeps owning it, no event is sent, and no warning is printed. Qt's `setParent` would
/// detach the child and leave it without a parent; here that would leave the child without an
/// owner, and the return type cannot hand it back together with an error.
///
/// The errors: `UnknownObject` when the child is not registered on the calling thread;
/// `InvalidParent` when the new parent is the child itself (answered before anything is
/// borrowed, also from inside the child's own callback), is not registered on the calling thread,
/// or is a descendant of the child; `NotOwned` when the child has no parent, its parent does not
/// own it, or that parent is not registered on the calling thread; `DifferentThread`; and `Busy`
/// when the child or a parent is borrowed by a running callback, or a lock is poisoned. A
/// parentless child, or a new parent, that is borrowed is `Busy` before the checks that need the
/// borrows: the descendant check is made last, once the borrows are held, so a descendant that is
/// borrowed answers `Busy`. Which of several applicable errors is returned is not otherwise
/// specified. Moving the child to the parent that already owns it changes nothing and returns
/// `Ok(None)`, as a real move does.
///
/// A released child comes back as an [`OwnedObject`], which shares its pointer with the
/// registry, so the caller may use the child between its callbacks while it stays registered. It
/// is never given up as a `Box`: a `Box` claims to be the only pointer to its object, and the
/// registry's pointer contradicts that.
pub fn reparent_owned_object(
    child_id: ObjectId,
    new_parent: Option<ObjectId>,
) -> Result<Option<OwnedObject<dyn QObject>>, ReparentError> {
    // The records and the borrows come first, with no lock held: the records by one registry
    // lookup each (which takes L0 and lets it go), the borrows by `borrow_record`, one
    // compare-and-swap that fails at once, so nothing ever waits for it. The handles to the data
    // the commit writes come from the objects' accessors (their own code) before any lock, too.
    //
    // The commit takes `HIERARCHY_MUTATION` and the registry (L0) once, checks that the records
    // are still the registered ones and that the new parent is not a descendant, takes the
    // `children` of the parents (L1, lower id first) and the `parent` of the child (L2), and
    // writes through the handles. The borrow guards are declared before all of that, so they are
    // dropped after L2, L1, L0 and `HIERARCHY_MUTATION`: dropping one takes the
    // `pending_child_changes` mutex and can free parked children, which unregisters them and so
    // takes L0 for writing.
    let (child_record, _) = registered_ptr(child_id).ok_or(ReparentError::UnknownObject)?;
    // An object is not its own parent. This is answered before the child is borrowed: the borrow
    // of the new parent, which would be the child, would answer `Busy` otherwise.
    if new_parent == Some(child_id) {
        return Err(ReparentError::InvalidParent);
    }
    let mut child_guard = borrow_record(&child_record).map_err(|_| ReparentError::Busy)?;

    let old_parent_id = (*child_record
        .parent
        .read()
        .map_err(|_| ReparentError::Busy)?)
    .ok_or(ReparentError::NotOwned)?;
    let old_record = record_of(old_parent_id)?.ok_or(ReparentError::NotOwned)?;

    if new_parent == Some(old_parent_id) {
        let owned = with_object(old_parent_id, |p| {
            p.object_data()
                .owned_children
                .iter()
                .any(|c| OwnedObject::object_id(c) == child_id)
        })
        .ok_or(ReparentError::Busy)?;
        return if owned {
            Ok(None)
        } else {
            Err(ReparentError::NotOwned)
        };
    }

    let new_record = match new_parent {
        Some(np) => {
            let child_thread = *child_record
                .thread_id
                .read()
                .map_err(|_| ReparentError::Busy)?;
            if in_different_thread(child_thread, np) {
                return Err(ReparentError::DifferentThread);
            }
            Some(record_of(np)?.ok_or(ReparentError::InvalidParent)?)
        }
        None => None,
    };

    // A parent that is not registered on the calling thread, or is dead, is not `Busy`: the
    // calling thread cannot do what is asked of it.
    let mut old_guard = borrow_record(&old_record).map_err(|error| match error {
        ReparentError::Busy => ReparentError::Busy,
        _ => ReparentError::NotOwned,
    })?;
    let mut new_guard = match &new_record {
        Some(record) => Some(borrow_record(record).map_err(|error| match error {
            ReparentError::Busy => ReparentError::Busy,
            _ => ReparentError::InvalidParent,
        })?),
        None => None,
    };

    let mut child_data = child_guard.object_data_mut();
    let mut old_data = old_guard.object_data_mut();
    let mut new_data = new_guard.as_mut().map(|guard| guard.object_data_mut());

    let pos = old_data
        .owned_children
        .iter()
        .position(|c| OwnedObject::object_id(c) == child_id)
        .ok_or(ReparentError::NotOwned)?;

    enum ParentLockPair<'a> {
        SingleOld(std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>),
        Both {
            old_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
            new_guard: std::sync::RwLockWriteGuard<'a, Vec<ObjectId>>,
        },
    }

    let released = {
        let hierarchy = lock_hierarchy();
        let registry = GLOBAL_OBJECT_REGISTRY
            .read()
            .map_err(|_| ReparentError::Busy)?;
        // The records taken above must still be the registered ones. Nothing is unregistered or
        // registered again while a callback of the object runs, and the borrows above stand for
        // one; this keeps a broken promise from becoming a commit on a record nobody can reach.
        let registered = |id: ObjectId, record: &Arc<ObjectRecord>| {
            registry
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, record))
        };
        if !registered(child_id, &child_record) {
            return Err(ReparentError::UnknownObject);
        }
        if !registered(old_parent_id, &old_record) {
            return Err(ReparentError::NotOwned);
        }
        if let (Some(np), Some(record)) = (new_parent, &new_record) {
            if !registered(np, record) {
                return Err(ReparentError::InvalidParent);
            }
            if would_create_cycle_in(&registry, child_id, np)? {
                return Err(ReparentError::InvalidParent);
            }
        }
        #[cfg(test)]
        test_hooks::run(test_hooks::Point::AfterCycleCheck);

        let parent_locks = match (new_parent, &new_record) {
            (Some(new_id), Some(new_rec)) => {
                if old_parent_id < new_id {
                    let o = old_record
                        .children
                        .write()
                        .map_err(|_| ReparentError::Busy)?;
                    let n = new_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both {
                        old_guard: o,
                        new_guard: n,
                    }
                } else {
                    let n = new_rec.children.write().map_err(|_| ReparentError::Busy)?;
                    let o = old_record
                        .children
                        .write()
                        .map_err(|_| ReparentError::Busy)?;
                    ParentLockPair::Both {
                        old_guard: o,
                        new_guard: n,
                    }
                }
            }
            _ => {
                let o = old_record
                    .children
                    .write()
                    .map_err(|_| ReparentError::Busy)?;
                ParentLockPair::SingleOld(o)
            }
        };

        let mut child_parent_guard = child_record
            .parent
            .write()
            .map_err(|_| ReparentError::Busy)?;
        if *child_parent_guard != Some(old_parent_id) {
            return Err(ReparentError::NotOwned);
        }

        // Infallible Commit: all locks and borrows held, and no code of an object runs.
        let owner = {
            old_data.children_mut().retain(|&id| id != child_id);
            old_data.owned_children_mut().remove(pos)
        };

        let new_liveness = new_parent
            .zip(new_record.as_ref())
            .map(|(id, record)| (id, Arc::clone(&record.liveness)));
        {
            let raw = child_data.raw_data_mut();
            raw.parent = new_parent;
            raw.parent_liveness = new_liveness;
            raw.owned_by_parent = new_parent.is_some();
        }

        match parent_locks {
            ParentLockPair::Both {
                mut old_guard,
                mut new_guard,
            } => {
                old_guard.retain(|&id| id != child_id);
                if !new_guard.contains(&child_id) {
                    new_guard.push(child_id);
                }
            }
            ParentLockPair::SingleOld(mut old_guard) => {
                old_guard.retain(|&id| id != child_id);
            }
        }
        store_link(
            &registry,
            &child_record,
            &mut child_parent_guard,
            new_parent,
        );

        let released = match new_data.as_mut() {
            Some(data) => {
                if !data.children.contains(&child_id) {
                    data.children_mut().push(child_id);
                }
                data.owned_children_mut().push(owner);
                None
            }
            None => Some(owner),
        };

        drop(child_parent_guard);
        drop(registry);
        drop(hierarchy);
        released
    };

    drop(new_guard);
    drop(old_guard);
    drop(child_guard);

    let mut removed = Event::new(EventKind::ChildRemoved { child_id });
    crate::event_loop::notify_helper(old_parent_id, &mut removed);
    if let Some(np) = new_parent {
        let mut added = Event::new(EventKind::ChildAdded { child_id });
        crate::event_loop::notify_helper(np, &mut added);
    }
    Ok(released)
}

/// Attaches `child` as an owned child of `parent_id` and sends `ChildAdded` to the parent
/// (Qt `setParent_helper`).
///
/// Returns the child id. If the parent's thread differs from the child's, returns
/// `DifferentThread`; if the parent is not registered, returns `UnknownObject`; if the child has
/// a parent already (take it from that parent first: that parent would go on listing it, and
/// dropping that parent would unregister the child the new parent owns), or the parent is the
/// child itself or one of its descendants (a child that is registered already can have
/// descendants), returns `InvalidParent`; if the parent is borrowed (for example from inside its
/// own callback), returns `Busy`, which is also what a borrowed descendant answers. In every case
/// the `Box` is
/// handed back and nothing is attached: the child gets no parent, the parent's children stay as
/// they were, and no event is sent. For `DifferentThread` Qt's constructor ends the same way
/// but prints a warning; this prints nothing.
///
/// On success the parent holds the child in an [`OwnedObject`] and the child is registered with
/// that owner's pointer, so moving the child between parents leaves the registry's pointer valid.
///
/// Dropping the parent from inside a callback of this child is allowed. The child, and everything
/// the registry lists below it, is then destroyed at once for everything but that callback: it is
/// unregistered, no longer alive, gets no events or timers, and its connections to other objects
/// are gone. Its memory, and so its `Drop`, waits until the callback returns, because the
/// callback still runs on `&mut self`. Qt frees the child there and then. A signal emission that
/// is under way ends as in Qt: no slot is called after the one that dropped the parent (see
/// [`Signal::emit`](crate::signal::Signal::emit)).
///
/// # Safety
/// The child must remain at its allocated address while owned and registered, and must be used on
/// its registration thread; the thread rules of [`register_qobject_ptr`] apply. While a callback
/// of the child runs, nothing outside it accesses the child. A reference to the child taken
/// before this call, from the `Box` or from a pointer made from it, is not valid after it.
pub unsafe fn add_owned_child<T: QObject + 'static>(
    parent_id: ObjectId,
    child: Box<T>,
) -> Result<ObjectId, (ReparentError, Box<T>)> {
    if in_different_thread(child.object_data().thread_id(), parent_id) {
        return Err((ReparentError::DifferentThread, child));
    }
    if registered_ptr(parent_id).is_none() {
        return Err((ReparentError::UnknownObject, child));
    }
    if child.object_data().parent().is_some() {
        return Err((ReparentError::InvalidParent, child));
    }
    let child_id = child.object_data().id();
    let mut pending = Some(child);
    let attached = with_object_mut(parent_id, |parent| {
        let mut child = pending.take().expect("closure runs at most once");
        let parent_liveness = liveness_of(parent_id).map(|alive| (parent_id, alive));
        {
            let mut data = child.object_data_mut();
            let raw = data.raw_data_mut();
            raw.parent = Some(parent_id);
            raw.parent_liveness = parent_liveness;
            raw.owned_by_parent = true;
        }
        // The parent holds the child through an owner made from the `Box`; the registry gets a
        // copy of the owner's pointer, so moving the owner later invalidates nothing.
        let owner = OwnedObject::from_box(child);
        let ptr: NonNull<dyn QObject> = OwnedObject::<T>::as_non_null(&owner);
        // The accessors of both objects (their own code) run before the registry is touched.
        let mut parent_data = parent.object_data_mut();
        // SAFETY: `ptr` is the owner's pointer to the heap allocation of `child`, which the owner
        // keeps alive at this address; `child` was moved in and nothing else holds a pointer to it
        // yet (the registry's is made below, from `ptr`). A shared reference is formed only for
        // `object_data` and `register_record`, which copies what it keeps.
        let child_data = unsafe { (*ptr.as_ptr()).object_data() };
        // The registration checks that the parent is not a descendant of the child, records the
        // child and lists it in the registry's list of the parent, all under
        // `HIERARCHY_MUTATION`. That list is what still reaches the child while a running
        // callback of the parent hides it from the parent's own `children`.
        match register_record(child_data, ptr.as_ptr(), Some(parent_id)) {
            Ok(()) => {
                parent_data.add_child(child_id);
                parent_data
                    .owned_children_mut()
                    .push(OwnedObject::into_dyn(owner));
                Ok(())
            }
            Err(error) => {
                // Nothing was inserted, so the registry holds no pointer made from this owner.
                // SAFETY: `register_record` failed before inserting anything, so the registry holds
                // no pointer made from `owner`, and `ptr` is used no more; `owner` is turned back
                // into the `Box` that `from_box` took, once.
                let mut child = unsafe { OwnedObject::into_box(owner) };
                {
                    let mut data = child.object_data_mut();
                    let raw = data.raw_data_mut();
                    raw.clear_parent();
                    raw.owned_by_parent = false;
                }
                pending = Some(child);
                Err(error)
            }
        }
    });
    match attached {
        Some(Ok(())) => {
            let mut added = Event::new(EventKind::ChildAdded { child_id });
            crate::event_loop::notify_helper(parent_id, &mut added);
            Ok(child_id)
        }
        Some(Err(error)) => Err((
            error,
            pending
                .take()
                .expect("the closure gave the Box back with the error"),
        )),
        None => Err((
            ReparentError::Busy,
            pending
                .take()
                .expect("closure did not run, so the Box is still here"),
        )),
    }
}

/// Sets the links of the three objects it is given and nothing else: it takes no registry lock,
/// sends no event and does not look at the registry. A registered object's registry record and
/// its parent's registry list stay as they were; use [`set_parent`] or [`reparent_owned_object`]
/// for those. Since it only edits the objects' own links it cannot make the registry cyclic: a
/// cycle built out of such links is not recorded when the objects are registered.
pub fn reparent(
    child: &mut ObjectData,
    old_parent: Option<&mut ObjectData>,
    new_parent: Option<&mut ObjectData>,
) {
    if let Some(old) = old_parent {
        old.children.retain(|&id| id != child.id);
    }
    if let Some(new_p) = new_parent {
        child.parent = Some(new_p.id);
        child.parent_liveness = Some((new_p.id, Arc::clone(&new_p.liveness)));
        if !new_p.children.contains(&child.id) {
            new_p.children.push(child.id);
        }
    } else {
        child.clear_parent();
    }
}
