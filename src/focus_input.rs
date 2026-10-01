//! Focus navigation and key bindings for a view: directional moves, focus
//! engagement, key chords, and focus prediction.
//!
//! [`NoesisFocus`](crate::focus::NoesisFocus) gives one named element keyboard
//! focus. [`NoesisFocusControl`] covers the rest of the `FocusManager` /
//! `KeyboardNavigation` surface:
//!
//! * [`FocusMove`]: `UIElement::MoveFocus` away from a named element in a
//!   [`FocusNavigationDirection`] (gamepad D-pad, Tab traversal). One-shot.
//! * [`FocusEngage`]: `UIElement::Focus(engage)`, the console engagement model
//!   where directional input drives *into* an element instead of moving focus
//!   off it. One-shot.
//! * [`KeyBindingSpec`]: a [`Key`] + [`ModifierKeys`] chord added to a named
//!   element's `InputBindings`. When the chord matches while that element (or
//!   something in its focus subtree) has focus, a [`NoesisFocusBindingFired`]
//!   is emitted. Retained: installed once the scene exists and kept.
//! * [`FocusPredict`]: polls `UIElement::PredictFocus` every frame and emits
//!   [`NoesisFocusPredicted`] when the answer changes.
//!
//! Add [`NoesisFocusControl`] to the [`NoesisView`](crate::NoesisView) camera
//! entity. It can sit next to [`NoesisFocus`](crate::focus::NoesisFocus) on the
//! same entity.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisFocusControl::new()
//!         .move_focus("First", FocusNavigationDirection::Right, false) // D-pad right
//!         .key_binding("Console", Key::Return, ModifierKeys::CONTROL)  // Ctrl+Enter
//!         .predict_to("First", FocusNavigationDirection::Right, "Second"),
//! );
//! ```
//!
//! Moves and engages also work on a [`UiPanel`](crate::panel::UiPanel) entity,
//! resolving names in the panel fragment. Key bindings and predictions only act
//! on a view's own scene.
//!
//! All systems run on the main thread in [`NoesisSet::Apply`]. Key-binding
//! commands fire during `View::Update` and are queued; the queue is drained
//! into [`NoesisFocusBindingFired`] messages in the next frame's `PreUpdate`.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;

// `Key` is already re-exported at the crate root via `crate::events`.
pub use noesis_runtime::input::{FocusNavigationDirection, ModifierKeys};
use noesis_runtime::view::Key;

use crate::render::{NoesisRenderState, NoesisSet};

/// Moves keyboard focus away from the element named `from` (`UIElement::MoveFocus`).
///
/// `Next` / `Previous` / `First` / `Last` traverse tab order; `Left` / `Right` /
/// `Up` / `Down` are spatial. A move that shifts nothing logs a warning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusMove {
    /// `x:Name` of the element to move focus away from.
    pub from: String,
    /// Direction to move in (spatial or tab-order).
    pub direction: FocusNavigationDirection,
    /// Wrap around to the other end when the traversal runs out of candidates.
    pub wrapped: bool,
}

/// Focuses the named element with `UIElement::Focus(engage)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusEngage {
    /// `x:Name` of the element to focus.
    pub name: String,
    /// `true` enters (engages) the element so directional input drives it;
    /// `false` focuses without engaging.
    pub engage: bool,
}

/// A [`Key`] + [`ModifierKeys`] chord added to the named element's
/// `InputBindings`. When it matches while the element or its focus subtree has
/// focus, a [`NoesisFocusBindingFired`] is emitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyBindingSpec {
    /// `x:Name` of the element whose `InputBindings` the chord is added to.
    pub name: String,
    /// The chord key.
    pub key: Key,
    /// Modifier keys that must be held with [`key`](Self::key) for the chord to match.
    pub modifiers: ModifierKeys,
}

impl KeyBindingSpec {
    /// Identity of this binding as `(name, key ordinal, modifier bits)`. Two
    /// specs with the same ident are the same installed binding.
    #[must_use]
    pub fn ident(&self) -> (String, i32, i32) {
        (self.name.clone(), self.key as i32, self.modifiers.bits())
    }
}

