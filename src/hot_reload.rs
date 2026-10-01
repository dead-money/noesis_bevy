//! Opt-in filesystem hot-reload for XAML.
//!
//! The render pipeline already rebuilds a view whenever the bytes behind its
//! URI change: [`crate::render`]'s `ensure_scene` spots a fresh `Arc` by
//! pointer identity and does a full teardown + rebuild, re-attaching every
//! bridge. The only missing piece for live-editing is something that notices a
//! file changed on disk and pushes the new bytes into [`XamlRegistry`]. That is
//! this module.
//!
//! Unlike Bevy's own asset file-watcher, this watches *real filesystem paths*
//! wherever they live — including files loaded outside the `assets/` root via
//! [`XamlRegistry::insert`] (the `xaml_viewer` example, standalone `.xaml`
//! files, SDK data directories). Register a `(uri, path)` pair with
//! [`NoesisHotReload::watch`]; on the next change to that file the bytes are
//! re-read and re-inserted, and the view rebuilds on the following frame.
//!
//! Data flow (the second half already existed; hot-reload feeds its front):
//!
//! ```text
//!   file change ─▶ notify thread ─▶ poll_hot_reload ─▶ XamlRegistry::insert
//!                                                            │ sync_xaml_provider_map
//!                                                            ▼
//!                                     SharedXamlMap ─▶ ensure_scene (Arc-swap rebuild)
//! ```
//!
//! Gated behind the off-by-default `hot_reload` cargo feature: it is a
//! development aid and pulls the `notify` crate plus a background watcher
//! thread, neither of which a shipped game wants.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};

use bevy::prelude::*;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::xaml::XamlRegistry;

/// Resource that owns the filesystem watcher and the map of watched files to
/// the [`XamlRegistry`] URIs they back.
///
/// Present only when the `hot_reload` feature is enabled and the watcher
/// started successfully; systems should take it as `Option<Res<…>>` so a failed
/// watcher (or a build that never registered anything) degrades to a no-op
/// rather than a panic.
///
/// Everything mutable lives behind one [`Mutex`], so the resource is `Send +
/// Sync` and [`watch`](Self::watch) takes `&self` — register files from an
/// ordinary `Res<NoesisHotReload>` system. The lock is only ever taken on the
/// main thread (registration and the poll system), so it is never contended;
/// the `notify` thread only touches the channel, never this map.
#[derive(Resource)]
pub struct NoesisHotReload {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Kept alive for the lifetime of the resource: dropping it stops the
    /// background thread and ends all watches.
    watcher: RecommendedWatcher,
    /// Change events delivered by the `notify` thread. `Receiver` is `!Sync`,
    /// which is why the whole `Inner` sits behind a `Mutex`.
    rx: Receiver<notify::Result<notify::Event>>,
    /// Canonicalized file path → the `XamlRegistry` URI it feeds. Canonical so
    /// it matches the paths `notify` reports (which resolve symlinks and `..`).
    files: HashMap<PathBuf, String>,
    /// Directories already handed to `watcher.watch`. We watch parent
    /// directories (not files) so editor atomic-saves — write-temp-then-rename,
    /// which replaces the inode the file watch was bound to — still deliver.
    /// De-duped so N scenes in one folder cost one watch.
    dirs: HashSet<PathBuf>,
}

impl NoesisHotReload {
    /// Build a watcher and its resource. Returns `None` (after logging) if the
    /// platform watcher cannot be created, so the plugin can skip inserting the
    /// resource and leave hot-reload simply inert.
    fn new() -> Option<Self> {
        let (tx, rx) = channel();
        let watcher = match notify::recommended_watcher(tx) {
            Ok(w) => w,
            Err(err) => {
                warn!("NoesisHotReload: could not create filesystem watcher: {err}");
                return None;
            }
        };
        Some(Self {
            inner: Mutex::new(Inner {
                watcher,
                rx,
                files: HashMap::new(),
                dirs: HashSet::new(),
            }),
        })
    }

