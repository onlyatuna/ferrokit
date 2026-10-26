// SPDX-License-Identifier: LGPL-3.0-only
//! Abstract model of the thread-owned slot protocol behind `Signal::connect_local`.
//!
//! # Model Scope and Limitations
//!
//! **IMPORTANT**: like `model.rs`, this is an **abstract simplified model** of the protocol in
//! `crates/ferrokit-core/src/signal/local.rs`. It is **NOT** a direct verification of that code:
//! nothing here is compiled from, or linked against, `ferrokit-core`, and the two can drift.
//!
//! What is modeled (names in parentheses are the `local.rs` counterparts):
//!
//! * a per-thread table of non-`Send` slots, a `thread_local!` of `Rc<Slot>` by key
//!   (`LOCAL_SLOTS`), reached only through `LocalKey::try_with` (I1);
//! * a `Send + Sync` [`Handle`] that holds only `{owner, key}` (`LocalHandle`), with `install`,
//!   `call` and a `Drop` that retires the slot on the owner at once or posts the key (I3) to the
//!   owner's queue otherwise;
//! * the owner's queue and its drain (`post_event_to_thread` + the event loop running the
//!   `MetaCallNoTarget` job), reduced to one mutex-protected `Vec<u64>` per live thread; a post
//!   to a thread that has ended is dropped;
//! * thread exit: the table is dropped by the thread-local destructor, on that thread, and the
//!   thread's queue disappears with it (the queue first, then the closures, then the exit is
//!   announced so that a test can wait for it);
//! * the process-wide key counter (I4, [`World::next_key`]) and the "table `RefCell` never
//!   borrowed while user code runs" rule (I2), including a slot whose captures hold the handle
//!   of another slot (so a destructor re-enters the table) and a slot that releases such a
//!   capture while it runs.
//!
//! What is deliberately abstracted away or changed:
//!
//! 1. Payload types, `Any` downcasts, the `Signal` subscriber list, `ConnectionType`,
//!    `catch_unwind`, single-shot/scoped connections, and the once-per-connection diagnostic.
//! 2. The event loop. `drain_retired` is called explicitly by the owner; a thread that never
//!    drains keeps its slots until it exits, as the real owner does without a loop.
//! 3. `ThreadId` and `ObjectId(0)` routing: threads get a model id from a counter the first time
//!    they touch the model, and posts are routed by that id.
//! 4. Identity inside a thread-local destructor. In Loom every mock local of a thread is
//!    destroyed *before* any destructor runs, so the model's thread id is unreadable there
//!    (`thread::current()` is unusable in Loom destructors too). A handle dropped in that window
//!    is therefore treated as foreign and posts; the post is dropped because the owner queue is
//!    already closed, and the slot is dropped by the table being torn down. The real code asks
//!    `ThreadId::current()`, which is always available; the end state (closure dropped exactly
//!    once, on the owner) is the same, the path differs.
//! 5. The bookkeeping `Audit` uses `std::sync::Mutex` on purpose: it is not part of the protocol,
//!    must stay invisible to the schedulers (no extra interleavings) and is never held across a
//!    modeled operation. The key counter, the queues, and the thread-local tables are modeled.
//! 6. Under Loom and Shuttle all mock threads share one OS thread, so "dropped on the owner
//!    thread" is checked against the model thread id, not an OS thread id. The native
//!    `stress_tests` (ThreadSanitizer) run real threads against the same code.
//! 7. Thread-local teardown is awaited with a `Condvar` ([`World::wait_exited`]), not with `join`:
//!    Loom's `join` returns before the joined thread's mock locals are dropped.
//! 8. Loom runs with a preemption bound (see `tests/loom_tests.rs`): interleavings that need more
//!    preemptions than the bound are not explored.
//!
//! The invariants checked by [`World::verify`] after every scenario:
//!
//! * E1: every installed closure's destructor ran exactly once (all threads finished);
//! * E2: it ran on the owner thread, or inside the owner's thread-local teardown (see item 4);
//! * E3: no slot was ever *called* on a thread other than its owner;
//! * E4: no key was installed twice (I4).
//!
//! E5: no protocol violation was recorded in flight: a post to a thread that has ended is dropped
//! (not an error) and the table is never borrowed re-entrantly (I2). The negative tests turn
//! each of these off in turn and expect `verify` to fail.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::AtomicU64 as StdAtomicU64;
use std::sync::atomic::Ordering as StdOrdering;
use std::sync::Mutex as StdMutex;

