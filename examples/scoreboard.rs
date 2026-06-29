//! Faithful Bevy-idiomatic port of the Noesis SDK **Scoreboard** sample
//! (`$NOESIS_SDK_DIR/Src/Packages/Samples/Scoreboard`).
//!
//! Unlike the other example ports, this one is a **conformance** test: it renders
//! the genuine reference UI. The sample's real `MainWindow.xaml` and its two
//! fonts are read **at runtime** from `$NOESIS_SDK_DIR` (the SDK is per-developer
//! licensed and is never vendored into this repo, the same rule as `assets/Data`).
//! Nothing here is a simplified re-creation of the layout: the emblem geometries,
//! gradient/radial brushes, `ComboBox`/`ScrollViewer` control templates, the
//! per-player `DataTemplate` with its `DataTrigger`s, and the `b:Interaction`
//! behaviors are all the SDK's own XAML, byte-for-byte.
//!
//! The reference C++ sample exposes a `Game` view model (`Name`, `ElapsedTime`,
//! `AllianceScore`, `HordeScore`, `SelectedTeam`, a `Players` collection and a
//! `VisibleTeams` collection) reflected to XAML. This port reproduces that data
//! through the crate's safe bridges, never raw FFI:
//!
//!   * [`NoesisVm`] attaches a DO-backed `Scoreboard.Game` instance as the view
//!     root `DataContext`, supplying the scalar bindings (`{Binding Name}`,
//!     `{Binding AllianceScore}`, `{Binding HordeScore}`, `{Binding ElapsedTime}`,
//!     `{Binding SelectedTeam}`).
//!   * [`NoesisItems::with_objects`] supplies the `Players` `ItemsControl` with
//!     ten **bindable object** items (one Rust-backed Noesis class per row), so
//!     the per-player `DataTemplate` bindings (`{Binding Name}`, `{Binding Score}`,
//!     `{Binding Kills}`, `{Binding Team}`, `{Binding Class}`, ...) resolve and
//!     the team/class `DataTrigger`s fire, matching the reference exactly.
//!   * [`NoesisItems::with`] supplies the `VisibleTeam` `ComboBox` with the three
//!     team strings; its `SelectedIndex` binds to `Game.SelectedTeam`.
//!
//! The seed dataset is the SDK's own `SampleData/ScoreboardSampleData.xaml`
//! (10 players, "Silvershard Mines", 16 minutes), so the rendered scoreboard
//! matches the reference's design-time view.
//!
//! Run it windowed (requires `$NOESIS_SDK_DIR`):
//!
//! ```sh
//! cargo run -p noesis_bevy --example scoreboard
//! # headless screenshot:
//! NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=scoreboard.png \
//!   cargo run -p noesis_bevy --example scoreboard
//! ```
//!
//! The headless data round-trip is asserted by
//! `tests/headless_example_scoreboard.rs`, which reuses this file's staging +
//! spawn helpers.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use noesis_bevy::classes::PropType;
use noesis_bevy::{
    DpKind, FontRegistry, NoesisCamera, NoesisDp, NoesisItems, NoesisPlugin, NoesisView, NoesisVm,
    NoesisWindowCompatPlugin, ObjectRow, ViewModelDef, XamlRegistry,
};

/// View / intermediate-target size. The XAML wraps a 900x600 layout in a
/// `Viewbox`. The SDK sample sets no explicit window size (its X11 display just
/// fills ~74% of the desktop), so we render at Full HD 1080p; the `Viewbox`
/// scales the authored 1000x700 layout (a `<Grid Width="900" Height="600"
/// Margin="50">`) crisply to fill it.
pub const VIEW_W: u32 = 1920;
pub const VIEW_H: u32 = 1080;

/// URI the sample XAML is registered under in [`XamlRegistry`].
pub const SCOREBOARD_URI: &str = "scoreboard/MainWindow.xaml";

/// `x:Name` of the per-player `ItemsControl` whose `ItemsSource` we drive with
/// bindable object items.
pub const PLAYERS_NAME: &str = "Players";

