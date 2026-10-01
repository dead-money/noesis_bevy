//! Custom XAML markup extensions (`{myns:Foo arg}`) backed by Rust handlers.
//!
//! Build a [`MarkupExtensionRegistration`] with the [`noesis_runtime::markup`]
//! API (re-exported here) and hand it to [`NoesisMarkupExtensionRegistry`],
//! which keeps it alive for the life of the app. [`crate::NoesisPlugin`]
//! installs the registry through [`NoesisMarkupExtensionPlugin`].
//!
//! Register an extension before any XAML that uses it loads: a `Startup`
//! system is early enough, since scenes build in `PostUpdate`.
//!
//! # Threading
//!
//! Handlers run synchronously inside Noesis's XAML parser on the main thread,
//! while the crate's `NoesisRenderState` is borrowed to load a scene or panel
//! fragment. A handler cannot reach the Bevy `World`; keep it small and queue
//! any ECS work for a later system. The FFI still requires handlers to be
//! `Send`.

use bevy::prelude::*;

pub use noesis_runtime::markup::{
    ClosureHandler, MarkupExtensionHandler, MarkupExtensionRegistration, MarkupValue,
};

/// Keeps [`MarkupExtensionRegistration`]s alive for the life of the app.
///
/// This is a non-send resource (registrations hold `!Send` Noesis handles), so
/// reach it with `NonSendMut<NoesisMarkupExtensionRegistry>`. Dropping a
/// registration unregisters the extension; the registry drops its
/// registrations when the app's resources drop.
#[derive(Default)]
pub struct NoesisMarkupExtensionRegistry {
    registrations: Vec<MarkupExtensionRegistration>,
}

impl NoesisMarkupExtensionRegistry {
    /// Keeps `registration` alive until the registry drops.
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

/// Installs [`NoesisMarkupExtensionRegistry`]. [`crate::NoesisPlugin`] adds
/// this plugin; you don't add it yourself.
pub struct NoesisMarkupExtensionPlugin;

impl Plugin for NoesisMarkupExtensionPlugin {
    fn build(&self, app: &mut App) {
        app.init_non_send::<NoesisMarkupExtensionRegistry>();
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