/// Watches `UIElement::PredictFocus` from `from` in `direction`, polled every
/// frame.
///
/// [`NoesisFocusPredicted`] carries the predicted element's `x:Name` and, when
/// `expect` is set, whether it equals `expect`. `PredictFocus` only answers the
/// spatial directions; `Next` / `Previous` / `First` / `Last` always report no
/// candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusPredict {
    /// `x:Name` of the element to predict focus from.
    pub from: String,
    /// Direction to predict in. Only the spatial directions answer; the
    /// tab-order ones always report no candidate.
    pub direction: FocusNavigationDirection,
    /// Optional name to compare the predicted element against. `None` means the
    /// message only reports whether a candidate exists.
    pub expect: Option<String>,
}

impl FocusPredict {
    /// Identity of this watch as `(from, direction ordinal, expect)`, used to
    /// dedupe [`NoesisFocusPredicted`] emissions.
    #[must_use]
    pub fn ident(&self) -> (String, i32, Option<String>) {
        (
            self.from.clone(),
            self.direction as i32,
            self.expect.clone(),
        )
    }
}

/// Focus navigation and key bindings for one view. Add it to the
/// [`NoesisView`](crate::NoesisView) camera entity (or a
/// [`UiPanel`](crate::panel::UiPanel) entity for moves and engages).
///
/// `moves` and `engages` are one-shot: they apply once, then the lists are
/// cleared. Actions queued before the scene is built or the panel is mounted
/// wait for that frame instead of being dropped. They are not replayed after a
/// later scene rebuild.
///
/// `bindings` is retained and reconciled every frame: a binding installs once
/// the scene exists, is reinstalled after a rebuild, and removing a spec
/// detaches it from its element. `predicts` is polled every frame and reports
/// changes as [`NoesisFocusPredicted`].
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisFocusControl {
    /// One-shot moves. Cleared once applied.
    pub moves: Vec<FocusMove>,
    /// One-shot engagement actions. Cleared once applied.
    pub engages: Vec<FocusEngage>,
    /// Key bindings, reconciled each frame against the live scene.
    pub bindings: Vec<KeyBindingSpec>,
    /// Focus-prediction watches, polled each frame.
    pub predicts: Vec<FocusPredict>,
}

impl NoesisFocusControl {
    /// An empty control. Chain the builder methods to fill it in.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queues a [`FocusMove`] from `from`.
    #[must_use]
    pub fn move_focus(
        mut self,
        from: impl Into<String>,
        direction: FocusNavigationDirection,
        wrapped: bool,
    ) -> Self {
        self.moves.push(FocusMove {
            from: from.into(),
            direction,
            wrapped,
        });
        self
    }

    /// Queues a [`FocusEngage`] on `name`.
    #[must_use]
    pub fn engage(mut self, name: impl Into<String>, engage: bool) -> Self {
        self.engages.push(FocusEngage {
            name: name.into(),
            engage,
        });
        self
    }

    /// Adds a [`KeyBindingSpec`] on `name`.
    #[must_use]
    pub fn key_binding(
        mut self,
        name: impl Into<String>,
        key: Key,
        modifiers: ModifierKeys,
    ) -> Self {
        self.bindings.push(KeyBindingSpec {
            name: name.into(),
            key,
            modifiers,
        });
        self
    }

    /// Watches focus prediction from `from` in `direction`, with no expected
    /// target.
    #[must_use]
    pub fn predict(mut self, from: impl Into<String>, direction: FocusNavigationDirection) -> Self {
        self.predicts.push(FocusPredict {
            from: from.into(),
            direction,
            expect: None,
        });
        self
    }

    /// Watches focus prediction from `from` in `direction` and reports whether
    /// the predicted element is the one named `expect`.
    #[must_use]
    pub fn predict_to(
        mut self,
        from: impl Into<String>,
        direction: FocusNavigationDirection,
        expect: impl Into<String>,
    ) -> Self {
        self.predicts.push(FocusPredict {
            from: from.into(),
            direction,
            expect: Some(expect.into()),
        });
        self
    }

