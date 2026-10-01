//! Engine-wide diagnostics: Noesis allocator counters, bridge bookkeeping and
//! timing in the [`NoesisDiagnostics`] resource, plus Noesis error reports
//! routed into Bevy's log.
//!
//! [`NoesisDiagnosticsPlugin`] is added by [`crate::NoesisPlugin`]. Read the
//! resource from any system:
//!
//! ```ignore
//! fn report(diag: Res<NoesisDiagnostics>) {
//!     if diag.is_changed() {
//!         info!("noesis: {} bytes live, {} scenes", diag.allocated_memory, diag.live_scenes);
//!     }
//! }
//! ```
//!
//! Absolute allocator figures vary between builds; compare them over time.
//! The `live_*` counts return to zero once their owners despawn, which makes them
//! useful for leak checks.

use bevy::prelude::*;
use noesis_runtime::diagnostics;

/// Noesis engine counters, refreshed every frame in `Update`.
///
/// The `live_*` counts read 0 in an app without Noesis render state (no
/// `RenderApp` and no headless harness). Change detection fires only on frames
/// where a value moved.
#[derive(Resource, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoesisDiagnostics {
    /// Bytes currently allocated through Noesis's allocator
    /// (`GetAllocatedMemory`). Rises and falls with object lifetimes.
    pub allocated_memory: u32,
    /// Cumulative bytes ever allocated (`GetAllocatedMemoryAccum`). Monotonic
    /// non-decreasing for the life of the process.
    pub allocated_memory_accum: u32,
    /// Number of live allocations (`GetAllocationsCount`).
    pub allocations_count: u32,
    /// Cumulative count of crate calls into Noesis (name lookups, property
    /// get/set, collection ops) since process start. Monotonic; the per-frame
    /// delta shows how much engine traffic a frame cost.
    pub ffi_hops: u64,
    /// Number of live Noesis scenes (one per built [`crate::NoesisView`]).
    pub live_scenes: usize,
    /// Number of mounted panels (one per [`crate::UiPanel`] whose fragment has
    /// been built).
    pub live_panels: usize,
    /// Number of live list bindings (one per `(view, x:Name)` a
    /// [`crate::UiList`] drives).
    pub live_lists: usize,
    /// Number of live [`crate::NoesisBinding`] targets (one per
    /// `(view, x:Name, property)`).
    pub live_bindings: usize,
    /// Wall time of the previous frame's [`NoesisSet::Apply`](crate::NoesisSet::Apply)
    /// phase, where every bridge writes into Noesis. `ZERO` before the first one.
    pub apply_time: std::time::Duration,
}

/// Inserts [`NoesisDiagnostics`] and, when [`route_errors`](Self::route_errors)
/// is set, routes Noesis error reports into Bevy's log.
///
/// [`crate::NoesisPlugin`] adds it with `route_errors: true`. Adding it a second
/// time panics, so `route_errors: false` only takes effect in an app that adds
/// neither `NoesisPlugin` nor [`NoesisPlugin::add_bridge_plugins`](crate::NoesisPlugin::add_bridge_plugins).
pub struct NoesisDiagnosticsPlugin {
    /// Install a process-global Noesis error handler that logs each report under
    /// the `noesis` target: `warn!`, or `error!` for fatal ones. Once installed it
    /// stays for the life of the process.
    pub route_errors: bool,
}

impl Default for NoesisDiagnosticsPlugin {
    fn default() -> Self {
        Self { route_errors: true }
    }
}

impl Plugin for NoesisDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisDiagnostics>();
        app.add_systems(Update, refresh_diagnostics);

        if self.route_errors {
            install_error_routing();
        }
    }
}

/// Install the error handler for the life of the process. Must run after
/// `noesis_runtime::init()`.
///
/// The guard is leaked on purpose. Dropping it restores the previous handler,
/// which crashes if it runs after `shutdown()`, and nothing would order a
/// resource holding it before `NoesisRenderState`'s `Drop` (which calls
/// `shutdown()`).
fn install_error_routing() {
    // Several `App`s in one process (tests) would otherwise stack a handler each.
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let guard = diagnostics::set_error_handler(|file, line, message, fatal| {
            if fatal {
                error!(target: "noesis", "{file}:{line}: {message}");
            } else {
                warn!(target: "noesis", "{file}:{line}: {message}");
            }
        });
        std::mem::forget(guard);
    });
}

/// Refresh [`NoesisDiagnostics`]; `set_if_neq` keeps change detection quiet.
#[allow(clippy::needless_pass_by_value)]
fn refresh_diagnostics(
    mut diag: ResMut<NoesisDiagnostics>,
    state: Option<NonSend<crate::render::NoesisRenderState>>,
    timer: Option<Res<crate::render::NoesisApplyTimer>>,
) {
    let next = NoesisDiagnostics {
        allocated_memory: diagnostics::allocated_memory(),
        allocated_memory_accum: diagnostics::allocated_memory_accum(),
        allocations_count: diagnostics::allocations_count(),
        ffi_hops: crate::render::ffi_hops(),
        live_scenes: state.as_ref().map_or(0, |s| s.live_scene_count()),
        live_panels: state.as_ref().map_or(0, |s| s.live_panel_count()),
        live_lists: state.as_ref().map_or(0, |s| s.live_list_count()),
        live_bindings: state.as_ref().map_or(0, |s| s.live_binding_count()),
        apply_time: timer.as_ref().map_or(std::time::Duration::ZERO, |t| t.last),
    };
    diag.set_if_neq(next);
}
