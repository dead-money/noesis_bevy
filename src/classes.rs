//! Register Rust-backed XAML classes (`<myns:Foo>`) with Noesis from Bevy systems.
//!
//! The resulting [`ClassRegistration`] is owned by the Bevy resource lifecycle:
//! it drops at app teardown, before [`noesis_runtime::shutdown`] runs via
//! `NoesisShutdownGuard`.
//!
//! The class machinery (`ClassBuilder`, `ClassRegistration`, `Instance`,
//! `PropertyChangeHandler`, `PropertyValue`) lives in [`noesis_runtime::classes`]
//! and is re-exported here. The Bevy side adds two pieces:
//!   * [`NoesisClassPlugin`] installs the registry resource.
//!   * [`NoesisClassRegistry`] owns the live `ClassRegistration` instances. Bevy
//!     0.18 runs resource cleanup before the `!Send` `NoesisShutdownGuard` Drop,
//!     so registrations release before Noesis shuts down.
//!
//! # Property-change threading
//!
//! Callbacks fire from inside Noesis's property pump. In a Bevy app that's
//! the **render thread** (which drives the View). Handlers must be `Send`;
//! mutations to Bevy ECS state should be queued and processed on the main
//! thread. For purely-derived properties (e.g. `NineSlicer` computing
//! viewbox rects from `SliceThickness`), the handler can do the math and
//! call `Instance::set_*` inline, with no main-world hop.
//!
//! # Usage
//!
//! ```ignore
//! use bevy::prelude::*;
//! use noesis_bevy::classes::{
//!     ClassBase, ClassBuilder, NoesisClassRegistry, PropType,
//!     PropertyChangeHandler, PropertyValue, Instance,
//! };
//!
//! struct NineSlicerHandler { source_idx: u32, thickness_idx: u32 /* ... */ }
//! impl PropertyChangeHandler for NineSlicerHandler {
//!     fn on_changed(&mut self, instance: Instance, idx: u32, value: PropertyValue<'_>) {
//!         if idx == self.thickness_idx {
//!             // Recompute derived properties and write back via instance.set_rect(...)
//!         }
//!     }
//! }
//!
//! fn register(mut registry: NonSendMut<NoesisClassRegistry>) {
//!     let mut b = ClassBuilder::new("AOR.NineSlicer", ClassBase::ContentControl,
//!                                   NineSlicerHandler { /* ... */ });
//!     b.add_property("Source", PropType::ImageSource);
//!     b.add_property("SliceThickness", PropType::Thickness);
//!     // ...
//!     if let Some(reg) = b.register() {
//!         registry.add(reg);
//!     }
//! }
//! ```

use bevy::prelude::*;

pub use noesis_runtime::classes::{
    ClassBuilder, ClassRegistration, Instance, PropertyChangeHandler, PropertyDefault,
    PropertyValue,
};
pub use noesis_runtime::ffi::{ClassBase, PropType};

/// Owns the live [`ClassRegistration`] instances for the app lifetime.
/// Insert finished registrations from a `Startup` system; the resource
/// drops them at app teardown, before [`noesis_runtime::shutdown`] runs.
///
/// Add registrations BEFORE any XAML referencing them is loaded: a `Startup`
/// system ordered after [`crate::NoesisPlugin`] initialization (Bevy's default
/// startup order suffices unless you override it).
///
/// Non-send resource: [`ClassRegistration`] holds `!Send`/`!Sync` Noesis handles,
/// so it is stored via `init_non_send_resource` and accessed through `NonSendMut`.
/// Class registration is a main-thread, startup-time concern anyway, since Noesis
/// is thread-affine.
#[derive(Default)]
pub struct NoesisClassRegistry {
    registrations: Vec<ClassRegistration>,
}

impl NoesisClassRegistry {
    /// Take ownership of a [`ClassRegistration`]. Holds for the resource's
    /// lifetime (= app lifetime in normal use).
    pub fn add(&mut self, registration: ClassRegistration) {
        self.registrations.push(registration);
    }

    /// Number of registered classes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// Whether no classes are registered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }
}

/// Plugin that installs [`NoesisClassRegistry`]. Add **after**
/// [`crate::NoesisPlugin`] so [`noesis_runtime::init`] has already run by the
/// time consumers register classes from `Startup` systems.
///
/// The plugin itself is intentionally minimal: registration is a startup-time
/// concern and class definitions are consumer-specific, so the plugin's only
/// job is to give consumers a well-known place to stash their registrations.
pub struct NoesisClassPlugin;

impl Plugin for NoesisClassPlugin {
    fn build(&self, app: &mut App) {
        app.init_non_send_resource::<NoesisClassRegistry>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_empty_by_default() {
        let r = NoesisClassRegistry::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }
}
