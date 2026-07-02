//! wgpu-backed [`noesis_runtime::render_device::RenderDevice`] implementation.
//!
//! - Pipelines live in [`PipelineCache`], keyed on `(shader, render_state,
//!   vertex_format)` and built lazily on first `draw_batch` per key.
//! - Vertex layouts come from the SDK lookup tables via
//!   [`crate::render_device::vertex_layout`].
//! - Shader source is one preprocessed WGSL template (`shaders/noesis.wgsl`);
//!   the active define set per `Shader::Enum` lives in
//!   [`crate::render_device::shader_defines`].
//! - Uniform uploads use ring buffers with dynamic-offset bind groups (see
//!   `UniformRing`), so each batch in a multi-batch frame reads its own slot.
//! - Textures and render targets are tracked in
//!   `WgpuRenderDevice::textures` / `WgpuRenderDevice::render_targets` and
//!   dropped when their handles release.
//! - Each `begin_*_render` opens its own encoder, submitted at the matching
//!   `end_*_render`. `draw_batch` renders into the onscreen `target_view`
//!   while the onscreen encoder is active, and into the current RT's color
//!   attachment while the offscreen encoder is active (selected via
//!   `set_render_target`).
//! - The onscreen target is optional and set per frame via
//!   [`WgpuRenderDevice::set_onscreen_target`], matching the render-graph flow
//!   where the target view comes from Bevy's `ViewTarget` or a graph-node
//!   intermediate texture.

use std::collections::HashMap;
use std::num::NonZeroU64;

use noesis_runtime::render_device::types::{
    Batch, DeviceCaps, SIZE_FOR_FORMAT, SamplerState, Shader, TextureFormat, Tile,
};
use noesis_runtime::render_device::{
    RenderDevice, RenderTargetBinding, RenderTargetDesc, RenderTargetHandle, TextureBinding,
    TextureDesc, TextureHandle, TextureRect,
};

use crate::render_device::pipeline::{PipelineCache, PipelineKey, STENCIL_FORMAT};

const DYNAMIC_VB_SIZE: u64 = 512 * 1024;
const DYNAMIC_IB_SIZE: u64 = 128 * 1024;

// Vertex-shader uniform buffer: cbuffer0_vs (mat4 projection, 16 floats = 64
// bytes) followed by cbuffer1_vs (glyph-atlas size, 2 floats padded to vec4 =
// 16 bytes). Total 80B. The SDF vertex shader reads `glyph_size.xy`; every
// other shader ignores the trailing 16 bytes. Layout matches `VsUniforms`
// in `shaders/noesis.wgsl` (mat4x4 + vec4).
const VS_UNIFORM_SIZE: u64 = 80;
/// Byte offset of `cbuffer1_vs` within the VS uniform slot.
const VS_GLYPH_SIZE_OFFSET: usize = 64;

// Pixel-shader uniform buffer 0: matches Noesis cbuffer0_ps (8 floats = 32
// bytes), exposed in WGSL as `array<vec4<f32>, 2>`.
const PS_UNIFORM0_SIZE: u64 = 32;

// Pixel-shader uniform buffer 1: Noesis cbuffer1_ps. Declared float[128] in the
// GL reference but only EFFECT_SHADOW (7 floats) / EFFECT_BLUR (1 float) read
// it, so we bind the first 8 floats (32 bytes) as `array<vec4<f32>, 2>`. Bound
// at group(1) binding(1); see the `HAS_CBUFFER1_PS` block in `noesis.wgsl`.
const PS_UNIFORM1_SIZE: u64 = 32;

// Upper bound on `draw_batch` calls per frame. Noesis.xaml is well under 100
// batches; 1024 gives comfortable headroom. Raise if real scenes hit the cap.
const UNIFORM_RING_SLOTS: u32 = 1024;

/// Color format every RT allocates with, and the format the pipeline cache
/// compiles all pipelines against. Onscreen views handed to
/// [`WgpuRenderDevice::set_onscreen_target`] must also be this format.
const RT_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Stencil attachment format for render targets created with `needs_stencil`
/// and the onscreen stencil. Shared with the pipeline cache so the pipeline's
/// `depth_stencil` format matches the attachment.
const RT_STENCIL_FORMAT: wgpu::TextureFormat = STENCIL_FORMAT;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum FramePhase {
    /// No encoder live.
    Idle,
    /// Inside `begin_offscreen_render` / `end_offscreen_render`. Draws target
    /// the currently-selected render target.
    Offscreen,
    /// Inside `begin_onscreen_render` / `end_onscreen_render`. Draws target
    /// `target_view`.
    Onscreen,
}

/// wgpu-backed implementation of [`noesis_runtime::render_device::RenderDevice`].
///
/// Noesis hands this device its draw stream (textures, render targets, and
/// per-batch geometry) and it translates each call into wgpu work: uploading
/// vertices and uniforms, building or fetching pipelines from the
/// [`PipelineCache`], and recording draws into the active encoder. Construct one
/// with [`WgpuRenderDevice::new`] from a Bevy `wgpu::Device` / `wgpu::Queue`
/// pair, then point each onscreen frame at its target view with
/// [`WgpuRenderDevice::set_onscreen_target`].
///
/// Lives on the render-app thread alongside the Noesis `Renderer`; the wgpu
/// handles and resource maps it holds aren't meant to cross the main/render
/// boundary.
pub struct WgpuRenderDevice {
    device: wgpu::Device,
    queue: wgpu::Queue,

    // Dynamic geometry: one growable stream each for vertices and indices.
    // Noesis fills a phase through repeated map/unmap cycles; each unmap
    // appends at a running cursor so draws recorded earlier in the phase still
    // read their own segment once the encoder submits (see `GeometryStream`).
    vertex_stream: GeometryStream,
    index_stream: GeometryStream,

    // Uniforms: ring-buffered with dynamic-offset bind groups so each batch
    // reads its own slice instead of racing on a single slot.
    vs_ring: UniformRing,
    vs_uniform_bind_group: wgpu::BindGroup,
    // group(1): cbuffer0_ps at binding(0) (`ps_ring`) + cbuffer1_ps at
    // binding(1) (`ps1_ring`). Both are dynamic-offset bindings in one bind
    // group; each draw passes `[ps0_offset, ps1_offset]`.
    ps_ring: UniformRing,
    ps1_ring: UniformRing,
    ps_uniform_bind_group: wgpu::BindGroup,

    // Pattern texture + sampler bind group (group 2). Shaders that don't
    // use PAINT_PATTERN still need a bind group at this slot since wgpu
    // requires every declared group to be set; `dummy_pattern_bg` fulfills
    // that. PAINT_PATTERN shaders use a per-draw bind group built from the
    // batch's pattern + sampler state, cached in `pattern_bind_groups`.
    pattern_bind_group_layout: wgpu::BindGroupLayout,
    dummy_pattern_bg: wgpu::BindGroup,
    samplers: HashMap<SamplerState, wgpu::Sampler>,
    pattern_bind_groups: HashMap<(TextureHandle, SamplerState), wgpu::BindGroup>,

    // Image (+shadow) bind group (group 3). Bindings 0/1 are the `image`
    // texture+sampler used by EFFECT_OPACITY / UPSAMPLE / SHADOW / BLUR;
    // bindings 2/3 are the `shadow` texture+sampler used only by SHADOW / BLUR.
    // Every pipeline binds *something* at group(3) because the layout is
    // shared; shaders that don't sample a slot get the dummy texture there.
    // Cache key is `(image, shadow)` where `shadow` is `None` for the
    // opacity/upsample shaders (dummy bound at 2/3).
    image_bind_group_layout: wgpu::BindGroupLayout,
    dummy_image_bg: wgpu::BindGroup,
    image_bind_groups: HashMap<ImageBindGroupKey, wgpu::BindGroup>,

