//! **The ECS-UI example**: all three primitives of the entity-driven Noesis API,
//! written the way a Bevy user would write them. The end-to-end proof that
//! UI is *just ECS*: panels are entities, list rows are entities, and UI events are
//! Bevy observers.
//!
//! ```sh
//! cargo run -p noesis_bevy --example ecs_ui
//! # nicer visuals when the SDK theme is available (renders real control chrome):
//! NOESIS_ECS_UI_THEME=DarkBlue cargo run -p noesis_bevy --example ecs_ui
//! # headless screenshot:
//! NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=ecs_ui.png \
//!   cargo run -p noesis_bevy --example ecs_ui
//! ```
//!
//! # The three primitives, end to end
//!
//! **Primitive 1: panel = entity.** [`spawn_player_hud`] spawns a [`UiPanel`]
//! entity carrying `Health` + `Score` components. Those components *are* the
//! panel's `DataContext`: the `hud.xaml` fragment binds `{Binding Health}` /
//! `{Binding Score}`, and an ordinary system ([`regen_and_decay`]) mutates the
//! components with a normal `Query<&mut Health, With<UiPanel>>`; change detection
//! re-snapshots them into the live bindings. Two HUDs are spawned (P1, P2) into two
//! host slots to show **multi-instance**: each binds independently.
//!
//! **Primitive 2: list = query.** The inventory rows *are* entities: each is an
//! `Item` component plus a [`ListedIn`] membership. A [`UiList`] on the view binds
//! the reconciled `ObservableCollection` to a `ListBox`. Spawning an entity
//! adds a row; despawning removes it; mutating one `Item` updates *only* that row
//! in place (no flicker, no Reset); flipping [`UiList::sorted_by`] reorders via
//! `Move` ops so a selected row keeps its selection.
//!
//! **Primitive 3: events = observers.** UI events arrive as Bevy `EntityEvent`s.
//! A named host `Button` fires a [`UiClicked`] whose `event_target()` is the panel
//! entity it was wired to (see [`ClickWatchEntry::target`]); a click on a list row
//! fires a [`UiClicked`] whose `event_target()` is *that row's entity*, recovered
//! with no `x:Name`, straight off the row's data. The observers below read those
//! targets with ordinary `Query`s.
//!
//! Selection is modeled with the [`Selected`] marker component: the row-click
//! observer sets it on the clicked row, and [`report_selection`] reads it back with
//! `Query<&Item, With<Selected>>`. Setting [`Selected`] also drives the bound
//! control's current item (currency *is* selection), so selection survives reorders.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use noesis_bevy::{
    ClickWatchEntry, FontRegistry, ListedIn, NoesisCamera, NoesisClickWatch, NoesisListAppExt,
    NoesisListSelection, NoesisPanelAppExt, NoesisPlugin, NoesisView, NoesisViewModel, Selected,
    UiClicked, UiList, UiPanel, XamlRegistry,
};

// ─────────────────────────────────────────────────────────────────────────────
// Bound data: plain components. Each `#[derive(NoesisViewModel)]` makes the
// component's fields bindable by name (`{Binding Health}`, `{Binding name}`, …).
// ─────────────────────────────────────────────────────────────────────────────

/// A player HUD's hit points. Bound on a [`UiPanel`] entity as `{Binding Health}`.
#[derive(Component, NoesisViewModel, Clone, Copy, Debug)]
pub struct Health(pub f32);

/// A player HUD's score. Bound on the same [`UiPanel`] entity as `{Binding Score}`.
#[derive(Component, NoesisViewModel, Clone, Copy, Debug)]
pub struct Score(pub i32);