    /// In-place form of [`move_focus`](Self::move_focus), for systems holding
    /// `&mut NoesisFocusControl`. Applied once in the next [`NoesisSet::Apply`].
    pub fn request_move(
        &mut self,
        from: impl Into<String>,
        direction: FocusNavigationDirection,
        wrapped: bool,
    ) {
        self.moves.push(FocusMove {
            from: from.into(),
            direction,
            wrapped,
        });
    }

    /// In-place form of [`engage`](Self::engage). Applied once in the next
    /// [`NoesisSet::Apply`].
    pub fn request_engage(&mut self, name: impl Into<String>, engage: bool) {
        self.engages.push(FocusEngage {
            name: name.into(),
            engage,
        });
    }

    /// In-place form of [`key_binding`](Self::key_binding).
    pub fn add_key_binding(&mut self, name: impl Into<String>, key: Key, modifiers: ModifierKeys) {
        self.bindings.push(KeyBindingSpec {
            name: name.into(),
            key,
            modifiers,
        });
    }

    /// In-place form of [`predict`](Self::predict).
    pub fn watch_predict(&mut self, from: impl Into<String>, direction: FocusNavigationDirection) {
        self.predicts.push(FocusPredict {
            from: from.into(),
            direction,
            expect: None,
        });
    }

    /// In-place form of [`predict_to`](Self::predict_to).
    pub fn watch_predict_to(
        &mut self,
        from: impl Into<String>,
        direction: FocusNavigationDirection,
        expect: impl Into<String>,
    ) {
        self.predicts.push(FocusPredict {
            from: from.into(),
            direction,
            expect: Some(expect.into()),
        });
    }
}

/// Emitted when a [`KeyBindingSpec`] chord matches. Arrives the frame after the
/// key press.
#[derive(Message, Debug, Clone)]
pub struct NoesisFocusBindingFired {
    /// The [`NoesisView`](crate::NoesisView) entity whose element holds the binding.
    pub view: Entity,
    /// `x:Name` of the element the binding was installed on.
    pub name: String,
    /// The chord key.
    pub key: Key,
    /// The chord modifiers.
    pub modifiers: ModifierKeys,
}

/// Emitted when a [`FocusPredict`] watch's answer changes. The first poll after
/// a watch is added (or the scene is rebuilt) always reports.
#[derive(Message, Debug, Clone)]
pub struct NoesisFocusPredicted {
    /// The [`NoesisView`](crate::NoesisView) entity this prediction was run on.
    pub view: Entity,
    /// The element the prediction started from.
    pub from: String,
    /// The queried direction.
    pub direction: FocusNavigationDirection,
    /// Whether `PredictFocus` found any candidate in that direction.
    pub candidate: bool,
    /// The predicted element's `x:Name`. `None` when there is no candidate or
    /// the predicted element is unnamed or not a `FrameworkElement`.
    pub predicted_name: Option<String>,
    /// Whether [`predicted_name`](Self::predicted_name) equals the watch's
    /// `expect`. Always `false` when the watch has no `expect` or
    /// `predicted_name` is `None`.
    pub matches_expected: bool,
}

/// Fired key bindings waiting to become [`NoesisFocusBindingFired`] messages.
/// Filled by the binding commands during `View::Update`, drained by
/// [`drain_focus_binding_queue`]. Cloning shares the same queue.
#[derive(Resource, Clone, Default)]
pub struct SharedFocusBindingQueue(pub(crate) Arc<Mutex<Vec<(Entity, String, Key, ModifierKeys)>>>);

impl SharedFocusBindingQueue {
    /// Push a fired binding from its command callback.
    pub(crate) fn push(&self, view: Entity, name: String, key: Key, modifiers: ModifierKeys) {
        self.0
            .lock()
            .expect("SharedFocusBindingQueue poisoned")
            .push((view, name, key, modifiers));
    }