    // Dummy 1×1 white texture view + sampler, kept so per-draw image bind
    // groups can fill unused `shadow` (and `image`) slots without a real
    // texture. The dummy texture itself stays alive behind these.
    #[allow(dead_code)] // owns the allocation behind `dummy_view`
    dummy_texture: wgpu::Texture,
    dummy_view: wgpu::TextureView,
    dummy_sampler: wgpu::Sampler,

    pipelines: PipelineCache,

    // Resources: dropped when their handles are released.
    textures: HashMap<TextureHandle, GpuTexture>,
    render_targets: HashMap<RenderTargetHandle, GpuRenderTarget>,

    // Onscreen target + frame state. The view must be set via
    // [`WgpuRenderDevice::set_onscreen_target`] before the first onscreen
    // frame; its format must be [`RT_COLOR_FORMAT`].
    target_view: Option<wgpu::TextureView>,
    // Stencil buffer for the onscreen target. The Noesis main (onscreen) render
    // clips with the stencil buffer (e.g. ScrollViewer content viewport), so
    // the intermediate needs one too. (Re)allocated by `set_onscreen_target`
    // to match the target size.
    onscreen_stencil: Option<GpuStencil>,
    /// Whether the onscreen stencil has been cleared since the current frame's
    /// `begin_onscreen_render`. Stencil starts undefined; Noesis assumes 0, so
    /// the first onscreen draw clears it.
    onscreen_stencil_cleared: bool,
    /// As above, for the offscreen RT bound by the latest `set_render_target`.
    /// Reset on each `set_render_target` (which the protocol says discards the
    /// surface's existing content).
    current_rt_stencil_cleared: bool,

    phase: FramePhase,
    encoder: Option<wgpu::CommandEncoder>,
    current_rt: Option<RenderTargetHandle>,
    current_tile: Option<Tile>,

    /// Test-only override for the pattern texture / sampler used in
    /// `draw_batch`. When set, `PAINT_PATTERN` draws use this handle instead
    /// of resolving `batch.pattern`. Lets the standalone-wgpu tests exercise
    /// the pattern pipeline without fabricating a Noesis-owned `Texture*`.
    /// Always `None` on the Noesis-driven path.
    forced_pattern: Option<(TextureHandle, SamplerState)>,

    /// Test-only override for the group(3) image texture, mirroring
    /// [`Self::forced_pattern`]. Lets standalone-wgpu tests exercise the
    /// OPACITY / UPSAMPLE image path without a Noesis-owned `Texture*`.
    /// Always `None` on the Noesis-driven path.
    forced_image: Option<(TextureHandle, SamplerState)>,

    /// Test-only override for the group(3) shadow texture (bindings 2/3),
    /// mirroring [`Self::forced_image`]. Lets standalone-wgpu tests exercise
    /// the SHADOW / BLUR path. Always `None` on the Noesis-driven path.
    forced_shadow: Option<(TextureHandle, SamplerState)>,

    next_handle: u64,
}

/// Cache key for a group(3) bind group: the `image` slot plus an optional
/// `shadow` slot. `None` shadow binds the dummy texture at bindings 2/3 (the
/// opacity / upsample shaders that don't read `shadow`).
type ImageBindGroupKey = (
    TextureHandle,
    SamplerState,
    Option<(TextureHandle, SamplerState)>,
);

/// A texture owned by the render device, reachable from a [`TextureHandle`].
///
/// `view`, `width`, and `height` are cached for the sampler bind-group path;
/// `texture` is what `update_texture` writes into.
#[allow(dead_code)] // some fields read only on the sampler bind-group path
struct GpuTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    noesis_format: TextureFormat,
    width: u32,
    height: u32,
    num_levels: u32,
}

/// A stencil attachment owned by the device (onscreen target's stencil). The
/// `texture` is kept alive for the lifetime of `view`; rendering binds `view`.
struct GpuStencil {
    #[allow(dead_code)] // keeps the allocation alive behind `view`
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
}

/// A render target owned by the render device, reachable from a
/// [`RenderTargetHandle`]. The associated resolve texture lives in the
/// parent [`WgpuRenderDevice::textures`] map under `resolve_handle`.
///
/// `resolve_handle` isn't read after construction (the resolve texture is
/// fetched directly via [`WgpuRenderDevice::texture`]), but it's kept so the
/// resource stays alive for its lifetime. `stencil` (color/stencil texture +
/// view) is read by `draw_batch` to attach the stencil for clip/mask draws.
struct GpuRenderTarget {
    /// View rendered into by `draw_batch`. Same underlying texture as the
    /// resolve texture; `sample_count` is restricted to 1, so this always
    /// aliases the resolve.
    color_view: wgpu::TextureView,
    #[allow(dead_code)] // kept to own the resolve texture's lifetime
    resolve_handle: TextureHandle,
    stencil: Option<(wgpu::Texture, wgpu::TextureView)>,
    width: u32,
    height: u32,
}

impl WgpuRenderDevice {
    /// Build a new render device. The onscreen target view is not set yet;
    /// callers must invoke [`Self::set_onscreen_target`] before the first
    /// onscreen frame. Offscreen render targets are allocated on demand by
    /// [`RenderDevice::create_render_target`].
    ///
    /// All pipelines are compiled against `RT_COLOR_FORMAT`
    /// (`Rgba8Unorm`); onscreen views handed in later must match that format.
    #[must_use]
    #[allow(clippy::too_many_lines)] // wgpu setup is linear and hard to split usefully
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let vertex_stream = GeometryStream::new(
            &device,
            "noesis_runtime vertex stream",
            DYNAMIC_VB_SIZE,
            wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        );
        let index_stream = GeometryStream::new(
            &device,
            "noesis_runtime index stream",
            DYNAMIC_IB_SIZE,
            wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        );

        let uniform_alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let vs_ring = UniformRing::new(
            &device,
            "noesis_runtime vs_uniforms ring (mat4 projection)",
            VS_UNIFORM_SIZE,
            UNIFORM_RING_SLOTS,
            uniform_alignment,
        );
        let ps_ring = UniformRing::new(
            &device,
            "noesis_runtime ps_uniforms0 ring (cbuffer0_ps[8])",
            PS_UNIFORM0_SIZE,
            UNIFORM_RING_SLOTS,
            uniform_alignment,
        );
        let ps1_ring = UniformRing::new(
            &device,
            "noesis_runtime ps_uniforms1 ring (cbuffer1_ps[8])",
            PS_UNIFORM1_SIZE,
            UNIFORM_RING_SLOTS,
            uniform_alignment,
        );

