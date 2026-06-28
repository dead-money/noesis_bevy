//! Generic dependency-property get/set bridge (TODO §3), keyed by
//! `(x:Name, property)`.
//!
//! A near-mechanical generalization of [`crate::text`]: where the text bridge
//! reads/writes the one fixed `Text` property, this reads/writes *any* named
//! dependency property by name and value type. It's the binding-free fallback —
//! poke `Slider.Value`, read `CheckBox.IsChecked`, flip `Button.IsEnabled` —
//! when wiring a full [`ViewModel`](crate::viewmodel) would be overkill.
//!
//! Two halves, exactly like [`crate::text`]:
//!
//! 1. **Write** — [`NoesisDpRequests`] queues `(x:Name, property, value)` writes
//!    (`set_f32` / `set_f64` / `set_i32` / `set_bool` / `set_string`), drained
//!    render-side and applied via `FrameworkElement::set_*`.
//! 2. **Read** — push a [`DpWatch`] onto [`NoesisDpReadWatch`] to subscribe; the
//!    render world polls the property each frame, dedupes against the previous
//!    snapshot, and emits a [`NoesisDpChanged`] message when it differs. The
//!    first frame after subscription always emits (the snapshot starts empty),
//!    so callers see the current value without issuing a probe.
//!
//! # Value types
//!
//! Noesis is a float engine — many "numeric" properties (`Slider.Value`,
//! `Width`, `Opacity`) are **`f32`**, not `f64`, so reach for [`DpKind::F32`] /
//! [`NoesisDpRequests::set_f32`] there; a `get_f64` against an `f32` property
//! type-mismatches and reads nothing. `CheckBox.IsChecked` is `Nullable<bool>`
//! and is *not* reachable through [`DpKind::Bool`] — bind it through a
//! [`ViewModel`](crate::viewmodel) instead.
//!
//! ```ignore
//! use dm_noesis_bevy::dp::{NoesisDpRequests, NoesisDpReadWatch, NoesisDpChanged, DpWatch, DpKind, DpValue};
//!
//! fn setup(mut commands: Commands, dp: Res<NoesisDpRequests>) {
//!     dp.set_f32("VolumeSlider", "Value", 0.8);                 // Rust -> UI
//!     commands.insert_resource(NoesisDpReadWatch::new([          // subscribe to reads
//!         DpWatch::new("VolumeSlider", "Value", DpKind::F32),
//!     ]));
//! }
//!
//! fn on_change(mut changed: MessageReader<NoesisDpChanged>) {
//!     for ev in changed.read() {
//!         if let ("VolumeSlider", DpValue::F32(v)) = (ev.name.as_str(), &ev.value) { /* ... */ }
//!     }
//! }
//! ```

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};
use dm_noesis_runtime::view::FrameworkElement;

use crate::render::NoesisRenderState;

// ─────────────────────────────────────────────────────────────────────────────
// Value + kind
// ─────────────────────────────────────────────────────────────────────────────

/// A typed dependency-property value crossing the bridge in either direction.
/// The variant selects the runtime getter/setter, so it must match the
/// property's actual Noesis type (see the module docs on `f32` vs `f64`).
#[derive(Debug, Clone, PartialEq)]
pub enum DpValue {
    F32(f32),
    F64(f64),
    I32(i32),
    Bool(bool),
    Str(String),
}

impl DpValue {
    /// Write this value into `element`'s `property` dependency property. Returns
    /// `false` on unknown property or type mismatch.
    #[must_use]
    pub fn write_to(&self, element: &mut FrameworkElement, property: &str) -> bool {
        match self {
            Self::F32(v) => element.set_f32(property, *v),
            Self::F64(v) => element.set_f64(property, *v),
            Self::I32(v) => element.set_i32(property, *v),
            Self::Bool(v) => element.set_bool(property, *v),
            Self::Str(v) => element.set_string(property, v),
        }
    }
}

/// Which value type to read a watched property as — picks the runtime getter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpKind {
    F32,
    F64,
    I32,
    Bool,
    Str,
}

