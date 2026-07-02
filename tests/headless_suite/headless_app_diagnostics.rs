//! Tests that `NoesisDiagnostics` mirrors Noesis allocator counters into the
//! resource each frame.
//!
//! The all-zero `Default` is the negative control: a broken or missing refresh
//! leaves every counter at 0. Two snapshots (early and late) prove the refresh
//! runs every frame, not one-shot. Font-free XAML so the scene builds without a
//! font gate.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisDiagnostics, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

const EARLY_AT_FRAME: usize = 3;
const SAMPLE_AT_FRAME: usize = 40;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Panel" Background="#400000FF"/>
</Grid>"##;

#[test]
fn diagnostics_resource_mirrors_allocator_counters() {
    let early: Arc<Mutex<Option<NoesisDiagnostics>>> = Arc::new(Mutex::new(None));
    let late: Arc<Mutex<Option<NoesisDiagnostics>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    app.add_systems(
        Startup,
        |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("diag.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: "diag.xaml".to_string(),
                    size: UVec2::new(64, 32),
                    ..default()
                },
            ));
        },
    );

    let early_sys = Arc::clone(&early);
    let late_sys = Arc::clone(&late);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>, diag: Res<NoesisDiagnostics>| {
            *frame += 1;
            if *frame == EARLY_AT_FRAME {
                *early_sys.lock().unwrap() = Some(*diag);
            }
            if *frame == SAMPLE_AT_FRAME {
                *late_sys.lock().unwrap() = Some(*diag);
            }
        },
    );

    // Event-driven exit: stop once the late (frame 40) snapshot has been captured
    // with a non-zero live allocation, which is the crux of the assertion. The
    // early/late captures stay frame-gated so the two-sample refresh proof holds.
    let pred_late = Arc::clone(&late);
    let sampled = run_until(&mut app, 240, move |_app| {
        pred_late
            .lock()
            .unwrap()
            .is_some_and(|d| d.allocated_memory > 0)
    });

    assert!(
        sampled,
        "late diagnostics snapshot with non-zero allocated_memory never captured within 240 frames",
    );

    let late = snapshot(&late, "late");
    let early = snapshot(&early, "early");
    eprintln!("--- NoesisDiagnostics early={early:?} late={late:?} ---");

    assert!(
        late.allocated_memory > 0,
        "allocated_memory should be non-zero after a scene builds (default 0); got {}",
        late.allocated_memory,
    );
    assert!(
        late.allocations_count > 0,
        "allocations_count should be non-zero after a scene builds (default 0); got {}",
        late.allocations_count,
    );
    // accum is cumulative-ever, allocated is live; catches a transposed/garbage read.
    assert!(
        late.allocated_memory_accum >= late.allocated_memory,
        "accum ({}) must be >= live allocated ({})",
        late.allocated_memory_accum,
        late.allocated_memory,
    );
    // early non-zero proves refresh runs every frame; init() allocates before any scene.
    assert!(
        early.allocations_count > 0,
        "early allocations_count should be non-zero (engine init allocates); got {}",
        early.allocations_count,
    );
}

fn snapshot(slot: &Arc<Mutex<Option<NoesisDiagnostics>>>, which: &str) -> NoesisDiagnostics {
    slot.lock()
        .unwrap()
        .unwrap_or_else(|| panic!("{which} diagnostics snapshot captured"))
}