use crate::sync::sys;
use crate::sync::sys::atomic::{AtomicU64, Ordering};
use crate::sync::sys::{Arc, Condvar, Mutex};

/// Model thread ids. Only compared for equality, so a plain process-wide counter (not a modeled
/// atomic) is enough; it does not take part in the protocol.
static NEXT_THREAD: StdAtomicU64 = StdAtomicU64::new(1);

sys::thread_local! {
    /// This thread's model id. No destructor of its own, but under Loom/Shuttle it is destroyed
    /// with the thread's other mock locals.
    static TID: u64 = NEXT_THREAD.fetch_add(1, StdOrdering::Relaxed);
    /// The slots connected on this thread (`LOCAL_SLOTS`).
    static TABLE: RefCell<Table> = RefCell::new(Table::new());
}

/// Deliberate protocol breakages, used by the negative tests to show that the checks bite.
#[derive(Default, Clone, Copy, Debug)]
pub struct LocalMutants {
    /// Every `install` gets key 1 (breaks I4).
    pub reuse_keys: bool,
    /// `retire_here` drops the removed closure while the table is still borrowed (breaks I2); the
    /// re-entrant borrow is recorded as a violation rather than panicking.
    pub hold_borrow_on_retire: bool,
    /// A post to an exited owner is treated as an error instead of being dropped.
    pub error_on_ended_post: bool,
    /// `call` does not compare the owner first. Not a breakage: the invariants must still hold,
    /// because a foreign thread looks the key up in its own, empty, table (the owner compare is
    /// only a diagnostic).
    pub skip_owner_check: bool,
}

/// Counters of what happened, for tests.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stats {
    pub installed: usize,
    pub calls: usize,
    pub foreign_skips: usize,
    pub drops: usize,
    pub posts_delivered: usize,
    pub posts_to_ended: usize,
    pub threads_exited: usize,
}

#[derive(Default)]
struct Audit {
    /// `(key, owner)`
    installed: Vec<(u64, u64)>,
    /// `(key, owner, caller)`
    calls: Vec<(u64, u64, u64)>,
    /// `(key, owner, thread the destructor ran on; None = thread-local teardown)`
    drops: Vec<(u64, u64, Option<u64>)>,
    foreign_skips: usize,
    posts_delivered: usize,
    posts_to_ended: usize,
    exited: Vec<u64>,
    /// Protocol breaches seen in flight. Recorded instead of panicking in the thread so that a
    /// breach in a thread that still owns a table cannot turn into a panic inside a thread-local
    /// destructor (an abort); [`World::verify`] turns them into a test failure.
    violations: Vec<&'static str>,
}

/// The shared side of the model: key counter, per-thread queues, bookkeeping.
pub struct World {
    next_key: AtomicU64,
    /// Live threads that have installed a slot -> keys posted to them and not yet drained.
    /// An absent entry is a thread that ended.
    queues: Mutex<BTreeMap<u64, Vec<u64>>>,
    /// Threads whose thread-local teardown has finished, and the signal for it.
    exits: Mutex<usize>,
    exited: Condvar,
    audit: StdMutex<Audit>,
    mutants: LocalMutants,
}

impl World {
    pub fn new(mutants: LocalMutants) -> Arc<Self> {
        Arc::new(Self {
            next_key: AtomicU64::new(1),
            queues: Mutex::new(BTreeMap::new()),
            exits: Mutex::new(0),
            exited: Condvar::new(),
            audit: StdMutex::new(Audit::default()),
            mutants,
        })
    }

    fn audit(&self) -> std::sync::MutexGuard<'_, Audit> {
        self.audit.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn alloc_key(&self) -> u64 {
        if self.mutants.reuse_keys {
            1
        } else {
            self.next_key.fetch_add(1, Ordering::Relaxed)
        }
    }

    /// Posts `key` to `owner`'s queue; dropped if that thread has ended.
    fn post_retire(&self, owner: u64, key: u64) {
        let delivered = {
            let mut queues = self.queues.lock().unwrap();
            match queues.get_mut(&owner) {
                Some(queue) => {
                    queue.push(key);
                    true
                }
                None => false,
            }
        };
        {
            let mut audit = self.audit();
            if delivered {
                audit.posts_delivered += 1;
            } else {
                audit.posts_to_ended += 1;
            }
        }
        if !delivered && self.mutants.error_on_ended_post {
            self.audit()
                .violations
                .push("a retire posted to an ended owner thread was treated as an error");
        }
    }

    /// The owner thread starts to end: its queue disappears, pending keys with it.
    fn close_queue(&self, tid: u64) {
        let pending = self.queues.lock().unwrap().remove(&tid);
        drop(pending);
    }

