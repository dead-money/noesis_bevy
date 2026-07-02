//! App-level application-resources bridge. Registers Rust-built resources
//! (code-built brushes, scalar values) and merged `<ResourceDictionary>` XAML
//! into the process-global application resources, so XAML
//! `{StaticResource Key}` references resolve them without authoring a theme
//! file on disk.
//!
//! Unlike the per-element bridges, resources are *global*: a `{StaticResource}`
//! is resolved when the element is parsed, walking the element's own
//! `Resources`, then its ancestors', then the application resources installed
//! via `GUI::SetApplicationResources`. Since a [`NoesisView`](crate::NoesisView)
//! parses its XAML atomically in [`crate::render`]'s `Ensure` phase, the only
//! injection point that a freshly-loaded scene's `{StaticResource}` can see is
//! the application resources. So this bridge is an **app-level Bevy
//! [`Resource`]**, not a per-entity component, and its reconcile system runs in
//! [`NoesisSet::Sync`] (before `Ensure`) so the resources are installed
//! before any view parses.
//!
//! ```ignore
//! app.insert_resource(
//!     NoesisResources::new()
//!         .solid("AccentBrush", [1.0, 0.0, 0.0, 1.0])
//!         .value("PanelWidth", DpValue::F32(40.0)),
//! );
//! // ...then in XAML:  Background="{StaticResource AccentBrush}"
//! //                   Width="{StaticResource PanelWidth}"
//! ```
//!
//! The bridge builds a fresh `Noesis::ResourceDictionary` from the spec whenever
//! the [`NoesisResources`] resource changes (Bevy change detection) and installs
//! it with `GUI::SetApplicationResources` (Noesis takes its own reference, so the
//! Rust handle drops right after). Re-applying replaces the global dictionary;
//! already-parsed scenes keep the `{StaticResource}` values they resolved at
//! parse time (`StaticResource` is a one-shot parse-time lookup), so a change is
//! only seen by views built afterwards.
//!
//! After installing, it confirms which declared keys are resolvable through the
//! live application resources (`GUI::GetApplicationResources` ➜ `contains`) and
//! emits a [`NoesisResourcesInstalled`] message listing them: the "look up"
//! half of register/look-up.
//!
//! # Relationship to `NoesisView::application_resources`
//!
//! `NoesisView::application_resources` names a chain of on-disk
//! `ResourceDictionary` *URIs* (a theme). This bridge and that chain feed the
//! **same** process-global application resources, and they are **merged** rather
//! than mutually exclusive: the reconcile system builds one dictionary holding
//! the chain URIs (as merged dictionaries), this bridge's `merged_xaml`, and the
//! code-built [`entries`](NoesisResources::entries) as base entries. Code-built
//! entries win over the theme on a key collision, so a `.solid()`/`.value()`
//! override survives a theme instead of being clobbered by it. Every view's
//! chain is unioned into that one dictionary.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::brushes::BrushSpec;
use crate::dp::DpValue;
use crate::render::{NoesisRenderState, NoesisSet, NoesisView, sync_xaml_provider_map};

/// One application-resource entry, declarative side. Resolved into a live
/// `Noesis::BaseComponent` only at install time (on the Noesis thread), so the
/// resource stays plain data.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceEntry {
    /// A code-built brush ([`SolidColorBrush`] / [`LinearGradientBrush`]).
    /// Resolves a `{StaticResource Key}` used where a `Brush` is expected
    /// (`Background`, `Fill`, `Stroke`, ...).
    ///
    /// [`SolidColorBrush`]: noesis_runtime::brushes::SolidColorBrush
    /// [`LinearGradientBrush`]: noesis_runtime::brushes::LinearGradientBrush
    Brush(BrushSpec),
    /// A boxed scalar value (string / number / bool). Resolves a
    /// `{StaticResource Key}` used where a plain value is expected (e.g. a
    /// `Single` `Width`, a `String` `Text`). The boxed variant must match the
    /// target property's runtime type exactly (a `Double` won't satisfy a
    /// `Single` `Width`), exactly as for
    /// [`DpValue`] writes (see the `dp` module docs on `f32`
    /// vs `f64`).
    Value(DpValue),
}

/// App-level application-resources bridge. Insert as a Bevy [`Resource`] (not a
/// per-entity component): the resources it installs are process-global and must
/// be in place before any [`NoesisView`](crate::NoesisView) parses.
#[derive(Resource, Clone, Default, Debug)]
pub struct NoesisResources {
    /// Code-built entries keyed by `x:Key`. Built into a `ResourceDictionary`
    /// and installed as the application resources whenever this resource changes.
    pub entries: HashMap<String, ResourceEntry>,
    /// Bare `<ResourceDictionary>` XAML fragments, each parsed via
    /// `GUI::ParseXaml` and added to the installed dictionary's
    /// `MergedDictionaries`. Entries in [`entries`](Self::entries) take
    /// precedence over merged keys on collision (WPF/Noesis merge semantics).
    pub merged_xaml: Vec<String>,
}

impl NoesisResources {
    /// Starts an empty resource set. Chain the builders
    /// ([`solid`](Self::solid), [`value`](Self::value), [`merged`](Self::merged),
    /// ...) to declare entries, then insert it as a Bevy [`Resource`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: register `entry` under `key`.
    #[must_use]
    pub fn entry(mut self, key: impl Into<String>, entry: ResourceEntry) -> Self {
        self.entries.insert(key.into(), entry);
        self
    }