        let vs_uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("noesis_runtime vs_uniforms layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(VS_UNIFORM_SIZE),
                    },
                    count: None,
                }],
            });
        let ps_uniform_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("noesis_runtime ps_uniforms layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: NonZeroU64::new(PS_UNIFORM0_SIZE),
                        },
                        count: None,
                    },
                    // cbuffer1_ps: only SHADOW / BLUR read it, but the shared
                    // layout always declares it so every pipeline matches.
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: NonZeroU64::new(PS_UNIFORM1_SIZE),
                        },
                        count: None,
                    },
                ],
            });

        // Bind groups expose a *single* struct-sized window into each ring
        // buffer. The dynamic offset passed to set_bind_group slides that
        // window to the per-batch slot.
        let vs_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_runtime vs_uniforms"),
            layout: &vs_uniform_bind_group_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: vs_ring.buffer(),
                    offset: 0,
                    size: NonZeroU64::new(VS_UNIFORM_SIZE),
                }),
            }],
        });
        let ps_uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_runtime ps_uniforms"),
            layout: &ps_uniform_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: ps_ring.buffer(),
                        offset: 0,
                        size: NonZeroU64::new(PS_UNIFORM0_SIZE),
                    }),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: ps1_ring.buffer(),
                        offset: 0,
                        size: NonZeroU64::new(PS_UNIFORM1_SIZE),
                    }),
                },
            ],
        });

        // Group(2): pattern texture + pattern sampler. Shared layout for
        // both PAINT_PATTERN draws and the dummy used by non-pattern draws.
        let pattern_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("noesis_runtime pattern layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        // Group(3): image texture+sampler (bindings 0/1) plus the shadow
        // texture+sampler (bindings 2/3) co-bound for SHADOW / BLUR. Separate
        // group from pattern so existing pipelines keep their group(2)-only
        // setup; OPACITY-class shaders layer the offscreen image on top and
        // leave the shadow slots dummy.
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let sampler_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let image_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("noesis_runtime image+shadow layout"),
                entries: &[
                    texture_entry(0),
                    sampler_entry(1),
                    texture_entry(2),
                    sampler_entry(3),
                ],
            });

        // Dummy 1x1 white texture + default sampler for non-pattern draws.
        // The pipeline layout always has group(2) so every draw must bind
        // something; the shader just doesn't sample it.
        let dummy_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("noesis_runtime dummy pattern"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &dummy_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0xFF_u8; 4],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
        let dummy_view = dummy_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let dummy_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("noesis_runtime dummy sampler"),
            ..Default::default()
        });
        let dummy_pattern_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_runtime dummy pattern bg"),
            layout: &pattern_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&dummy_sampler),
                },
            ],
        });
        let dummy_image_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_runtime dummy image bg"),
            layout: &image_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&dummy_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&dummy_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&dummy_sampler),
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("noesis_runtime pipeline layout"),
            bind_group_layouts: &[
                &vs_uniform_bind_group_layout,
                &ps_uniform_bind_group_layout,
                &pattern_bind_group_layout,
                &image_bind_group_layout,
            ],
            push_constant_ranges: &[],
        });

        let pipelines = PipelineCache::new(device.clone(), pipeline_layout, RT_COLOR_FORMAT);

        Self {
            device,
            queue,
            vertex_stream,
            index_stream,
            vs_ring,
            vs_uniform_bind_group,
            ps_ring,
            ps1_ring,
            ps_uniform_bind_group,
            pattern_bind_group_layout,
            dummy_pattern_bg,
            samplers: HashMap::new(),
            pattern_bind_groups: HashMap::new(),
            image_bind_group_layout,
            dummy_image_bg,
            image_bind_groups: HashMap::new(),
            dummy_texture,
            dummy_view,
            dummy_sampler,
            pipelines,
            textures: HashMap::new(),
            render_targets: HashMap::new(),
            target_view: None,
            onscreen_stencil: None,
            onscreen_stencil_cleared: false,
            current_rt_stencil_cleared: false,
            phase: FramePhase::Idle,
            encoder: None,
            current_rt: None,
            current_tile: None,
            forced_pattern: None,
            forced_image: None,
            forced_shadow: None,
            next_handle: 1,
        }
    }

    /// Test-only: force `PAINT_PATTERN` draws to use `(handle, state)`
    /// instead of resolving the batch's `pattern` pointer. Standalone wgpu
    /// tests can't produce a Noesis-owned `Texture*` so they use this hook.
    /// Production code never calls this.
    pub fn test_set_forced_pattern(&mut self, forced: Option<(TextureHandle, SamplerState)>) {
        self.forced_pattern = forced;
    }

    /// Test-only sibling of [`Self::test_set_forced_pattern`] for the group(3)
    /// image texture. Production code never calls this.
    pub fn test_set_forced_image(&mut self, forced: Option<(TextureHandle, SamplerState)>) {
        self.forced_image = forced;
    }

    /// Test-only sibling of [`Self::test_set_forced_image`] for the group(3)
    /// shadow texture (bindings 2/3), exercising the SHADOW / BLUR path.
    /// Production code never calls this.
    pub fn test_set_forced_shadow(&mut self, forced: Option<(TextureHandle, SamplerState)>) {
        self.forced_shadow = forced;
    }

    /// Build or fetch the cached pattern bind group for `(texture, state)`.
    /// Dropped textures invalidate entries via `drop_texture`.
    fn pattern_bind_group_for(
        &mut self,
        handle: TextureHandle,
        state: SamplerState,
    ) -> &wgpu::BindGroup {
        if self.pattern_bind_groups.contains_key(&(handle, state)) {
            return self
                .pattern_bind_groups
                .get(&(handle, state))
                .expect("just checked contains_key");
        }
        // Build outside the borrow on `pattern_bind_groups`.
        let view = self
            .textures
            .get(&handle)
            .map(|t| &t.view)
            .expect("pattern_bind_group_for: unknown TextureHandle");
        let sampler = self
            .samplers
            .entry(state)
            .or_insert_with(|| build_sampler(&self.device, state));
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_runtime pattern bg"),
            layout: &self.pattern_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        self.pattern_bind_groups
            .entry((handle, state))
            .or_insert(bg)
    }

    /// Sibling of [`Self::pattern_bind_group_for`] for the group(3) image
    /// (+shadow) texture. Bindings 0/1 hold the `image` texture (whatever
    /// offscreen RT Noesis rendered the layer into); bindings 2/3 hold the
    /// `shadow` texture for SHADOW / BLUR, or the dummy when `shadow` is
    /// `None` (opacity / upsample shaders that don't read it).
    fn image_bind_group_for(
        &mut self,
        image: (TextureHandle, SamplerState),
        shadow: Option<(TextureHandle, SamplerState)>,
    ) -> &wgpu::BindGroup {
        let key: ImageBindGroupKey = (image.0, image.1, shadow);
        if self.image_bind_groups.contains_key(&key) {
            return self
                .image_bind_groups
                .get(&key)
                .expect("just checked contains_key");
        }
        // Ensure the samplers exist before borrowing the maps for the bind
        // group entries.
        self.samplers
            .entry(image.1)
            .or_insert_with(|| build_sampler(&self.device, image.1));
        if let Some((_, sstate)) = shadow {
            self.samplers
                .entry(sstate)
                .or_insert_with(|| build_sampler(&self.device, sstate));
        }

        let image_view = self
            .textures
            .get(&image.0)
            .map(|t| &t.view)
            .expect("image_bind_group_for: unknown image TextureHandle");
        let image_sampler = &self.samplers[&image.1];
        let (shadow_view, shadow_sampler) = match shadow {
            Some((handle, sstate)) => {
                let view = self
                    .textures
                    .get(&handle)
                    .map(|t| &t.view)
                    .expect("image_bind_group_for: unknown shadow TextureHandle");
                (view, &self.samplers[&sstate])
            }
            None => (&self.dummy_view, &self.dummy_sampler),
        };
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis_runtime image+shadow bg"),
            layout: &self.image_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(image_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(image_sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(shadow_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(shadow_sampler),
                },
            ],
        });
        self.image_bind_groups.entry(key).or_insert(bg)
    }

    /// Point the onscreen phase at `view`. Used by the Bevy plugin so each
    /// frame's UI lands in the graph-node-owned intermediate texture (later
    /// blitted into the camera's `ViewTarget`). The view's format must be
    /// `RT_COLOR_FORMAT`; `wgpu::TextureView` erases format, so the invariant
    /// is only debug-asserted at the call site.
    ///
    /// Call every frame before driving a frame, or once at setup for
    /// fixed-target tests; either pattern is fine.
    ///
    /// `width`/`height` are the target's pixel dimensions; the device keeps a
    /// matching `Stencil8` buffer so the Noesis onscreen render can clip with
    /// the stencil (`ScrollViewer` content viewport, `ClipToBounds`, opacity
    /// masks). The stencil is reallocated only when the size changes.
    ///
    /// # Panics
    ///
    /// Panics if called while a frame phase (offscreen or onscreen) is
    /// active; swap only between `end_*_render` and the next `begin_*_render`.
    pub fn set_onscreen_target(&mut self, view: wgpu::TextureView, width: u32, height: u32) {
        assert_eq!(
            self.phase,
            FramePhase::Idle,
            "set_onscreen_target called while a frame phase is active",
        );
        let need_alloc = self
            .onscreen_stencil
            .as_ref()
            .is_none_or(|s| s.width != width || s.height != height);
        if need_alloc {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("noesis_runtime onscreen stencil"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: RT_STENCIL_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.onscreen_stencil = Some(GpuStencil {
                texture,
                view,
                width,
                height,
            });
        }
        self.target_view = Some(view);
    }

    /// Borrow the underlying `wgpu::Texture` for `handle`. Returns `None` when
    /// the handle has been dropped (or was never registered). Used by the
    /// Bevy plugin to hand the resolve texture to Bevy's asset system.
    #[must_use]
    pub fn texture(&self, handle: TextureHandle) -> Option<&wgpu::Texture> {
        self.textures.get(&handle).map(|t| &t.texture)
    }

    /// Dimensions the render target `handle` was created with. `None` when
    /// the handle has been dropped.
    #[must_use]
    pub fn render_target_size(&self, handle: RenderTargetHandle) -> Option<(u32, u32)> {
        self.render_targets
            .get(&handle)
            .map(|rt| (rt.width, rt.height))
    }

    fn alloc_handle(&mut self) -> NonZeroU64 {
        let h = self.next_handle;
        self.next_handle += 1;
        NonZeroU64::new(h).expect("alloc_handle starts at 1")
    }

    /// Allocate ring-buffer slots for this batch's uniforms and return the
    /// dynamic offsets to bind. The VS slot packs `cbuffer0_vs` (mat4) into
    /// bytes 0..64 and `cbuffer1_vs` (glyph-atlas size) into bytes 64..72;
    /// the trailing 8 bytes are zeroed so non-SDF shaders see vec4(0).
    fn upload_uniforms(&mut self, batch: &Batch) -> (u32, u32, u32) {
        let cbuf0 = batch.vertex_uniforms[0].as_bytes();
        let cbuf1 = batch.vertex_uniforms[1].as_bytes();
        let mut vs_buf = [0u8; VS_UNIFORM_SIZE as usize];
        let cbuf0_len = cbuf0.len().min(VS_GLYPH_SIZE_OFFSET);
        vs_buf[..cbuf0_len].copy_from_slice(&cbuf0[..cbuf0_len]);
        let cbuf1_len = cbuf1
            .len()
            .min(VS_UNIFORM_SIZE as usize - VS_GLYPH_SIZE_OFFSET);
        vs_buf[VS_GLYPH_SIZE_OFFSET..VS_GLYPH_SIZE_OFFSET + cbuf1_len]
            .copy_from_slice(&cbuf1[..cbuf1_len]);
        let vs_offset = self.vs_ring.write(&self.queue, &vs_buf);
        let ps_offset = self
            .ps_ring
            .write(&self.queue, batch.pixel_uniforms[0].as_bytes());
        // cbuffer1_ps: only SHADOW / BLUR populate pixel_uniforms[1]; the ring
        // zero-pads, so non-shadow draws still get a valid (zeroed) slot bound.
        let ps1_offset = self
            .ps1_ring
            .write(&self.queue, batch.pixel_uniforms[1].as_bytes());
        (vs_offset, ps_offset, ps1_offset)
    }
}