    /// The owner thread's table is gone, closures included.
    fn finish_exit(&self, tid: u64) {
        self.audit().exited.push(tid);
        *self.exits.lock().unwrap() += 1;
        self.exited.notify_all();
    }

    /// Blocks until `n` threads that installed a slot have finished their thread-local teardown.
    /// `join` is not enough under Loom (it returns before the joined thread's locals are
    /// dropped), so every scenario waits here before [`World::verify`].
    pub fn wait_exited(&self, n: usize) {
        let mut exits = self.exits.lock().unwrap();
        while *exits < n {
            exits = self.exited.wait(exits).unwrap();
        }
    }

    pub fn stats(&self) -> Stats {
        let audit = self.audit();
        Stats {
            installed: audit.installed.len(),
            calls: audit.calls.len(),
            foreign_skips: audit.foreign_skips,
            drops: audit.drops.len(),
            posts_delivered: audit.posts_delivered,
            posts_to_ended: audit.posts_to_ended,
            threads_exited: audit.exited.len(),
        }
    }

    /// How many times the closure installed under `key` was dropped.
    pub fn drop_count(&self, key: u64) -> usize {
        self.audit().drops.iter().filter(|d| d.0 == key).count()
    }

    /// The thread each destructor of `key` ran on (`None` = inside thread-local teardown).
    pub fn drop_sites(&self, key: u64) -> Vec<Option<u64>> {
        self.audit()
            .drops
            .iter()
            .filter(|d| d.0 == key)
            .map(|d| d.2)
            .collect()
    }

    /// Checks E1-E4. Call after every thread that touched the world has been joined.
    pub fn verify(&self) {
        let audit = self.audit();

        assert!(
            audit.violations.is_empty(),
            "protocol violations: {:?}",
            audit.violations
        );

        // E4
        let mut keys: Vec<u64> = audit.installed.iter().map(|i| i.0).collect();
        let total = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(
            keys.len(),
            total,
            "a key was installed twice: {:?}",
            audit.installed
        );

        for &(key, _) in &audit.installed {
            // E1
            let n = audit.drops.iter().filter(|d| d.0 == key).count();
            assert_eq!(
                n, 1,
                "closure {key} was dropped {n} times, expected exactly once"
            );
        }
        // E2
        for &(key, owner, here) in &audit.drops {
            if let Some(here) = here {
                assert_eq!(
                    here, owner,
                    "closure {key} of thread {owner} dropped on thread {here}"
                );
            }
        }
        // E3
        for &(key, owner, caller) in &audit.calls {
            assert_eq!(
                caller, owner,
                "closure {key} of thread {owner} called on thread {caller}"
            );
        }
    }
}

/// What the closure of a slot captures and does.
#[derive(Default)]
pub struct SlotSpec {
    /// A handle held by the closure, like a closure that owns a clone of another signal. Its
    /// destructor re-enters the table when this slot is retired.
    pub capture: Option<Arc<Handle>>,
    /// The closure drops `capture` the first time it runs (a slot that disconnects another).
    pub release_on_call: bool,
}

/// The non-`Send` closure. Only ever reachable through `Rc` in a thread's table.
struct Slot {
    key: u64,
    owner: u64,
    world: Arc<World>,
    capture: RefCell<Option<Arc<Handle>>>,
    release_on_call: bool,
    runs: Cell<u32>,
}

impl Slot {
    fn run(&self) {
        let here = TID.with(|t| *t);
        {
            let mut audit = self.world.audit();
            audit.calls.push((self.key, self.owner, here));
        }
        self.runs.set(self.runs.get() + 1);
        if self.release_on_call {
            let released = self.capture.borrow_mut().take();
            drop(released);
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        // `try_with`: this can run inside the owner's thread-local teardown.
        let here = TID.try_with(|t| *t).ok();
        self.world.audit().drops.push((self.key, self.owner, here));
    }
}

struct Table {
    slots: BTreeMap<u64, Rc<Slot>>,
    /// Set by the first `install`: this thread's id and the world whose queue it listens on.
    home: Option<(u64, Arc<World>)>,
}

impl Table {
    fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            home: None,
        }
    }
}

impl Drop for Table {
    fn drop(&mut self) {
        // Thread exit: the queue goes first, then the closures are dropped, then the exit is
        // announced.
        if let Some((tid, world)) = self.home.take() {
            world.close_queue(tid);
            drop(std::mem::take(&mut self.slots));
            world.finish_exit(tid);
        }
    }
}

