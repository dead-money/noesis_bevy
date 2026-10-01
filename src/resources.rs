//! Application resources built in Rust: brushes, scalar values, and
//! `<ResourceDictionary>` XAML fragments that XAML `{StaticResource Key}`
//! references resolve without a theme file on disk.
//!
//! `{StaticResource}` resolves at parse time, walking the element's own
//! `Resources`, its ancestors', and then the process-global application
//! resources. A [`NoesisView`] parses its XAML in one go, so the application
//! resources are the only place a fresh scene can find Rust-built keys. That is
//! why [`NoesisResources`] is an app-level Bevy [`Resource`] rather than a
//! component on the view, and why its reconcile system runs in
//! [`NoesisSet::Sync`], before views are built.
//!
//! ```ignore
//! app.insert_resource(
//!     NoesisResources::new()
//!         .solid("AccentBrush", [1.0, 0.0, 0.0, 1.0])
//!         .value("PanelWidth", DpValue::F32(40.0)),
//! );
//! // In XAML:  Background="{StaticResource AccentBrush}"
//! //           Width="{StaticResource PanelWidth}"
//! ```
//!
//! Each frame the system compares the declared inputs with what it last
//! installed. When anything differs (the code-built entries, `merged_xaml`, the
//! views' theme URIs, or the bytes behind those URIs), it builds a new
//! dictionary and installs it with `GUI::SetApplicationResources`. A reinstall
//! rebuilds every already-built scene so its `{StaticResource}` lookups
//! re-resolve; expect any scene state that is not held in components to reset.
//! The install waits until every theme URI and every font the views list in
//! `wait_for_fonts` / `wait_for_font_files` has reached the providers.
//!
//! After an install, a [`NoesisResourcesInstalled`] message lists which declared
//! keys the live application resources actually contain.
//!
//! # Relationship to `NoesisView::application_resources`
//!
//! [`NoesisView::application_resources`] names a chain of on-disk
//! `ResourceDictionary` URIs (a theme). The chain and this resource feed one
//! process-global dictionary: the chain URIs and [`merged_xaml`] become merged
//! dictionaries (in that order, so `merged_xaml` overrides the theme), and
//! [`entries`] become base entries, which win over everything merged. A
//! `.solid()` or `.value()` override therefore survives a theme. The chains of
//! all views are unioned, with a one-time warning if views declare different
//! chains.
//!
//! Removing [`NoesisResources`] while no view declares a theme leaves the last
//! install in place.
//!
//! [`merged_xaml`]: NoesisResources::merged_xaml
//! [`entries`]: NoesisResources::entries

use std::collections::HashMap;

use bevy::prelude::*;

use crate::brushes::BrushSpec;
use crate::dp::DpValue;
use crate::render::{
    NoesisRenderState, NoesisSet, NoesisView, sync_font_provider_map, sync_xaml_provider_map,
};

/// One application-resource value. Plain data; the live Noesis object is built
/// at install time.
#[derive(Debug, Clone, PartialEq)]
pub enum ResourceEntry {
    /// A code-built brush ([`SolidColorBrush`] / [`LinearGradientBrush`]).
    /// Resolves a `{StaticResource Key}` used where a `Brush` is expected
    /// (`Background`, `Fill`, `Stroke`, ...).
    ///
    /// [`SolidColorBrush`]: noesis_runtime::brushes::SolidColorBrush
    /// [`LinearGradientBrush`]: noesis_runtime::brushes::LinearGradientBrush
    Brush(BrushSpec),
    /// A boxed scalar value (string, number, bool). Resolves a
    /// `{StaticResource Key}` used where a plain value is expected (a `Single`
    /// `Width`, a `String` `Text`). The variant must match the target
    /// property's runtime type exactly: a `Double` won't satisfy a `Single`
    /// `Width`. See the [`dp`](crate::dp) module on `f32` vs `f64`.
    Value(DpValue),
}

/// Rust-built application resources. Insert as a Bevy [`Resource`], not on a
/// view: the dictionary it installs is process-global. See the
/// [module docs](self) for install timing and precedence.
#[derive(Resource, Clone, Default, Debug)]
pub struct NoesisResources {
    /// Code-built entries keyed by `x:Key`. They win over every merged
    /// dictionary and over the views' theme chain on a key collision.
    pub entries: HashMap<String, ResourceEntry>,
    /// Bare `<ResourceDictionary>` XAML fragments, each parsed and added to the
    /// installed dictionary's `MergedDictionaries` after the theme chain, so
    /// they override it. A fragment that fails to parse is skipped with a
    /// warning.
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

    /// Builder: register a `SolidColorBrush` of `[r, g, b, a]` (each `0..=1`)
    /// under `key`.
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

/// Emitted after each install of the application resources, when
/// [`NoesisResources::entries`] is non-empty. A declared key missing from
/// `present` failed to install.
#[derive(Message, Debug, Clone)]
pub struct NoesisResourcesInstalled {
    /// Keys of [`NoesisResources::entries`] found in the live application
    /// resources, sorted.
    pub present: Vec<String>,
}

/// Merge [`NoesisResources`] and every view's theme chain into one installed
/// dictionary, then emit [`NoesisResourcesInstalled`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_resources_bridge(
    resources: Option<Res<NoesisResources>>,
    views: Query<&NoesisView>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut installed: MessageWriter<NoesisResourcesInstalled>,
    mut warned_conflict: Local<bool>,
) {
    // Render state exists only after `noesis_runtime::init()`, which
    // `GUI::SetApplicationResources` requires.
    let Some(mut state) = state else {
        return;
    };

    // Font waits are unioned too: the install honors the same gates as scene
    // build, since chain dictionaries resolve `FontFamily` at parse time.
    let mut chain_uris: Vec<String> = Vec::new();
    let mut distinct_chains: Vec<&[String]> = Vec::new();
    let mut wait_fonts: Vec<String> = Vec::new();
    let mut wait_font_files: Vec<(String, String)> = Vec::new();
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
        for folder in &view.wait_for_fonts {
            if !wait_fonts.contains(folder) {
                wait_fonts.push(folder.clone());
            }
        }
        for pair in &view.wait_for_font_files {
            if !wait_font_files.contains(pair) {
                wait_font_files.push(pair.clone());
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

    let empty = NoesisResources::default();
    let resources = resources.as_deref().unwrap_or(&empty);

    if let Some(present) = state.reconcile_app_resources(
        &resources.entries,
        &resources.merged_xaml,
        &chain_uris,
        &wait_fonts,
        &wait_font_files,
    ) {
        if !resources.entries.is_empty() {
            installed.write(NoesisResourcesInstalled { present });
        }
    }
}

/// Registers the application-resources system. Added by
/// [`crate::NoesisPlugin`]. It does not insert a [`NoesisResources`]; without
/// one, only the views' theme chains are installed.
pub struct NoesisResourcesPlugin;

impl Plugin for NoesisResourcesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisResourcesInstalled>().add_systems(
            PostUpdate,
            sync_resources_bridge
                .in_set(NoesisSet::Sync)
                .after(sync_xaml_provider_map)
                // Unordered, a rebuild can flip the two Sync systems and install
                // the theme against an empty font cache.
                .after(sync_font_provider_map),
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