    /// Watch `fs_path` and, whenever it changes on disk, re-read it and
    /// re-insert its bytes into [`XamlRegistry`] under `uri` — which the render
    /// pipeline turns into a live view rebuild.
    ///
    /// `uri` must match the key the scene was registered under (the same string
    /// passed to [`XamlRegistry::insert`] or used with `AssetServer::load`).
    /// Registering the same file twice is harmless. Paths that don't exist yet
    /// (or can't be canonicalized) are logged and skipped rather than watched.
    pub fn watch(&self, uri: impl Into<String>, fs_path: impl AsRef<Path>) {
        let uri = uri.into();
        let raw = fs_path.as_ref();
        let canonical = match std::fs::canonicalize(raw) {
            Ok(p) => p,
            Err(err) => {
                warn!(
                    "NoesisHotReload: cannot watch {} ({uri}): {err}",
                    raw.display()
                );
                return;
            }
        };
        let Some(parent) = canonical.parent().map(Path::to_path_buf) else {
            warn!(
                "NoesisHotReload: {} has no parent directory; not watched",
                canonical.display()
            );
            return;
        };

        let mut inner = self.inner.lock().expect("NoesisHotReload mutex poisoned");
        // First file in this directory: start a (non-recursive) watch on it.
        if inner.dirs.insert(parent.clone()) {
            if let Err(err) = inner.watcher.watch(&parent, RecursiveMode::NonRecursive) {
                warn!(
                    "NoesisHotReload: failed to watch {}: {err}",
                    parent.display()
                );
                inner.dirs.remove(&parent);
                return;
            }
        }
        inner.files.insert(canonical, uri);
    }
}

/// Drain pending filesystem events and refresh [`XamlRegistry`] for every
/// watched file that changed. Runs in `Update`, upstream of the `PostUpdate`
/// sync that feeds Noesis, so an edit lands in the view on the next frame.
///
/// A single save can emit several events (write, chmod, rename); we collapse
/// them to one re-read per file by URI. Read failures — common mid-save, when
/// an editor has truncated the file before writing — are logged and dropped;
/// the trailing event carries the final contents.
#[allow(clippy::needless_pass_by_value)] // Bevy systems take Res<T> by value
pub fn poll_hot_reload(hot: Option<Res<NoesisHotReload>>, mut registry: ResMut<XamlRegistry>) {
    let Some(hot) = hot else {
        return;
    };

    // Collect (uri → path) for changed files, then release the watcher lock
    // before touching the filesystem or the registry.
    let dirty: HashMap<String, PathBuf> = {
        let guard = hot.inner.lock().expect("NoesisHotReload mutex poisoned");
        let mut dirty = HashMap::new();
        while let Ok(event) = guard.rx.try_recv() {
            let event = match event {
                Ok(ev) => ev,
                Err(err) => {
                    warn!("NoesisHotReload: watch error: {err}");
                    continue;
                }
            };
            if !is_reload_trigger(&event.kind) {
                continue;
            }
            for path in &event.paths {
                // Canonicalize so a reported path matches our canonical keys;
                // fall back to the raw path if the file is momentarily gone.
                let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                if let Some(uri) = guard.files.get(&key) {
                    dirty.insert(uri.clone(), key);
                }
            }
        }
        dirty
    };

    for (uri, path) in dirty {
        match std::fs::read(&path) {
            Ok(bytes) => {
                info!(
                    "NoesisHotReload: reloading {uri} from {} ({} bytes)",
                    path.display(),
                    bytes.len()
                );
                registry.insert(uri, std::sync::Arc::new(bytes));
            }
            Err(err) => warn!(
                "NoesisHotReload: re-read of {} failed: {err}",
                path.display()
            ),
        }
    }
}

/// Which `notify` event kinds should trigger a re-read. Data-bearing changes
/// (create — atomic saves land as one — and modify) count; metadata-only
/// `Access` events and removals do not.
fn is_reload_trigger(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
    )
}