/// `x:Name` of the team-filter `ComboBox`.
pub const VISIBLE_TEAM_NAME: &str = "VisibleTeam";

/// Noesis class name registered for the bindable player items.
pub const PLAYER_CLASS: &str = "Scoreboard.Player";

/// Noesis class name registered for the root `Game` view model.
pub const GAME_CLASS: &str = "Scoreboard.Game";

/// One row of the SDK's `ScoreboardSampleData.xaml`, verbatim.
struct PlayerSeed {
    class: &'static str,
    deaths: i32,
    damage: i32,
    heal: i32,
    kills: i32,
    name: &'static str,
    score: i32,
    team: &'static str,
}

/// The exact 10-player seed dataset from
/// `$NOESIS_SDK_DIR/Src/Packages/Samples/Scoreboard/Data/SampleData/ScoreboardSampleData.xaml`.
const SAMPLE_PLAYERS: [PlayerSeed; 10] = [
    PlayerSeed {
        class: "Mage",
        deaths: 96,
        damage: 8_134_124,
        heal: 1_831,
        kills: 43,
        name: "Nam Cras Aenean",
        score: 476,
        team: "Alliance",
    },
    PlayerSeed {
        class: "Rogue",
        deaths: 98,
        damage: 8_324_715,
        heal: 2_954,
        kills: 79,
        name: "Sed Vestibulum",
        score: 414,
        team: "Horde",
    },
    PlayerSeed {
        class: "Hunter",
        deaths: 45,
        damage: 797_117,
        heal: 2_615,
        kills: 99,
        name: "Curae Praesent",
        score: 383,
        team: "Horde",
    },
    PlayerSeed {
        class: "Hunter",
        deaths: 93,
        damage: 481_757,
        heal: 6_353,
        kills: 34,
        name: "Adipiscing Quisque",
        score: 327,
        team: "Alliance",
    },
    PlayerSeed {
        class: "Fighter",
        deaths: 82,
        damage: 743_715,
        heal: 37_415,
        kills: 80,
        name: "Estonec Vivamus",
        score: 289,
        team: "Horde",
    },
    PlayerSeed {
        class: "Rogue",
        deaths: 21,
        damage: 383_571,
        heal: 82_114,
        kills: 90,
        name: "Duisleo Curabitur",
        score: 265,
        team: "Alliance",
    },
    PlayerSeed {
        class: "Cleric",
        deaths: 86,
        damage: 441_751,
        heal: 255_131,
        kills: 37,
        name: "Musetiam Aliquam",
        score: 259,
        team: "Alliance",
    },
    PlayerSeed {
        class: "Mage",
        deaths: 60,
        damage: 201_175,
        heal: 4_915,
        kills: 63,
        name: "Numauris Accumsan",
        score: 225,
        team: "Horde",
    },
    PlayerSeed {
        class: "Fighter",
        deaths: 30,
        damage: 271_735,
        heal: 6_715,
        kills: 20,
        name: "Phasellus Nullam",
        score: 195,
        team: "Alliance",
    },
    PlayerSeed {
        class: "Cleric",
        deaths: 18,
        damage: 87_537,
        heal: 95_717,
        kills: 54,
        name: "Consequat Bibendum",
        score: 180,
        team: "Horde",
    },
];

/// The seed game name + elapsed minutes, from the same sample-data file.
pub const GAME_NAME: &str = "Silvershard Mines";
pub const ELAPSED_MINUTES: i32 = 16;

/// `Game.SelectedTeam` seed: index into `["Overall", "Alliance", "Horde"]`.
/// "Overall" (0) shows every player, matching the reference's default.
pub const SELECTED_TEAM: i32 = 0;

