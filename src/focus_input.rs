//! Per-view focus-navigation and input-binding bridge: the directional,
//! engagement, and key-chord layer on top of the one-shot [`crate::NoesisFocus`] bridge.
//!
//! [`NoesisFocus`](crate::focus::NoesisFocus) answers "give *this* named element
//! keyboard focus". This module answers the rest of the `FocusManager` /
//! `KeyboardNavigation` surface:
//!
//!  * **directional / tab move** ([`FocusMove`]): `UIElement::MoveFocus` away
//!    from a named element in a [`FocusNavigationDirection`] (gamepad D-pad,
//!    Tab traversal). A one-shot action, applied once per component change.
//!  * **focus engagement** ([`FocusEngage`]): `UIElement::Focus(engage)`, the
//!    console focus-engagement model where directional input drives *into* an
//!    element rather than moving focus off it. One-shot action.
//!  * **key bindings** ([`KeyBindingSpec`]): add a `KeyBinding` (a [`Key`] +
//!    [`ModifierKeys`] chord bound to a command) to a named element's
//!    `InputBindings`. When the chord is matched while that element (or its
//!    focus subtree) has focus, a [`NoesisFocusBindingFired`] message is
//!    emitted carrying the originating `view`. Reconciled every frame so it
//!    installs once the scene exists and persists across frames.
//!  * **focus prediction** ([`FocusPredict`]): poll `UIElement::PredictFocus`
//!    every frame (read-watch) and emit [`NoesisFocusPredicted`] when the
//!    answer changes: whether a candidate exists in that direction, the
//!    predicted element's actual `x:Name`, and (if an `expect` name was given)
//!    whether the predicted element *is* that one.
//!
//! Attach a [`NoesisFocusControl`] to the view's camera entity. It is purely
//! additive: the existing [`NoesisFocus`](crate::focus::NoesisFocus) bridge is
//! untouched and the two coexist on the same entity.
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
//! Everything runs on the main thread (Noesis is thread-affine and lives
//! there): the reconcile systems read each view's component and act against
//! that view's live scene. Key-binding callbacks fire (also on the main thread,
//! during `View::Update`) onto a [`SharedFocusBindingQueue`], drained into
//! messages the next frame, like the click/keydown event bridges.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;

// `Key` is already re-exported at the crate root via `crate::events`.
pub use noesis_runtime::input::{FocusNavigationDirection, ModifierKeys};
use noesis_runtime::view::Key;

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Spec value types
// ─────────────────────────────────────────────────────────────────────────────

/// One directional / tab focus move: move keyboard focus away from the element
/// named `from`, in `direction`, wrapping at the ends when `wrapped`. Backs
/// `UIElement::MoveFocus`. `Next` / `Previous` / `First` / `Last` are tab-order
/// traversal; `Left` / `Right` / `Up` / `Down` are spatial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusMove {
    /// `x:Name` of the element to move focus away from.
    pub from: String,
    /// Direction to move in (spatial or tab-order).
    pub direction: FocusNavigationDirection,
    /// Wrap around to the other end when the traversal runs out of candidates.
    pub wrapped: bool,
}

/// One focus-engagement action: `UIElement::Focus(engage)` on the named element.
/// `engage = true` enters the element so directional input drives it; `false`
/// focuses without engaging.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FocusEngage {
    /// `x:Name` of the element to focus.
    pub name: String,
    /// `true` enters (engages) the element so directional input drives it;
    /// `false` focuses without engaging.
    pub engage: bool,
}

/// One key binding: a [`Key`] + [`ModifierKeys`] chord added to the named
/// element's `InputBindings`. When matched (while the element or its focus
/// subtree has focus), it fires a [`NoesisFocusBindingFired`].
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
    /// Stable identity key for the per-scene installed-binding map. `Key` and
    /// `ModifierKeys` are both `#[repr(i32)]`-style mirrors, so their ordinals
    /// make a cheap, hashable tuple alongside the element name.
    #[must_use]
    pub fn ident(&self) -> (String, i32, i32) {
        (self.name.clone(), self.key as i32, self.modifiers.bits())
    }
}

