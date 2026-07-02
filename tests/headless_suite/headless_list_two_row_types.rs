//! Regression for the multi-row-type clobber (audit P0.3).
//!
//! Two [`NoesisView`]s each own a `ListBox` bound by a [`UiList`], but the two
//! lists carry *different* row component types: view 1 lists `RowA`, view 2 lists
//! `RowB`. Both types are registered with [`NoesisListAppExt::add_noesis_list`], so
//! each gets its own `diff_list::<T>` system iterating **every** list. The bug: a
//! type with no rows in a given list still overwrote that list's `schema` / `rows`
//! / `selected`, so whichever `diff_list` ran last emptied the other type's list
//! (and could freeze the row class with the wrong field layout).
//!
//! After the fix only the owning type writes a slot, so both lists realize their
//! rows regardless of scheduler order.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    ListedIn, NoesisCamera, NoesisListAppExt, NoesisListOps, NoesisView, NoesisViewModel, UiList,
    XamlRegistry,
};

use crate::common::{headless_app, run_until};

const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <ListBox x:Name="List">
    <ListBox.ItemTemplate>
      <DataTemplate>
        <TextBlock Text="{Binding label}"/>
      </DataTemplate>
    </ListBox.ItemTemplate>
  </ListBox>
</Grid>"##;

/// Row type for view 1.
#[derive(Component, NoesisViewModel)]
struct RowA {
    label: String,
}

/// Row type for view 2 (distinct field layout).
#[derive(Component, NoesisViewModel)]
struct RowB {
    name: String,
    weight: i32,
}

#[test]
fn two_row_types_do_not_clobber_each_others_lists() {
    let views: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));
    // Cumulative adds observed per view: proves each list realized its own rows.
    let adds_a: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
    let adds_b: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));

    let mut app = headless_app();
    app.add_noesis_list::<RowA>();
    app.add_noesis_list::<RowB>();

    let views_startup = Arc::clone(&views);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "host.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );

            let view_a = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                    UiList::new("List"),
                ))
                .id();
            let view_b = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                    UiList::new("List"),
                ))
                .id();

            commands.spawn((RowA { label: "a1".into() }, ListedIn(view_a)));
            commands.spawn((RowA { label: "a2".into() }, ListedIn(view_a)));
            commands.spawn((
                RowB {
                    name: "b1".into(),
                    weight: 1,
                },
                ListedIn(view_b),
            ));
            commands.spawn((
                RowB {
                    name: "b2".into(),
                    weight: 2,
                },
                ListedIn(view_b),
            ));
            commands.spawn((
                RowB {
                    name: "b3".into(),
                    weight: 3,
                },
                ListedIn(view_b),
            ));

            *views_startup.lock().unwrap() = Some((view_a, view_b));
        },
    );

    let views_sys = Arc::clone(&views);
    let adds_a_sys = Arc::clone(&adds_a);
    let adds_b_sys = Arc::clone(&adds_b);
    app.add_systems(Update, move |mut ops: MessageReader<NoesisListOps>| {
        let Some((view_a, view_b)) = *views_sys.lock().unwrap() else {
            return;
        };
        for ev in ops.read() {
            if ev.view == view_a {
                *adds_a_sys.lock().unwrap() += ev.adds as u32;
            } else if ev.view == view_b {
                *adds_b_sys.lock().unwrap() += ev.adds as u32;
            }
        }
    });

    // Exit once both lists have realized all their own rows (A: 2, B: 3). If one
    // type's diff_list clobbered the other's slot, its adds never arrive and this
    // times out.
    let pred_a = Arc::clone(&adds_a);
    let pred_b = Arc::clone(&adds_b);
    let realized = run_until(&mut app, 160, move |_app| {
        *pred_a.lock().unwrap() == 2 && *pred_b.lock().unwrap() == 3
    });

    let realized_a = *adds_a.lock().unwrap();
    let realized_b = *adds_b.lock().unwrap();

    assert!(
        realized,
        "both lists never realized all their rows (A expects 2, B expects 3) \
         within 160 frames; got A={realized_a} B={realized_b}",
    );

    assert_eq!(
        realized_a, 2,
        "view A's RowA list did not realize its 2 rows (clobbered by RowB's \
         diff_list?); got {realized_a} adds",
    );
    assert_eq!(
        realized_b, 3,
        "view B's RowB list did not realize its 3 rows (clobbered by RowA's \
         diff_list?); got {realized_b} adds",
    );
}
