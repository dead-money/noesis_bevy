//! Per-view code-built imaging bridge: drive a named `<Image>`'s pixels from a
//! Rust-provided bitmap (RGBA8 + size), no image file on disk required. The
//! imaging counterpart of the [`crate::brushes`] / [`crate::geometry`] write
//! bridges.
//!
//! Add a [`NoesisImaging`] component to the view's camera entity. Its `images`
//! map is the desired bitmap per `x:Name`: each entry carries the raw RGBA8
//! pixels, the bitmap's pixel size, and the `uri` the element's XAML `Source`
//! references. On change the bridge stages those pixels into the shared
//! [`crate::ImageRegistry`] under `uri`, so the live `Noesis::TextureProvider`
//! resolves `<Image Source="uri"/>` to the Rust bytes (the same path a `.png`
//! asset takes, but fed from memory).
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
//! Noesis resolves a `<Image>`'s `BitmapImage` source from the texture provider
//! **once, when the scene is first laid out**, and does not retry a miss. So the
//! bytes must be staged *before* the view's scene is built. Attach
//! [`NoesisImaging`] (populated) at spawn time, alongside the
//! [`NoesisView`](crate::NoesisView), rather than filling it in a later frame.
//! The staging system runs before the per-frame registry→provider sync precisely
//! so a same-frame spawn lands the bitmap ahead of scene build.
//!
//! # Why URI registration rather than `Image.Source = bitmap`
//!
//! Assigning a Rust-built `ImageSource` straight onto an element
//! (`Image::SetSource`) is an `unsafe` raw-pointer call in `noesis_runtime`, and
//! this crate is `unsafe_code = forbid` with no safe typed setter for it in the
//! runtime (unlike brushes' `set_background` / transforms' `set_render_transform`).
//! The texture-provider URI path is the safe route, and it is the canonical one
//! for tiled / streamed bitmaps anyway.
//!
//! # Observable
//!
//! The bridge *polls back* the live element and emits [`NoesisImageChanged`] when
//! a watched `<Image>`'s resolved size changes. Noesis sizes an `Image` from its
//! source's pixel dimensions, which it obtains from our texture provider's
//! `GetTextureInfo` during the layout pass, so a Rust-registered `13x7` bitmap
//! drives the element's `ActualWidth`/`ActualHeight` to `13`/`7` (with the
//! element authored `Stretch="None"`), with **no GPU render pass required**. An
//! unresolvable `Source` (nothing registered for its `uri`) measures to `0`, the
//! built-in negative control: a no-op apply, a wrong `uri`, or a wrong size all
//! read back differently from the requested dimensions.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system stages each view's bytes into the registry, polls the
//! element read-back, and emits messages directly. No cross-world queues.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::prelude::*;

use crate::image::ImageRegistry;
use crate::render::{NoesisRenderState, NoesisSet};

/// A Rust-provided bitmap, declarative side: tightly-packed RGBA8 pixels plus
/// the `uri` the target element's `Source` references. Staged into the shared
/// [`ImageRegistry`] at apply time so the live texture provider resolves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageBitmap {
    /// The `Source` URI the element's XAML references (e.g. `dm-bitmap://logo`).
    pub uri: String,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Tightly-packed RGBA8 pixels; `bytes.len()` must be `width * height * 4`.
    /// `Arc` so staging into the registry shares the allocation rather than
    /// copying every frame.
    pub bytes: Arc<Vec<u8>>,
}

/// Per-view imaging bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisImaging {
    /// Desired bitmap per `x:Name`. Staged into the [`ImageRegistry`] whenever
    /// this component changes; writes to the same name apply last-wins.
    pub images: HashMap<String, ImageBitmap>,
}

impl NoesisImaging {
    /// An empty bridge with no bitmaps. Chain [`set`](Self::set) to add the
    /// element bitmaps you want staged.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: drive element `name`'s `<Image>` from `bytes` (tightly-packed
    /// RGBA8, `width * height * 4` long), staged under `uri`, which must match
    /// the element's authored `Source`.
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

/// What was read back from a watched `<Image>` element after layout: the
/// observable proof a Rust-provided bitmap actually reached the live element. A
/// `Source` that resolved to our registered bytes carries the bitmap's pixel
/// dimensions in [`actual_size`](Self::actual_size); an unresolvable source
/// measures to `[0.0, 0.0]`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageReadback {
    /// Whether the element currently has a non-null `Source` (`ImageSource`) DP.
    pub has_source: bool,
    /// The element's `[ActualWidth, ActualHeight]` after the last layout pass.
    /// Equals the registered bitmap's `[width, height]` once resolved (element
    /// authored `Stretch="None"`); `[0.0, 0.0]` for an unresolvable source.
    pub actual_size: [f32; 2],
}

/// Emitted when a watched `<Image>`'s read-back changes from the previous frame's
/// snapshot. Proves a [`ImageBitmap`] reached the element: once the texture
/// provider resolves the staged bytes, `readback.actual_size` becomes the
/// bitmap's pixel size. Read with `MessageReader<NoesisImageChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisImageChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose image changed.
    pub view: Entity,
    /// `x:Name` of the watched `<Image>` element.
    pub name: String,
    /// What was read back from the live element.
    pub readback: ImageReadback,
}

/// Per-entity record of the registry URIs each [`NoesisImaging`] currently
/// stages, maintained by [`stage_imaging_bitmaps`]. [`reap_removed_imaging`]
/// reads it to reclaim exactly the bitmaps a removed component owned — minus any
/// a surviving imaging component still stages under the same URI. Without it a
/// removed component's full-size RGBA buffers stay in the [`ImageRegistry`] for
/// the life of the process.
#[derive(Resource, Default)]
pub(crate) struct StagedImagingUris(HashMap<Entity, HashSet<String>>);

/// Stage every changed [`NoesisImaging`]'s bitmaps into the [`ImageRegistry`].
/// Runs before the registry→provider sync (and thus before scene build) so a
/// same-frame spawn lands the bytes ahead of Noesis's one-shot source
/// resolution. Independent of [`NoesisRenderState`]: a plain resource write.
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

/// Reap a removed [`NoesisImaging`]: drop the registry bitmaps it staged (those
/// no surviving imaging component still references) and its read-back snapshots.
/// Runs before [`NoesisSet::Sync`] (and after [`stage_imaging_bitmaps`]) so a
/// buffer removed this frame is gone before the registry→provider sync copies it.
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

/// Poll each view's watched `<Image>` elements and emit [`NoesisImageChanged`]
/// when a resolved size / source presence changes. Runs in
/// [`NoesisSet::Apply`], reading the layout the previous frame's drive produced.
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

/// Wires the per-view imaging bridge. Added transitively by [`crate::NoesisPlugin`].
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