/// Build the ten bindable player rows (object items). Each field name matches a
/// `{Binding ...}` path in the SDK's per-player `DataTemplate`. `Class` and
/// `Team` are strings so the template's `DataTrigger`s (which compare against
/// `"Fighter"`, `"Alliance"`, ...) fire exactly as in the reference.
#[must_use]
pub fn player_rows() -> Vec<ObjectRow> {
    SAMPLE_PLAYERS
        .iter()
        .map(|p| {
            vec![
                ("Name".to_string(), p.name.into()),
                ("Score".to_string(), p.score.into()),
                ("Kills".to_string(), p.kills.into()),
                ("Deaths".to_string(), p.deaths.into()),
                ("Damage".to_string(), p.damage.into()),
                ("Heal".to_string(), p.heal.into()),
                ("Class".to_string(), p.class.into()),
                ("Team".to_string(), p.team.into()),
            ]
        })
        .collect()
}

/// Total score of the Alliance team (mirrors `Game::GetAllianceScore`).
#[must_use]
pub fn alliance_score() -> i32 {
    SAMPLE_PLAYERS
        .iter()
        .filter(|p| p.team == "Alliance")
        .map(|p| p.score)
        .sum()
}

/// Total score of the Horde team (mirrors `Game::GetHordeScore`).
#[must_use]
pub fn horde_score() -> i32 {
    SAMPLE_PLAYERS
        .iter()
        .filter(|p| p.team == "Horde")
        .map(|p| p.score)
        .sum()
}

/// Resolve the sample's data directory inside the SDK, or `None` when
/// `$NOESIS_SDK_DIR` is unset (the example/test then skips).
#[must_use]
pub fn sample_data_dir() -> Option<PathBuf> {
    let sdk = std::env::var_os("NOESIS_SDK_DIR")?;
    Some(PathBuf::from(sdk).join("Src/Packages/Samples/Scoreboard/Data"))
}

/// Read the SDK's real `MainWindow.xaml` + its two fonts at runtime and register
/// them. Returns `true` on success; `false` (with a warning) when the SDK isn't
/// reachable, so callers should then skip spawning. No SDK bytes are vendored.
#[must_use]
pub fn stage_assets(xaml: &mut XamlRegistry, fonts: &mut FontRegistry) -> bool {
    let Some(data) = sample_data_dir() else {
        warn!("scoreboard: NOESIS_SDK_DIR unset — skipping (no SDK assets to load)");
        return false;
    };

    let xaml_path = data.join("MainWindow.xaml");
    let bytes = match std::fs::read(&xaml_path) {
        Ok(b) => b,
        Err(err) => {
            warn!(
                "scoreboard: cannot read {} ({err}) — skipping",
                xaml_path.display()
            );
            return false;
        }
    };
    xaml.insert(SCOREBOARD_URI.to_string(), Arc::new(bytes));

    // The two sample fonts, read from the SDK at runtime (never vendored).
    // Family names embedded in the files: "Cheboygan" (Cheboyga.ttf) and
    // "PerryGothic" (PERRYGOT.TTF), matched by the XAML's FontFamily refs.
    let mut staged_any = false;
    for filename in ["Cheboyga.ttf", "PERRYGOT.TTF"] {
        let path = data.join("Fonts").join(filename);
        match std::fs::read(&path) {
            Ok(b) => {
                fonts.insert("Fonts", filename, Arc::new(b));
                staged_any = true;
            }
            Err(err) => warn!("scoreboard: font {} not read ({err})", path.display()),
        }
    }
    if !staged_any {
        warn!("scoreboard: no sample fonts staged — text may render font-free");
    }
    true
}

