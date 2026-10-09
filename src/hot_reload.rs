//! Live XAML editing: watch files on disk and push their new bytes into
//! [`XamlRegistry`].
//!
//! Requires the off-by-default `hot_reload` cargo feature, which pulls in
//! `notify` and a background watcher thread. With it on,
//! [`NoesisPlugin`](crate::NoesisPlugin) adds [`NoesisHotReloadPlugin`].
//!
//! Register each file with [`NoesisHotReload::watch`]. Any filesystem path
//! works, including files outside `assets/` that were staged with
//! [`XamlRegistry::insert`]. When a watched file changes, [`poll_hot_reload`]
//! re-reads it and re-inserts the bytes under its URI. In the same frame,
//! [`NoesisSet::Ensure`](crate::NoesisSet::Ensure) rebuilds every view whose
//! scene used that URI, as its root or as a merged dictionary.
//!
//! ```ignore
//! fn watch_root(hot: Option<Res<NoesisHotReload>>) {
//!     if let Some(hot) = hot {
//!         hot.watch("ui/root.xaml", "assets/ui/root.xaml");
//!     }
//! }
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};

use bevy::prelude::*;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::xaml::XamlRegistry;

/// The filesystem watcher and the files it maps to [`XamlRegistry`] URIs.
///
/// Inserted only if the platform watcher starts, so take it as
/// `Option<Res<NoesisHotReload>>`. [`watch`](Self::watch) takes `&self`, so a
/// plain `Res` is enough to register files.
#[derive(Resource)]
pub struct NoesisHotReload {
    inner: Mutex<Inner>,
}

struct Inner {
    watcher: RecommendedWatcher,
    /// `Receiver` is `!Sync`, hence the `Mutex` around `Inner`.
    rx: Receiver<notify::Result<notify::Event>>,
    /// Canonical so keys match the paths `notify` reports.
    files: HashMap<PathBuf, String>,
    /// Parent directories are watched, not files, so atomic saves
    /// (write temp, rename over) still deliver after the inode changes.
    dirs: HashSet<PathBuf>,
}

impl NoesisHotReload {
    /// `None` (logged) when the platform watcher can't be created.
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

    /// Watches `fs_path`; on each change its bytes are re-inserted into
    /// [`XamlRegistry`] under `uri`, which rebuilds the views that use it.
    ///
    /// `uri` must be the registry key the XAML was loaded under (the string
    /// given to [`XamlRegistry::insert`] or `AssetServer::load`). Watching a
    /// file again replaces its URI. A path that doesn't exist yet is logged
    /// and not watched.
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
        if inner.dirs.insert(parent.clone())
            && let Err(err) = inner.watcher.watch(&parent, RecursiveMode::NonRecursive)
        {
            warn!(
                "NoesisHotReload: failed to watch {}: {err}",
                parent.display()
            );
            inner.dirs.remove(&parent);
            return;
        }
        inner.files.insert(canonical, uri);
    }
}

/// Drains watcher events and re-inserts every changed file into
/// [`XamlRegistry`]. Runs in `Update`, so the view rebuilds in the same
/// frame's `PostUpdate`.
///
/// Several events for one save collapse into one re-read. A failed read
/// (common mid-save) is logged and skipped; a later event carries the final
/// contents.
#[allow(clippy::needless_pass_by_value)]
pub fn poll_hot_reload(hot: Option<Res<NoesisHotReload>>, mut registry: ResMut<XamlRegistry>) {
    let Some(hot) = hot else {
        return;
    };

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
                // Raw path if the file is momentarily gone mid-save.
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

/// `Create` counts because atomic saves arrive as one; `Access` and `Remove`
/// don't.
fn is_reload_trigger(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
    )
}

/// Inserts [`NoesisHotReload`] and adds [`poll_hot_reload`]. Added by
/// [`NoesisPlugin`](crate::NoesisPlugin) when the `hot_reload` feature is on;
/// the app registers the files to watch.
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
