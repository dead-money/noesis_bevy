//! Generalized XAML viewer: load a single `.xaml` file or a directory of
//! them and page through scenes interactively.
//!
//! Cycle between scenes with `[` / `]`, jump with `Home` / `End`, reload the
//! current one with `R`, and trigger a screenshot with `S`. With
//! `NOESIS_VIEWER_EXIT_AFTER=1` set, it waits a few frames, shoots the
//! configured target (`NOESIS_SCREENSHOT`) and exits, for headless eval.
//!
//! `P` toggles PPAA (Noesis per-primitive edge anti-aliasing) by flipping
//! `NoesisScene.ppaa`; the render-world picks the change up per frame via
//! `apply_live_scene_flags` and calls `View::set_flags` only on change.
//!
//! When `NOESIS_VIEWER_THEME=<name>` is set (e.g. `DarkBlue`, `LightRed`),
//! the viewer stages the Noesis SDK's theme XAMLs and PT Root UI fonts
//! from `$NOESIS_SDK_DIR/Src/Packages/App/Theme/Data/Theme/` into the
//! XAML + font registries, and points `NoesisScene.application_resources`
//! at `NoesisTheme.<name>.xaml` so unstyled controls pick up real
//! `ControlTemplates` instead of Noesis's magenta placeholders.
//!
//! ```bash
//! # Single file
//! cargo run -p noesis_bevy --example xaml_viewer assets/viewer_samples/01_button_hover.xaml
//!
//! # Directory (cycle with [/])
//! cargo run -p noesis_bevy --example xaml_viewer assets/viewer_samples
//!
//! # Point at the SDK's Data/ (symlink assets/Data -> $NOESIS_SDK_DIR/Data first)
//! cargo run -p noesis_bevy --example xaml_viewer assets/Data
//!
//! # Headless screenshot for CI / visual eval
//! NOESIS_VIEWER_EXIT_AFTER=1 NOESIS_SCREENSHOT=out.png \
//!   cargo run -p noesis_bevy --example xaml_viewer assets/viewer_samples/01_button_hover.xaml
//! ```
//!
//! Environment:
//! - `NOESIS_VIEWER_PATH`:        fallback for the positional arg.
//! - `NOESIS_SCREENSHOT`:         screenshot output path (default: `<stem>.png`).
//! - `NOESIS_SCREENSHOT_FRAMES`:  frame to shoot on in headless mode (default 120).
//! - `NOESIS_VIEWER_EXIT_AFTER`:  any value takes one screenshot and exits.
//! - `NOESIS_VIEWER_SIZE`:        `WxH` override for the Noesis view size
//!   (default: match the window's initial physical size).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bevy::input::ButtonState;
use bevy::input::keyboard::KeyboardInput;
use bevy::prelude::*;
use bevy::render::view::screenshot::{Screenshot, save_to_disk};
use bevy::window::PrimaryWindow;
use noesis_bevy::{FontRegistry, ImageAsset, NoesisCamera, NoesisPlugin, NoesisView, XamlRegistry};

/// Carries the initial [`NoesisView`] config from `main` to `setup_camera`,
/// which spawns it onto the camera entity.
#[derive(Resource)]
struct InitialView(NoesisView);

#[derive(Resource)]
struct Viewer {
    scenes: Vec<ScenePath>,
    current: usize,
    pending_screenshot: bool,
    screenshot_override: Option<PathBuf>,
    headless: HeadlessMode,
    frame: u32,
}

#[derive(Clone, Debug)]
struct ScenePath {
    /// URI Noesis asks for; matches the `XamlRegistry` key.
    uri: String,
    /// On-disk absolute path. Kept for reload + default screenshot naming.
    fs_path: PathBuf,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum HeadlessMode {
    Off,
    Waiting { trigger_at: u32 },
    Captured { exit_at: u32 },
}

fn main() {
    let arg_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("NOESIS_VIEWER_PATH").ok());
    let arg_path = arg_path.as_deref().unwrap_or("assets/viewer_samples");
    let scenes = collect_scenes(arg_path);

    if scenes.is_empty() {
        eprintln!("xaml_viewer: no .xaml files found at {arg_path:?}");
        std::process::exit(1);
    }

    let theme = std::env::var("NOESIS_VIEWER_THEME").ok();
    let theme_files = theme.as_deref().map(stage_theme).unwrap_or_default();
    let application_resources: Vec<String> = theme
        .as_deref()
        .map(|t| vec![format!("NoesisTheme.{t}.xaml")])
        .unwrap_or_default();
    // Require every theme font to be present before Noesis's
    // CachedFontProvider does its one-shot `scan_folder("Fonts")`.
    let wait_for_font_files: Vec<(String, String)> = theme_files
        .fonts
        .iter()
        .map(|(folder, filename, _)| (folder.clone(), filename.clone()))
        .collect();