/// One focus-prediction watch: poll `UIElement::PredictFocus` from `from` in
/// `direction`. The emitted message always carries the predicted element's
/// actual `x:Name` (via `FrameworkElement::predict_focus_name`). If `expect` is
/// set, the message additionally reports whether that name equals `expect`.
/// `PredictFocus` only answers the spatial directions; `Next` / `Previous` /
/// `First` / `Last` always report no candidate.
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
    /// Stable identity key for the per-scene prediction snapshot map.
    #[must_use]
    pub fn ident(&self) -> (String, i32, Option<String>) {
        (
            self.from.clone(),
            self.direction as i32,
            self.expect.clone(),
        )
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view focus-navigation + input-binding bridge. Attach to a
/// [`NoesisView`](crate::NoesisView) entity. Additive to
/// [`NoesisFocus`](crate::focus::NoesisFocus); both may live on one entity.
///
/// `moves` and `engages` are **one-shot actions**: applied once when the
/// component changes (Bevy change detection), then drained, so they neither
/// accumulate nor replay on a later change or a scene rebuild. As with
/// [`crate::NoesisFocus`], fill them in *after* the scene exists or the apply is
/// lost. `bindings` is **reconciled every frame** (installs once the scene
/// appears, persists thereafter): retained config that survives a rebuild.
/// `predicts` is **polled every frame** and surfaces changes as messages.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisFocusControl {
    /// One-shot directional / tab moves, applied on change.
    pub moves: Vec<FocusMove>,
    /// One-shot focus-engagement actions, applied on change.
    pub engages: Vec<FocusEngage>,
    /// Key bindings, reconciled each frame against the live scene.
    pub bindings: Vec<KeyBindingSpec>,
    /// Focus-prediction watches, polled each frame.
    pub predicts: Vec<FocusPredict>,
}

impl NoesisFocusControl {
    /// An empty control with no moves, engages, bindings, or predictions.
    /// Chain the builder methods to fill it in.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: queue a directional / tab [`FocusMove`] from `from`.
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

    /// Builder: queue a [`FocusEngage`] on `name`.
    #[must_use]
    pub fn engage(mut self, name: impl Into<String>, engage: bool) -> Self {
        self.engages.push(FocusEngage {
            name: name.into(),
            engage,
        });
        self
    }

    /// Builder: install a [`KeyBindingSpec`] (chord → command) on `name`.
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

    /// Builder: watch focus prediction from `from` in `direction` (no expected
    /// target, so the message only reports whether a candidate exists).
    #[must_use]
    pub fn predict(mut self, from: impl Into<String>, direction: FocusNavigationDirection) -> Self {
        self.predicts.push(FocusPredict {
            from: from.into(),
            direction,
            expect: None,
        });
        self
    }

    /// Builder: watch focus prediction from `from` in `direction`, additionally
    /// reporting whether the predicted element is the one named `expect`.
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

    /// Queue a directional / tab [`FocusMove`] from a system holding
    /// `&mut NoesisFocusControl`. The runtime counterpart of
    /// [`move_focus`](Self::move_focus): the next reconcile applies it once to
    /// the live scene.
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

    /// Queue a [`FocusEngage`] on `name` from a system holding
    /// `&mut NoesisFocusControl`. The runtime counterpart of
    /// [`engage`](Self::engage): the next reconcile applies it once.
    pub fn request_engage(&mut self, name: impl Into<String>, engage: bool) {
        self.engages.push(FocusEngage {
            name: name.into(),
            engage,
        });
    }

    /// Install a [`KeyBindingSpec`] (chord to command) on `name` from a system
    /// holding `&mut NoesisFocusControl`. The runtime counterpart of
    /// [`key_binding`](Self::key_binding): reconciled into the live scene next
    /// frame and kept thereafter.
    pub fn add_key_binding(&mut self, name: impl Into<String>, key: Key, modifiers: ModifierKeys) {
        self.bindings.push(KeyBindingSpec {
            name: name.into(),
            key,
            modifiers,
        });
    }

