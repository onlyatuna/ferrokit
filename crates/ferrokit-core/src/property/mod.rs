// SPDX-License-Identifier: LGPL-3.0-only
//! Qt 6 QProperty / QBindable reactive dependency binding engine.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use crate::signal::Signal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PropertyId(pub u64);

impl PropertyId {
    pub fn next() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        PropertyId(NEXT_ID.fetch_add(1, Ordering::Relaxed))
    }
}

thread_local! {
    static CURRENT_BINDING_STACK: RefCell<Vec<PropertyId>> = const { RefCell::new(Vec::new()) };
}

type PropertyNotifier = Arc<dyn Fn(&mut HashSet<PropertyId>) + Send + Sync>;
type PropertyBinding<T> = Arc<dyn Fn() -> T + Send + Sync>;

static PROPERTY_DIRTY_NOTIFIERS: LazyLock<RwLock<HashMap<PropertyId, PropertyNotifier>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn register_property_notifier(id: PropertyId, notifier: PropertyNotifier) {
    if let Ok(mut map) = PROPERTY_DIRTY_NOTIFIERS.write() {
        map.insert(id, notifier);
    }
}

fn unregister_property_notifier(id: PropertyId) {
    if let Ok(mut map) = PROPERTY_DIRTY_NOTIFIERS.write() {
        map.remove(&id);
    }
}

pub fn mark_property_dirty(id: PropertyId) {
    let mut visited = HashSet::new();
    mark_property_dirty_recursive(id, &mut visited);
}

fn mark_property_dirty_recursive(id: PropertyId, visited: &mut HashSet<PropertyId>) {
    if !visited.insert(id) {
        return;
    }
    let notifier = {
        match PROPERTY_DIRTY_NOTIFIERS.read() {
            Ok(map) => map.get(&id).cloned(),
            _ => None,
        }
    };
    if let Some(n) = notifier {
        n(visited);
    }
}

/// Reactive property equivalent to Qt 6 `QProperty<T>`.
///
/// Features:
/// - Direct value storage or lazy declarative binding evaluation.
/// - Automatic dependency tracking graph when evaluated inside another property's binding.
/// - Circular dependency cycle detection to prevent stack overflow.
/// - Integrated `notify_signal` emitted on value change.
pub struct Property<T: Clone + PartialEq + Send + Sync + 'static> {
    id: PropertyId,
    value: Arc<RwLock<T>>,
    binding: Arc<RwLock<Option<PropertyBinding<T>>>>,
    dependents: Arc<RwLock<HashSet<PropertyId>>>,
    notify_signal: Signal<T>,
    dirty: Arc<AtomicBool>,
    in_evaluation: Arc<AtomicBool>,
    /// Shared by all clones; unregisters the dirty notifier when the last clone drops.
    _registration: Arc<NotifierRegistration>,
}

struct NotifierRegistration(PropertyId);

impl Drop for NotifierRegistration {
    fn drop(&mut self) {
        unregister_property_notifier(self.0);
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> Clone for Property<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            value: Arc::clone(&self.value),
            binding: Arc::clone(&self.binding),
            dependents: Arc::clone(&self.dependents),
            notify_signal: self.notify_signal.clone(),
            dirty: Arc::clone(&self.dirty),
            in_evaluation: Arc::clone(&self.in_evaluation),
            _registration: Arc::clone(&self._registration),
        }
    }
}

