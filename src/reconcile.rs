//! How the data-driven bridges split work between parallel ECS systems and the
//! serial Noesis section.
//!
//! Noesis is thread-affine: every engine call runs on the main thread inside
//! [`NoesisSet::Apply`](crate::NoesisSet::Apply), holding the non-send
//! `NoesisRenderState`. That section blocks the schedule, so the data bridges
//! keep as much work out of it as they can:
//!
//! 1. **Gather in parallel.** Ordinary `PostUpdate` systems read the ECS
//!    (queries, change detection, [`Entity`] as the key) and write the desired
//!    state into a plain `Send` component that holds no Noesis handles.
//! 2. **Push serially.** One system in `NoesisSet::Apply` reads that component
//!    and makes the FFI calls.
//!
//! [`crate::panel`] collects each changed bound component's snapshot into a
//! per-panel aggregate in parallel, then pushes only those components'
//! properties.
//! [`crate::list`] builds each list's desired ordered rows in parallel; the
//! serial push compares them against the live `ObservableCollection` and emits
//! Add/Remove/Move/Update ops, never a `Reset` (which would drop selection and
//! scroll position). Mixing inserts with reorders can emit more moves than
//! strictly needed.
//!
//! The per-element bridges (`text`, `dp`, ...) do their small comparisons
//! inline in their Apply system.

use bevy::prelude::*;

/// A queue of engine operations for one entity, filled by a parallel system and
/// drained by the serial push in [`NoesisSet::Apply`](crate::NoesisSet::Apply).
/// `Op` is the bridge's own operation enum.
#[derive(Component, Debug)]
#[allow(dead_code)] // scaffolding; not yet wired into a bridge
pub(crate) struct NoesisDelta<Op> {
    ops: Vec<Op>,
}

#[allow(dead_code)] // scaffolding; not yet wired into a bridge
impl<Op> NoesisDelta<Op> {
    /// An empty queue.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Queues one operation.
    pub(crate) fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Whether there is nothing to push, so the push system can skip the FFI.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Number of queued operations.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.ops.len()
    }

    /// Takes the queued operations in push order, leaving the queue empty.
    #[must_use]
    pub(crate) fn take(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.ops)
    }
}

impl<Op> Default for NoesisDelta<Op> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::NoesisDelta;

    #[test]
    fn delta_pushes_drains_and_resets() {
        let mut d: NoesisDelta<i32> = NoesisDelta::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);

        d.push(1);
        d.push(2);
        d.push(3);
        assert!(!d.is_empty());
        assert_eq!(d.len(), 3);

        assert_eq!(d.take(), vec![1, 2, 3]);
        assert!(d.is_empty());
        assert_eq!(d.take(), Vec::<i32>::new());
    }

    #[test]
    fn default_is_empty() {
        let d: NoesisDelta<String> = NoesisDelta::default();
        assert!(d.is_empty());
    }
}
