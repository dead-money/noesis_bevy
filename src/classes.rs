//! Register Rust-backed XAML classes (`<myns:Foo>`) from Bevy systems.
//!
//! The class machinery ([`ClassBuilder`], [`ClassRegistration`], [`Instance`],
//! [`PropertyChangeHandler`], [`PropertyValue`]) comes from
//! [`noesis_runtime::classes`] and is re-exported here. This module adds
//! [`NoesisClassRegistry`], a non-send resource that keeps registrations alive
//! for the app's lifetime. [`crate::NoesisPlugin`] installs it.
//!
//! Register classes in a `Startup` system, before any XAML that uses them
//! loads. [`ClassBuilder::register`] returns `None` if the name is already
//! registered.
//!
//! # Property-change callbacks
//!
//! [`PropertyChangeHandler::on_changed`] runs on the main thread, inside
//! Noesis's property system, while the crate holds its Noesis state borrowed.
//! It must not reach back into the Bevy `World`; queue ECS work for a later
//! system instead. Pure derivations (computing one property from another) can
//! write back inline through the [`Instance`] setters. Handlers must be `Send`.
//!
//! # Example
//!
//! ```ignore
//! use bevy::prelude::*;
//! use noesis_bevy::classes::{
//!     ClassBase, ClassBuilder, Instance, NoesisClassRegistry, PropType,
//!     PropertyChangeHandler, PropertyValue,
//! };
//!
//! struct NineSlicerHandler { thickness_idx: u32 }
//!
//! impl PropertyChangeHandler for NineSlicerHandler {
//!     fn on_changed(&self, instance: Instance, idx: u32, value: PropertyValue<'_>) {
//!         if idx == self.thickness_idx {
//!             // Recompute derived properties and write them back through `instance`.
//!         }
//!     }
//! }
//!
//! fn register(mut registry: NonSendMut<NoesisClassRegistry>) {
//!     let mut b = ClassBuilder::new(
//!         "AOR.NineSlicer",
//!         ClassBase::ContentControl,
//!         NineSlicerHandler { thickness_idx: 1 },
//!     );
//!     b.add_property("Source", PropType::ImageSource);
//!     b.add_property("SliceThickness", PropType::Thickness);
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

/// Keeps [`ClassRegistration`]s alive for the app's lifetime. Access it with
/// `NonSendMut<NoesisClassRegistry>`: registrations hold `!Send` Noesis handles.
///
/// Add registrations from a `Startup` system, before any XAML that references
/// the class loads. They are released at app teardown, before Noesis shuts down:
/// Bevy 0.18 drops non-send resources in insertion order, and this one is
/// inserted at plugin build, before the render state whose `Drop` calls
/// [`noesis_runtime::shutdown`].
#[derive(Default)]
pub struct NoesisClassRegistry {
    registrations: Vec<ClassRegistration>,
}

impl NoesisClassRegistry {
    /// Keep `registration` alive until app teardown. A registration dropped
    /// earlier unregisters its class.
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

/// Installs [`NoesisClassRegistry`]. Added by [`crate::NoesisPlugin`]; adding it
/// again panics.
pub struct NoesisClassPlugin;

impl Plugin for NoesisClassPlugin {
    fn build(&self, app: &mut App) {
        app.init_non_send::<NoesisClassRegistry>();
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
