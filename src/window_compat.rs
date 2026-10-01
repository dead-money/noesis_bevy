//! Stand-in for the `Window` root element, so `<Window>`-rooted XAML loads.
//!
//! Noesis's `Window` type belongs to its App framework, which this crate doesn't
//! use. Without it the XAML parser rejects a `<Window>` root (as in most SDK
//! samples) with `Unknown element type 'Window'`.
//!
//! [`NoesisWindowCompatPlugin`] registers a Rust-backed `Window` class derived
//! from `UserControl`, so it carries `Content`, `Resources` and `FontFamily` and
//! renders its content through `UserControl`'s default template. It also declares
//! the `Window`-only attributes samples set (`Title`, `WindowStyle`,
//! `WindowState`, `WindowStartupLocation`, `ResizeMode`, `SizeToContent`) as inert
//! string properties. It has no OS-window behavior.
//!
//! The plugin is opt-in. Scenes rooted at a `Grid`, `UserControl` or other
//! `FrameworkElement` don't need it.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use noesis_bevy::{NoesisPlugin, NoesisWindowCompatPlugin};
//!
//! App::new()
//!     .add_plugins((DefaultPlugins, NoesisPlugin::default()))
//!     .add_plugins(NoesisWindowCompatPlugin)
//!     .run();
//! ```

use bevy::prelude::*;
use noesis_runtime::classes::{ClassBuilder, Instance, PropertyChangeHandler, PropertyValue};
use noesis_runtime::ffi::{ClassBase, PropType};

use crate::classes::NoesisClassRegistry;

/// Class name the stand-in registers under, matched by a `<Window>` root.
pub const WINDOW_CLASS: &str = "Window";

struct NoopChangeHandler;

impl PropertyChangeHandler for NoopChangeHandler {
    fn on_changed(&self, _instance: Instance, _prop_index: u32, _value: PropertyValue<'_>) {}
}

/// Warns and does nothing if the name is already registered.
fn register_window_type(registry: &mut NoesisClassRegistry) {
    let mut builder = ClassBuilder::new(WINDOW_CLASS, ClassBase::UserControl, NoopChangeHandler);
    // Inert, but must exist as DPs or the parser rejects the attribute. String
    // also covers the enum-valued ones.
    builder.add_property("Title", PropType::String);
    builder.add_property("WindowStyle", PropType::String);
    builder.add_property("WindowState", PropType::String);
    builder.add_property("WindowStartupLocation", PropType::String);
    builder.add_property("ResizeMode", PropType::String);
    builder.add_property("SizeToContent", PropType::String);

    match builder.register() {
        Some(registration) => {
            registry.add(registration);
            info!("NoesisWindowCompat: registered '{WINDOW_CLASS}' stand-in (UserControl)");
        }
        None => {
            warn!("NoesisWindowCompat: '{WINDOW_CLASS}' already registered or registration failed",)
        }
    }
}

fn install_window_type(mut registry: NonSendMut<NoesisClassRegistry>) {
    register_window_type(&mut registry);
}

/// Registers the [`WINDOW_CLASS`] stand-in at `Startup` so `<Window>`-rooted XAML
/// parses. Needs the [`NoesisClassRegistry`] that [`crate::NoesisPlugin`] adds;
/// the registry keeps the class registered for the app's lifetime.
pub struct NoesisWindowCompatPlugin;

impl Plugin for NoesisWindowCompatPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, install_window_type.run_if(run_once));
    }
}