    let headless = if std::env::var_os("NOESIS_VIEWER_EXIT_AFTER").is_some() {
        let frames = std::env::var("NOESIS_SCREENSHOT_FRAMES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120u32);
        HeadlessMode::Waiting { trigger_at: frames }
    } else {
        HeadlessMode::Off
    };

    let screenshot_override = std::env::var_os("NOESIS_SCREENSHOT").map(PathBuf::from);
    let size = parse_size_env().unwrap_or(UVec2::new(1280, 720));

    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: format!("xaml_viewer — {arg_path}"),
                resolution: size.into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(NoesisPlugin::default())
        .insert_resource(InitialView(NoesisView {
            xaml_uri: scenes[0].uri.clone(),
            size,
            wait_for_fonts: vec!["Fonts".into()],
            wait_for_font_files,
            wait_for_images: Vec::new(),
            ppaa: true,
            application_resources,
            ..default()
        }))
        .insert_resource(StagedTheme(theme_files))
        .insert_resource(Viewer {
            scenes,
            current: 0,
            pending_screenshot: false,
            screenshot_override,
            headless,
            frame: 0,
        })
        .add_systems(Startup, (setup_camera, load_scenes_into_registry))
        .add_systems(Update, load_theme_into_registries_once)
        .add_systems(
            Update,
            (
                viewer_controls,
                apply_scene_changes,
                run_screenshot,
                tick_headless,
            )
                .chain(),
        )
        .run();
}

fn parse_size_env() -> Option<UVec2> {
    let s = std::env::var("NOESIS_VIEWER_SIZE").ok()?;
    let (w, h) = s.split_once('x')?;
    Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
}

/// Resolved theme XAML + font paths, discovered under
/// `$NOESIS_SDK_DIR/Src/Packages/App/Theme/Data/Theme/`. Cached in a
/// `StagedTheme` resource and pushed into `XamlRegistry` / `FontRegistry`
/// at startup so theme resolution has everything it needs.
#[derive(Default, Clone)]
struct StagedThemeFiles {
    xamls: Vec<(String, PathBuf)>,
    fonts: Vec<(String, String, PathBuf)>, // (folder, filename, absolute path)
}

#[derive(Resource, Default)]
struct StagedTheme(StagedThemeFiles);

fn stage_theme(theme: &str) -> StagedThemeFiles {
    let Some(sdk) = std::env::var_os("NOESIS_SDK_DIR") else {
        warn!("NOESIS_VIEWER_THEME set but NOESIS_SDK_DIR unset — skipping theme");
        return StagedThemeFiles::default();
    };
    let root = PathBuf::from(sdk).join("Src/Packages/App/Theme/Data/Theme");
    if !root.is_dir() {
        warn!("Theme source {} not found — skipping", root.display());
        return StagedThemeFiles::default();
    }

    let mut files = StagedThemeFiles::default();

    // Pull every *.xaml in the theme dir into the registry under its plain
    // filename; the theme's nested `<ResourceDictionary Source="..."/>`
    // uses the same bare-name form.
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "xaml")
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            files.xamls.push((name.to_string(), path.clone()));
        }
    }

    // PT Root UI goes into the `Fonts/` folder so NoesisTheme.Fonts.xaml's
    // `FontFamily="Fonts/#PT Root UI"` resolves. Single folder shared with
    // any scene fonts the user already has under assets/Fonts.
    let fonts_dir = root.join("Fonts");
    for entry in std::fs::read_dir(&fonts_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "otf" || e == "ttf")
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            files
                .fonts
                .push(("Fonts".to_string(), name.to_string(), path.clone()));
        }
    }

    info!(
        "stage_theme({theme}): {} XAMLs + {} fonts from {}",
        files.xamls.len(),
        files.fonts.len(),
        root.display(),
    );

    let want = format!("NoesisTheme.{theme}.xaml");
    if !files.xamls.iter().any(|(n, _)| n == &want) {
        warn!(
            "NOESIS_VIEWER_THEME={theme:?} but {want} not found under {}. \
             Available root themes start with `NoesisTheme.` in that dir.",
            root.display(),
        );
    }

    files
}