/// Spawn the scoreboard view entity wired with the Game `DataContext` VM, the
/// bindable player items, the team `ComboBox` items, and a DP watch on the
/// combo's `SelectedIndex` (proving `Game.SelectedTeam` reached a named element).
pub fn spawn_scoreboard(commands: &mut Commands) -> Entity {
    let mut game = NoesisVm::new(
        ViewModelDef::new(GAME_CLASS)
            .property("Name", PropType::String)
            .property("AllianceScore", PropType::Int32)
            .property("HordeScore", PropType::Int32)
            .property("ElapsedTime", PropType::Int32)
            .property("SelectedTeam", PropType::Int32),
    );
    game.set_string("Name", GAME_NAME);
    game.set_i32("AllianceScore", alliance_score());
    game.set_i32("HordeScore", horde_score());
    game.set_i32("ElapsedTime", ELAPSED_MINUTES);
    game.set_i32("SelectedTeam", SELECTED_TEAM);

    commands
        .spawn((
            Camera2d,
            NoesisCamera,
            NoesisView {
                xaml_uri: SCOREBOARD_URI.to_string(),
                size: UVec2::new(VIEW_W, VIEW_H),
                ppaa: true,
                // Gate scene build on the two sample fonts so the Cheboygan /
                // PerryGothic text renders rather than falling through invisibly.
                wait_for_fonts: vec!["Fonts".to_string()],
                wait_for_font_files: vec![
                    ("Fonts".to_string(), "Cheboyga.ttf".to_string()),
                    ("Fonts".to_string(), "PERRYGOT.TTF".to_string()),
                ],
                font_fallbacks: vec!["Fonts/#Cheboygan".to_string()],
                ..default()
            },
            game,
            // Players: ten bindable object items → the per-player DataTemplate.
            // VisibleTeam: the three team strings (SelectedIndex binds to
            // Game.SelectedTeam, so we don't drive selection here).
            NoesisItems::new()
                .with_objects(PLAYERS_NAME, PLAYER_CLASS, player_rows())
                .with(VISIBLE_TEAM_NAME, ["Overall", "Alliance", "Horde"]),
            NoesisDp::new().watch(VISIBLE_TEAM_NAME, "SelectedIndex", DpKind::I32),
        ))
        .id()
}

/// Shared app config both the windowed `main` and the headless test boot.
pub fn configure_scoreboard(app: &mut App) {
    app.add_plugins(NoesisPlugin::default())
        // The SDK sample's root is a `<Window>` (an App-framework type absent from
        // the core runtime we link); this registers a content-host stand-in so the
        // genuine XAML parses unmodified.
        .add_plugins(NoesisWindowCompatPlugin)
        .add_systems(
            Startup,
            |mut commands: Commands,
             mut xaml: ResMut<XamlRegistry>,
             mut fonts: ResMut<FontRegistry>| {
                if stage_assets(&mut xaml, &mut fonts) {
                    spawn_scoreboard(&mut commands);
                }
            },
        );
}

// ─────────────────────────────────────────────────────────────────────────────
// Windowed entry point (+ optional headless screenshot)
// ─────────────────────────────────────────────────────────────────────────────

/// Headless screenshot driver: when `NOESIS_VIEWER_EXIT_AFTER` is set, wait a few
/// frames, capture `NOESIS_SCREENSHOT` (default `scoreboard.png`), then exit.
#[derive(Resource)]
struct Headless {
    capture_at: u32,
    exit_at: u32,
    path: PathBuf,
    captured: bool,
}

fn main() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "noesis_bevy — Scoreboard".into(),
            resolution: (VIEW_W, VIEW_H).into(),
            ..default()
        }),
        ..default()
    }));
    configure_scoreboard(&mut app);

    if std::env::var_os("NOESIS_VIEWER_EXIT_AFTER").is_some() {
        let capture_at: u32 = std::env::var("NOESIS_SCREENSHOT_FRAMES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        let path = std::env::var_os("NOESIS_SCREENSHOT")
            .map_or_else(|| PathBuf::from("scoreboard.png"), PathBuf::from);
        app.insert_resource(Headless {
            capture_at,
            exit_at: capture_at + 30,
            path,
            captured: false,
        });
        app.add_systems(Update, tick_headless);
    }

    app.run();
}

#[allow(clippy::needless_pass_by_value)]
fn tick_headless(
    mut frame: Local<u32>,
    mut headless: ResMut<Headless>,
    mut commands: Commands,
    mut exit: MessageWriter<AppExit>,
) {
    *frame += 1;
    if *frame == headless.capture_at && !headless.captured {
        headless.captured = true;
        info!("scoreboard: capturing → {}", headless.path.display());
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(headless.path.clone()));
    }
    if *frame >= headless.exit_at {
        exit.write(AppExit::Success);
    }
}