    /// Builder: register a code-built brush under `key`.
    #[must_use]
    pub fn brush(self, key: impl Into<String>, spec: BrushSpec) -> Self {
        self.entry(key, ResourceEntry::Brush(spec))
    }

    /// Builder: register a flat `SolidColorBrush` of `[r, g, b, a]` (each
    /// `0..=1`) under `key`.
    #[must_use]
    pub fn solid(self, key: impl Into<String>, rgba: [f32; 4]) -> Self {
        self.brush(key, BrushSpec::Solid(rgba))
    }

    /// Builder: register a boxed scalar value under `key`.
    #[must_use]
    pub fn value(self, key: impl Into<String>, value: DpValue) -> Self {
        self.entry(key, ResourceEntry::Value(value))
    }

    /// Builder: append a bare `<ResourceDictionary>` XAML fragment to be parsed
    /// and merged into the installed dictionary.
    #[must_use]
    pub fn merged(mut self, xaml: impl Into<String>) -> Self {
        self.merged_xaml.push(xaml.into());
        self
    }
}

/// Emitted after the bridge (re)installs the application resources, listing the
/// declared [`entries`](NoesisResources::entries) keys confirmed resolvable
/// through the live application resources (`GUI::GetApplicationResources` ➜
/// `contains`). A key missing from `present` failed to install (e.g. a null
/// brush, or a `key` that collided away). Read with
/// `MessageReader<NoesisResourcesInstalled>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisResourcesInstalled {
    /// Declared own keys confirmed present in the installed application
    /// resources, sorted.
    pub present: Vec<String>,
}

/// Reconcile the process-global application resources from the two sources that
/// feed them — the code-built [`NoesisResources`] bridge and every view's
/// [`NoesisView::application_resources`](crate::NoesisView::application_resources)
/// URI chain — into one merged `ResourceDictionary`, then emit a
/// [`NoesisResourcesInstalled`] read-back reflecting what actually installed.
///
/// Merging both sources here (rather than letting the per-view chain clobber the
/// code-built dictionary during `Ensure`, as it used to) means opting into a
/// theme no longer silently drops `.solid()`/`.value()` entries. Runs in
/// [`NoesisSet::Sync`], after the XAML provider map is populated, so the chain
/// URIs are visible and the install lands before the scene-build (`Ensure`)
/// phase parses any `{StaticResource}`.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_resources_bridge(
    resources: Option<Res<NoesisResources>>,
    views: Query<&NoesisView>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut installed: MessageWriter<NoesisResourcesInstalled>,
    mut warned_conflict: Local<bool>,
) {
    // Gate on the render state: its existence proves `noesis_runtime::init()`
    // has run, which `GUI::SetApplicationResources` requires.
    let Some(mut state) = state else {
        return;
    };

    // Union the views' chains (deduped, first-seen order). With merged-dictionary
    // semantics several views can share one global set of resources; warn once if
    // views declare *different* chains, since the global is process-wide.
    let mut chain_uris: Vec<String> = Vec::new();
    let mut distinct_chains: Vec<&[String]> = Vec::new();
    for view in &views {
        if view.application_resources.is_empty() {
            continue;
        }
        if !distinct_chains.contains(&view.application_resources.as_slice()) {
            distinct_chains.push(&view.application_resources);
        }
        for uri in &view.application_resources {
            if !chain_uris.contains(uri) {
                chain_uris.push(uri.clone());
            }
        }
    }
    if distinct_chains.len() > 1 && !*warned_conflict {
        warn!(
            "NoesisView.application_resources: views declare different chains {distinct_chains:?}; \
             application resources are process-global, so all are merged into one dictionary"
        );
        *warned_conflict = true;
    }

    // The code-built side is optional; a theme-only app still installs its chain.
    let empty = NoesisResources::default();
    let resources = resources.as_deref().unwrap_or(&empty);

    if let Some(present) =
        state.reconcile_app_resources(&resources.entries, &resources.merged_xaml, &chain_uris)
    {
        // The read-back is the code-built bridge's "look up" half; only surface it
        // when the consumer actually declared code-built resources.
        if !resources.entries.is_empty() {
            installed.write(NoesisResourcesInstalled { present });
        }
    }
}

/// Wires the app-level application-resources bridge. Added transitively by
/// [`crate::NoesisPlugin`]. Does not insert a default [`NoesisResources`]; the
/// bridge is opt-in (no resource ⇒ theme chain only, or no-op).
pub struct NoesisResourcesPlugin;

impl Plugin for NoesisResourcesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisResourcesInstalled>().add_systems(
            PostUpdate,
            sync_resources_bridge
                .in_set(NoesisSet::Sync)
                // The provider map must hold the chain URIs before we read their
                // bytes to build the merged dictionary.
                .after(sync_xaml_provider_map),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_entries() {
        let r = NoesisResources::new()
            .solid("AccentBrush", [1.0, 0.0, 0.0, 1.0])
            .value("PanelWidth", DpValue::F64(40.0))
            .merged("<ResourceDictionary/>");

        assert_eq!(
            r.entries.get("AccentBrush"),
            Some(&ResourceEntry::Brush(BrushSpec::Solid([
                1.0, 0.0, 0.0, 1.0
            ]))),
        );
        assert_eq!(
            r.entries.get("PanelWidth"),
            Some(&ResourceEntry::Value(DpValue::F64(40.0))),
        );
        assert_eq!(r.merged_xaml, vec!["<ResourceDictionary/>".to_string()]);
    }
}
