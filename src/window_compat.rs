//! `Window` root-element compatibility shim.
//!
//! Noesis's `Window` type lives in the **App framework** (`NsApp`), which this
//! crate deliberately does not link. We drive only the core + GUI of
//! `libNoesis.so` and host the result inside a Bevy `View`. As a result the XAML
//! parser doesn't know the `<Window>` element, and any scene authored with a
//! `<Window>` root (as most of the SDK *Samples* are) fails to load with
//! `Unknown element type 'Window'`.
//!
//! Rather than hand-edit each sample's XAML, [`NoesisWindowCompatPlugin`]
//! registers a lightweight stand-in: a Rust-backed `Window` class derived from
//! `UserControl` (so it carries `Content`, `Resources`, `FontFamily`, and renders
//! its content through `UserControl`'s default template) plus the handful of
//! `Window`-only properties samples set as attributes (`Title`, `WindowStyle`,
//! `WindowStartupLocation`, ...). The stand-in has no OS-window behaviour; it is
//! purely a content host, which is exactly what an embedded Bevy view needs.
//!
//! Add the plugin after [`crate::NoesisPlugin`]; it registers the type once at
//! startup (held for the app's lifetime in [`NoesisClassRegistry`]). It is
//! intentionally **opt-in**: scenes with a `FrameworkElement` root (`Grid`,
//! `UserControl`, ...) don't need it.
//!
//! ```ignore
//! app.add_plugins(NoesisPlugin::default())
//!    .add_plugins(NoesisWindowCompatPlugin);
//! ```

use bevy::prelude::*;
use noesis_runtime::classes::{ClassBuilder, Instance, PropertyChangeHandler, PropertyValue};
use noesis_runtime::ffi::{ClassBase, PropType};

use crate::classes::NoesisClassRegistry;

/// The Noesis class name the stand-in registers under: the bare `Window` the
/// XAML parser resolves a `<Window>` root against.
pub const WINDOW_CLASS: &str = "Window";

/// No-op change handler: the stand-in's properties are inert (it's a content
/// host, not a real window), so nothing forwards.
struct NoopChangeHandler;

impl PropertyChangeHandler for NoopChangeHandler {
    fn on_changed(&self, _instance: Instance, _prop_index: u32, _value: PropertyValue<'_>) {}
}

/// Register the `Window` stand-in class into the global Noesis reflection. Safe
/// to call once after `noesis_runtime::init`; the returned registration is kept
/// alive for the app's lifetime by [`NoesisClassRegistry`]. No-op (with a warning)
/// if the name is already taken.
fn register_window_type(registry: &mut NoesisClassRegistry) {
    let mut builder = ClassBuilder::new(WINDOW_CLASS, ClassBase::UserControl, NoopChangeHandler);
    // `Window`-only properties samples set as attributes. They're inert here, but
    // must exist as DPs or the parser rejects the attribute. Strings cover the
    // enum-valued ones too (the value is parsed as a string and ignored).
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

/// Startup system that installs the `Window` stand-in once (the plugin gates it
/// with `run_once`).
fn install_window_type(mut registry: NonSendMut<NoesisClassRegistry>) {
    register_window_type(&mut registry);
}

/// Installs the [`Window`](WINDOW_CLASS) root-element stand-in so `<Window>`-rooted
/// XAML (the SDK samples) parses against the core + GUI runtime. Add **after**
/// [`crate::NoesisPlugin`].
pub struct NoesisWindowCompatPlugin;

impl Plugin for NoesisWindowCompatPlugin {
    fn build(&self, app: &mut App) {
        // Registry comes from NoesisPlugin; it keeps the registration alive for the app's lifetime.
        app.add_systems(Startup, install_window_type.run_if(run_once));
    }
}