const fn round_up_to_4(n: usize) -> usize {
    (n + 3) & !3
}

const fn align_up_u64(n: u64, align: u64) -> u64 {
    (n + align - 1) & !(align - 1)
}

// ────────────────────────────────────────────────────────────────────────────
// UniformRing: ring buffer + scratch used to hand each `draw_batch` its own
// slice of a single UNIFORM buffer. The bind group binds a struct-sized window
// at offset 0; the dynamic offset passed to `set_bind_group` slides that
// window to the per-batch slot.
//
// Reset at the start of each frame (the first `begin_*_render` that opens an
// encoder). All writes for a frame land in the same submit, so the shader
// reads whatever the ring held at submission time, distinct per slot.
// ────────────────────────────────────────────────────────────────────────────

struct UniformRing {
    buffer: wgpu::Buffer,
    /// Bytes the shader actually reads from each slot.
    struct_size: u64,
    /// Distance between slot starts: `struct_size` rounded up to
    /// `min_uniform_buffer_offset_alignment`.
    slot_stride: u64,
    slot_capacity: u32,
    next_slot: u32,
    /// Zero-padded scratch sized to `struct_size`. Each `write` copies the
    /// batch-owned uniform bytes into the prefix and zero-fills the tail, then
    /// ships the whole thing via `queue.write_buffer`; the shader reads a
    /// fully-initialized slot regardless of the Noesis payload length.
    scratch: Vec<u8>,
}

impl UniformRing {
    fn new(
        device: &wgpu::Device,
        label: &str,
        struct_size: u64,
        slot_capacity: u32,
        alignment: u64,
    ) -> Self {
        assert!(
            struct_size.is_multiple_of(4),
            "uniform struct_size must be a multiple of 4"
        );
        let slot_stride = align_up_u64(struct_size, alignment);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: slot_stride * u64::from(slot_capacity),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            struct_size,
            slot_stride,
            slot_capacity,
            next_slot: 0,
            scratch: vec![0u8; struct_size as usize],
        }
    }

    fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    fn reset(&mut self) {
        self.next_slot = 0;
    }

    /// Claim the next slot, upload `bytes` into it (zero-padded to
    /// `struct_size`), and return the dynamic offset to bind.
    ///
    /// # Panics
    ///
    /// Panics if the ring is exhausted (more than `slot_capacity` batches in
    /// one frame). Raise [`UNIFORM_RING_SLOTS`] if real scenes hit it.
    fn write(&mut self, queue: &wgpu::Queue, bytes: &[u8]) -> u32 {
        assert!(
            self.next_slot < self.slot_capacity,
            "uniform ring (struct_size={}) exhausted at {} slots — raise UNIFORM_RING_SLOTS",
            self.struct_size,
            self.slot_capacity,
        );
        let slot = self.next_slot;
        self.next_slot += 1;
        let offset = u64::from(slot) * self.slot_stride;

        let len = bytes.len().min(self.struct_size as usize);
        self.scratch[..len].copy_from_slice(&bytes[..len]);
        self.scratch[len..].fill(0);
        queue.write_buffer(&self.buffer, offset, &self.scratch);

        u32::try_from(offset).expect("uniform ring offset overflowed u32")
    }
}