/// Installs [`NoesisHotReload`] and the [`poll_hot_reload`] system.
///
/// Added automatically by [`crate::NoesisPlugin`] when the `hot_reload` feature
/// is on. Registering files to watch is left to the app (via
/// [`NoesisHotReload::watch`]); this plugin only wires the resource and the
/// polling.
pub struct NoesisHotReloadPlugin;

impl Plugin for NoesisHotReloadPlugin {
    fn build(&self, app: &mut App) {
        if let Some(hot) = NoesisHotReload::new() {
            app.insert_resource(hot);
        }
        app.add_systems(Update, poll_hot_reload);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, ModifyKind};

    #[test]
    fn reload_triggers_on_data_changes_only() {
        assert!(is_reload_trigger(&EventKind::Modify(ModifyKind::Any)));
        assert!(is_reload_trigger(&EventKind::Create(CreateKind::File)));
        assert!(is_reload_trigger(&EventKind::Any));
        assert!(!is_reload_trigger(&EventKind::Access(
            notify::event::AccessKind::Read
        )));
        assert!(!is_reload_trigger(&EventKind::Remove(
            notify::event::RemoveKind::File
        )));
    }

    #[test]
    fn watch_records_canonical_path_and_dedupes_dirs() {
        let dir = std::env::temp_dir().join(format!("noesis_hot_reload_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.xaml");
        let b = dir.join("b.xaml");
        std::fs::write(&a, b"<Grid/>").unwrap();
        std::fs::write(&b, b"<Grid/>").unwrap();

        let hot = NoesisHotReload::new().expect("watcher");
        hot.watch("a.xaml", &a);
        hot.watch("b.xaml", &b);

        let inner = hot.inner.lock().unwrap();
        // Both files mapped to their URIs, keyed by canonical path.
        assert_eq!(inner.files.len(), 2);
        assert_eq!(
            inner.files.get(&std::fs::canonicalize(&a).unwrap()),
            Some(&"a.xaml".to_string())
        );
        // Two files in one directory collapse to a single directory watch.
        assert_eq!(inner.dirs.len(), 1);
        drop(inner);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_is_skipped_not_watched() {
        let hot = NoesisHotReload::new().expect("watcher");
        hot.watch("ghost.xaml", "/definitely/not/here/ghost.xaml");
        let inner = hot.inner.lock().unwrap();
        assert!(inner.files.is_empty());
        assert!(inner.dirs.is_empty());
    }

    /// End-to-end: a real `notify` thread + the poll system push a disk edit
    /// into `XamlRegistry`. Filesystem events are asynchronous, so we drive the
    /// app in a bounded retry loop rather than a single update.
    #[test]
    fn edit_on_disk_updates_registry() {
        let dir = std::env::temp_dir().join(format!("noesis_hot_e2e_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("live.xaml");
        std::fs::write(&file, b"<Grid Background=\"Red\"/>").unwrap();

        let mut app = App::new();
        app.init_resource::<XamlRegistry>();
        let hot = NoesisHotReload::new().expect("watcher");
        // Seed the registry the way an app's initial load would.
        app.world_mut().resource_mut::<XamlRegistry>().insert(
            "live.xaml",
            std::sync::Arc::new(b"<Grid Background=\"Red\"/>".to_vec()),
        );
        hot.watch("live.xaml", &file);
        app.insert_resource(hot);
        app.add_systems(Update, poll_hot_reload);

        // Change the file; the watcher thread should deliver an event that the
        // poll system turns into a registry refresh within a few updates.
        std::fs::write(&file, b"<Grid Background=\"Blue\"/>").unwrap();

        let mut reloaded = false;
        for _ in 0..50 {
            app.update();
            let registry = app.world().resource::<XamlRegistry>();
            if registry.get("live.xaml").map(|b| b.as_slice())
                == Some(b"<Grid Background=\"Blue\"/>")
            {
                reloaded = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        std::fs::remove_dir_all(&dir).ok();
        assert!(reloaded, "registry never picked up the on-disk edit");
    }
}
