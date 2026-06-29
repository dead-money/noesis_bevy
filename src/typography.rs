//! Per-view typography bridge — restyle the `TextElement` font properties
//! (size / family / weight / style / stretch) of named XAML elements on a
//! single [`NoesisView`](crate::NoesisView).
//!
//! `TextBlock.FontSize`, `Run.FontWeight`, and friends are ordinary
//! `TextElement` attached dependency properties; the generic [`NoesisDp`](crate::dp)
//! bridge can already poke the scalar ones (`FontSize` is a plain `f32` DP). This
//! bridge is the *typed, font-shaped* front for them: one [`NoesisTypography`]
//! component carries a per-`x:Name` [`FontStyling`] block, so a single change can
//! restyle a label's size, family, and weight together without spelling out DP
//! names or worrying that `FontWeight`/`FontStyle`/`FontStretch` are enums rather
//! than bare ints.
//!
//! Add a [`NoesisTypography`] component to the view's camera entity. Its `set` map
//! is the desired [`FontStyling`] per `x:Name` — applied to the view's elements
//! whenever the component changes (Bevy change detection). It is **write-only**:
//! each block's `Some` fields are pushed into the live element; `None` fields are
//! left untouched, so two blocks for the same name compose last-write-wins per
//! field. Read the resulting values back through a [`NoesisDp`](crate::dp) watch
//! (`FontSize` is a readable `f32` DP) when you need observation.
//!
//! ```ignore
//! use dm_noesis_bevy::{NoesisTypography, FontWeight};
//!
//! commands.entity(view).insert(
//!     NoesisTypography::new()
//!         .font_size("Title", 28.0)
//!         .font_family("Title", "#PT Root UI")
//!         .font_weight("Title", FontWeight::Bold),
//! );
//! ```
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies the writes against
//! that view's live scene — no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

// Re-export the runtime's typed font enums so callers don't reach across crates.
pub use noesis_runtime::typography::{FontStretch, FontStyle, FontWeight};

// ─────────────────────────────────────────────────────────────────────────────
// Styling block
// ─────────────────────────────────────────────────────────────────────────────

/// The desired `TextElement` font properties for one element. Every field is
/// optional: `Some` is written to the live element on apply, `None` leaves the
/// element's current value untouched (so partial restyles compose).
#[derive(Clone, Default, Debug, PartialEq)]
pub struct FontStyling {
    /// `TextElement.FontSize`, in device-independent pixels.
    pub font_size: Option<f32>,
    /// `TextElement.FontFamily` *source* string (e.g. `"Arial"`, `"#PT Root UI"`,
    /// or a comma-separated fallback list). A fresh Noesis `FontFamily` is built
    /// from it on each apply; Noesis takes its own reference.
    pub font_family: Option<String>,
    /// `TextElement.FontWeight`.
    pub font_weight: Option<FontWeight>,
    /// `TextElement.FontStyle`.
    pub font_style: Option<FontStyle>,
    /// `TextElement.FontStretch`.
    pub font_stretch: Option<FontStretch>,
}

impl FontStyling {
    /// True when no field is set — apply skips these to avoid needless FFI hops.
    pub(crate) fn is_empty(&self) -> bool {
        self.font_size.is_none()
            && self.font_family.is_none()
            && self.font_weight.is_none()
            && self.font_style.is_none()
            && self.font_stretch.is_none()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view typography bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisTypography {
    /// Desired [`FontStyling`] per element `x:Name`. Written to the view's
    /// elements whenever this component changes. Each target should be a
    /// `TextElement` (`TextBlock` / `Run` / `TextBox` / …); a non-text element
    /// silently ignores font properties it doesn't expose.
    pub set: HashMap<String, FontStyling>,
}

impl NoesisTypography {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s `FontSize` (device-independent pixels).
    #[must_use]
    pub fn font_size(mut self, name: impl Into<String>, size: f32) -> Self {
        self.entry(name).font_size = Some(size);
        self
    }

    /// Builder: set element `name`'s `FontFamily` from a source string.
    #[must_use]
    pub fn font_family(mut self, name: impl Into<String>, source: impl Into<String>) -> Self {
        self.entry(name).font_family = Some(source.into());
        self
    }

    /// Builder: set element `name`'s `FontWeight`.
    #[must_use]
    pub fn font_weight(mut self, name: impl Into<String>, weight: FontWeight) -> Self {
        self.entry(name).font_weight = Some(weight);
        self
    }

    /// Builder: set element `name`'s `FontStyle`.
    #[must_use]
    pub fn font_style(mut self, name: impl Into<String>, style: FontStyle) -> Self {
        self.entry(name).font_style = Some(style);
        self
    }

    /// Builder: set element `name`'s `FontStretch`.
    #[must_use]
    pub fn font_stretch(mut self, name: impl Into<String>, stretch: FontStretch) -> Self {
        self.entry(name).font_stretch = Some(stretch);
        self
    }

    /// Builder: replace element `name`'s full [`FontStyling`] block.
    #[must_use]
    pub fn styling(mut self, name: impl Into<String>, styling: FontStyling) -> Self {
        self.set.insert(name.into(), styling);
        self
    }

    fn entry(&mut self, name: impl Into<String>) -> &mut FontStyling {
        self.set.entry(name.into()).or_default()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisTypography`]: apply desired font-property
/// writes when the component changed.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_typography_bridge(
    views: Query<(Entity, Ref<NoesisTypography>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, typography) in &views {
        if typography.is_changed() {
            state.apply_typography_for(entity, &typography.set);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the per-view typography bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisTypographyPlugin;

impl Plugin for NoesisTypographyPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_typography_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_per_name_styling() {
        let t = NoesisTypography::new()
            .font_size("Title", 28.0)
            .font_family("Title", "#PT Root UI")
            .font_weight("Title", FontWeight::Bold)
            .font_style("Sub", FontStyle::Italic)
            .font_stretch("Sub", FontStretch::Condensed);

        let title = t.set.get("Title").expect("Title styling present");
        assert_eq!(title.font_size, Some(28.0));
        assert_eq!(title.font_family.as_deref(), Some("#PT Root UI"));
        assert_eq!(title.font_weight, Some(FontWeight::Bold));
        assert_eq!(title.font_style, None);

        let sub = t.set.get("Sub").expect("Sub styling present");
        assert_eq!(sub.font_style, Some(FontStyle::Italic));
        assert_eq!(sub.font_stretch, Some(FontStretch::Condensed));
        assert_eq!(sub.font_size, None);
    }

    #[test]
    fn last_write_wins_per_field() {
        let t = NoesisTypography::new()
            .font_size("Title", 12.0)
            .font_size("Title", 24.0);
        assert_eq!(t.set.get("Title").unwrap().font_size, Some(24.0));
    }
}
