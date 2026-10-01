//! Feeds a named `<Image>` pixels from Rust, with no image file.
//!
//! Add a [`NoesisImaging`] to the [`NoesisView`](crate::NoesisView) camera
//! entity. Each entry in [`images`](NoesisImaging::images) names an `<Image>`
//! and carries RGBA8 pixels, their size, and the `uri` the element's XAML
//! `Source` points at. The bridge stages the pixels into [`ImageRegistry`]
//! under that `uri`, so the texture provider serves them the same way it
//! serves a loaded `.png`.
//!
//! ```ignore
//! // XAML: <Image x:Name="Pic" Source="dm-bitmap://logo" Stretch="None"/>
//! let rgba = Arc::new(vec![255u8; 13 * 7 * 4]); // 13x7 opaque white
//! commands.entity(view).insert(
//!     NoesisImaging::new().set("Pic", "dm-bitmap://logo", 13, 7, rgba),
//! );
//! ```
//!
//! # Timing
//!
//! Noesis resolves an `<Image>`'s source once, when the scene first lays out,
//! and does not retry a miss. Insert a populated [`NoesisImaging`] when you
//! spawn the view, not in a later frame. Staging runs before
//! [`NoesisSet::Sync`], so a bitmap spawned in the same frame as the view is in
//! place before the scene builds.
//!
//! Changing the bytes of a URI that is already staged (a new `Arc`) rebuilds
//! every live scene so Noesis reloads the texture. See [`crate::image`].
//!
//! # Read-back
//!
//! Every frame the bridge reads each named `<Image>` back and emits
//! [`NoesisImageChanged`] when its source presence or size changes. Noesis
//! sizes an `Image` from the texture provider's reported dimensions, so a
//! `Stretch="None"` element showing a 13x7 bitmap reads back `[13.0, 7.0]`. An
//! unresolved `Source` reads back `[0.0, 0.0]`. Read-back covers views only, not
//! [`UiPanel`](crate::panel::UiPanel) entities.
//!
//! Staging uses a URI rather than assigning an `ImageSource` to the element
//! because the runtime has no safe setter for `Image::SetSource`, and this
//! crate forbids `unsafe`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;

use crate::image::ImageRegistry;
use crate::render::{NoesisRenderState, NoesisSet};

/// Pixels for one `<Image>`, plus the `uri` its XAML `Source` references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBitmap {
    /// The `Source` URI the element's XAML references (e.g. `dm-bitmap://logo`).
    pub uri: String,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Tightly-packed RGBA8 with premultiplied alpha, `width * height * 4`
    /// bytes long. Staged as-is, without conversion.
    pub bytes: Arc<Vec<u8>>,
}

/// Code-supplied bitmaps for named `<Image>` elements. See the
/// [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisImaging {
    /// Bitmap per `<Image>` `x:Name`. Staged into [`ImageRegistry`] whenever
    /// this component changes. The registry is global, so two components
    /// staging the same `uri` overwrite each other. Removing the component
    /// frees the bitmaps no other [`NoesisImaging`] still stages.
    pub images: HashMap<String, ImageBitmap>,
}

impl NoesisImaging {
    /// An empty bridge. Chain [`set`](Self::set) to add bitmaps.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Shows `bytes` in the `<Image>` named `name`. `uri` must match the
    /// element's authored `Source`; see [`ImageBitmap::bytes`] for the pixel
    /// format.
    #[must_use]
    pub fn set(
        mut self,
        name: impl Into<String>,
        uri: impl Into<String>,
        width: u32,
        height: u32,
        bytes: Arc<Vec<u8>>,
    ) -> Self {
        self.images.insert(
            name.into(),
            ImageBitmap {
                uri: uri.into(),
                width,
                height,
                bytes,
            },
        );
        self
    }
}

/// State of an `<Image>` after layout, read from the live element.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageReadback {
    /// Whether the element currently has a non-null `Source` (`ImageSource`) DP.
    pub has_source: bool,
    /// `[ActualWidth, ActualHeight]` from the last layout pass. With
    /// `Stretch="None"` this is the bitmap's pixel size once resolved;
    /// `[0.0, 0.0]` for an unresolved source.
    pub actual_size: [f32; 2],
}

