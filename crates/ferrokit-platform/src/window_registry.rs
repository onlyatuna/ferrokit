// SPDX-License-Identifier: LGPL-3.0-only
//! Routing table from a native window handle to the shared state its window procedure serves.
//!
//! A window procedure is a bare `extern "system"` function: it gets the window handle and nothing
//! else, so it finds its state through a table keyed by that handle. The state is *not* the Rust
//! object that owns the window (a `TrayIcon`, say, which lives in a `Box` its owner moves around
//! and borrows as `&mut self` while it calls into the shell, which may deliver a sent message to
//! this very window). It is a separate `Rc<S>` allocation that the owner holds and the table
//! refers to by `Weak<S>`:
//!
//! - the owner's moves and `&mut` borrows never touch the allocation, only the owner's own
//!   `Rc` handle, so what the procedure reads cannot alias a live `&mut Owner`;
//! - the procedure takes an owned `Rc<S>` from [`SharedRegistry::lookup`], so the state stays
//!   alive for the whole message even if a slot it calls drops the owner;
//! - once the owner [`unregister`](SharedRegistry::unregister)s the handle (before it destroys
//!   the window), the procedure finds nothing and falls back to the default handling.
//!
//! No raw pointer is involved, so there is nothing to keep valid by hand. The table is meant to
//! be a `thread_local!`: windows, their procedures and their owners (`!Send`) all belong to the
//! creating thread. Every access to the inner `RefCell` is a single statement that calls no
//! foreign code, so it cannot be re-entered.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};

/// Window handle (as an integer) to the shared state of the window's procedure.
pub(crate) struct SharedRegistry<S> {
    entries: RefCell<HashMap<isize, Weak<S>>>,
}

impl<S> SharedRegistry<S> {
    pub(crate) fn new() -> Self {
        Self {
            entries: RefCell::new(HashMap::new()),
        }
    }

    /// Routes `key` to `shared`, replacing a stale entry for the same handle (a handle may be
    /// reused once its window is destroyed). The table keeps no strong reference: the state lives
    /// as long as its owner's `Rc`, and as long as any procedure invocation that looked it up.
    pub(crate) fn register(&self, key: isize, shared: &Rc<S>) {
        let previous = self.entries.borrow_mut().insert(key, Rc::downgrade(shared));
        // Dropped after the borrow ended (a `Weak` drop runs no user code, but the rule is the
        // same everywhere: nothing runs under the borrow).
        drop(previous);
    }

    /// Removes the route for `key`, if any. Idempotent.
    pub(crate) fn unregister(&self, key: isize) {
        let removed = self.entries.borrow_mut().remove(&key);
        drop(removed);
    }

    /// The state routed from `key`, or `None` when nothing is registered or the owner is gone.
    /// The returned `Rc` keeps the state alive for as long as the caller holds it.
    pub(crate) fn lookup(&self, key: isize) -> Option<Rc<S>> {
        self.entries.borrow().get(&key).and_then(Weak::upgrade)
    }
}

#[cfg(test)]
mod tests {
    //! A model of the tray's window procedure protocol with no Win32: an `Owner` that is boxed,
    //! moved and borrowed `&mut` while a "message" reaches its shared state through the table.
    //! Meant to run under Miri (Stacked and Tree Borrows), which checks that the procedure never
    //! forms a reference into the owner.

    use super::*;
    use std::cell::{Cell, RefCell};

    const KEY: isize = 0x1234;

    /// What the window procedure reads and writes.
    struct State {
        visible: Cell<bool>,
        log: RefCell<Vec<&'static str>>,
        dropped: Rc<Cell<bool>>,
    }

    impl Drop for State {
        fn drop(&mut self) {
            self.dropped.set(true);
        }
    }

    thread_local! {
        static TABLE: SharedRegistry<State> = SharedRegistry::new();
    }

    fn deliver(key: isize, text: &'static str) -> bool {
        // The window procedure: owned state out of the table, then only shared access.
        let Some(state) = TABLE.with(|t| t.lookup(key)) else {
            return false;
        };
        state.log.borrow_mut().push(text);
        state.visible.set(true);
        true
    }

    struct Owner {
        shared: Rc<State>,
        key: isize,
        /// Owner-only data, written through `&mut self`.
        counter: u64,
    }