/// One inventory row. The fields bind in the item template as `{Binding name}` /
/// `{Binding qty}`. Field order fixes the property indices, so `qty` is index 1,
/// the [`UiList::sorted_by`] key used below.
#[derive(Component, NoesisViewModel, Clone, Debug)]
pub struct Item {
    /// Display name, `{Binding name}`.
    pub name: String,
    /// Quantity, `{Binding qty}`; also the sort key (property index 1).
    pub qty: i32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Scene identifiers (XAML URIs + element x:Names shared with the integration test)
// ─────────────────────────────────────────────────────────────────────────────

/// `XamlRegistry` key for the host view scene.
pub const HOST_URI: &str = "ecs_ui/host.xaml";
/// `XamlRegistry` key for the per-panel HUD fragment.
pub const HUD_URI: &str = "ecs_ui/hud.xaml";

/// Fragment-scope `x:Name` of the HUD's `{Binding Health}` value `TextBlock`.
pub const HUD_HEALTH_VALUE: &str = "HealthValue";
/// Fragment-scope `x:Name` of the HUD's `{Binding Score}` value `TextBlock`.
pub const HUD_SCORE_VALUE: &str = "ScoreValue";

/// `x:Name` of the left HUD mount slot (player 1).
pub const HUD1_SLOT: &str = "Hud1";
/// `x:Name` of the right HUD mount slot (player 2).
pub const HUD2_SLOT: &str = "Hud2";

/// `x:Name` of the inventory `ListBox` (a `Selector`, so row selection is real).
pub const INVENTORY_NAME: &str = "Inventory";

/// `x:Name` of the "heal player 1" host button.
pub const HEAL_P1_BTN: &str = "HealP1";
/// `x:Name` of the "heal player 2" host button.
pub const HEAL_P2_BTN: &str = "HealP2";
/// `x:Name` of the "add item" host button.
pub const ADD_ITEM_BTN: &str = "AddItem";

/// Host view scene: two HUD mount slots, three named buttons, and the inventory
/// `ListBox`. The HUD slots are empty `StackPanel`s that [`UiPanel`] fragments
/// mount into; the buttons are watched by [`NoesisClickWatch`]; the `ListBox` is
/// bound by [`UiList`]. The `Button`s and the `ListBox` are skinned by the SDK
/// theme (loaded by default; see `main`); the `Border`/`TextBlock` chrome renders
/// either way.
pub const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="640" Height="480" Background="#FF101418">
  <Grid.RowDefinitions>
    <RowDefinition Height="Auto"/>
    <RowDefinition Height="Auto"/>
    <RowDefinition Height="*"/>
  </Grid.RowDefinitions>

  <!-- Primitive 1: two HUD mount slots, side by side (multi-instance). -->
  <StackPanel Grid.Row="0" Orientation="Horizontal" Margin="12">
    <Border Background="#FF1B2330" CornerRadius="6" Margin="0,0,12,0" Padding="10">
      <StackPanel>
        <TextBlock Text="Player 1" Foreground="#FF8FB8FF" FontSize="14"/>
        <StackPanel x:Name="Hud1"/>
      </StackPanel>
    </Border>
    <Border Background="#FF1B2330" CornerRadius="6" Padding="10">
      <StackPanel>
        <TextBlock Text="Player 2" Foreground="#FFFF9F8F" FontSize="14"/>
        <StackPanel x:Name="Hud2"/>
      </StackPanel>
    </Border>
  </StackPanel>

  <!-- Primitive 3: named buttons. Their UiClicked is re-targeted at panel entities
       (heal) or handled on the view (add item). -->
  <StackPanel Grid.Row="1" Orientation="Horizontal" Margin="12,0,12,8">
    <Button x:Name="HealP1" Content="Heal P1" Margin="0,0,8,0" Padding="10,4"/>
    <Button x:Name="HealP2" Content="Heal P2" Margin="0,0,8,0" Padding="10,4"/>
    <Button x:Name="AddItem" Content="Add Item" Padding="10,4"/>
  </StackPanel>