impl<T: Clone + PartialEq + Send + Sync + 'static> Property<T> {
    /// Creates a new property with an initial value.
    pub fn new(initial: T) -> Self {
        Self::with_id(PropertyId::next(), initial, None)
    }

    fn with_id(id: PropertyId, initial: T, binding: Option<PropertyBinding<T>>) -> Self {
        let dirty = Arc::new(AtomicBool::new(false));
        let in_evaluation = Arc::new(AtomicBool::new(false));
        let dependents = Arc::new(RwLock::new(HashSet::new()));

        let dirty_clone = Arc::clone(&dirty);
        let dependents_clone = Arc::clone(&dependents);

        let notifier: PropertyNotifier = Arc::new(move |visited| {
            dirty_clone.store(true, Ordering::Release);
            let deps: Vec<PropertyId> = match dependents_clone.read() {
                Ok(d) => d.iter().copied().collect(),
                _ => Vec::new(),
            };
            for dep in deps {
                mark_property_dirty_recursive(dep, visited);
            }
        });

        register_property_notifier(id, notifier);

        Self {
            id,
            value: Arc::new(RwLock::new(initial)),
            binding: Arc::new(RwLock::new(binding)),
            dependents,
            notify_signal: Signal::new(),
            dirty,
            in_evaluation,
            _registration: Arc::new(NotifierRegistration(id)),
        }
    }

    /// Creates a new property with an initial binding.
    pub fn with_binding<F>(compute: F) -> Self
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        let id = PropertyId::next();
        let initial_val = {
            let _stack_guard = BindingStackGuard;
            CURRENT_BINDING_STACK.with(|stack| stack.borrow_mut().push(id));
            compute()
        };
        Self::with_id(id, initial_val, Some(Arc::new(compute)))
    }

    pub fn id(&self) -> PropertyId {
        self.id
    }

    pub fn notify_signal(&self) -> &Signal<T> {
        &self.notify_signal
    }

    /// Reads the current property value.
    /// If evaluated inside another property's binding, automatically records dependency.
    pub fn get(&self) -> T {
        CURRENT_BINDING_STACK.with(|stack| {
            if let Some(&evaluating_id) = stack.borrow().last()
                && evaluating_id != self.id
                && let Ok(mut deps) = self.dependents.write()
            {
                deps.insert(evaluating_id);
            }
        });

        if !self.in_evaluation.load(Ordering::Acquire) && self.dirty.load(Ordering::Acquire) {
            self.recompute();
        }

        self.value.read().unwrap().clone()
    }

    /// Directly sets a new value, breaking any existing binding.
    pub fn set(&self, new_val: T) {
        *self.binding.write().unwrap() = None;

        let changed = {
            let mut val_lock = self.value.write().unwrap();
            if *val_lock != new_val {
                *val_lock = new_val.clone();
                true
            } else {
                false
            }
        };

        if changed {
            self.dirty.store(false, Ordering::Release);
            self.notify();
            self.notify_signal.emit(&new_val);
        }
    }

    /// Attaches a reactive binding function to this property.
    pub fn set_binding<F>(&self, compute: F)
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        *self.binding.write().unwrap() = Some(Arc::new(compute));
        self.dirty.store(true, Ordering::Release);
        self.recompute();
    }

    /// Returns true if this property currently has an active binding.
    pub fn has_binding(&self) -> bool {
        self.binding.read().unwrap().is_some()
    }

    fn recompute(&self) {
        let binding_opt = self.binding.read().unwrap().clone();
        if let Some(compute) = binding_opt {
            if self.in_evaluation.swap(true, Ordering::AcqRel) {
                // Cycle detected, retain current value
                return;
            }

            // Clear before computing: an invalidation that lands during compute()
            // leaves the flag set so the next read recomputes. If `compute()` panics,
            // `EvaluationGuard` restores `dirty = true` so the aborted evaluation is not
            // treated as clean.
            self.dirty.store(false, Ordering::Release);
            let mut eval_guard = EvaluationGuard {
                in_evaluation: &self.in_evaluation,
                dirty: &self.dirty,
                completed: false,
            };
            let new_val = {
                let _stack_guard = BindingStackGuard;
                CURRENT_BINDING_STACK.with(|stack| stack.borrow_mut().push(self.id));
                compute()
            };
            eval_guard.completed = true;
            drop(eval_guard);

            let changed = {
                let mut val_lock = self.value.write().unwrap();
                if *val_lock != new_val {
                    *val_lock = new_val.clone();
                    true
                } else {
                    false
                }
            };

            if changed {
                self.notify();
                self.notify_signal.emit(&new_val);
            }
        }
    }

    fn notify(&self) {
        let deps: Vec<PropertyId> = match self.dependents.read() {
            Ok(d) => d.iter().copied().collect(),
            _ => Vec::new(),
        };
        for dep in deps {
            mark_property_dirty(dep);
        }
    }
}

/// Helper wrapper for bindable properties matching Qt 6 `QBindable<T>`.
pub struct Bindable<'a, T: Clone + PartialEq + Send + Sync + 'static> {
    property: &'a Property<T>,
}

impl<'a, T: Clone + PartialEq + Send + Sync + 'static> Bindable<'a, T> {
    pub fn new(property: &'a Property<T>) -> Self {
        Self { property }
    }

    pub fn value(&self) -> T {
        self.property.get()
    }

    pub fn set_binding<F>(&self, compute: F)
    where
        F: Fn() -> T + Send + Sync + 'static,
    {
        self.property.set_binding(compute);
    }

    pub fn has_binding(&self) -> bool {
        self.property.has_binding()
    }
}

struct BindingStackGuard;

impl Drop for BindingStackGuard {
    fn drop(&mut self) {
        let _ = CURRENT_BINDING_STACK.try_with(|stack| {
            stack.borrow_mut().pop();
        });
    }
}

struct EvaluationGuard<'a> {
    in_evaluation: &'a AtomicBool,
    dirty: &'a AtomicBool,
    completed: bool,
}

impl Drop for EvaluationGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.dirty.store(true, Ordering::Release);
        }
        self.in_evaluation.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn with_binding_evaluates_once_and_preserves_outer_stack() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_inner = Arc::clone(&calls);
        let source = Property::new(10i32);
        let source_clone = source.clone();

        let outer = Property::with_binding(move || {
            let calls_nested = Arc::clone(&calls_inner);
            let _nested = Property::with_binding(move || {
                calls_nested.fetch_add(1, Ordering::SeqCst);
                1
            });
            source_clone.get() * 2 + 1
        });
        assert_eq!(outer.get(), 21);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Outer binding was still on CURRENT_BINDING_STACK when `source_clone.get()` ran.
        source.set(20);
        assert_eq!(outer.get(), 41);
    }

    #[test]
    fn recompute_panic_restores_evaluation_flag_and_stack() {
        let should_panic = Arc::new(AtomicBool::new(true));
        let flag = Arc::clone(&should_panic);
        let prop = Property::new(1i32);
        let p_clone = prop.clone();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            p_clone.set_binding(move || {
                if flag.load(Ordering::SeqCst) {
                    panic!("boom");
                }
                42
            });
        }));
        assert!(res.is_err());
        assert!(!prop.in_evaluation.load(Ordering::SeqCst));
        assert!(CURRENT_BINDING_STACK.with(|s| s.borrow().is_empty()));

        should_panic.store(false, Ordering::SeqCst);
        assert_eq!(prop.get(), 42);
    }
}
