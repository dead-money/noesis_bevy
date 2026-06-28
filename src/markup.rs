//! Custom `MarkupExtension` registration (Phase 5.D) — Bevy wrapper.
//!
//! Lets you register Rust-backed `{myns:Foo positional_arg}` markup
//! extensions from Bevy systems, with the resulting
//! [`MarkupExtensionRegistration`] managed by the Bevy resource lifecycle.
//!
//! # Why this layer is intentionally thin
//!
//! The heavy lifting lives in [`noesis_runtime::markup`] —
//! `MarkupExtensionRegistration`, `MarkupExtensionHandler`, `MarkupValue`,
//! all re-exported here. The Bevy side adds:
//!   * [`NoesisMarkupExtensionPlugin`] to declare the dependency explicitly
//!     and install the registry resource.
//!   * [`NoesisMarkupExtensionRegistry`] resource to own the live
//!     `MarkupExtensionRegistration` instances. Drop order matches
//!     resource cleanup, which Bevy 0.18 runs before the !Send
//!     `NoesisShutdownGuard` Drop, so registrations clean up before
//!     Noesis shuts down.
//!
//! # Threading
//!
//! Callbacks fire from inside Noesis's XAML parser, on whichever thread
//! triggered the load. In a Bevy app that's the **render thread** (which
//! drives the View). Handlers must be `Send`; if you need cross-thread
//! fan-out (e.g. Bevy ECS state), keep the body small and queue the work.

use bevy::prelude::*;

pub use noesis_runtime::markup::{
    ClosureHandler, MarkupExtensionHandler, MarkupExtensionRegistration, MarkupValue,
};

/// Owns the live [`MarkupExtensionRegistration`] instances for the app
/// lifetime. Insert finished registrations from a `Startup` system; the
/// resource drops them at app teardown, before [`noesis_runtime::shutdown`]
/// runs.
///
/// Registrations must be added BEFORE any XAML referencing the extension
/// loads — in practice that means a `Startup` system ordered after
/// [`crate::NoesisPlugin`] initialization (Bevy's default startup order
/// suffices unless explicitly overridden).
/// **Non-send** resource: [`MarkupExtensionRegistration`] holds `!Send`/`!Sync`
/// Noesis handles, so this is stored via `init_non_send_resource` and accessed
/// through `NonSendMut`. Mirrors [`crate::classes::NoesisClassRegistry`].
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
