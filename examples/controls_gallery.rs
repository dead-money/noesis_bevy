//! Controls-gallery example — a Bevy-idiomatic port of the Noesis SDK's
//! `Data/Styles.xaml` sample (the "Styles" controls showcase: buttons, toggles
//! and a status line). It mirrors that sample's *intent* — an interactive panel
//! of themed controls you click and toggle — but is **self-contained**: every
//! control carries an inline `ControlTemplate` with `CommonStates` /
//! `CheckStates` visual states, so it renders and hit-tests with **no external
//! theme dictionary**. The only outside dependency is one font
//! (`Roboto-Regular.ttf` from `$NOESIS_SDK_DIR/Data/Fonts`) staged for the
//! labels; if the SDK isn't present the scene still runs, just font-free.
//!
//! Everything is driven through the **bridge components**, never raw FFI:
//!   * [`NoesisClickWatch`] — subscribes `BaseButton::Click` on the named
//!     buttons; clicks surface as [`NoesisClicked`] messages.
//!   * [`NoesisText`] — writes the `Status` `TextBlock`'s `Text` in reaction to
//!     those clicks (the demo's "logic").
//!   * [`NoesisDp`] — writes `LevelBar.Width` as the fire count grows and watches
//!     both `LevelBar.ActualWidth` and `Status.Text` read back as
//!     [`NoesisDpChanged`] (the latter confirms the `NoesisText` write landed —
//!     a cross-bridge round-trip).
//!
//! Run it windowed:
//!   `cargo run -p noesis_bevy --example controls_gallery`
//!
//! The headless smoke test (`tests/headless_controls_gallery.rs`) `#[path]`-
//! includes this file and boots [`configure_gallery`] under the headless
//! `ScheduleRunnerPlugin`, injects synthetic clicks, and asserts the bridge
//! read-backs (a [`NoesisClicked`], the `LevelBar` width, the `Status` text)
//! come back.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, FontRegistry, NoesisCamera, NoesisClickWatch, NoesisClicked, NoesisDp,
    NoesisPlugin, NoesisText, NoesisView, XamlRegistry,
};

/// Intermediate render-target size; also the view-pixel coordinate space the
/// headless test injects clicks into.
pub const VIEW_W: u32 = 220;
pub const VIEW_H: u32 = 180;

/// Hit-test centres (view pixels) of the three interactive controls, derived
/// from the `Canvas.Left`/`Top` + `Width`/`Height` in [`GALLERY_XAML`]. Exposed
/// so the smoke test clicks the exact same spots without re-deriving them.
pub const FIRE_CENTER: (i32, i32) = (57, 51);
pub const RESET_CENTER: (i32, i32) = (163, 51);
pub const TOGGLE_CENTER: (i32, i32) = (57, 91);

/// URI the gallery XAML is registered under in [`XamlRegistry`].
pub const GALLERY_URI: &str = "controls_gallery.xaml";