  <!-- Primitive 2: the inventory. Rows ARE entities; this ListBox binds the
       reconciled collection. A ListBox (a Selector) makes selection real: clicking
       a row marks its entity `Selected` (read back via With<Selected>), and the
       row's MouseLeftButtonUp still bubbles out as a per-row UiClicked. -->
  <Border Grid.Row="2" Background="#FF161B22" Margin="12" CornerRadius="6">
    <ListBox x:Name="Inventory" Background="Transparent" BorderThickness="0">
      <!-- The SDK theme (loaded by default; see `main`) skins the ListBox and its
           ListBoxItems, including the selection highlight. A bare ListBox without a
           theme renders as magenta "no template" placeholders, which is why the
           example defaults to a theme rather than hand-rolling control chrome. -->
      <ListBox.ItemTemplate>
        <DataTemplate>
          <StackPanel Orientation="Horizontal">
            <TextBlock Text="{Binding name}" Foreground="#FFE6EDF3" Width="160"/>
            <TextBlock Text="x" Foreground="#FF7D8590"/>
            <TextBlock Text="{Binding qty}" Foreground="#FFE6EDF3" Margin="4,0,0,0"/>
          </StackPanel>
        </DataTemplate>
      </ListBox.ItemTemplate>
    </ListBox>
  </Border>
</Grid>"##;

/// Per-panel HUD fragment: its `DataContext` is the [`UiPanel`] entity's aggregated
/// `Health` + `Score` components. Mounted once per panel into a host slot, each
/// copy keeps its own namescope so the two HUDs never cross-bind.
pub const HUD_XAML: &str = r##"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <StackPanel Orientation="Horizontal">
    <TextBlock Text="HP " Foreground="#FF7D8590"/>
    <TextBlock x:Name="HealthValue" Text="{Binding Health, StringFormat=F0}" Foreground="#FF6BE675"/>
  </StackPanel>
  <StackPanel Orientation="Horizontal">
    <TextBlock Text="Score " Foreground="#FF7D8590"/>
    <TextBlock x:Name="ScoreValue" Text="{Binding Score}" Foreground="#FFE6EDF3"/>
  </StackPanel>
</StackPanel>"##;

// ─────────────────────────────────────────────────────────────────────────────
// Marker components, so observers can tell the two panels apart.
// ─────────────────────────────────────────────────────────────────────────────

/// Marks the player-1 HUD panel entity (the [`HEAL_P1_BTN`] heals this one).
#[derive(Component)]
pub struct PlayerOne;

/// Marks the player-2 HUD panel entity (the [`HEAL_P2_BTN`] heals this one).
#[derive(Component)]
pub struct PlayerTwo;

// ─────────────────────────────────────────────────────────────────────────────
// Spawning
// ─────────────────────────────────────────────────────────────────────────────

/// Register the example's XAML strings under their URIs. Shared with the headless
/// integration test so both load the exact same scenes.
pub fn register_xaml(reg: &mut XamlRegistry) {
    reg.insert(
        HOST_URI.to_string(),
        Arc::new(HOST_XAML.as_bytes().to_vec()),
    );
    reg.insert(HUD_URI.to_string(), Arc::new(HUD_XAML.as_bytes().to_vec()));
}

/// Spawn the host [`NoesisView`] (the camera entity that owns the scene), wired
/// with the inventory [`UiList`] and a [`NoesisClickWatch`] that re-targets each
/// host button's [`UiClicked`] at the entity that should handle it.
///
/// Returns the view entity (a list entity is spawned inside; rows reference *it*
/// via [`ListedIn`]).
pub fn spawn_view(commands: &mut Commands, application_resources: Vec<String>) -> Entity {
    let view = commands
        .spawn((
            Camera2d,
            NoesisCamera,
            NoesisView {
                xaml_uri: HOST_URI.to_string(),
                size: UVec2::new(640, 480),
                application_resources,
                // Noesis renders text invisibly if no font is registered before the
                // scene builds; gate the build on PT Root UI as the process-wide
                // fallback so the theme-less `cargo run` shows any text at all.
                wait_for_fonts: vec!["Fonts".to_string()],
                wait_for_font_files: vec![(
                    "Fonts".to_string(),
                    "PT Root UI_Regular.otf".to_string(),
                )],
                font_fallbacks: vec!["Fonts/#PT Root UI".to_string()],
                ..default()
            },
        ))
        .id();

    // Primitive 2: the list is its own entity naming its control + owner view. Rows
    // ordered by qty (property index 1), ascending; row-object class auto-generated.
    let list = commands
        .spawn(UiList::new(view, INVENTORY_NAME).sorted_by(1, false))
        .id();

    // Primitive 1: two independent HUD panels mounted into the two host slots.
    let p1 = spawn_player_hud(commands, view, HUD1_SLOT, Health(100.0), Score(0));
    commands.entity(p1).insert(PlayerOne);
    let p2 = spawn_player_hud(commands, view, HUD2_SLOT, Health(100.0), Score(0));
    commands.entity(p2).insert(PlayerTwo);

    // Primitive 3 (named): watch the three host buttons. "Heal" clicks are
    // re-targeted at the matching panel entity, so the heal observer recovers it
    // straight from `On::event_target()`; "Add Item" keeps the default view target.
    commands
        .entity(view)
        .insert(NoesisClickWatch::from_entries([
            ClickWatchEntry::new(HEAL_P1_BTN).target(p1),
            ClickWatchEntry::new(HEAL_P2_BTN).target(p2),
            ClickWatchEntry::new(ADD_ITEM_BTN),
        ]));

    // Seed a few inventory rows. Each is just an entity in the list.
    for (name, qty) in [("Potion", 3), ("Sword", 1), ("Shield", 2), ("Gold", 50)] {
        commands.spawn((
            Item {
                name: name.to_string(),
                qty,
            },
            ListedIn(list),
        ));
    }

    view
}

/// Spawn one HUD panel entity: a [`UiPanel`] that loads [`HUD_URI`] and mounts into
/// the host slot `slot`, with `health` + `score` as its bound `DataContext`. Two of
/// these (P1, P2) prove multi-instance isolation.
pub fn spawn_player_hud(
    commands: &mut Commands,
    view: Entity,
    slot: &str,
    health: Health,
    score: Score,
) -> Entity {
    commands
        .spawn((UiPanel::new(HUD_URI).mount_into(view, slot), health, score))
        .id()
}

// ─────────────────────────────────────────────────────────────────────────────
// Ordinary systems drive the UI (Primitive 1); no Noesis types in sight.
// ─────────────────────────────────────────────────────────────────────────────

/// Slowly decay both HUDs' health and tick their score, as a plain ECS system over
/// the panel entities. Change detection pushes each mutation into the live
/// `{Binding Health}` / `{Binding Score}`. Demonstrates that the UI is driven by
/// the same systems that drive the game.
fn regen_and_decay(time: Res<Time>, mut huds: Query<(&mut Health, &mut Score), With<UiPanel>>) {
    for (mut health, mut score) in &mut huds {
        let next = (health.0 - 6.0 * time.delta_secs()).max(0.0);
        // Only write when it actually changed, so we don't trip change detection
        // (and a binding re-push) on a frame where the rounded value is the same.
        if (next.floor() - health.0.floor()).abs() >= 1.0 {
            health.0 = next;
            score.0 += 1;
        } else {
            health.0 = next;
        }
    }
}

/// React to the live inventory: bump one row's quantity on a timer (an in-place
/// row update, only that row's container changes), and despawn a row when it hits
/// zero (a `Remove` op). Pure ECS over the row entities.
fn churn_inventory(
    time: Res<Time>,
    mut elapsed: Local<f32>,
    mut commands: Commands,
    mut items: Query<(Entity, &mut Item)>,
) {
    *elapsed += time.delta_secs();
    if *elapsed < 1.5 {
        return;
    }
    *elapsed = 0.0;
    for (entity, mut item) in &mut items {
        if item.name == "Potion" {
            item.qty -= 1; // in-place Update; no Reset, selection/scroll survive.
            if item.qty <= 0 {
                commands.entity(entity).despawn(); // Remove op.
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Observers: UI events as Bevy `EntityEvent`s (Primitive 3).
// ─────────────────────────────────────────────────────────────────────────────

/// Heal observer: a `UiClicked` from a "Heal" button arrives targeting the *panel
/// entity* it was wired to (via [`ClickWatchEntry::target`]). We recover that
/// entity from [`On::event_target`] and mutate its `Health` with an ordinary query,
/// no element names, no per-button branching beyond the add-item case.
fn on_button_click(
    on: On<UiClicked>,
    mut huds: Query<&mut Health, With<UiPanel>>,
    mut commands: Commands,
    list: Single<Entity, With<UiList>>,
) {
    if on.name == ADD_ITEM_BTN {
        // The add-item button kept the default (view) target; spawn a new row.
        commands.spawn((
            Item {
                name: "Elixir".to_string(),
                qty: 9,
            },
            ListedIn(*list),
        ));
        info!("add-item: spawned a new inventory row entity");
        return;
    }
    // A heal button: event_target() is the panel entity to heal.
    if let Ok(mut health) = huds.get_mut(on.event_target()) {
        health.0 = (health.0 + 25.0).min(100.0);
        info!(
            "heal: button {:?} healed panel {:?} -> {:.0} hp",
            on.name,
            on.event_target(),
            health.0
        );
    }
}

/// Row-click observer: a click on an inventory row arrives targeting *that row's
/// entity*, recovered with no `x:Name`, from the row's data. We make the click
/// select the row: clear any prior [`Selected`] and mark this one. Setting
/// [`Selected`] also drives the bound control's current item (currency is
/// selection), so the choice survives later reorders.
pub fn on_row_click(
    on: On<UiClicked>,
    items: Query<&Item>,
    selected: Query<Entity, With<Selected>>,
    mut commands: Commands,
) {
    let row = on.event_target();
    // Only react to row entities (carry an `Item`); ignore the named-button twins,
    // which target panel/view entities and are handled by `on_button_click`.
    let Ok(item) = items.get(row) else {
        return;
    };
    for prev in &selected {
        commands.entity(prev).remove::<Selected>();
    }
    commands.entity(row).insert(Selected);
    info!("row click: selected {:?} ({})", row, item.name);
}

/// Read selection back out with an ordinary `Query<&Item, With<Selected>>`, the
/// other half of the round-trip. Logs only when the selection changes.
fn report_selection(
    selected: Query<(Entity, &Item), Added<Selected>>,
    mut sel_msgs: MessageReader<NoesisListSelection>,
) {
    for (entity, item) in &selected {
        info!(
            "selection now: {:?} ({}) — read via With<Selected>",
            entity, item.name
        );
    }
    // The bridge also emits a message on UI-driven selection changes; drained here
    // so a real app could react to it too.
    for _ in sel_msgs.read() {}
}

// ─────────────────────────────────────────────────────────────────────────────
// App wiring
// ─────────────────────────────────────────────────────────────────────────────

/// Register the panel field types, the list row type, and all systems/observers.
/// Shared by `main` and the headless integration test so they exercise the exact
/// same wiring.
pub fn configure(app: &mut App) {
    app.add_plugins(NoesisPlugin::default());

    // Register the bound types: panel fields (Primitive 1) and the row type
    // (Primitive 2).
    app.add_noesis_panel_field::<Health>()
        .add_noesis_panel_field::<Score>()
        .add_noesis_list::<Item>();

    // Ordinary systems + observers. Observers are global; they self-filter by what
    // the trigger target carries (a panel `Health`, or a row `Item`).
    app.add_systems(Update, (regen_and_decay, churn_inventory, report_selection));
    app.add_observer(on_button_click);
    app.add_observer(on_row_click);
}

// ─────────────────────────────────────────────────────────────────────────────
// Windowed entry point (+ optional headless screenshot / optional SDK theme)
// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }

    // Default to the DarkBlue SDK theme so the Buttons and the inventory ListBox
    // render skinned (and the ListBox highlights its selected row). Override with a
    // different NOESIS_ECS_UI_THEME, or set it empty to run theme-less (magenta
    // placeholder chrome).
    let theme =
        Some(std::env::var("NOESIS_ECS_UI_THEME").unwrap_or_else(|_| "DarkBlue".to_string()));

    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "noesis_bevy — ECS UI".into(),
            resolution: (640u32, 480u32).into(),
            ..default()
        }),
        ..default()
    }));
    configure(&mut app);

    let theme_for_startup = theme.clone();
    app.add_systems(
        Startup,
        move |mut commands: Commands,
              mut xaml: ResMut<XamlRegistry>,
              mut fonts: ResMut<FontRegistry>| {
            register_xaml(&mut xaml);
            // Always stage the PT Root UI fallback font: text is invisible without
            // a registered font, independent of the (optional) theme below.
            stage_fonts(&mut fonts);
            // Optional SDK theme for real control chrome; degrades to placeholder
            // chrome (and still works) when unset or the SDK isn't reachable.
            let app_resources = theme_for_startup
                .as_deref()
                .map(|t| stage_theme(t, &mut xaml, &mut fonts))
                .unwrap_or_default();
            spawn_view(&mut commands, app_resources);
        },
    );

    if std::env::var_os("NOESIS_VIEWER_EXIT_AFTER").is_some() {
        let capture_at: u32 = std::env::var("NOESIS_SCREENSHOT_FRAMES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);
        let path = std::env::var_os("NOESIS_SCREENSHOT")
            .map_or_else(|| PathBuf::from("ecs_ui.png"), PathBuf::from);
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

/// Stage the SDK's PT Root UI font under the `Fonts` folder so text renders even
/// with no theme. The SDK is present at runtime (the runtime crate links it), so
/// this normally succeeds; it warns rather than fails if a face is missing.
fn stage_fonts(fonts: &mut FontRegistry) {
    let Some(sdk) = std::env::var_os("NOESIS_SDK_DIR") else {
        warn!("NOESIS_SDK_DIR unset — no fallback font staged; text may be invisible");
        return;
    };
    let dir = PathBuf::from(sdk).join("Src/Packages/App/Theme/Data/Theme/Fonts");
    let mut any = false;
    for file in ["PT Root UI_Regular.otf", "PT Root UI_Bold.otf"] {
        let path = dir.join(file);
        match std::fs::read(&path) {
            Ok(bytes) => {
                fonts.insert("Fonts", file, Arc::new(bytes));
                any = true;
            }
            Err(err) => warn!("ecs_ui: font {} not read ({err})", path.display()),
        }
    }
    if !any {
        warn!("ecs_ui: no fallback font staged — text may render invisibly");
    }
}

/// Stage the SDK theme `theme` (its XAML chain + PT Root UI fonts) into the
/// registries and return the `application_resources` chain to hand the view. A
/// no-op (returns empty) when `$NOESIS_SDK_DIR` is unset or the theme is missing.
/// The example then renders with placeholder control chrome.
fn stage_theme(theme: &str, xaml: &mut XamlRegistry, fonts: &mut FontRegistry) -> Vec<String> {
    let Some(sdk) = std::env::var_os("NOESIS_SDK_DIR") else {
        warn!("NOESIS_ECS_UI_THEME set but NOESIS_SDK_DIR unset — skipping theme");
        return Vec::new();
    };
    let root = PathBuf::from(sdk).join("Src/Packages/App/Theme/Data/Theme");
    let want = format!("NoesisTheme.{theme}.xaml");
    if !root.join(&want).exists() {
        warn!("theme {want} not found under {} — skipping", root.display());
        return Vec::new();
    }
    if let Ok(dir) = std::fs::read_dir(&root) {
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "xaml")
                && let (Some(name), Ok(bytes)) = (
                    path.file_name().and_then(|n| n.to_str()),
                    std::fs::read(&path),
                )
            {
                xaml.insert(name.to_string(), Arc::new(bytes));
            }
        }
    }
    if let Ok(dir) = std::fs::read_dir(root.join("Fonts")) {
        for entry in dir.flatten() {
            let path = entry.path();
            if let (Some(name), Ok(bytes)) = (
                path.file_name().and_then(|n| n.to_str()),
                std::fs::read(&path),
            ) {
                fonts.insert("Fonts", name, Arc::new(bytes));
            }
        }
    }
    vec![want]
}

/// Headless screenshot driver (mirrors the scoreboard example): wait, capture
/// `NOESIS_SCREENSHOT`, then exit.
#[derive(Resource)]
struct Headless {
    capture_at: u32,
    exit_at: u32,
    path: PathBuf,
    captured: bool,
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
        info!("ecs_ui: capturing → {}", headless.path.display());
        commands
            .spawn(Screenshot::primary_window())
            .observe(save_to_disk(headless.path.clone()));
    }
    if *frame >= headless.exit_at {
        exit.write(AppExit::Success);
    }
}
