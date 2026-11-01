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
        let id = PropertyId::next();
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
            binding: Arc::new(RwLock::new(None)),
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
        CURRENT_BINDING_STACK.with(|stack| stack.borrow_mut().clear());
        let initial_val = compute();
        let prop = Self::new(initial_val);
        prop.set_binding(compute);
        prop
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
            // leaves the flag set so the next read recomputes.
            self.dirty.store(false, Ordering::Release);
            CURRENT_BINDING_STACK.with(|stack| stack.borrow_mut().push(self.id));
            let new_val = compute();
            CURRENT_BINDING_STACK.with(|stack| stack.borrow_mut().pop());

            self.in_evaluation.store(false, Ordering::Release);

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