    fn drain(&self) -> Vec<(Entity, String, Key, ModifierKeys)> {
        let mut guard = self.0.lock().expect("SharedFocusBindingQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Applies queued [`FocusMove`]s and [`FocusEngage`]s once the target root is
/// ready, then clears them so each fires exactly once.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_focus_control(
    mut views: Query<(Entity, Mut<NoesisFocusControl>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, mut ctl) in &mut views {
        if (!ctl.is_changed()
            && !state.scene_rebuilt_this_frame(entity)
            && !state.panel_mounted_this_frame(entity))
            || (ctl.moves.is_empty() && ctl.engages.is_empty())
        {
            continue;
        }
        // Both applies gate on the same root readiness (an empty half reports
        // ready), so a retry never double-fires. Bypass change detection so the
        // clear doesn't re-trigger this system next frame.
        let moves_applied = state.apply_focus_moves_for(entity, &ctl.moves);
        let engages_applied = state.apply_focus_engages_for(entity, &ctl.engages);
        if moves_applied && engages_applied {
            let ctl = ctl.bypass_change_detection();
            ctl.moves.clear();
            ctl.engages.clear();
        }
    }
}

/// Reconciles every view's key bindings against its live scene. Ungated, so a
/// binding installs as soon as the scene exists.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_focus_bindings(
    views: Query<(Entity, &NoesisFocusControl)>,
    queue: Res<SharedFocusBindingQueue>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, ctl) in &views {
        state.sync_key_bindings_for(entity, &ctl.bindings, &queue);
    }
}

/// Polls every view's focus predictions and emits [`NoesisFocusPredicted`] on
/// change.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn poll_focus_predictions(
    views: Query<(Entity, &NoesisFocusControl)>,
    mut messages: MessageWriter<NoesisFocusPredicted>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, ctl) in &views {
        for (from, direction, candidate, predicted_name, matches_expected) in
            state.poll_focus_predictions_for(entity, &ctl.predicts)
        {
            messages.write(NoesisFocusPredicted {
                view: entity,
                from,
                direction,
                candidate,
                predicted_name,
                matches_expected,
            });
        }
    }
}

/// Drains [`SharedFocusBindingQueue`] into [`NoesisFocusBindingFired`]
/// messages. Runs in `PreUpdate`.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_focus_binding_queue(
    queue: Res<SharedFocusBindingQueue>,
    mut messages: MessageWriter<NoesisFocusBindingFired>,
) {
    for (view, name, key, modifiers) in queue.drain() {
        messages.write(NoesisFocusBindingFired {
            view,
            name,
            key,
            modifiers,
        });
    }
}

/// Registers [`NoesisFocusControl`]'s systems and messages. Added by
/// [`NoesisPlugin`](crate::NoesisPlugin).
pub struct NoesisFocusControlPlugin;

impl Plugin for NoesisFocusControlPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisFocusBindingFired>()
            .add_message::<NoesisFocusPredicted>()
            .insert_resource(SharedFocusBindingQueue::default())
            .add_systems(PreUpdate, drain_focus_binding_queue)
            // After `sync_panels`, which sets `panel_mounted_this_frame`.
            .add_systems(
                PostUpdate,
                (
                    sync_focus_control,
                    sync_focus_bindings,
                    poll_focus_predictions,
                )
                    .in_set(NoesisSet::Apply)
                    .after(crate::panel::sync_panels),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_specs() {
        let c = NoesisFocusControl::new()
            .move_focus("First", FocusNavigationDirection::Right, false)
            .engage("Pad", true)
            .key_binding("Console", Key::Return, ModifierKeys::CONTROL)
            .predict_to("First", FocusNavigationDirection::Right, "Second");

        assert_eq!(c.moves.len(), 1);
        assert_eq!(c.moves[0].from, "First");
        assert_eq!(c.moves[0].direction, FocusNavigationDirection::Right);
        assert!(c.engages[0].engage);
        assert_eq!(c.bindings[0].key, Key::Return);
        assert_eq!(c.bindings[0].modifiers, ModifierKeys::CONTROL);
        assert_eq!(c.predicts[0].expect.as_deref(), Some("Second"));
    }

    #[test]
    fn idents_are_stable() {
        let b = KeyBindingSpec {
            name: "X".into(),
            key: Key::A,
            modifiers: ModifierKeys::CONTROL,
        };
        assert_eq!(
            b.ident(),
            ("X".to_string(), Key::A as i32, ModifierKeys::CONTROL.bits())
        );
    }
}