/// Self-contained controls gallery. Two `Button`s and a `ToggleButton` share
/// inline `ControlTemplate`s with `CommonStates` (Normal/MouseOver/Pressed) so
/// they react to the pointer; the toggle adds `CheckStates` so it latches green
/// when checked. No `<ResourceDictionary Source=.../>`, so it parses with only
/// the bytes below.
pub const GALLERY_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="220" Height="180" Background="#FF1B2733"
      TextElement.FontFamily="Fonts/#Roboto"
      TextElement.Foreground="#FFEAEAEA" TextElement.FontSize="13">
  <Grid.Resources>
    <ControlTemplate x:Key="GalleryButton" TargetType="ButtonBase">
      <Border x:Name="Bg" CornerRadius="4" Background="#FF35506B">
        <VisualStateManager.VisualStateGroups>
          <VisualStateGroup x:Name="CommonStates">
            <VisualState x:Name="Normal"/>
            <VisualState x:Name="MouseOver">
              <Storyboard>
                <ColorAnimation Storyboard.TargetName="Bg"
                  Storyboard.TargetProperty="(Border.Background).(SolidColorBrush.Color)"
                  To="#FF4A6E92" Duration="0"/>
              </Storyboard>
            </VisualState>
            <VisualState x:Name="Pressed">
              <Storyboard>
                <ColorAnimation Storyboard.TargetName="Bg"
                  Storyboard.TargetProperty="(Border.Background).(SolidColorBrush.Color)"
                  To="#FF6FA8DC" Duration="0"/>
              </Storyboard>
            </VisualState>
          </VisualStateGroup>
        </VisualStateManager.VisualStateGroups>
        <ContentPresenter HorizontalAlignment="Center" VerticalAlignment="Center"/>
      </Border>
    </ControlTemplate>

    <ControlTemplate x:Key="GalleryToggle" TargetType="ButtonBase">
      <Border x:Name="Bg" CornerRadius="4" Background="#FF3A3F46">
        <VisualStateManager.VisualStateGroups>
          <VisualStateGroup x:Name="CommonStates">
            <VisualState x:Name="Normal"/>
            <VisualState x:Name="MouseOver">
              <Storyboard>
                <ColorAnimation Storyboard.TargetName="Bg"
                  Storyboard.TargetProperty="(Border.Background).(SolidColorBrush.Color)"
                  To="#FF4A4F57" Duration="0"/>
              </Storyboard>
            </VisualState>
          </VisualStateGroup>
          <VisualStateGroup x:Name="CheckStates">
            <VisualState x:Name="Unchecked"/>
            <VisualState x:Name="Checked">
              <Storyboard>
                <ColorAnimation Storyboard.TargetName="Bg"
                  Storyboard.TargetProperty="(Border.Background).(SolidColorBrush.Color)"
                  To="#FF2E7D32" Duration="0"/>
              </Storyboard>
            </VisualState>
          </VisualStateGroup>
        </VisualStateManager.VisualStateGroups>
        <ContentPresenter HorizontalAlignment="Center" VerticalAlignment="Center"/>
      </Border>
    </ControlTemplate>
  </Grid.Resources>

  <Canvas>
    <TextBlock x:Name="Title" Canvas.Left="12" Canvas.Top="8" FontSize="16"
               Text="Controls Gallery"/>
    <Button x:Name="FireButton" Template="{StaticResource GalleryButton}" Content="Fire"
            Canvas.Left="12" Canvas.Top="36" Width="90" Height="30"/>
    <Button x:Name="ResetButton" Template="{StaticResource GalleryButton}" Content="Reset"
            Canvas.Left="118" Canvas.Top="36" Width="90" Height="30"/>
    <ToggleButton x:Name="PowerToggle" Template="{StaticResource GalleryToggle}" Content="Power"
            Canvas.Left="12" Canvas.Top="76" Width="90" Height="30"/>
    <TextBlock x:Name="Status" Canvas.Left="12" Canvas.Top="120" Text="Ready"/>
    <Border x:Name="LevelBar" Canvas.Left="12" Canvas.Top="148" Height="12" Width="10"
            CornerRadius="2" Background="#FF6FA8DC"/>
  </Canvas>
</Grid>"##;

/// The view entity that hosts the gallery scene + bridges. Inserted by the
/// startup spawn so reactive systems (and the smoke test) can find it.
#[derive(Resource, Clone, Copy)]
pub struct GalleryView(pub Entity);

/// How many times `FireButton` has been clicked — surfaced into the `Status`
/// line, so the demo visibly "does something" through the bridges.
#[derive(Resource, Default)]
pub struct FireCount(pub u32);

