// SPDX-License-Identifier: LGPL-3.0-only
//! Scaled-down hierarchy transaction model for Loom and Shuttle.

use crate::sync::sys::*;

pub type ObjectId = u32;

#[derive(Debug)]
pub struct ObjectRecord {
    pub id: ObjectId,
    pub parent: RwLock<Option<ObjectId>>, // L2
    pub children: RwLock<Vec<ObjectId>>,  // L1
}

impl ObjectRecord {
    pub fn new(id: ObjectId) -> Self {
        Self {
            id,
            parent: RwLock::new(None),
            children: RwLock::new(Vec::new()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelError {
    CycleDetected,
    AncestorConflict,
    InvalidParent,
    LockViolation,
}

#[derive(Default, Clone, Copy, Debug)]
pub struct MutantConfig {
    pub omit_h: bool,
    pub invert_l1: bool,
    pub skip_revalidation: bool,
}

pub struct HierarchyRegistry {
    pub h: Mutex<()>,                          // H
    pub l0: RwLock<()>,                        // L0
    pub records: Vec<Arc<ObjectRecord>>,
    pub mutants: MutantConfig,
}

impl HierarchyRegistry {
    pub fn new(count: usize) -> Self {
        Self::with_mutants(count, MutantConfig::default())
    }

    pub fn with_mutants(count: usize, mutants: MutantConfig) -> Self {
        let mut records = Vec::with_capacity(count);
        for i in 0..count {
            records.push(Arc::new(ObjectRecord::new(i as ObjectId)));
        }
        Self {
            h: Mutex::new(()),
            l0: RwLock::new(()),
            records,
            mutants,
        }
    }

    #[inline]
    pub fn get(&self, id: ObjectId) -> Arc<ObjectRecord> {
        Arc::clone(&self.records[id as usize])
    }

    /// Walk ancestor chain to check if `child_id` would become an ancestor of itself if placed under `new_parent_id`.
    /// Caller MUST hold H (or L0 read lock).
    pub fn would_create_cycle(&self, child_id: ObjectId, new_parent_id: ObjectId) -> bool {
        if child_id == new_parent_id {
            return true;
        }
        let mut cur = Some(new_parent_id);
        let mut depth = 0;
        while let Some(pid) = cur {
            if pid == child_id {
                return true;
            }
            depth += 1;
            if depth > 50 {
                return true;
            }
            let rec = self.get(pid);
            let p_guard = rec.parent.read().unwrap();
            cur = *p_guard;
        }
        false
    }

    /// Check if `ancestor_id` is an ancestor of `descendant_id`.
    pub fn is_ancestor(&self, ancestor_id: ObjectId, descendant_id: ObjectId) -> bool {
        if ancestor_id == descendant_id {
            return true;
        }
        let mut cur = Some(descendant_id);
        let mut depth = 0;
        while let Some(pid) = cur {
            if pid == ancestor_id {
                return true;
            }
            depth += 1;
            if depth > 50 {
                return true;
            }
            let rec = self.get(pid);
            let p_guard = rec.parent.read().unwrap();
            cur = *p_guard;
        }
        false
    }

    /// Set child's parent to new_parent (or None to detach).
    pub fn set_parent(&self, child_id: ObjectId, new_parent: Option<ObjectId>) -> Result<(), ModelError> {
        if Some(child_id) == new_parent {
            return Err(ModelError::InvalidParent);
        }

        // 1. Acquire H unless omitted by mutant
        let _h = if !self.mutants.omit_h {
            Some(self.h.lock().unwrap())
        } else {
            None
        };

        // 2. Acquire L0 (read)
        let _l0 = self.l0.read().unwrap();

        let child_rec = self.get(child_id);
        let old_parent = *child_rec.parent.read().unwrap();
        if old_parent == new_parent {
            return Ok(()); // no-op
        }

        // 3. Cycle check
        if let Some(np) = new_parent {
            if self.would_create_cycle(child_id, np) {
                return Err(ModelError::CycleDetected);
            }
        }

        // 4. Lock parents' L1 write guards
        // Rule: Ascending order (min_id < max_id) unless inverted by mutant
        let (first_p, second_p) = match (old_parent, new_parent) {
            (Some(o), Some(n)) => {
                if !self.mutants.invert_l1 {
                    if o < n { (Some(self.get(o)), Some(self.get(n))) } else { (Some(self.get(n)), Some(self.get(o))) }
                } else {
                    // Deliberately inverted order for mutant testing: argument order (old, then new)
                    (Some(self.get(o)), Some(self.get(n)))
                }
            }
            (Some(o), None) => (Some(self.get(o)), None),
            (None, Some(n)) => (Some(self.get(n)), None),
            (None, None) => (None, None),
        };

        let mut g1 = first_p.as_ref().map(|r| r.children.write().unwrap());
        let mut g2 = second_p.as_ref().map(|r| r.children.write().unwrap());

        // 5. Lock child's L2 write guard
        let mut child_p_guard = child_rec.parent.write().unwrap();

        // Pre-commit revalidation:
        if !self.mutants.skip_revalidation {
            if *child_p_guard != old_parent {
                return Err(ModelError::LockViolation);
            }
        }

        // 6. Commit phase
        if let Some(op) = old_parent {
            if first_p.as_ref().map(|r| r.id) == Some(op) {
                let list = g1.as_deref_mut().unwrap();
                list.retain(|&id| id != child_id);
            } else if second_p.as_ref().map(|r| r.id) == Some(op) {
                let list = g2.as_deref_mut().unwrap();
                list.retain(|&id| id != child_id);
            }
        }

        if let Some(np) = new_parent {
            if first_p.as_ref().map(|r| r.id) == Some(np) {
                let list = g1.as_deref_mut().unwrap();
                if !list.contains(&child_id) {
                    list.push(child_id);
                }
            } else if second_p.as_ref().map(|r| r.id) == Some(np) {
                let list = g2.as_deref_mut().unwrap();
                if !list.contains(&child_id) {
                    list.push(child_id);
                }
            }
        }

        *child_p_guard = new_parent;

        // 7. Strict release order: L2 -> L1 -> L0 -> H
        drop(child_p_guard);
        drop(g2);
        drop(g1);
        drop(_l0);
        drop(_h);

        Ok(())
    }

    /// Swap children lists between object a and object b.
    pub fn swap_hierarchy(&self, a_id: ObjectId, b_id: ObjectId) -> Result<(), ModelError> {
        if a_id == b_id {
            return Ok(());
        }

        // 1. Acquire H
        let _h = if !self.mutants.omit_h {
            Some(self.h.lock().unwrap())
        } else {
            None
        };

        // 2. Acquire L0
        let _l0 = self.l0.read().unwrap();

        // Flawed mutant pre-validation (B2 lost update reproducer):
        let stale_snapshots = if self.mutants.skip_revalidation {
            let s_a = self.get(a_id).children.read().unwrap().clone();
            let s_b = self.get(b_id).children.read().unwrap().clone();
            Some((s_a, s_b))
        } else {
            None
        };

        // 3. Ancestor check
        if self.is_ancestor(a_id, b_id) || self.is_ancestor(b_id, a_id) {
            return Err(ModelError::AncestorConflict);
        }

        // 4. Lock a and b in ascending order (L1) unless inverted
        let (first_id, second_id) = if !self.mutants.invert_l1 {
            if a_id < b_id { (a_id, b_id) } else { (b_id, a_id) }
        } else {
            (a_id, b_id) // argument order
        };

        let first = self.get(first_id);
        let second = self.get(second_id);

        let mut g_first = first.children.write().unwrap();
        let mut g_second = second.children.write().unwrap();
        // Collect all children to update L2
        let mut all_children = Vec::new();
        let (a_children, b_children) = if first_id == a_id {
            (g_first.clone(), g_second.clone())
        } else {
            (g_second.clone(), g_first.clone())
        };

        for &c in a_children.iter() {
            all_children.push((c, b_id));
        }
        for &c in b_children.iter() {
            all_children.push((c, a_id));
        }

        // Sort children in ascending ObjectId order for L2 locking
        all_children.sort_by_key(|&(c_id, _)| c_id);

        let child_recs: Vec<_> = all_children
            .iter()
            .map(|&(c_id, _)| self.get(c_id))
            .collect();
        let mut child_guards: Vec<_> = child_recs
            .iter()
            .map(|rec| rec.parent.write().unwrap())
            .collect();

        // 5. Commit phase
        for (idx, &(_, new_p)) in all_children.iter().enumerate() {
            *child_guards[idx] = Some(new_p);
        }

        if let Some((s_a, s_b)) = stale_snapshots {
            if first_id == a_id {
                *g_first = s_b;
                *g_second = s_a;
            } else {
                *g_first = s_a;
                *g_second = s_b;
            }
        } else {
            std::mem::swap(&mut *g_first, &mut *g_second);
        }

        // 6. Release in reverse order: L2 -> L1 -> L0 -> H
        drop(child_guards);
        drop(g_second);
        drop(g_first);
        drop(_l0);
        drop(_h);

        Ok(())
    }

    /// Reparent an owned child from old_parent to new_parent.
    pub fn reparent(&self, child_id: ObjectId, _old_parent: ObjectId, new_parent: ObjectId) -> Result<(), ModelError> {
        self.set_parent(child_id, Some(new_parent))
    }

    /// Assert all tree invariants:
    /// 1. No cycles (finite acyclic paths terminating at None).
    /// 2. Bidirectional parent-child consistency (p.parent == Some(parent) <=> parent.children contains p).
    /// 3. Unique containment (no child appears multiple times or in multiple parents' children).
    pub fn assert_invariants(&self) {
        let count = self.records.len();

        for i in 0..count {
            let child_id = i as ObjectId;
            let child_rec = &self.records[i];
            let parent_id = *child_rec.parent.read().unwrap();

            // 1. No cycle check
            let mut cur = parent_id;
            let mut visited = Vec::new();
            while let Some(pid) = cur {
                assert!(pid != child_id, "CYCLE DETECTED: child {} is its own ancestor!", child_id);
                assert!(!visited.contains(&pid), "CYCLE DETECTED: cycle among ancestors of {} at {}!", child_id, pid);
                visited.push(pid);
                assert!(visited.len() <= count, "Cycle detected: path longer than object count!");
                let p_rec = &self.records[pid as usize];
                cur = *p_rec.parent.read().unwrap();
            }

            // 2. Bidirectional consistency
            if let Some(pid) = parent_id {
                let p_rec = &self.records[pid as usize];
                let p_children = p_rec.children.read().unwrap();
                assert!(
                    p_children.contains(&child_id),
                    "INCONSISTENCY: child {} has parent {}, but parent's children list does not contain it!",
                    child_id, pid
                );
            }

            // Verify that if any node lists child_id, child_id's parent is that node
            for other_i in 0..count {
                let other_rec = &self.records[other_i];
                let other_children = other_rec.children.read().unwrap();
                let occurrences = other_children.iter().filter(|&&c| c == child_id).count();
                assert!(occurrences <= 1, "DUPLICATE: child {} appears multiple times in parent {}!", child_id, other_i);

                if other_children.contains(&child_id) {
                    assert_eq!(
                        parent_id,
                        Some(other_i as ObjectId),
                        "LOST UPDATE / ORPHAN: parent {} claims child {}, but child's parent pointer is {:?}!",
                        other_i, child_id, parent_id
                    );
                }
            }
        }
    }
}