/// Inject theme XAMLs + fonts into the registries the moment the Bevy
/// asset system has finished loading `assets/Fonts/`. Deferring until
/// then makes both font sets land simultaneously in `FontRegistry` →
/// `SharedFontMap`, which in turn keeps Noesis's `CachedFontProvider`
/// from caching an empty-or-partial `scan_folder("Fonts")` result. Runs
/// once, then clears the staged list so it's effectively idempotent.
#[allow(clippy::needless_pass_by_value)]
fn load_theme_into_registries_once(
    mut staged: ResMut<StagedTheme>,
    mut xaml_registry: ResMut<XamlRegistry>,
    mut font_registry: ResMut<FontRegistry>,
) {
    if staged.0.xamls.is_empty() && staged.0.fonts.is_empty() {
        return;
    }
    // Wait for a sentinel font from `assets/Fonts/` to land. `Bitter-
    // Regular.ttf` is present in every scene we care about; it's also the
    // first fallback, so if it's missing our text rendering was never
    // going to work anyway.
    if font_registry.get("Fonts", "Bitter-Regular.ttf").is_none() {
        return;
    }
    info!(
        "Theme staging: asset fonts ready; injecting {} XAML + {} font(s)",
        staged.0.xamls.len(),
        staged.0.fonts.len(),
    );
    let files = std::mem::take(&mut staged.0);
    for (name, path) in files.xamls {
        match std::fs::read(&path) {
            Ok(bytes) => xaml_registry.insert(name, Arc::new(bytes)),
            Err(err) => warn!("theme xaml read failed {}: {err}", path.display()),
        }
    }
    for (folder, filename, path) in files.fonts {
        match std::fs::read(&path) {
            Ok(bytes) => font_registry.insert(folder, filename, Arc::new(bytes)),
            Err(err) => warn!("theme font read failed {}: {err}", path.display()),
        }
    }
}

fn collect_scenes(arg: &str) -> Vec<ScenePath> {
    let path = Path::new(arg);
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut out = Vec::new();
    if abs.is_file() {
        if let Some(scene) = scene_from_file(&abs) {
            out.push(scene);
        }
    } else if abs.is_dir() {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&abs)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .map(|e| e.path())
                    .filter(|p| p.is_file())
                    .filter(|p| p.extension().is_some_and(|e| e == "xaml"))
                    .collect()
            })
            .unwrap_or_default();
        paths.sort();
        for p in paths {
            if let Some(scene) = scene_from_file(&p) {
                out.push(scene);
            }
        }
    }
    out
}

fn scene_from_file(abs: &Path) -> Option<ScenePath> {
    let file_name = abs.file_name()?.to_string_lossy().into_owned();
    Some(ScenePath {
        // Use the bare filename as the URI; matches how Noesis.xaml references
        // other XAMLs (`Source="Styles.xaml"`) and how `AssetServer::load`
        // pathways shape their keys.
        uri: file_name,
        fs_path: abs.to_path_buf(),
    })
}

fn setup_camera(mut commands: Commands, asset_server: Res<AssetServer>, initial: Res<InitialView>) {
    commands.spawn((
        Camera2d,
        Camera {
            clear_color: ClearColorConfig::Custom(Color::srgb(0.05, 0.05, 0.05)),
            ..default()
        },
        NoesisCamera,
        initial.0.clone(),
    ));
    // Pull Fonts/ into the asset system so the Bevy FontProvider populates
    // FontRegistry; any scene that references `FontFamily="Fonts/#..."`
    // will then resolve. Handle is kept alive by the resource below.
    let fonts_handle = asset_server.load_folder("Fonts");
    commands.insert_resource(KeepFonts(fonts_handle));

    // Pre-load any images `NOESIS_VIEWER_IMAGES` names (comma-separated
    // paths relative to `assets/`). The ImageBrush / Image loader only
    // resolves pixels Noesis asks for *after* the asset server has
    // populated ImageRegistry, so we need an explicit trigger.
    //
    // Default list covers the SDK samples that reference common image paths,
    // letting `xaml_viewer assets/Data/Transform3D.xaml` work out of the box.
    let image_list = std::env::var("NOESIS_VIEWER_IMAGES")
        .unwrap_or_else(|_| "Data/Images/BgTile.png".to_string());
    let mut image_handles = Vec::new();
    for path in image_list
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        image_handles.push(asset_server.load::<ImageAsset>(path.to_string()));
    }
    commands.insert_resource(KeepImages(image_handles));
}

#[derive(Resource)]
#[allow(dead_code)]
struct KeepFonts(Handle<bevy::asset::LoadedFolder>);

#[derive(Resource)]
#[allow(dead_code)]
struct KeepImages(Vec<Handle<ImageAsset>>);

