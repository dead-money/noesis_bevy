//! App-level diagnostics bridge: surface Noesis's process-global allocator
//! counters as a Bevy resource and route its error handler into Bevy's log.
//!
//! Unlike the per-element bridges (visibility, layout, brushes, …) this targets
//! nothing in the visual tree; it watches the engine itself. The plugin owns:
//!
//!   * [`NoesisDiagnostics`], a resource refreshed every frame from
//!     `noesis_runtime::diagnostics::{allocated_memory, allocated_memory_accum,
//!     allocations_count}`. Absolute values aren't meaningful across builds;
//!     reason about deltas and monotonicity (`accum` is monotonic
//!     non-decreasing; the others rise and fall with object lifetimes).
//!   * an optional process-global error handler ([`route_errors`]) that forwards
//!     Noesis `NS_ERROR` reports into Bevy `tracing` (`warn!` / `error!`). It is
//!     installed for the process lifetime (see `install_error_routing`).
//!
//! [`route_errors`]: NoesisDiagnosticsPlugin::route_errors
//!
//! Both halves need [`crate::NoesisPlugin`] to have called `noesis_runtime::init`
//! first, which it has, since this plugin is added from inside `NoesisPlugin`
//! after `init()`.

use bevy::prelude::*;
use noesis_runtime::diagnostics;

/// Snapshot of Noesis's allocator counters, refreshed once per frame.
///
/// Starts at all-zero (`Default`); a working refresh fills it with the live
/// figures after the engine has allocated anything (which it has by the time the
/// first scene builds). All values are bytes/counts straight from Noesis.
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
    /// Cumulative count of FFI "hops" into the Noesis engine — name lookups, DP
    /// get/set, collection ops — since process start. Monotonic non-decreasing;
    /// reason about the per-frame *delta* to see how much engine traffic a frame
    /// cost. Stays 0 in a build with no live view. This is the lever later perf
    /// work tunes against.
    pub ffi_hops: u64,
    /// Number of live Noesis scenes (one per built [`crate::NoesisView`]). Returns
    /// to 0 after every view despawns, which is the despawn-teardown invariant.
    pub live_scenes: usize,
    /// Number of live mounted panels (one per [`crate::UiPanel`] entity whose
    /// fragment has been built). Returns to 0 after every panel despawns, mirroring
    /// [`live_scenes`](Self::live_scenes) for the panel primitive.
    pub live_panels: usize,
    /// Number of live entity-keyed list bindings (one per `(view, x:Name)` a
    /// [`crate::UiList`] reconciles). Returns to 0 after every owning view
    /// despawns, mirroring [`live_panels`](Self::live_panels) for the list
    /// primitive.
    pub live_lists: usize,
    /// Wall-time of the previous frame's `NoesisSet::Apply` phase — every bridge's
    /// FFI push. `ZERO` until the first frame with a live view has run.
    pub apply_time: std::time::Duration,
}

/// App-level plugin exposing [`NoesisDiagnostics`] and (optionally) routing the
/// Noesis error handler into Bevy's log.
///
/// Added by [`crate::NoesisPlugin`] with [`route_errors`](Self::route_errors)
/// enabled. Construct it directly to opt out of error routing:
///
/// ```ignore
/// app.add_plugins(NoesisDiagnosticsPlugin { route_errors: false });
/// ```
pub struct NoesisDiagnosticsPlugin {
    /// When `true`, install a process-global Noesis error handler that forwards
    /// reports into Bevy `tracing` (`warn!`, or `error!` for fatal). The handler
    /// stays installed for the process lifetime (see `install_error_routing`).
    pub route_errors: bool,
}

impl Default for NoesisDiagnosticsPlugin {
    fn default() -> Self {
        // Default on: log-only, so it can't break an app that doesn't want it.
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

/// Install a process-global Noesis error handler that forwards into Bevy's log,
/// for the lifetime of the process.
///
/// `NoesisPlugin` adds this only after `noesis_runtime::init()`, so the handler
/// slot is live. We deliberately **leak** the RAII guard: it restores the
/// predecessor handler on drop, but Bevy gives no drop-order guarantee between a
/// main-world resource and the render-world `NoesisRenderState` that owns
/// `shutdown()`. If the guard dropped after shutdown, the restore call would
/// reach into a torn-down `NsCore` kernel and crash. A process-global log hook
/// wants process lifetime anyway (the same reason `log::set_logger` never
/// uninstalls), so leaking is the correct trade, not a workaround.
fn install_error_routing() {
    let guard = diagnostics::set_error_handler(|file, line, message, fatal| {
        if fatal {
            error!(target: "noesis", "{file}:{line}: {message}");
        } else {
            warn!(target: "noesis", "{file}:{line}: {message}");
        }
    });
    std::mem::forget(guard);
}

/// Pull the current allocator counters, FFI-hop tally, live-scene count and last
/// Apply wall-time into [`NoesisDiagnostics`]. The Noesis-sourced figures
/// (`ffi_hops`, `live_scenes`) come from the render state when it exists;
/// headless builds without a `RenderApp` have none, so they read 0. `set_if_neq`
/// keeps change-detection quiet on the (rare, now that `ffi_hops` ticks)
/// frames where nothing moved.
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
        apply_time: timer.as_ref().map_or(std::time::Duration::ZERO, |t| t.last),
    };
    diag.set_if_neq(next);
}