/// Stage the gallery XAML + (best-effort) the Roboto label font, then spawn the
/// view entity wired with the three bridge components. Returns the gating
/// applied so callers know whether labels will render.
fn spawn_gallery(commands: &mut Commands, xaml: &mut XamlRegistry, fonts: &mut FontRegistry) {
    xaml.insert(
        GALLERY_URI.to_string(),
        Arc::new(GALLERY_XAML.as_bytes().to_vec()),
    );

    // Best-effort: stage one font from the SDK so the labels render. If the SDK
    // isn't reachable we skip the font gate and run font-free (controls still
    // render + react; only the glyphs are missing).
    let (wait_for_fonts, wait_for_font_files) = match stage_roboto(fonts) {
        Some(filename) => (
            vec!["Fonts".to_string()],
            vec![("Fonts".to_string(), filename)],
        ),
        None => {
            warn!("controls_gallery: Roboto not staged — running font-free (labels won't render)");
            (Vec::new(), Vec::new())
        }
    };

    let view = commands
        .spawn((
            Camera2d,
            NoesisCamera,
            NoesisView {
                xaml_uri: GALLERY_URI.to_string(),
                size: UVec2::new(VIEW_W, VIEW_H),
                wait_for_fonts,
                wait_for_font_files,
                ..default()
            },
            // Bridge 1: surface BaseButton::Click on these named controls.
            NoesisClickWatch::new(["FireButton", "ResetButton", "PowerToggle"]),
            // Bridge 2: we own the Status text — seed it and rewrite it on clicks.
            // (Only the write side here: a single NoesisText component suppresses
            // read-backs of its *own* writes, so the Status text is observed
            // instead through the DP bridge below — a cross-bridge round-trip.)
            NoesisText::new().with("Status", "Ready"),
            // Bridge 3: drive + observe plain dependency properties. We write
            // `LevelBar.Width` on Fire clicks and watch `ActualWidth` read back;
            // we also watch `Status.Text` to see the NoesisText writes land.
            // (The toggle's `IsChecked` is `Nullable<bool>` and isn't reachable
            // through `DpKind::Bool` — that path needs a ViewModel.)
            NoesisDp::new()
                .watch("LevelBar", "ActualWidth", DpKind::F32)
                .watch("Status", "Text", DpKind::Str),
        ))
        .id();
    commands.insert_resource(GalleryView(view));
}

/// Read one font off `$NOESIS_SDK_DIR/Data/Fonts/Roboto-Regular.ttf` into the
/// registry under `Fonts/`. Returns the filename on success.
fn stage_roboto(fonts: &mut FontRegistry) -> Option<String> {
    let sdk = std::env::var("NOESIS_SDK_DIR").ok()?;
    let filename = "Roboto-Regular.ttf";
    let path = PathBuf::from(sdk).join("Data/Fonts").join(filename);
    let bytes = std::fs::read(&path).ok()?;
    fonts.insert("Fonts", filename, Arc::new(bytes));
    Some(filename.to_string())
}

/// Demo "logic", expressed entirely through bridge components: when a button's
/// `Click` surfaces as a [`NoesisClicked`], rewrite the `Status` `TextBlock`
/// via [`NoesisText`] and grow the `LevelBar` via [`NoesisDp`]. This is the
/// idiomatic round-trip — Noesis raises the event, Bevy reacts, Bevy writes
/// back into the scene.
fn react_to_clicks(
    mut clicks: MessageReader<NoesisClicked>,
    mut fire: ResMut<FireCount>,
    mut q: Query<(&mut NoesisText, &mut NoesisDp)>,
) {
    for ev in clicks.read() {
        let Ok((mut text, mut dp)) = q.get_mut(ev.view) else {
            continue;
        };
        let status = match ev.name.as_str() {
            "FireButton" => {
                fire.0 += 1;
                format!("Fired x{}", fire.0)
            }
            "ResetButton" => {
                fire.0 = 0;
                "Reset".to_string()
            }
            "PowerToggle" => "Power toggled".to_string(),
            other => format!("Clicked {other}"),
        };
        // Mutating each component re-runs its apply: Status.Text + LevelBar.Width.
        text.set.insert("Status".to_string(), status);
        let width = 10.0 + 20.0 * fire.0 as f32;
        dp.set.insert(
            ("LevelBar".to_string(), "Width".to_string()),
            DpValue::F32(width),
        );
    }
}

/// Add the gallery to an existing `App` — assumes Bevy's plugins are already
/// installed by the caller (windowed in [`main`], headless in the smoke test).
/// This is the single shared "app config" both entry points boot.
pub fn configure_gallery(app: &mut App) {
    app.add_plugins(NoesisPlugin::default())
        .init_resource::<FireCount>()
        .add_systems(
            Startup,
            |mut commands: Commands,
             mut xaml: ResMut<XamlRegistry>,
             mut fonts: ResMut<FontRegistry>| {
                spawn_gallery(&mut commands, &mut xaml, &mut fonts);
            },
        )
        .add_systems(Update, react_to_clicks);
}

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "Noesis Controls Gallery".to_string(),
            resolution: (VIEW_W * 2, VIEW_H * 2).into(),
            ..default()
        }),
        ..default()
    }));
    configure_gallery(&mut app);
    app.run();
}
