//! "Diff in parallel, push serially": the reconcile convention the data-driven
//! bridges (list reconcile, panel/DataContext reconcile) are built on.
//!
//! Noesis is thread-affine: every engine call has to happen on the one main
//! thread, inside [`NoesisSet::Apply`](crate::NoesisSet::Apply), while holding the
//! `NonSend` `NoesisRenderState`. That serial section is the scarcest resource in
//! the frame; anything done there blocks the whole schedule. So we keep it doing
//! the *minimum*: only the FFI pushes themselves.
//!
//! The split, then, is:
//!
//! 1. **Diff in parallel.** Ordinary `Update` systems (fully parallel, no Noesis
//!    state in sight) read the ECS world (component queries, change detection,
//!    the reconcile key being [`Entity`]) and compute a *desired delta*: the
//!    minimal set of engine operations that would bring Noesis in line with the
//!    world. They write that delta into plain `Send` storage: a per-entity
//!    [`NoesisDelta<Op>`] component, or a resource. No FFI happens here, so it
//!    parallelizes against the rest of the app for free.
//!
//! 2. **Push serially.** A single system in [`NoesisSet::Apply`](crate::NoesisSet::Apply),
//!    holding `NonSendMut<NoesisRenderState>`, *drains* each precomputed delta and
//!    pushes it through FFI. It does no diffing (the expensive part already ran
//!    in step 1), so the serial section stays short.
//!
//! Why it matters here specifically: the list bridge must emit *minimal*
//! Add/Remove/Move/Update against the bound `ObservableCollection`, never
//! Clear/Reset (Reset destroys selection + scroll; "Reset is the enemy").
//! Computing that minimal diff is real work; doing it in the parallel `Update`
//! phase keeps it off the serial Apply critical path, and the `Op` queue that
//! crosses the boundary is exactly the minimal delta to replay.
//!
//! ```ignore
//! // Step 1 (parallel `Update`): diff the query into a per-panel delta.
//! fn diff_rows(
//!     mut panels: Query<&mut NoesisDelta<RowOp>, With<UiPanel>>,
//!     rows: Query<(Entity, &Item, &ListedIn), Changed<Item>>,
//! ) {
//!     // …compute minimal Add/Remove/Move/Update keyed by row Entity,
//!     //   `delta.push(op)` for each. No Noesis calls.
//! }
//!
//! // Step 2 (serial `NoesisSet::Apply`): drain + push, no diffing.
//! fn push_rows(
//!     mut panels: Query<(Entity, &mut NoesisDelta<RowOp>)>,
//!     state: Option<NonSendMut<NoesisRenderState>>,
//! ) {
//!     let Some(mut state) = state else { return };
//!     for (panel, mut delta) in &mut panels {
//!         if delta.is_empty() { continue; }
//!         state.apply_row_ops_for(panel, delta.take());
//!     }
//! }
//! ```
//!
//! The existing per-element bridges (`text`, `dp`, …) predate this split and
//! still diff inline inside their Apply system; they are cheap enough that it
//! does not matter. New, heavier reconcile bridges should follow the pattern above
//! and carry their delta in [`NoesisDelta<Op>`].

use bevy::prelude::*;

/// A precomputed batch of pending engine operations for one entity, produced by a
/// parallel `Update` "diff" system and drained by the serial
/// [`NoesisSet::Apply`](crate::NoesisSet::Apply) "push" system. See the
/// [module docs](self) for the full convention.
///
/// `Op` is the bridge's own minimal-operation enum (e.g. a list's
/// Add/Remove/Move/Update). The type is a plain `Send` component (it holds no
/// Noesis handles), so it crosses the parallel→serial boundary safely.
#[derive(Component, Debug)]
#[allow(dead_code)] // scaffolding; not yet wired into a bridge
pub(crate) struct NoesisDelta<Op> {
    ops: Vec<Op>,
}

#[allow(dead_code)] // scaffolding; not yet wired into a bridge
impl<Op> NoesisDelta<Op> {
    /// An empty delta (also the [`Default`]).
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { ops: Vec::new() }
    }

    /// Queue one operation (called from the parallel diff system).
    pub(crate) fn push(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// Whether there is nothing to push this frame (the common case), so the push
    /// system can skip the FFI hop entirely.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Number of queued operations.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.ops.len()
    }

    /// Take the queued operations, leaving the delta empty for next frame. The
    /// drain-and-replay step the serial push system performs.
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