    impl Owner {
        fn new(key: isize, dropped: &Rc<Cell<bool>>) -> Box<Self> {
            let shared = Rc::new(State {
                visible: Cell::new(false),
                log: RefCell::new(Vec::new()),
                dropped: Rc::clone(dropped),
            });
            TABLE.with(|t| t.register(key, &shared));
            Box::new(Self {
                shared,
                key,
                counter: 0,
            })
        }

        /// A `&mut self` method that calls out (as `Shell_NotifyIconW` does), during which a
        /// message for its window is delivered.
        fn call_out(&mut self, message: &'static str) {
            self.counter += 1;
            assert!(deliver(self.key, message));
            self.counter += 1;
            self.shared.log.borrow_mut().push("after the call out");
        }
    }

    impl Drop for Owner {
        fn drop(&mut self) {
            TABLE.with(|t| t.unregister(self.key));
        }
    }

    #[test]
    fn a_registration_made_before_the_owner_moves_stays_valid() {
        let dropped = Rc::new(Cell::new(false));
        let owner = Owner::new(KEY, &dropped);
        let first = TABLE.with(|t| t.lookup(KEY)).expect("registered");
        assert!(Rc::ptr_eq(&first, &owner.shared));
        drop(first);

        // Move the box, then the value out of it and into a new box, to a vector and back.
        let mut moved = vec![owner];
        let owner = moved.pop().unwrap();
        let mut owner = Box::new(*owner);
        owner.counter = 1;

        assert!(deliver(KEY, "after the moves"));
        assert!(owner.shared.visible.get());
        assert_eq!(*owner.shared.log.borrow(), ["after the moves"]);
    }

    #[test]
    fn the_owner_may_hold_mut_self_while_a_message_reaches_the_shared_state() {
        let dropped = Rc::new(Cell::new(false));
        let mut owner = Owner::new(KEY, &dropped);

        owner.call_out("during the call out");

        assert_eq!(owner.counter, 2);
        assert!(owner.shared.visible.get());
        assert_eq!(
            *owner.shared.log.borrow(),
            ["during the call out", "after the call out"]
        );
    }

    #[test]
    fn the_state_outlives_an_owner_dropped_while_a_message_is_handled() {
        let dropped = Rc::new(Cell::new(false));
        let owner = Owner::new(KEY, &dropped);

        // The procedure has looked the state up and is inside a slot...
        let in_flight = TABLE.with(|t| t.lookup(KEY)).expect("registered");
        // ...which drops the owner ("Quit").
        drop(owner);
        assert!(
            !dropped.get(),
            "the in-flight message keeps the state alive"
        );
        in_flight.log.borrow_mut().push("still usable");
        assert!(
            TABLE.with(|t| t.lookup(KEY)).is_none(),
            "a message sent from now on finds no state"
        );

        drop(in_flight);
        assert!(dropped.get(), "the last user frees the state");
    }

    #[test]
    fn a_missing_or_unregistered_entry_is_tolerated() {
        assert!(!deliver(KEY, "nobody registered"));

        let dropped = Rc::new(Cell::new(false));
        let owner = Owner::new(KEY, &dropped);
        TABLE.with(|t| t.unregister(KEY));
        assert!(!deliver(KEY, "after unregister"));
        TABLE.with(|t| t.unregister(KEY));
        assert!(owner.shared.log.borrow().is_empty());
        drop(owner);
        assert!(dropped.get());
    }

    #[test]
    fn a_reused_handle_replaces_a_stale_entry() {
        let gone = Rc::new(Cell::new(false));
        let stale = Rc::new(State {
            visible: Cell::new(false),
            log: RefCell::new(Vec::new()),
            dropped: Rc::clone(&gone),
        });
        TABLE.with(|t| t.register(KEY, &stale));
        drop(stale);
        assert!(gone.get());
        assert!(TABLE.with(|t| t.lookup(KEY)).is_none(), "owner is gone");

        let dropped = Rc::new(Cell::new(false));
        let owner = Owner::new(KEY, &dropped);
        assert!(deliver(KEY, "to the new owner"));
        assert_eq!(*owner.shared.log.borrow(), ["to the new owner"]);
    }
}