fn load_scenes_into_registry(viewer: Res<Viewer>, mut registry: ResMut<XamlRegistry>) {
    for scene in &viewer.scenes {
        match std::fs::read(&scene.fs_path) {
            Ok(bytes) => {
                info!(
                    "xaml_viewer: loaded {} ({} bytes)",
                    scene.fs_path.display(),
                    bytes.len()
                );
                registry.insert(scene.uri.clone(), Arc::new(bytes));
            }
            Err(err) => {
                warn!(
                    "xaml_viewer: failed to read {}: {err}",
                    scene.fs_path.display()
                );
            }
        }
    }
    info!(
        "xaml_viewer: {} scene(s) available. Current: {}",
        viewer.scenes.len(),
        viewer.scenes[0].uri,
    );
}

#[allow(clippy::needless_pass_by_value)]
fn viewer_controls(
    mut viewer: ResMut<Viewer>,
    mut keys: MessageReader<KeyboardInput>,
    mut registry: ResMut<XamlRegistry>,
    mut views: Query<&mut NoesisView>,
) {
    for ev in keys.read() {
        if !matches!(ev.state, ButtonState::Pressed) || ev.repeat {
            continue;
        }
        match ev.key_code {
            KeyCode::BracketRight => {
                viewer.current = (viewer.current + 1) % viewer.scenes.len();
                info!("xaml_viewer: → {}", viewer.scenes[viewer.current].uri);
            }
            KeyCode::BracketLeft => {
                let n = viewer.scenes.len();
                viewer.current = (viewer.current + n - 1) % n;
                info!("xaml_viewer: ← {}", viewer.scenes[viewer.current].uri);
            }
            KeyCode::Home => {
                viewer.current = 0;
                info!("xaml_viewer: ⇱ {}", viewer.scenes[viewer.current].uri);
            }
            KeyCode::End => {
                viewer.current = viewer.scenes.len() - 1;
                info!("xaml_viewer: ⇲ {}", viewer.scenes[viewer.current].uri);
            }
            KeyCode::KeyR => {
                let scene = &viewer.scenes[viewer.current];
                match std::fs::read(&scene.fs_path) {
                    Ok(bytes) => {
                        registry.insert(scene.uri.clone(), Arc::new(bytes));
                        info!("xaml_viewer: reloaded {}", scene.uri);
                    }
                    Err(err) => warn!("xaml_viewer: reload failed: {err}"),
                }
            }
            KeyCode::KeyP => {
                if let Some(mut scene) = views.iter_mut().next() {
                    scene.ppaa = !scene.ppaa;
                    info!(
                        "xaml_viewer: PPAA {}",
                        if scene.ppaa { "on" } else { "off" }
                    );
                }
            }
            KeyCode::KeyS => {
                viewer.pending_screenshot = true;
                info!("xaml_viewer: screenshot queued");
            }
            _ => {}
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn apply_scene_changes(viewer: Res<Viewer>, mut views: Query<&mut NoesisView>) {
    let desired = &viewer.scenes[viewer.current].uri;
    for mut scene in &mut views {
        if scene.xaml_uri != *desired {
            scene.xaml_uri = desired.clone();
        }
    }
}

fn run_screenshot(mut commands: Commands, mut viewer: ResMut<Viewer>) {
    if !viewer.pending_screenshot {
        return;
    }
    let path = viewer.screenshot_override.clone().unwrap_or_else(|| {
        let stem = viewer.scenes[viewer.current]
            .fs_path
            .file_stem()
            .map_or("screenshot".into(), |s| s.to_string_lossy().into_owned());
        PathBuf::from(format!("{stem}.png"))
    });
    info!("xaml_viewer: capturing → {}", path.display());
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
    viewer.pending_screenshot = false;
}

#[allow(clippy::needless_pass_by_value)]
fn tick_headless(
    mut viewer: ResMut<Viewer>,
    mut exit: MessageWriter<AppExit>,
    _window: Option<Single<&Window, With<PrimaryWindow>>>,
) {
    viewer.frame += 1;
    match viewer.headless {
        HeadlessMode::Off => {}
        HeadlessMode::Waiting { trigger_at } if viewer.frame >= trigger_at => {
            viewer.pending_screenshot = true;
            viewer.headless = HeadlessMode::Captured {
                exit_at: viewer.frame + 30,
            };
        }
        HeadlessMode::Waiting { .. } => {}
        HeadlessMode::Captured { exit_at } if viewer.frame >= exit_at => {
            exit.write(AppExit::Success);
        }
        HeadlessMode::Captured { .. } => {}
    }
}