// ────────────────────────────────────────────────────────────────────────────
// GeometryStream: growable GPU buffer + CPU staging for one dynamic geometry
// stream (vertices or indices). Noesis fills a phase's geometry through repeated
// map/unmap cycles inside a single `begin_*_render` encoder; each `unmap`
// appends its bytes at a running `cursor` instead of overwriting offset 0, so a
// draw recorded earlier in the phase still reads its own segment once the
// encoder is submitted. `draw_batch` adds `segment_base` — the base of the
// segment the most recent `unmap` wrote — to the batch-relative offset. This is
// the vertex/index analogue of `UniformRing`; reset alongside the rings at the
// start of each phase.
// ────────────────────────────────────────────────────────────────────────────

struct GeometryStream {
    buffer: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
    /// Scratch the current `map` hands to Noesis; uploaded to `buffer` at
    /// `cursor` on `unmap`. Grown to fit the largest single map.
    staging: Vec<u8>,
    /// Bytes claimed by the in-flight `map`; `None` outside a map/unmap pair.
    mapped_bytes: Option<u32>,
    /// Byte offset for the next `unmap` within the phase. Reset to 0 by `reset`.
    cursor: u64,
    /// Base byte offset of the segment the most recent `unmap` wrote; added to
    /// batch-relative offsets in `draw_batch`.
    segment_base: u64,
}

impl GeometryStream {
    fn new(
        device: &wgpu::Device,
        label: &'static str,
        size: u64,
        usage: wgpu::BufferUsages,
    ) -> Self {
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage,
            mapped_at_creation: false,
        });
        Self {
            buffer,
            label,
            usage,
            staging: vec![0u8; size as usize],
            mapped_bytes: None,
            cursor: 0,
            segment_base: 0,
        }
    }

    fn buffer(&self) -> &wgpu::Buffer {
        &self.buffer
    }

    fn segment_base(&self) -> u64 {
        self.segment_base
    }

    fn reset(&mut self) {
        self.cursor = 0;
    }

    fn map(&mut self, bytes: u32) -> &mut [u8] {
        assert!(self.mapped_bytes.is_none(), "map without unmap");
        let len = bytes as usize;
        if len > self.staging.len() {
            self.staging.resize(len, 0);
        }
        self.mapped_bytes = Some(bytes);
        &mut self.staging[..len]
    }

    fn unmap(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        let bytes = self.mapped_bytes.take().expect("unmap without map");
        let padded = round_up_to_4(bytes as usize) as u64;
        // Grow when appending this segment would overflow. Draws recorded
        // earlier this phase keep a reference to the old buffer through the
        // encoder, and each draw only reads the segment its own `unmap` wrote,
        // so the old bytes left uncopied in the previous buffer are never read
        // again. Double for amortized O(1) growth.
        if self.cursor + padded > self.buffer.size() {
            let new_size = (self.cursor + padded).max(self.buffer.size() * 2);
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(self.label),
                size: new_size,
                usage: self.usage,
                mapped_at_creation: false,
            });
        }
        self.segment_base = self.cursor;
        queue.write_buffer(&self.buffer, self.cursor, &self.staging[..padded as usize]);
        self.cursor += padded;
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Noesis → wgpu format helpers
// ────────────────────────────────────────────────────────────────────────────

const fn wgpu_format_for(format: TextureFormat) -> wgpu::TextureFormat {
    match format {
        // Rgbx8's alpha channel is "unused" but wgpu has no Rgbx8 format; we
        // store as Rgba8 and let the shader ignore the alpha. The has_alpha
        // hint in the binding already advertises this.
        TextureFormat::Rgba8 | TextureFormat::Rgbx8 => wgpu::TextureFormat::Rgba8Unorm,
        TextureFormat::R8 => wgpu::TextureFormat::R8Unorm,
        // `TextureFormat` is `#[non_exhaustive]`; a format the SDK adds later
        // defaults to RGBA8. Never panic here: this runs inside a Noesis FFI
        // trampoline, where unwinding into C++ is UB.
        _ => wgpu::TextureFormat::Rgba8Unorm,
    }
}

const fn bytes_per_pixel(format: TextureFormat) -> u32 {
    match format {
        TextureFormat::Rgba8 | TextureFormat::Rgbx8 => 4,
        TextureFormat::R8 => 1,
        // See `wgpu_format_for`: non-exhaustive, FFI-safe default (4 = RGBA8).
        _ => 4,
    }
}

/// True when `shader` reads from a texture at group(2) binding(0). Keep in
/// sync with [`crate::render_device::shader_defines::defines_for_shader`]:
/// every variant that sets `HAS_PAINT_TEXTURE` must appear here. The "paint
/// texture" slot is reused across paint kinds: pattern/ramps/glyphs all
/// land at the same group(2) binding, and the Rust side picks the right
/// `Batch.*_handle()` based on shader.
const fn shader_uses_paint_texture(shader: u8) -> bool {
    shader == Shader::PATH_PATTERN.0
        || shader == Shader::PATH_AA_PATTERN.0
        || shader == Shader::PATH_PATTERN_CLAMP.0
        || shader == Shader::PATH_AA_PATTERN_CLAMP.0
        || shader == Shader::PATH_PATTERN_REPEAT.0
        || shader == Shader::PATH_AA_PATTERN_REPEAT.0
        || shader == Shader::PATH_PATTERN_MIRROR_U.0
        || shader == Shader::PATH_AA_PATTERN_MIRROR_U.0
        || shader == Shader::PATH_PATTERN_MIRROR_V.0
        || shader == Shader::PATH_AA_PATTERN_MIRROR_V.0
        || shader == Shader::PATH_PATTERN_MIRROR.0
        || shader == Shader::PATH_AA_PATTERN_MIRROR.0
        || shader == Shader::PATH_LINEAR.0
        || shader == Shader::PATH_AA_LINEAR.0
        || shader == Shader::PATH_RADIAL.0
        || shader == Shader::PATH_AA_RADIAL.0
        || shader == Shader::SDF_SOLID.0
        || shader == Shader::SDF_LCD_SOLID.0
        || shader == Shader::OPACITY_LINEAR.0
        || shader == Shader::OPACITY_RADIAL.0
        || shader == Shader::OPACITY_PATTERN.0
        || shader == Shader::OPACITY_PATTERN_CLAMP.0
        || shader == Shader::OPACITY_PATTERN_REPEAT.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_U.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_V.0
        || shader == Shader::OPACITY_PATTERN_MIRROR.0
        // DOWNSAMPLE/UPSAMPLE read the source image at group(2) `pattern`.
        || shader == Shader::DOWNSAMPLE.0
        || shader == Shader::UPSAMPLE.0
    // OPACITY_SOLID has paint = vertex color (no texture). SDF_LINEAR /
    // SDF_RADIAL / SDF_PATTERN_* / SHADOW / BLUR land with their
    // shader_defines entries.
}