/// Names a slot in the table of the thread that installed it (`LocalHandle`). Holds no closure,
/// so it is `Send + Sync` without `unsafe`.
pub struct Handle {
    world: Arc<World>,
    owner: u64,
    key: u64,
}

fn assert_send_sync<T: Send + Sync>() {}
const _: fn() = assert_send_sync::<Handle>;

/// Stores a closure described by `spec` in this thread's table and returns its handle.
pub fn install(world: &Arc<World>, spec: SlotSpec) -> Arc<Handle> {
    let owner = TID.with(|t| *t);
    let key = world.alloc_key();
    {
        let mut audit = world.audit();
        audit.installed.push((key, owner));
    }
    let slot = Rc::new(Slot {
        key,
        owner,
        world: Arc::clone(world),
        capture: RefCell::new(spec.capture),
        release_on_call: spec.release_on_call,
        runs: Cell::new(0),
    });
    let displaced = TABLE.with(|table| {
        let mut table = table.borrow_mut();
        match &table.home {
            Some((_, home)) => assert!(
                Arc::ptr_eq(home, world),
                "model limitation: one world per thread"
            ),
            None => {
                world.queues.lock().unwrap().insert(owner, Vec::new());
                table.home = Some((owner, Arc::clone(world)));
            }
        }
        table.slots.insert(key, slot)
    });
    if displaced.is_some() {
        world
            .audit()
            .violations
            .push("a key was installed twice in one table");
    }
    drop(displaced);
    Arc::new(Handle {
        world: Arc::clone(world),
        owner,
        key,
    })
}

impl Handle {
    pub fn key(&self) -> u64 {
        self.key
    }

    pub fn owner(&self) -> u64 {
        self.owner
    }

    /// Runs the slot if this is the owner thread and the slot is still installed. Returns whether
    /// it ran.
    pub fn call(&self) -> bool {
        let here = TID.with(|t| *t);
        if here != self.owner && !self.world.mutants.skip_owner_check {
            self.world.audit().foreign_skips += 1;
            return false;
        }
        // I1, I2: clone the `Rc` out, so the borrow ends before the closure runs.
        let entry = TABLE
            .try_with(|table| table.borrow().slots.get(&self.key).cloned())
            .ok()
            .flatten();
        match entry {
            Some(slot) => {
                slot.run();
                true
            }
            None => {
                if here != self.owner {
                    self.world.audit().foreign_skips += 1;
                }
                false
            }
        }
    }
}

/// Removes `key` from this thread's table and drops its closure outside the borrow (I2).
fn retire_here(world: &World, key: u64) {
    if world.mutants.hold_borrow_on_retire {
        let _ = TABLE.try_with(|table| match table.try_borrow_mut() {
            Ok(mut table) => {
                let removed = table.slots.remove(&key);
                drop(removed); // table still borrowed while the closure's destructor runs
            }
            // A destructor re-entered the table while it was borrowed: with `borrow_mut` this is
            // the `BorrowMutError` panic that I2 exists to prevent.
            Err(_) => world
                .audit()
                .violations
                .push("the table was re-entered while borrowed"),
        });
        return;
    }
    let removed = TABLE
        .try_with(|table| table.borrow_mut().slots.remove(&key))
        .ok()
        .flatten();
    drop(removed);
}

impl Drop for Handle {
    fn drop(&mut self) {
        // No thread id available (own thread-local teardown): treat as foreign and post. Either
        // the owner's queue is already closed (dropped) or this is another thread, for which
        // posting is the normal path.
        match TID.try_with(|t| *t) {
            Ok(here) if here == self.owner => retire_here(&self.world, self.key),
            _ => self.world.post_retire(self.owner, self.key),
        }
    }
}

/// The owner runs the keys posted to it (the event loop turn that runs the `MetaCallNoTarget`
/// jobs). Returns how many keys it took.
pub fn drain_retired(world: &World) -> usize {
    let Ok(me) = TID.try_with(|t| *t) else {
        return 0;
    };
    let keys = {
        let mut queues = world.queues.lock().unwrap();
        queues.get_mut(&me).map(std::mem::take).unwrap_or_default()
    };
    for key in &keys {
        retire_here(world, *key);
    }
    keys.len()
}

/// Scenarios shared by the Loom, Shuttle, std and ThreadSanitizer tests. Every scenario joins all
/// of its threads and ends with [`World::verify`].
pub mod scenarios {
    use super::*;

    fn plain() -> SlotSpec {
        SlotSpec::default()
    }

