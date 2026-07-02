//! Custom `MarkupExtension` registration for Bevy.
//!
//! Register Rust-backed `{myns:Foo positional_arg}` markup extensions from
//! Bevy systems. The resulting [`MarkupExtensionRegistration`] is owned by
//! [`NoesisMarkupExtensionRegistry`], tied to the Bevy resource lifecycle.
//!
//! The primitives ([`MarkupExtensionRegistration`], [`MarkupExtensionHandler`],
//! [`MarkupValue`]) come from [`noesis_runtime::markup`] and are re-exported
//! here. The Bevy layer adds [`NoesisMarkupExtensionPlugin`] to install the
//! registry resource and [`NoesisMarkupExtensionRegistry`] to own the live
//! registrations. Registrations drop during resource cleanup, which Bevy 0.18
//! runs before the `!Send` `NoesisShutdownGuard` Drop, so they clean up before
//! Noesis shuts down.
//!
//! # Threading
//!
//! Callbacks fire from inside Noesis's XAML parser, on whichever thread
//! triggered the load. In a Bevy app that's the main thread, during the
//! scene-build pass that drives the View. The handler runs while Noesis (and
//! the `NoesisRenderState` that owns it) is borrowed, so it must not reenter
//! the Bevy `World`; keep the body small and queue any ECS work for a later
//! system. Handlers are still `Send`-bound by the FFI.

use bevy::prelude::*;

pub use noesis_runtime::markup::{
    ClosureHandler, MarkupExtensionHandler, MarkupExtensionRegistration, MarkupValue,
};

/// Owns the live [`MarkupExtensionRegistration`] instances for the app
/// lifetime. Insert finished registrations from a `Startup` system; the
/// resource drops them at app teardown, before [`noesis_runtime::shutdown`]
/// runs.
///
/// Add registrations BEFORE any XAML referencing the extension loads. In
/// practice that means a `Startup` system ordered after [`crate::NoesisPlugin`]
/// initialization (Bevy's default startup order suffices unless overridden).
///
/// Non-send resource: [`MarkupExtensionRegistration`] holds `!Send`/`!Sync`
/// Noesis handles, so this is stored via `init_non_send_resource` and accessed
/// through `NonSendMut`.
#[derive(Default)]
pub struct NoesisMarkupExtensionRegistry {
    registrations: Vec<MarkupExtensionRegistration>,
}

impl NoesisMarkupExtensionRegistry {
    /// Take ownership of a [`MarkupExtensionRegistration`]. Holds for the
    /// resource's lifetime (= app lifetime in normal use).
    pub fn add(&mut self, registration: MarkupExtensionRegistration) {
        self.registrations.push(registration);
    }

    /// Number of registered extensions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.registrations.len()
    }

    /// Whether no extensions are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registrations.is_empty()
    }
}

/// Plugin that installs [`NoesisMarkupExtensionRegistry`]. Add **after**
/// [`crate::NoesisPlugin`] so [`noesis_runtime::init`] has run by the time
/// consumers register from `Startup` systems.
pub struct NoesisMarkupExtensionPlugin;

impl Plugin for NoesisMarkupExtensionPlugin {
    fn build(&self, app: &mut App) {
        app.init_non_send_resource::<NoesisMarkupExtensionRegistry>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_empty_by_default() {
        let r = NoesisMarkupExtensionRegistry::default();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
    }
}