/// Resolve which (texture, sampler) to bind at group(2) for `batch` based
/// on which paint variant its shader uses. Returns `None` for shaders that
/// don't read a paint texture (solid paths, effects, etc.).
fn batch_paint_texture(batch: &Batch) -> Option<(TextureHandle, SamplerState)> {
    match batch.shader.0 {
        s if s == Shader::PATH_PATTERN.0
            || s == Shader::PATH_AA_PATTERN.0
            || s == Shader::PATH_PATTERN_CLAMP.0
            || s == Shader::PATH_AA_PATTERN_CLAMP.0
            || s == Shader::PATH_PATTERN_REPEAT.0
            || s == Shader::PATH_AA_PATTERN_REPEAT.0
            || s == Shader::PATH_PATTERN_MIRROR_U.0
            || s == Shader::PATH_AA_PATTERN_MIRROR_U.0
            || s == Shader::PATH_PATTERN_MIRROR_V.0
            || s == Shader::PATH_AA_PATTERN_MIRROR_V.0
            || s == Shader::PATH_PATTERN_MIRROR.0
            || s == Shader::PATH_AA_PATTERN_MIRROR.0
            || s == Shader::OPACITY_PATTERN.0
            || s == Shader::OPACITY_PATTERN_CLAMP.0
            || s == Shader::OPACITY_PATTERN_REPEAT.0
            || s == Shader::OPACITY_PATTERN_MIRROR_U.0
            || s == Shader::OPACITY_PATTERN_MIRROR_V.0
            || s == Shader::OPACITY_PATTERN_MIRROR.0 =>
        {
            batch.pattern_handle().map(|h| (h, batch.pattern_sampler))
        }
        s if s == Shader::PATH_LINEAR.0
            || s == Shader::PATH_AA_LINEAR.0
            || s == Shader::PATH_RADIAL.0
            || s == Shader::PATH_AA_RADIAL.0
            || s == Shader::OPACITY_LINEAR.0
            || s == Shader::OPACITY_RADIAL.0 =>
        {
            batch.ramps_handle().map(|h| (h, batch.ramps_sampler))
        }
        s if s == Shader::SDF_SOLID.0 || s == Shader::SDF_LCD_SOLID.0 => {
            batch.glyphs_handle().map(|h| (h, batch.glyphs_sampler))
        }
        // DOWNSAMPLE/UPSAMPLE source the (to-be-)blurred layer at group(2).
        s if s == Shader::DOWNSAMPLE.0 || s == Shader::UPSAMPLE.0 => {
            batch.pattern_handle().map(|h| (h, batch.pattern_sampler))
        }
        _ => None,
    }
}

/// Mirror of [`shader_uses_paint_texture`] for OPACITY-class shaders that
/// sample the offscreen `image` at group(3).
const fn shader_uses_image_texture(shader: u8) -> bool {
    shader == Shader::OPACITY_SOLID.0
        || shader == Shader::OPACITY_LINEAR.0
        || shader == Shader::OPACITY_RADIAL.0
        || shader == Shader::OPACITY_PATTERN.0
        || shader == Shader::OPACITY_PATTERN_CLAMP.0
        || shader == Shader::OPACITY_PATTERN_REPEAT.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_U.0
        || shader == Shader::OPACITY_PATTERN_MIRROR_V.0
        || shader == Shader::OPACITY_PATTERN_MIRROR.0
        // UPSAMPLE blends the lower-res `image` (group 3) with `pattern`.
        || shader == Shader::UPSAMPLE.0
        // SHADOW / BLUR read `image` at bindings 0/1 and `shadow` at 2/3.
        || shader == Shader::SHADOW.0
        || shader == Shader::BLUR.0
}

/// True when `shader` additionally reads the `shadow` texture at group(3)
/// bindings 2/3 (co-bound with `image`). Only the SHADOW / BLUR effects do.
const fn shader_uses_shadow_texture(shader: u8) -> bool {
    shader == Shader::SHADOW.0 || shader == Shader::BLUR.0
}

fn wgpu_wrap_mode(wrap_raw: u8) -> wgpu::AddressMode {
    // ClampToZero (1) has no wgpu equivalent on downlevel defaults (needs a
    // border color); approximate with ClampToEdge. The CLAMP_PATTERN shader
    // variant discards out-of-rect samples in-shader, so the visible delta
    // is rare.
    //
    // MirrorU / MirrorV can't be expressed as one `AddressMode` (wgpu sets
    // per-axis via `address_mode_u/v/w`); pick MirrorRepeat for anything
    // mirrored.
    match wrap_raw {
        0 | 1 => wgpu::AddressMode::ClampToEdge,
        2 => wgpu::AddressMode::Repeat,
        3..=5 => wgpu::AddressMode::MirrorRepeat,
        other => panic!("unknown Noesis WrapMode raw value: {other}"),
    }
}

fn build_sampler(device: &wgpu::Device, state: SamplerState) -> wgpu::Sampler {
    let wrap = wgpu_wrap_mode(state.wrap_mode_raw());
    let filter = match state.minmag_filter_raw() {
        0 => wgpu::FilterMode::Nearest,
        _ => wgpu::FilterMode::Linear,
    };
    let mipmap_filter = match state.mip_filter_raw() {
        0 | 1 => wgpu::FilterMode::Nearest,
        _ => wgpu::FilterMode::Linear,
    };
    let lod_max = match state.mip_filter_raw() {
        0 => 0.25, // disabled: restrict to mip 0
        _ => 32.0,
    };

    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("noesis_runtime sampler"),
        address_mode_u: wrap,
        address_mode_v: wrap,
        address_mode_w: wrap,
        mag_filter: filter,
        min_filter: filter,
        mipmap_filter,
        lod_min_clamp: 0.0,
        lod_max_clamp: lod_max,
        compare: None,
        anisotropy_clamp: 1,
        border_color: None,
    })
}

// ────────────────────────────────────────────────────────────────────────────
// RenderDevice impl
// ────────────────────────────────────────────────────────────────────────────

impl RenderDevice for WgpuRenderDevice {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn caps(&self) -> DeviceCaps {
        DeviceCaps {
            center_pixel_offset: 0.0,
            linear_rendering: false,
            // The SDF_LCD_SOLID subpixel shader + SrcOver_Dual blend are
            // implemented (see noesis.wgsl / pipeline.rs), but reporting
            // `subpixel_rendering = true` makes Noesis emit the SDF_LCD_*
            // matrix on every device, which requires the wgpu
            // `DUAL_SOURCE_BLENDING` feature (not in downlevel defaults) and a
            // glyph-orientation-aware coverage that we can't validate against
            // the SDK (it ships no LCD reference). Kept off until the render
            // app negotiates the feature per-device and the algorithm is
            // verified; the path is exercised directly by `tests/wgpu_sdf_lcd`.
            subpixel_rendering: false,
            depth_range_zero_to_one: true,
            clip_space_y_inverted: false,
        }
    }