    /// Watch focus prediction from `from` in `direction` from a system holding
    /// `&mut NoesisFocusControl` (no expected target, so the message only reports
    /// whether a candidate exists). The runtime counterpart of
    /// [`predict`](Self::predict).
    pub fn watch_predict(&mut self, from: impl Into<String>, direction: FocusNavigationDirection) {
        self.predicts.push(FocusPredict {
            from: from.into(),
            direction,
            expect: None,
        });
    }

    /// Watch focus prediction from `from` in `direction` from a system holding
    /// `&mut NoesisFocusControl`, additionally reporting whether the predicted
    /// element is the one named `expect`. The runtime counterpart of
    /// [`predict_to`](Self::predict_to).
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

// ─────────────────────────────────────────────────────────────────────────────
// Messages
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when a [`KeyBindingSpec`] chord matches and fires its command.
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

/// Emitted when a [`FocusPredict`] watch's answer changes (deduped per scene).
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
    /// The predicted element's actual `x:Name`, as reported by
    /// `FrameworkElement::predict_focus_name`. `None` when there is no candidate
    /// or the predicted element is unnamed / not a `FrameworkElement`.
    pub predicted_name: Option<String>,
    /// Whether the predicted element's name equals the watch's `expect` target.
    /// Always `false` when the watch had no `expect`, or when there is no
    /// candidate / the predicted element is unnamed.
    pub matches_expected: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// Key-binding fire queue
// ─────────────────────────────────────────────────────────────────────────────

/// Queue between the (main-thread) key-binding command callbacks and the drain
/// system. `Clone` is an `Arc` clone. Entries carry the originating view entity.
/// Mirrors [`SharedClickQueue`](crate::events::NoesisClickWatch)'s role.
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

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Apply the one-shot actions ([`FocusMove`] / [`FocusEngage`]) when the
/// component changed, then drain them. Write-only: each queued action fires
/// exactly once. Unlike the retained bridges these are *not* reapplied on a
/// scene rebuild — they are transient requests, not config to replay.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_focus_control(
    mut views: Query<(Entity, Mut<NoesisFocusControl>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, mut ctl) in &mut views {
        if !ctl.is_changed() || (ctl.moves.is_empty() && ctl.engages.is_empty()) {
            continue;
        }
        state.apply_focus_moves_for(entity, &ctl.moves);
        state.apply_focus_engages_for(entity, &ctl.engages);
        // Drain so accumulated one-shots don't replay on the next change or a
        // rebuild. Bypass change detection so the clear doesn't re-trigger this
        // system next frame (only this system reads the change flag).
        let ctl = ctl.bypass_change_detection();
        ctl.moves.clear();
        ctl.engages.clear();
    }
}

/// Reconcile every view's key bindings against its live scene. Runs every frame
/// (not gated on change) so a binding installs as soon as the scene exists and
/// persists afterwards, like
/// [`sync_click_subscriptions`](crate::events).
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

/// Poll every view's focus predictions, emitting [`NoesisFocusPredicted`] on
/// change (deduped against the per-scene snapshot).
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

/// Drain the fired-binding queue into [`NoesisFocusBindingFired`] messages.
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

/// Wires the per-view focus-navigation + input-binding bridge. Added
/// transitively by [`crate::NoesisPlugin`].
pub struct NoesisFocusControlPlugin;

impl Plugin for NoesisFocusControlPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisFocusBindingFired>()
            .add_message::<NoesisFocusPredicted>()
            .insert_resource(SharedFocusBindingQueue::default())
            // Drain last frame's fires before user systems read them (mirrors
            // the click/keydown drains).
            .add_systems(PreUpdate, drain_focus_binding_queue)
            .add_systems(
                PostUpdate,
                (
                    sync_focus_control,
                    sync_focus_bindings,
                    poll_focus_predictions,
                )
                    .in_set(NoesisSet::Apply),
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
