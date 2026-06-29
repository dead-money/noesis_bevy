//! Commands example — a Bevy-idiomatic port of the Noesis SDK's **`Commands`**
//! sample (`$NOESIS_SDK_DIR/Src/Packages/Samples/Commands`, the `ICommand`
//! tutorial).
//!
//! The SDK original binds a `Button`'s `Command="{Binding SayHelloCommand}"`
//! (with a `CommandParameter`) to a `DelegateCommand` on its view model, which
//! writes an `Output` string the UI shows. This port keeps that flow but drives
//! it entirely through the crate's **bridge components**, never raw FFI:
//!
//!   * [`NoesisCommands`] declares three named commands — `SayHello`, `Goodbye`,
//!     `Locked` — as the view-root `DataContext`. The XAML `Button`s bind
//!     `Command="{Binding SayHello}"` etc. and pass a `CommandParameter` literal.
//!     This is the command bridge: UI → Rust.
//!   * [`NoesisCommands::set_enabled`] gates the `Locked` command off at startup.
//!     Its `CanExecute` then reports `false`, so the bound `Button` enters its
//!     `Disabled` visual state and clicking it invokes nothing — the SDK
//!     `DelegateCommand(canExecute, execute)` `CanExecute` path.
//!   * [`NoesisText`] writes the `Output` `TextBlock` in reaction to a
//!     [`NoesisCommandInvoked`] (the demo's "logic"), mirroring the sample's
//!     `Output = "Hello, {0} ({1})"`.
//!   * [`NoesisDp`] watches `Output.Text` read back as [`NoesisDpChanged`], so
//!     the reaction is observable as a cross-bridge round-trip.
//!
//! Self-contained: every `Button` carries an inline `ControlTemplate` with
//! `CommonStates` (Normal/MouseOver/Pressed/Disabled) so it renders and
//! hit-tests with **no external theme dictionary**. The only outside dependency
//! is one font (`Roboto-Regular.ttf` from `$NOESIS_SDK_DIR/Data/Fonts`) staged
//! for the labels; absent the SDK the scene still runs, just font-free.
//!
//! Run it windowed:
//!   `cargo run -p dm_noesis_bevy --example commands`
//!
//! The headless smoke test (`tests/headless_example_commands.rs`) `#[path]`-
//! includes this file, boots [`configure_commands`] under the headless
//! `ScheduleRunnerPlugin`, injects synthetic clicks, and asserts the command
//! round-trip (a [`NoesisCommandInvoked`] with name + view + decoded parameter)
//! plus that the disabled command does NOT fire.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use dm_noesis_bevy::{
    CommandsDef, DpKind, FontRegistry, NoesisCamera, NoesisCommandInvoked, NoesisCommands,
    NoesisDp, NoesisPlugin, NoesisText, NoesisView, XamlRegistry,
};

/// Intermediate render-target size; also the view-pixel coordinate space the
/// headless test injects clicks into.
pub const VIEW_W: u32 = 320;
pub const VIEW_H: u32 = 220;

/// Hit-test centres (view pixels) of the three command buttons, derived from the
/// `Canvas.Left`/`Top` + `Width`/`Height` in [`COMMANDS_XAML`]. Exposed so the
/// smoke test clicks the exact same spots without re-deriving them.
pub const HELLO_CENTER: (i32, i32) = (82, 61);
pub const BYE_CENTER: (i32, i32) = (236, 61);
pub const LOCK_CENTER: (i32, i32) = (82, 105);

/// URI the commands XAML is registered under in [`XamlRegistry`].
pub const COMMANDS_URI: &str = "commands.xaml";

/// Globally-unique Noesis class name for the command host (class registration is
/// keyed by name).
pub const COMMANDS_CLASS: &str = "Example.Commands.Host";

/// Command parameters the two enabled buttons pass to their commands. The
/// reaction stringifies these into the `Output` line, mirroring the SDK's
/// `"Hello, {0} ({1})"`.
pub const HELLO_PARAM: &str = "World";
pub const BYE_PARAM: &str = "Moon";

/// Self-contained command panel. Three `Button`s share an inline
/// `ControlTemplate` with `CommonStates` (Normal/MouseOver/Pressed/Disabled) so
/// they react to the pointer and grey out when their command's `CanExecute` is
/// `false`. Each binds `Command="{Binding <name>}"` against the [`NoesisCommands`]
/// host attached as the view-root `DataContext`. No `<ResourceDictionary
/// Source=.../>`, so it parses with only the bytes below.
pub const COMMANDS_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="320" Height="220" Background="#FF14202B"
      TextElement.FontFamily="Fonts/#Roboto"
      TextElement.Foreground="#FFEAEAEA" TextElement.FontSize="14">
  <Grid.Resources>
    <ControlTemplate x:Key="CmdButton" TargetType="ButtonBase">
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
            <VisualState x:Name="Disabled">
              <Storyboard>
                <DoubleAnimation Storyboard.TargetName="Bg"
                  Storyboard.TargetProperty="Opacity" To="0.4" Duration="0"/>
              </Storyboard>
            </VisualState>
          </VisualStateGroup>
        </VisualStateManager.VisualStateGroups>
        <ContentPresenter HorizontalAlignment="Center" VerticalAlignment="Center"/>
      </Border>
    </ControlTemplate>
  </Grid.Resources>

  <Canvas>
    <TextBlock Canvas.Left="12" Canvas.Top="10" FontSize="18" Text="Commands"/>
    <Button x:Name="HelloButton" Template="{StaticResource CmdButton}" Content="Say Hello"
            Command="{Binding SayHello}" CommandParameter="World"
            Canvas.Left="12" Canvas.Top="44" Width="140" Height="34"/>
    <Button x:Name="ByeButton" Template="{StaticResource CmdButton}" Content="Goodbye"
            Command="{Binding Goodbye}" CommandParameter="Moon"
            Canvas.Left="166" Canvas.Top="44" Width="140" Height="34"/>
    <Button x:Name="LockButton" Template="{StaticResource CmdButton}" Content="Locked"
            Command="{Binding Locked}"
            Canvas.Left="12" Canvas.Top="88" Width="140" Height="34"/>
    <TextBlock x:Name="Output" Canvas.Left="12" Canvas.Top="150" FontSize="16"
               Text="Awaiting command..."/>
  </Canvas>