    fn create_texture(&mut self, desc: TextureDesc<'_>) -> TextureBinding {
        let handle = TextureHandle(self.alloc_handle());
        let wgpu_format = wgpu_format_for(desc.format);

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(desc.label),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: desc.num_levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu_format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        if let Some(levels) = desc.data {
            assert_eq!(
                levels.len() as u32,
                desc.num_levels,
                "create_texture: data.len() ({}) must equal num_levels ({})",
                levels.len(),
                desc.num_levels,
            );
            let bpp = bytes_per_pixel(desc.format);
            for (level, bytes) in levels.iter().enumerate() {
                let level_u32 = level as u32;
                let w = (desc.width >> level_u32).max(1);
                let h = (desc.height >> level_u32).max(1);
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &texture,
                        mip_level: level_u32,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    bytes,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(w * bpp),
                        rows_per_image: Some(h),
                    },
                    wgpu::Extent3d {
                        width: w,
                        height: h,
                        depth_or_array_layers: 1,
                    },
                );
            }
        }

        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let has_alpha = !matches!(desc.format, TextureFormat::Rgbx8);
        self.textures.insert(
            handle,
            GpuTexture {
                texture,
                view,
                noesis_format: desc.format,
                width: desc.width,
                height: desc.height,
                num_levels: desc.num_levels,
            },
        );

        TextureBinding {
            handle,
            width: desc.width,
            height: desc.height,
            has_mipmaps: desc.num_levels > 1,
            inverted: false,
            has_alpha,
        }
    }

    fn update_texture(
        &mut self,
        handle: TextureHandle,
        level: u32,
        rect: TextureRect,
        data: &[u8],
    ) {
        let tex = self
            .textures
            .get(&handle)
            .expect("update_texture: unknown TextureHandle");
        let bpp = bytes_per_pixel(tex.noesis_format);
        assert!(level < tex.num_levels, "update_texture level out of range");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex.texture,
                mip_level: level,
                origin: wgpu::Origin3d {
                    x: rect.x,
                    y: rect.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(rect.width * bpp),
                rows_per_image: Some(rect.height),
            },
            wgpu::Extent3d {
                width: rect.width,
                height: rect.height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn end_updating_textures(&mut self, _textures: &[TextureHandle]) {
        // wgpu handles state transitions implicitly; no barrier needed.
    }

    fn drop_texture(&mut self, handle: TextureHandle) {
        self.textures.remove(&handle);
        // Invalidate any pattern / image bind group referencing this
        // texture. Small caches; a linear scan is fine.
        self.pattern_bind_groups.retain(|(h, _), _| *h != handle);
        self.image_bind_groups.retain(|(img, _, shadow), _| {
            *img != handle && shadow.is_none_or(|(s, _)| s != handle)
        });
    }

    fn create_render_target(&mut self, desc: RenderTargetDesc<'_>) -> RenderTargetBinding {
        assert_eq!(
            desc.sample_count, 1,
            "Phase 4.B only supports sample_count = 1 (use PPAA for anti-aliasing); \
             MSAA support lands later",
        );

        let rt_handle = RenderTargetHandle(self.alloc_handle());
        let resolve_handle = TextureHandle(self.alloc_handle());

        let color_texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some(desc.label),
            size: wgpu::Extent3d {
                width: desc.width,
                height: desc.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: RT_COLOR_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let resolve_view = color_texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Stencil attachment. `draw_batch` attaches this view when the RT has
        // it, clears it once per frame, and builds pipelines with a matching
        // `depth_stencil` state driven by the batch's stencil mode (see
        // `PipelineKey::has_stencil` / `pipeline::depth_stencil_for`).
        let stencil = desc.needs_stencil.then(|| {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("{} stencil", desc.label)),
                size: wgpu::Extent3d {
                    width: desc.width,
                    height: desc.height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: RT_STENCIL_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
            (tex, view)
        });

        self.textures.insert(
            resolve_handle,
            GpuTexture {
                texture: color_texture,
                view: resolve_view,
                noesis_format: TextureFormat::Rgba8,
                width: desc.width,
                height: desc.height,
                num_levels: 1,
            },
        );
        self.render_targets.insert(
            rt_handle,
            GpuRenderTarget {
                color_view,
                resolve_handle,
                stencil,
                width: desc.width,
                height: desc.height,
            },
        );

        RenderTargetBinding {
            handle: rt_handle,
            resolve_texture: TextureBinding {
                handle: resolve_handle,
                width: desc.width,
                height: desc.height,
                has_mipmaps: false,
                inverted: false,
                has_alpha: true,
            },
        }
    }

    fn clone_render_target(&mut self, label: &str, src: RenderTargetHandle) -> RenderTargetBinding {
        // "Clone" here means "give me another RT that reuses the transient
        // buffers of src", a Noesis optimization for ping-pong post-process
        // chains. We allocate a fresh RT at the same dimensions; the buffer
        // sharing optimization is not implemented.
        let (width, height, needs_stencil) = {
            let src_rt = self
                .render_targets
                .get(&src)
                .expect("clone_render_target: unknown src RenderTargetHandle");
            (src_rt.width, src_rt.height, src_rt.stencil.is_some())
        };
        self.create_render_target(RenderTargetDesc {
            label,
            width,
            height,
            sample_count: 1,
            needs_stencil,
        })
    }

    fn drop_render_target(&mut self, handle: RenderTargetHandle) {
        // The resolve texture has its own `drop_texture` callback fired when
        // the C++ wrapper releases its Ptr<Texture>; we only own the color +
        // stencil attachments here.
        self.render_targets.remove(&handle);
    }

    fn begin_offscreen_render(&mut self) {
        assert_eq!(
            self.phase,
            FramePhase::Idle,
            "begin_offscreen_render while a frame phase is already active",
        );
        self.vs_ring.reset();
        self.ps_ring.reset();
        self.ps1_ring.reset();
        self.vertex_stream.reset();
        self.index_stream.reset();
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("noesis_runtime offscreen frame"),
                }),
        );
        self.phase = FramePhase::Offscreen;
        self.current_rt = None;
        self.current_tile = None;
    }

    fn end_offscreen_render(&mut self) {
        assert_eq!(
            self.phase,
            FramePhase::Offscreen,
            "end_offscreen_render without a matching begin_offscreen_render",
        );
        let encoder = self.encoder.take().expect("offscreen encoder missing");
        self.queue.submit(Some(encoder.finish()));
        self.phase = FramePhase::Idle;
        self.current_rt = None;
        self.current_tile = None;
    }

    fn begin_onscreen_render(&mut self) {
        assert_eq!(
            self.phase,
            FramePhase::Idle,
            "begin_onscreen_render while a frame phase is already active",
        );
        self.vs_ring.reset();
        self.ps_ring.reset();
        self.ps1_ring.reset();
        self.vertex_stream.reset();
        self.index_stream.reset();
        self.encoder = Some(
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("noesis_runtime onscreen frame"),
                }),
        );
        self.phase = FramePhase::Onscreen;
        self.onscreen_stencil_cleared = false;
    }

    fn end_onscreen_render(&mut self) {
        assert_eq!(
            self.phase,
            FramePhase::Onscreen,
            "end_onscreen_render without a matching begin_onscreen_render",
        );
        let encoder = self.encoder.take().expect("onscreen encoder missing");
        self.queue.submit(Some(encoder.finish()));
        self.phase = FramePhase::Idle;
    }

    fn set_render_target(&mut self, handle: RenderTargetHandle) {
        assert_eq!(
            self.phase,
            FramePhase::Offscreen,
            "set_render_target outside offscreen phase",
        );
        assert!(
            self.render_targets.contains_key(&handle),
            "set_render_target: unknown RenderTargetHandle",
        );
        self.current_rt = Some(handle);
        self.current_tile = None;
        // SetRenderTarget discards the surface's existing content (per the
        // RenderDevice protocol), so the next draw into this RT clears its
        // stencil before Noesis builds clip masks on top.
        self.current_rt_stencil_cleared = false;
    }

    fn begin_tile(&mut self, handle: RenderTargetHandle, tile: Tile) {
        assert_eq!(
            self.current_rt,
            Some(handle),
            "begin_tile for a handle that isn't the currently-bound RT",
        );
        self.current_tile = Some(tile);
    }

    fn end_tile(&mut self, handle: RenderTargetHandle) {
        assert_eq!(
            self.current_rt,
            Some(handle),
            "end_tile for a handle that isn't the currently-bound RT",
        );
        self.current_tile = None;
    }

    fn resolve_render_target(&mut self, _handle: RenderTargetHandle, _tiles: &[Tile]) {
        // sample_count == 1 → color attachment IS the resolve texture, so
        // there's nothing to copy.
    }

    fn map_vertices(&mut self, bytes: u32) -> &mut [u8] {
        self.vertex_stream.map(bytes)
    }
    fn unmap_vertices(&mut self) {
        self.vertex_stream.unmap(&self.device, &self.queue);
    }
    fn map_indices(&mut self, bytes: u32) -> &mut [u8] {
        self.index_stream.map(bytes)
    }
    fn unmap_indices(&mut self) {
        self.index_stream.unmap(&self.device, &self.queue);
    }

    fn draw_batch(&mut self, batch: &Batch) {
        // Resolve whether the active target carries a stencil attachment, and
        // whether this is the first draw into it this frame (so we clear the
        // stencil to 0 once; Noesis assumes a 0-initialized stencil and then
        // manages it via Clear/Incr/Decr ops). Done before the pipeline key so
        // `has_stencil` can drive the pipeline's depth_stencil declaration.
        let (has_stencil, clear_stencil) = match self.phase {
            FramePhase::Onscreen => {
                let has = self.onscreen_stencil.is_some();
                let clear = has && !self.onscreen_stencil_cleared;
                self.onscreen_stencil_cleared = true;
                (has, clear)
            }
            FramePhase::Offscreen => {
                let rt_handle = self
                    .current_rt
                    .expect("draw_batch during offscreen phase without set_render_target");
                let has = self
                    .render_targets
                    .get(&rt_handle)
                    .expect("current_rt dangles")
                    .stencil
                    .is_some();
                let clear = has && !self.current_rt_stencil_cleared;
                self.current_rt_stencil_cleared = true;
                (has, clear)
            }
            FramePhase::Idle => panic!("draw_batch outside begin/end_*_render"),
        };

        let key = PipelineKey::from_batch(batch, has_stencil);

        let (vs_offset, ps_offset, ps1_offset) = self.upload_uniforms(batch);
        self.pipelines.ensure(key);

        // Resolve / build the group(2) bind group before we take a &mut borrow
        // of `encoder`, so the shared-&/mutable-& pair stays non-overlapping.
        let pattern_slot = if shader_uses_paint_texture(batch.shader.0) {
            let slot = self.forced_pattern.unwrap_or_else(|| {
                batch_paint_texture(batch).expect(
                    "paint-texture batch with null texture handle — Noesis should always populate the right slot",
                )
            });
            let _ = self.pattern_bind_group_for(slot.0, slot.1);
            Some(slot)
        } else {
            None
        };

        // Group(3): the `image` slot (OPACITY / UPSAMPLE / SHADOW / BLUR) plus
        // the `shadow` slot co-bound for SHADOW / BLUR. The cache key carries
        // both; `None` shadow binds the dummy at bindings 2/3.
        let image_slot: Option<ImageBindGroupKey> = if shader_uses_image_texture(batch.shader.0) {
            let image = self.forced_image.unwrap_or_else(|| {
                let handle = batch.image_handle().expect(
                    "OPACITY/UPSAMPLE/SHADOW/BLUR batch with null image handle — Noesis should populate batch.image",
                );
                (handle, batch.image_sampler)
            });
            let shadow = if shader_uses_shadow_texture(batch.shader.0) {
                Some(self.forced_shadow.unwrap_or_else(|| {
                    let handle = batch.shadow_handle().expect(
                        "SHADOW/BLUR batch with null shadow handle — Noesis should populate batch.shadow",
                    );
                    (handle, batch.shadow_sampler)
                }))
            } else {
                None
            };
            let _ = self.image_bind_group_for(image, shadow);
            Some((image.0, image.1, shadow))
        } else {
            None
        };

        // `batch.vertex_offset` / `start_index` are relative to the segment the
        // most recent unmap wrote; add the segment base so this draw reads its
        // own geometry rather than whichever segment landed last in the buffer.
        let stride = u64::from(SIZE_FOR_FORMAT[key.vertex_format as usize]);
        let vertex_offset = self.vertex_stream.segment_base() + u64::from(batch.vertex_offset);
        let vertex_byte_count = u64::from(batch.num_vertices) * stride;
        let index_byte_offset = self.index_stream.segment_base() + u64::from(batch.start_index) * 2;
        let index_byte_count = u64::from(batch.num_indices) * 2;

        // Resolve the color attachment view + optional scissor based on the
        // active frame phase. Offscreen renders target the current RT's
        // color view and clip to `current_tile`; onscreen renders target
        // `target_view` with no scissor.
        let (target_view, scissor, stencil_view) = match self.phase {
            FramePhase::Onscreen => {
                let view = self
                    .target_view
                    .as_ref()
                    .expect("onscreen draw without set_onscreen_target");
                let stencil = self.onscreen_stencil.as_ref().map(|s| &s.view);
                (view, None, stencil)
            }
            FramePhase::Offscreen => {
                let rt_handle = self
                    .current_rt
                    .expect("draw_batch during offscreen phase without set_render_target");
                let rt = self
                    .render_targets
                    .get(&rt_handle)
                    .expect("current_rt dangles");
                // Tile coords have origin at the LOWER-left per the Noesis
                // docstring; wgpu's scissor origin is upper-left. Convert.
                let scissor = self.current_tile.map(|t| {
                    let y_top = rt.height.saturating_sub(t.y + t.height);
                    (t.x, y_top, t.width, t.height)
                });
                let stencil = rt.stencil.as_ref().map(|(_, view)| view);
                (&rt.color_view, scissor, stencil)
            }
            FramePhase::Idle => panic!("draw_batch outside begin/end_*_render"),
        };
        debug_assert_eq!(
            has_stencil,
            stencil_view.is_some(),
            "has_stencil must agree with the resolved stencil view",
        );

        let pipeline = self.pipelines.get(key);
        let vertex_buffer = self.vertex_stream.buffer();
        let index_buffer = self.index_stream.buffer();
        let vs_bg = &self.vs_uniform_bind_group;
        let ps_bg = &self.ps_uniform_bind_group;
        let pattern_bg = if let Some(slot) = pattern_slot {
            self.pattern_bind_groups
                .get(&slot)
                .expect("pattern bind group not cached")
        } else {
            &self.dummy_pattern_bg
        };
        let image_bg = if let Some(slot) = image_slot {
            self.image_bind_groups
                .get(&slot)
                .expect("image bind group not cached")
        } else {
            &self.dummy_image_bg
        };
        let encoder = self
            .encoder
            .as_mut()
            .expect("draw_batch outside begin/end_*_render");

        // Attach the stencil iff the target has one. The first draw into a
        // target this frame clears the stencil to 0; later draws load it so
        // Noesis's clip stack (Incr/Decr/Equal) accumulates across batches.
        let depth_stencil_attachment =
            stencil_view.map(|view| wgpu::RenderPassDepthStencilAttachment {
                view,
                depth_ops: None,
                stencil_ops: Some(wgpu::Operations {
                    load: if clear_stencil {
                        wgpu::LoadOp::Clear(0)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                }),
            });

        let mut rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("noesis_runtime draw_batch"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        rpass.set_pipeline(pipeline);
        if has_stencil {
            rpass.set_stencil_reference(u32::from(batch.stencil_ref));
        }
        rpass.set_bind_group(0, vs_bg, &[vs_offset]);
        rpass.set_bind_group(1, ps_bg, &[ps_offset, ps1_offset]);
        rpass.set_bind_group(2, pattern_bg, &[]);
        rpass.set_bind_group(3, image_bg, &[]);
        if let Some((x, y, w, h)) = scissor {
            rpass.set_scissor_rect(x, y, w, h);
        }
        rpass.set_vertex_buffer(
            0,
            vertex_buffer.slice(vertex_offset..vertex_offset + vertex_byte_count),
        );
        rpass.set_index_buffer(
            index_buffer.slice(index_byte_offset..index_byte_offset + index_byte_count),
            wgpu::IndexFormat::Uint16,
        );
        rpass.draw_indexed(0..batch.num_indices, 0, 0..1);
    }
}
