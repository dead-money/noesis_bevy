//! App-level diagnostics bridge: surface Noesis's process-global allocator
//! counters as a Bevy resource and route its error handler into Bevy's log.
//!
//! Unlike the per-element bridges (visibility, layout, brushes, …) this targets
//! nothing in the visual tree — it watches the engine itself. The plugin owns:
//!
//!   * [`NoesisDiagnostics`] — a resource refreshed every frame from
//!     `noesis_runtime::diagnostics::{allocated_memory, allocated_memory_accum,
//!     allocations_count}`. Absolute values aren't meaningful across builds;
//!     reason about deltas and monotonicity (`accum` is monotonic
//!     non-decreasing; the others rise and fall with object lifetimes).
//!   * an optional process-global error handler ([`route_errors`]) that forwards
//!     Noesis `NS_ERROR` reports into Bevy `tracing` (`warn!` / `error!`). It is
//!     installed for the process lifetime (see [`install_error_routing`]).
//!
//! [`route_errors`]: NoesisDiagnosticsPlugin::route_errors
//!
//! Both halves need [`crate::NoesisPlugin`] to have called `noesis_runtime::init`
//! first — which it has, since this plugin is added from inside `NoesisPlugin`
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
    /// restores its predecessor when the world drops.
    pub route_errors: bool,
}

impl Default for NoesisDiagnosticsPlugin {
    fn default() -> Self {
        // Routing engine errors into the app log is the useful default; it only
        // logs, and any predecessor handler is restored on drop.
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

/// Pull the current allocator counters into [`NoesisDiagnostics`]. `set_if_neq`
/// keeps change-detection quiet on the (common) frames where nothing moved.
fn refresh_diagnostics(mut diag: ResMut<NoesisDiagnostics>) {
    let next = NoesisDiagnostics {
        allocated_memory: diagnostics::allocated_memory(),
        allocated_memory_accum: diagnostics::allocated_memory_accum(),
        allocations_count: diagnostics::allocations_count(),
    };
    diag.set_if_neq(next);
}