</Grid>"##;

/// The view entity that hosts the commands scene + bridges. Inserted by the
/// startup spawn so reactive systems (and the smoke test) can find it.
#[derive(Resource, Clone, Copy)]
pub struct CommandsView(pub Entity);

/// Stage the commands XAML + (best-effort) the Roboto label font, then spawn the
/// view entity wired with the command host, the output-text writer, and a DP
/// watch on `Output.Text`.
fn spawn_commands(commands: &mut Commands, xaml: &mut XamlRegistry, fonts: &mut FontRegistry) {
    xaml.insert(
        COMMANDS_URI.to_string(),
        Arc::new(COMMANDS_XAML.as_bytes().to_vec()),
    );

    // Best-effort: stage one font from the SDK so the labels render. If the SDK
    // isn't reachable we skip the font gate and run font-free (buttons still
    // render + react; only the glyphs are missing).
    let (wait_for_fonts, wait_for_font_files) = match stage_roboto(fonts) {
        Some(filename) => (
            vec!["Fonts".to_string()],
            vec![("Fonts".to_string(), filename)],
        ),
        None => {
            warn!("commands: Roboto not staged — running font-free (labels won't render)");
            (Vec::new(), Vec::new())
        }
    };

    let view = commands
        .spawn((
            Camera2d,
            NoesisCamera,
            NoesisView {
                xaml_uri: COMMANDS_URI.to_string(),
                size: UVec2::new(VIEW_W, VIEW_H),
                wait_for_fonts,
                wait_for_font_files,
                ..default()
            },
            // Bridge 1: declare the three commands and attach the host as the
            // view-root DataContext. `Command="{Binding SayHello}"` etc. resolve
            // against it. `Locked` is gated off below.
            NoesisCommands::new(
                CommandsDef::new(COMMANDS_CLASS)
                    .command("SayHello")
                    .command("Goodbye")
                    .command("Locked"),
            ),
            // Bridge 2: we own the Output text — seed it and rewrite it on each
            // command invocation. (NoesisText suppresses read-backs of its own
            // writes, so the result is observed through the DP watch below.)
            NoesisText::new().with("Output", "Awaiting command..."),
            // Bridge 3: watch `Output.Text` read back so the reaction surfaces as
            // a NoesisDpChanged — a cross-bridge round-trip.
            NoesisDp::new().watch("Output", "Text", DpKind::Str),
        ))
        .id();
    commands.insert_resource(CommandsView(view));
}

/// Disable the `Locked` command once the view exists. A disabled command's
/// `CanExecute` reports `false`, so the bound `Button` enters its `Disabled`
/// state and clicking it invokes nothing. Runs every frame but only acts once.
fn gate_locked_command(mut q: Query<&mut NoesisCommands>, mut done: Local<bool>) {
    if *done {
        return;
    }
    for mut cmds in &mut q {
        cmds.set_enabled("Locked", false);
        *done = true;
    }
}

/// Demo "logic", expressed through bridge components: when a command surfaces as
/// a [`NoesisCommandInvoked`], rewrite the `Output` `TextBlock` via [`NoesisText`].
/// This is the idiomatic round-trip — XAML invokes the command, Bevy reacts,
/// Bevy writes back into the scene.
fn react_to_commands(
    mut invoked: MessageReader<NoesisCommandInvoked>,
    mut q: Query<&mut NoesisText>,
) {
    for ev in invoked.read() {
        let Ok(mut text) = q.get_mut(ev.view) else {
            continue;
        };
        let who = ev.parameter.as_deref().unwrap_or("(no param)");
        let output = match ev.name.as_str() {
            "SayHello" => format!("Hello, {who}!"),
            "Goodbye" => format!("Goodbye, {who}."),
            other => format!("Invoked {other}"),
        };
        text.set.insert("Output".to_string(), output);
    }
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

/// Add the commands demo to an existing `App` — assumes Bevy's plugins are
/// already installed by the caller (windowed in [`main`], headless in the smoke
/// test). This is the single shared "app config" both entry points boot.
pub fn configure_commands(app: &mut App) {
    app.add_plugins(NoesisPlugin::default())
        .add_systems(
            Startup,
            |mut commands: Commands,
             mut xaml: ResMut<XamlRegistry>,
             mut fonts: ResMut<FontRegistry>| {
                spawn_commands(&mut commands, &mut xaml, &mut fonts);
            },
        )
        .add_systems(Update, (gate_locked_command, react_to_commands));
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
            title: "dm_noesis_bevy — Commands".to_string(),
            resolution: (VIEW_W, VIEW_H).into(),
            ..default()
        }),
        ..default()
    }));
    configure_commands(&mut app);
    app.run();
}