    /// The owner installs a slot and shares its handle with a second thread. The second thread
    /// calls it (skipped) and drops its clone; the owner calls it, drains, drops its own clone,
    /// drains again, exits. Whichever clone is last dropped retires the slot (at once on the
    /// owner, by post otherwise, or by the table destructor if the post came too late).
    pub fn foreign_drop_then_owner_drain(mutants: LocalMutants) {
        let world = World::new(mutants);
        let w = Arc::clone(&world);
        let owner = sys::thread::spawn(move || {
            let h = install(&w, plain());
            let hf = Arc::clone(&h);
            let foreign = sys::thread::spawn(move || {
                assert!(!hf.call(), "a slot must not run off its owner");
                drop(hf);
            });
            assert!(h.call());
            drain_retired(&w);
            drop(h);
            drain_retired(&w);
            foreign
        });
        let foreign = owner.join().unwrap();
        foreign.join().unwrap();
        world.wait_exited(1);
        world.verify();
    }

    /// As above, but the owner exits without waiting: the foreign drop races the thread exit and
    /// may post to an owner that has already ended.
    pub fn foreign_drop_after_owner_exit(mutants: LocalMutants) {
        let world = World::new(mutants);
        let w = Arc::clone(&world);
        let owner = sys::thread::spawn(move || {
            let h = install(&w, plain());
            let hf = Arc::clone(&h);
            let foreign = sys::thread::spawn(move || {
                assert!(!hf.call(), "a slot must not run off its owner");
                drop(hf);
            });
            assert!(h.call());
            drop(h);
            foreign
        });
        let foreign = owner.join().unwrap();
        foreign.join().unwrap();
        world.wait_exited(1);
        world.verify();
    }

    /// Slot A's closure owns the handle of slot B. Retiring A (by a foreign drop that is
    /// drained, or by the owner) drops B's handle inside A's destructor, re-entering the table.
    pub fn nested_retire(mutants: LocalMutants) {
        let world = World::new(mutants);
        let w = Arc::clone(&world);
        let owner = sys::thread::spawn(move || {
            let hb = install(&w, plain());
            let ha = install(
                &w,
                SlotSpec {
                    capture: Some(hb),
                    release_on_call: false,
                },
            );
            let hf = Arc::clone(&ha);
            let foreign = sys::thread::spawn(move || {
                assert!(!hf.call());
                drop(hf);
            });
            drain_retired(&w);
            drop(ha);
            drain_retired(&w);
            foreign
        });
        let foreign = owner.join().unwrap();
        foreign.join().unwrap();
        world.wait_exited(1);
        world.verify();
    }

    /// Slot A drops its capture (B's handle) while it runs, so B is retired in the middle of a
    /// call, with A cloned out of the table and the table not borrowed.
    pub fn call_releases_capture(mutants: LocalMutants) {
        let world = World::new(mutants);
        let w = Arc::clone(&world);
        let owner = sys::thread::spawn(move || {
            let hb = install(&w, plain());
            let ha = install(
                &w,
                SlotSpec {
                    capture: Some(hb),
                    release_on_call: true,
                },
            );
            let hf = Arc::clone(&ha);
            let foreign = sys::thread::spawn(move || {
                assert!(!hf.call());
                drop(hf);
            });
            assert!(ha.call());
            drain_retired(&w);
            drop(ha);
            foreign
        });
        let foreign = owner.join().unwrap();
        foreign.join().unwrap();
        world.wait_exited(1);
        world.verify();
    }

    /// Two owners install concurrently (the shared key counter), hand their handles out and exit.
    /// The joiner then calls and drops both handles from a third thread: both calls are skipped
    /// and both drops are posts to ended threads, which must be dropped without a panic.
    pub fn two_owners_exit_then_foreign_use(mutants: LocalMutants) {
        let world = World::new(mutants);
        let spawn_owner = |w: Arc<World>| {
            sys::thread::spawn(move || {
                let h = install(&w, plain());
                assert!(h.call());
                drain_retired(&w);
                h
            })
        };
        let a = spawn_owner(Arc::clone(&world));
        let b = spawn_owner(Arc::clone(&world));
        let ha = a.join().unwrap();
        let hb = b.join().unwrap();
        world.wait_exited(2);
        assert_ne!(ha.key(), hb.key());
        assert_ne!(ha.owner(), hb.owner());
        assert!(!ha.call());
        assert!(!hb.call());
        drop(ha);
        drop(hb);
        assert_eq!(world.stats().posts_to_ended, 2);
        world.verify();
    }
}