impl DpKind {
    /// Read `element`'s `property` as this kind. `None` on unknown property or
    /// type mismatch.
    #[must_use]
    pub fn read_from(self, element: &FrameworkElement, property: &str) -> Option<DpValue> {
        match self {
            Self::F32 => element.get_f32(property).map(DpValue::F32),
            Self::F64 => element.get_f64(property).map(DpValue::F64),
            Self::I32 => element.get_i32(property).map(DpValue::I32),
            Self::Bool => element.get_bool(property).map(DpValue::Bool),
            Self::Str => element.get_string(property).map(DpValue::Str),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Write side — NoesisDpRequests
// ─────────────────────────────────────────────────────────────────────────────

/// Main-app queue of pending property writes. Push via `set_*`; the render
/// world drains and applies during `RenderSystems::Prepare`. The render world
/// receives an `Arc`-aliased copy via [`ExtractResource`], so writes pushed
/// here are drained render-side without copying.
///
/// All methods take `&self`, so they're callable from a plain
/// `Res<NoesisDpRequests>`. Writes to the same `(name, property)` within a frame
/// apply in order (last wins).
#[derive(Resource, Clone, Default)]
pub struct NoesisDpRequests {
    queue: Arc<Mutex<Vec<(String, String, DpValue)>>>,
}

impl NoesisDpRequests {
    /// Queue an `f32` write — the right choice for Noesis's float-typed
    /// properties (`Slider.Value`, `Width`, `Opacity`, …).
    pub fn set_f32(&self, name: impl Into<String>, property: impl Into<String>, value: f32) {
        self.push(name, property, DpValue::F32(value));
    }

    /// Queue an `f64` (`Double`) write.
    pub fn set_f64(&self, name: impl Into<String>, property: impl Into<String>, value: f64) {
        self.push(name, property, DpValue::F64(value));
    }

    /// Queue an `i32` write.
    pub fn set_i32(&self, name: impl Into<String>, property: impl Into<String>, value: i32) {
        self.push(name, property, DpValue::I32(value));
    }

    /// Queue a `bool` write (plain `Boolean` DPs; not `CheckBox.IsChecked`).
    pub fn set_bool(&self, name: impl Into<String>, property: impl Into<String>, value: bool) {
        self.push(name, property, DpValue::Bool(value));
    }

    /// Queue a `String` write.
    pub fn set_string(
        &self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: impl Into<String>,
    ) {
        self.push(name, property, DpValue::Str(value.into()));
    }

    fn push(&self, name: impl Into<String>, property: impl Into<String>, value: DpValue) {
        self.queue
            .lock()
            .expect("NoesisDpRequests queue poisoned")
            .push((name.into(), property.into(), value));
    }

    /// Drain pending writes. Render-world only; cheap when empty.
    pub(crate) fn drain(&self) -> Vec<(String, String, DpValue)> {
        let mut guard = self.queue.lock().expect("NoesisDpRequests queue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

impl ExtractResource for NoesisDpRequests {
    type Source = NoesisDpRequests;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Read side — NoesisDpReadWatch + NoesisDpChanged
// ─────────────────────────────────────────────────────────────────────────────

/// One subscription: an element's `x:Name`, the `property` to read, and the
/// [`DpKind`] to read it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpWatch {
    pub name: String,
    pub property: String,
    pub kind: DpKind,
}

impl DpWatch {
    pub fn new(name: impl Into<String>, property: impl Into<String>, kind: DpKind) -> Self {
        Self {
            name: name.into(),
            property: property.into(),
            kind,
        }
    }
}

/// Properties the render world should poll each frame, emitting a
/// [`NoesisDpChanged`] whenever a value differs from the previous frame.
/// Mirrors [`crate::text::NoesisTextReadWatch`] — add entries to subscribe,
/// remove them to unsubscribe. A newly-added entry emits on its first resolved
/// frame (the snapshot starts empty).
#[derive(Resource, ExtractResource, Clone, Default, Debug)]
pub struct NoesisDpReadWatch {
    pub entries: Vec<DpWatch>,
}

impl NoesisDpReadWatch {
    pub fn new(entries: impl IntoIterator<Item = DpWatch>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

/// Bevy message written in `PreUpdate` when a watched property differs from the
/// previous frame's snapshot. Read with `MessageReader<NoesisDpChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisDpChanged {
    /// `x:Name` of the element whose property changed.
    pub name: String,
    /// The property that changed.
    pub property: String,
    /// Current value, read as the [`DpWatch::kind`] requested.
    pub value: DpValue,
}

/// Shared queue between the render-world poll and the main-world drain. `Clone`
/// is an `Arc` clone; both apps see the same `Vec`. Mirrors
/// [`crate::text::SharedTextChangedQueue`].
#[derive(Resource, Clone, Default)]
pub struct SharedDpChangedQueue(Arc<Mutex<Vec<(String, String, DpValue)>>>);

impl ExtractResource for SharedDpChangedQueue {
    type Source = SharedDpChangedQueue;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

impl SharedDpChangedQueue {
    pub(crate) fn push(&self, name: String, property: String, value: DpValue) {
        self.0
            .lock()
            .expect("SharedDpChangedQueue poisoned")
            .push((name, property, value));
    }

    fn drain(&self) -> Vec<(String, String, DpValue)> {
        let mut guard = self.0.lock().expect("SharedDpChangedQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Render-app system: drain the write queue onto the live view.
pub(crate) fn apply_dp_writes(
    requests: Option<Res<NoesisDpRequests>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(requests), Some(mut state)) = (requests, state) else {
        return;
    };
    state.apply_dp_writes(&requests);
}

/// Render-app system: poll watched properties and push changes onto the queue.
pub(crate) fn poll_dp_reads(
    watch: Option<Res<NoesisDpReadWatch>>,
    queue: Option<Res<SharedDpChangedQueue>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(watch), Some(queue), Some(mut state)) = (watch, queue, state) else {
        return;
    };
    state.poll_dp_reads(&watch.entries, &queue);
}

/// Main-app system: drain the shared queue into [`NoesisDpChanged`] messages.
/// Runs in `PreUpdate` so Update-stage systems see them the same frame.
pub fn drain_dp_changed_queue(
    queue: Res<SharedDpChangedQueue>,
    mut messages: MessageWriter<NoesisDpChanged>,
) {
    for (name, property, value) in queue.drain() {
        messages.write(NoesisDpChanged {
            name,
            property,
            value,
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the generic DP write + read bridges. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisDpPlugin;

impl Plugin for NoesisDpPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisDpRequests>()
            .init_resource::<NoesisDpReadWatch>()
            .insert_resource(SharedDpChangedQueue::default())
            .add_message::<NoesisDpChanged>()
            .add_plugins((
                ExtractResourcePlugin::<NoesisDpRequests>::default(),
                ExtractResourcePlugin::<NoesisDpReadWatch>::default(),
                ExtractResourcePlugin::<SharedDpChangedQueue>::default(),
            ))
            .add_systems(PreUpdate, drain_dp_changed_queue);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app.add_systems(
            Render,
            (apply_dp_writes, poll_dp_reads).in_set(RenderSystems::Prepare),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_queue_round_trips_each_setter() {
        let dp = NoesisDpRequests::default();
        dp.set_f32("S", "Value", 0.5);
        dp.set_f64("S", "Double", 1.5);
        dp.set_i32("C", "SelectedIndex", 2);
        dp.set_bool("B", "IsEnabled", false);
        dp.set_string("T", "Text", "hi");

        let drained = dp.drain();
        assert_eq!(
            drained,
            vec![
                ("S".into(), "Value".into(), DpValue::F32(0.5)),
                ("S".into(), "Double".into(), DpValue::F64(1.5)),
                ("C".into(), "SelectedIndex".into(), DpValue::I32(2)),
                ("B".into(), "IsEnabled".into(), DpValue::Bool(false)),
                ("T".into(), "Text".into(), DpValue::Str("hi".into())),
            ],
        );
        assert!(dp.drain().is_empty());
    }

    #[test]
    fn changed_queue_drains_in_push_order() {
        let q = SharedDpChangedQueue::default();
        q.push("S".into(), "Value".into(), DpValue::F32(0.25));
        q.push("B".into(), "IsEnabled".into(), DpValue::Bool(true));
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![
                ("S".into(), "Value".into(), DpValue::F32(0.25)),
                ("B".into(), "IsEnabled".into(), DpValue::Bool(true)),
            ],
        );
        assert!(q.drain().is_empty());
    }

    #[test]
    fn watch_constructor_collects_entries() {
        let w = NoesisDpReadWatch::new([
            DpWatch::new("S", "Value", DpKind::F32),
            DpWatch::new("C", "SelectedIndex", DpKind::I32),
        ]);
        assert_eq!(w.entries.len(), 2);
        assert_eq!(w.entries[0], DpWatch::new("S", "Value", DpKind::F32));
        assert_eq!(w.entries[1].kind, DpKind::I32);
    }
}