/// Emitted when an `<Image>` named in [`NoesisImaging::images`] reads back
/// differently from the previous frame. The first poll after a name is added
/// or the scene is rebuilt always reports.
#[derive(Message, Debug, Clone)]
pub struct NoesisImageChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose image changed.
    pub view: Entity,
    /// `x:Name` of the watched `<Image>` element.
    pub name: String,
    /// What was read back from the live element.
    pub readback: ImageReadback,
}

/// URIs each [`NoesisImaging`] entity stages, so [`reap_removed_imaging`] can
/// free a removed component's bitmaps without dropping ones another component
/// still stages.
#[derive(Resource, Default)]
pub(crate) struct StagedImagingUris(HashMap<Entity, HashSet<String>>);

/// Stages every changed [`NoesisImaging`] into [`ImageRegistry`]. Runs before
/// [`NoesisSet::Sync`] so a same-frame spawn is staged before scene build.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn stage_imaging_bitmaps(
    views: Query<(Entity, Ref<NoesisImaging>)>,
    mut registry: ResMut<ImageRegistry>,
    mut staged: ResMut<StagedImagingUris>,
) {
    for (entity, imaging) in &views {
        if !imaging.is_changed() {
            continue;
        }
        let mut uris = HashSet::with_capacity(imaging.images.len());
        for bitmap in imaging.images.values() {
            registry.insert(
                bitmap.uri.clone(),
                bitmap.width,
                bitmap.height,
                Arc::clone(&bitmap.bytes),
            );
            uris.insert(bitmap.uri.clone());
        }
        staged.0.insert(entity, uris);
    }
}

/// Frees the bitmaps and read-back snapshots of removed [`NoesisImaging`]
/// components. Runs before [`NoesisSet::Sync`] so the provider never copies a
/// freed bitmap.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn reap_removed_imaging(
    mut removed: RemovedComponents<NoesisImaging>,
    mut staged: ResMut<StagedImagingUris>,
    mut registry: ResMut<ImageRegistry>,
    mut state: Option<NonSendMut<NoesisRenderState>>,
) {
    for entity in removed.read() {
        if let Some(uris) = staged.0.remove(&entity) {
            for uri in uris {
                if !staged.0.values().any(|s| s.contains(&uri)) {
                    registry.remove(&uri);
                }
            }
        }
        if let Some(state) = state.as_deref_mut() {
            state.reap_imaging_snapshots_for(entity);
        }
    }
}

/// Emits [`NoesisImageChanged`] for read-back changes. Runs in
/// [`NoesisSet::Apply`], so it sees the previous frame's layout.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn poll_imaging_reads(
    views: Query<(Entity, &NoesisImaging)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisImageChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, imaging) in &views {
        for (name, readback) in state.poll_image_reads_for(entity, &imaging.images) {
            changed.write(NoesisImageChanged {
                view: entity,
                name,
                readback,
            });
        }
    }
}

/// Registers [`NoesisImaging`]'s systems and message. Added by
/// [`NoesisPlugin`](crate::NoesisPlugin).
pub struct NoesisImagingPlugin;

impl Plugin for NoesisImagingPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisImageChanged>()
            .init_resource::<StagedImagingUris>()
            .add_systems(
                PostUpdate,
                (
                    stage_imaging_bitmaps.before(NoesisSet::Sync),
                    reap_removed_imaging
                        .after(stage_imaging_bitmaps)
                        .before(NoesisSet::Sync),
                    poll_imaging_reads.in_set(NoesisSet::Apply),
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_images() {
        let bytes = Arc::new(vec![255u8; 13 * 7 * 4]);
        let i = NoesisImaging::new().set("Pic", "dm-bitmap://logo", 13, 7, Arc::clone(&bytes));
        let got = i.images.get("Pic").expect("Pic entry");
        assert_eq!(got.uri, "dm-bitmap://logo");
        assert_eq!((got.width, got.height), (13, 7));
        assert_eq!(got.bytes.len(), 13 * 7 * 4);
    }
}
