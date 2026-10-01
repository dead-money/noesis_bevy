//! Pipeline cache keyed on `(Shader, RenderState, VertexFormat)` plus the
//! lazy build path. Pipelines are constructed on first `draw_batch` for a
//! key and reused thereafter.
//!
//! `RenderState` and `VertexFormat` are part of the key so the same `Shader`
//! can produce multiple pipelines when batches differ in blend mode, stencil
//! mode, color-write mask, wireframe flag, or vertex stride.

use std::collections::HashMap;

use bevy::log::warn_once;
use noesis_runtime::render_device::types::{
    Batch, FORMAT_FOR_VERTEX, RenderState, VERTEX_FOR_SHADER,
};

use crate::render_device::shader_defines::defines_for_shader;
use crate::render_device::shader_preproc::preprocess;
use crate::render_device::vertex_layout::{attributes_for_format, stride_for_format};

const NOESIS_WGSL: &str = include_str!("shaders/noesis.wgsl");

/// Identifies a unique pipeline state combination. Each unique key produces
/// one cached `wgpu::RenderPipeline`.
///
/// `vertex_format` is derived from `shader` via the SDK lookup tables but is
/// stored explicitly so it participates in the hash (the key must roundtrip
/// through `HashMap`'s `Hash` cleanly).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PipelineKey {
    /// Raw `Shader::Enum` value selecting which WGSL variant to compile.
    pub shader: u8,
    /// Raw `RenderState` bits driving blend mode, stencil mode, color-write
    /// mask, and wireframe flag.
    pub render_state: u8,
    /// Raw `VertexFormat::Enum` value selecting the vertex stride and
    /// attribute layout. Derived from `shader` but stored so it hashes.
    pub vertex_format: u8,
    /// Whether the render pass this pipeline draws into has a stencil
    /// attachment. wgpu requires the pipeline's `depth_stencil` presence to
    /// match the pass's `depth_stencil_attachment`, and the same
    /// `(shader, render_state, vertex_format)` can be drawn both into a
    /// stenciled offscreen RT (and the onscreen intermediate) and into a
    /// stencil-less RT, so it has to be part of the key.
    pub has_stencil: bool,
}

impl PipelineKey {
    /// Derive the key for a draw `batch`, looking the vertex format up from
    /// the batch's shader. `has_stencil` records whether the destination
    /// render pass carries a stencil attachment so stenciled and stencil-less
    /// passes get distinct pipelines.
    #[must_use]
    pub fn from_batch(batch: &Batch, has_stencil: bool) -> Self {
        let vshader = VERTEX_FOR_SHADER[batch.shader.0 as usize];
        let vfmt = FORMAT_FOR_VERTEX[vshader as usize];
        Self {
            shader: batch.shader.0,
            render_state: batch.render_state.0,
            vertex_format: vfmt,
            has_stencil,
        }
    }
}

/// Stencil attachment format used by render targets and the onscreen stencil.
/// `Stencil8` is the tightest format that covers Noesis's clip/mask stencil
/// ops; we don't allocate depth because the device exposes no depth-buffered
/// caps (the `*_ZTest` stencil modes degrade to their non-depth twins).
pub const STENCIL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Stencil8;

/// Build the `wgpu::DepthStencilState` for a `RenderState`'s stencil mode.
///
/// The stencil *reference* is dynamic and applied per draw via
/// `set_stencil_reference`, not baked here. Depth is always `Always` / no-write:
/// `Stencil8` carries no depth aspect, and the `Disabled_ZTest` /
/// `Equal_Keep_ZTest` modes (which need a depth buffer for 3D-transformed UI)
/// degrade to their non-depth equivalents.
fn depth_stencil_for(render_state: RenderState) -> wgpu::DepthStencilState {
    use wgpu::{CompareFunction, StencilOperation};

    let (compare, pass_op, fail_op) = match render_state.stencil_mode_raw() {
        // Disabled / Disabled_ZTest: stencil test off (always pass, no write).
        0 | 5 => (
            CompareFunction::Always,
            StencilOperation::Keep,
            StencilOperation::Keep,
        ),
        // Equal_Keep / Equal_Keep_ZTest: pass where stencil == ref; keep.
        1 | 6 => (
            CompareFunction::Equal,
            StencilOperation::Keep,
            StencilOperation::Keep,
        ),
        // Equal_Incr: pass where == ref; increment (wrap) on pass.
        2 => (
            CompareFunction::Equal,
            StencilOperation::IncrementWrap,
            StencilOperation::Keep,
        ),
        // Equal_Decr: pass where == ref; decrement (wrap) on pass.
        3 => (
            CompareFunction::Equal,
            StencilOperation::DecrementWrap,
            StencilOperation::Keep,
        ),
        // Clear: always pass; zero the stencil. `pass_op != Keep` makes wgpu enable
        // VK_DYNAMIC_STATE_STENCIL_REFERENCE, but `compare: Always` keeps
        // `needs_ref_value()` false, so wgpu drops every `set_stencil_reference` and
        // the ref goes unset (VUID-vkCmdDrawIndexed-None-07839). `Always` never fails,
        // so a dead `fail_op: Replace` flips `needs_ref_value()` true to keep it emitted.
        4 => (
            CompareFunction::Always,
            StencilOperation::Zero,
            StencilOperation::Replace,
        ),
        // Stencil mode is an SDK-controlled raw; a value added later warns and
        // degrades to the disabled mode (always pass, no write) rather than
        // panicking on the pipeline-build path — a benign default keeps the
        // device rendering. Matches the raw-conversion policy in `wgpu_device`.
        other => {
            warn_once!("unknown StencilMode raw value {other}; disabling stencil test");
            (
                CompareFunction::Always,
                StencilOperation::Keep,
                StencilOperation::Keep,
            )
        }
    };
    let face = wgpu::StencilFaceState {
        compare,
        fail_op,
        depth_fail_op: StencilOperation::Keep,
        pass_op,
    };
    wgpu::DepthStencilState {
        format: STENCIL_FORMAT,
        depth_write_enabled: Some(false),
        depth_compare: Some(CompareFunction::Always),
        stencil: wgpu::StencilState {
            front: face,
            back: face,
            read_mask: 0xff,
            write_mask: 0xff,
        },
        bias: wgpu::DepthBiasState::default(),
    }
}

/// Lazy pipeline cache. Holds the bits needed to build a new pipeline when a
/// fresh `PipelineKey` arrives at `draw_batch`: the wgpu device, the layout,
/// and the target color format.
///
/// The pipeline layout binds four groups: `group(0)` vs uniforms, `group(1)`
/// ps uniforms (`cbuffer0_ps` + `cbuffer1_ps`), `group(2)` pattern texture +
/// sampler, `group(3)` image + shadow textures + samplers. Shaders that don't
/// use a group's bindings still share this layout; the Rust side binds a dummy
/// bind group there since wgpu requires every declared group to be set.
pub struct PipelineCache {
    device: wgpu::Device,
    pipeline_layout: wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    cache: HashMap<PipelineKey, wgpu::RenderPipeline>,
}

impl PipelineCache {
    /// Create an empty cache. `device` and `pipeline_layout` build pipelines
    /// on demand, and `target_format` is the color format every pipeline
    /// writes into (the onscreen intermediate or an offscreen render target).
    #[must_use]
    pub fn new(
        device: wgpu::Device,
        pipeline_layout: wgpu::PipelineLayout,
        target_format: wgpu::TextureFormat,
    ) -> Self {
        Self {
            device,
            pipeline_layout,
            target_format,
            cache: HashMap::new(),
        }
    }

    /// Ensure a pipeline exists for `key`, building it if necessary.
    ///
    /// Returns nothing; pair with [`Self::get`] to fetch the pipeline. The
    /// split lets `draw_batch` borrow other fields of
    /// `WgpuRenderDevice` (the encoder) between the two calls without
    /// tripping the borrow checker.
    ///
    /// # Panics
    ///
    /// Panics if the WGSL build path fails (unported `Shader` variant, or
    /// `naga` rejecting the preprocessed source). Both are bugs in
    /// `shader_defines` / `noesis.wgsl`, not user input.
    pub fn ensure(&mut self, key: PipelineKey) {
        self.cache.entry(key).or_insert_with(|| {
            build_pipeline(&self.device, &self.pipeline_layout, self.target_format, key)
        });
    }

    /// Look up a previously-ensured pipeline.
    ///
    /// # Panics
    ///
    /// Panics if [`Self::ensure`] wasn't called for `key` first.
    #[must_use]
    pub fn get(&self, key: PipelineKey) -> &wgpu::RenderPipeline {
        self.cache
            .get(&key)
            .expect("pipeline not built — call ensure() before get()")
    }
}

/// Map a Noesis `BlendMode::Enum` raw value to a wgpu `BlendState`.
///
/// `None` means "no blending" (the wgpu default): the source value overwrites
/// the destination. That's `BlendMode::Src`. Every other variant returns `Some`
/// with the appropriate factor / op pair.
///
/// `SrcOverDual` uses dual-source blending (a second `@location(0)
/// @blend_src(1)` fragment output) for SDF LCD subpixel rendering; the second
/// output carries per-channel coverage that drives the `OneMinusSrc1` factor.
fn blend_state_for(blend_mode_raw: u8) -> Option<wgpu::BlendState> {
    let comp = |src, dst| wgpu::BlendComponent {
        src_factor: src,
        dst_factor: dst,
        operation: wgpu::BlendOperation::Add,
    };
    let src_over_alpha = comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha);

    match blend_mode_raw {
        0 => None, // BlendMode::Src: straight overwrite
        1 => Some(wgpu::BlendState {
            // BlendMode::SrcOver: cs + cd*(1-as), as + ad*(1-as); premultiplied alpha
            color: src_over_alpha,
            alpha: src_over_alpha,
        }),
        2 => Some(wgpu::BlendState {
            // BlendMode::SrcOverMultiply: cs*cd + cd*(1-as)
            color: comp(wgpu::BlendFactor::Dst, wgpu::BlendFactor::OneMinusSrcAlpha),
            alpha: src_over_alpha,
        }),
        3 => Some(wgpu::BlendState {
            // BlendMode::SrcOverScreen: cs + cd*(1-cs)
            color: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc),
            alpha: src_over_alpha,
        }),
        4 => Some(wgpu::BlendState {
            // BlendMode::SrcOverAdditive: cs + cd (additive)
            color: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::One),
            alpha: src_over_alpha,
        }),
        5 => Some(wgpu::BlendState {
            // BlendMode::SrcOverDual: cs + cd*(1 - src1) per channel, used by
            // the SDF LCD subpixel shader. The fragment's second output
            // (`@blend_src(1)`) carries the per-channel coverage; `Src1` /
            // `OneMinusSrc1` pull it into the blend. Requires the device's
            // `DUAL_SOURCE_BLENDING` feature (only reached when the SDF_LCD_*
            // shaders are emitted, which needs `DeviceCaps::subpixel_rendering`).
            color: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc1),
            alpha: comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrc1Alpha),
        }),
        // Unknown SDK blend raw: warn and fall back to premultiplied SrcOver,
        // the common case, rather than panic on the pipeline-build path.
        other => {
            warn_once!("unknown BlendMode raw value {other}; using SrcOver");
            Some(wgpu::BlendState {
                color: src_over_alpha,
                alpha: src_over_alpha,
            })
        }
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    key: PipelineKey,
) -> wgpu::RenderPipeline {
    let defines = defines_for_shader(noesis_runtime::render_device::types::Shader(key.shader));
    let source = preprocess(NOESIS_WGSL, &defines);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("noesis_runtime Shader({})", key.shader)),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });

    let attrs = attributes_for_format(key.vertex_format);
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: stride_for_format(key.vertex_format),
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attrs,
    };

    // BlendMode comes from RenderState bits 1-3. ColorEnable gates color
    // writes: Noesis emits stencil-only MASK draws with `color_enable=0` that
    // write `vec4(1.0)` from the fragment shader. Without honoring the flag,
    // those white pixels land in the color attachment and obscure subsequent
    // draws (seen as a white panel over hommlet's dev console log on the
    // second open).
    let render_state = RenderState(key.render_state);
    let blend = blend_state_for(render_state.blend_mode_raw());
    let write_mask = if render_state.color_enable() {
        wgpu::ColorWrites::ALL
    } else {
        wgpu::ColorWrites::empty()
    };
    // The pipeline must declare a depth_stencil state iff the render pass it's
    // used in has a stencil attachment (see `PipelineKey::has_stencil`). The
    // stencil op/compare comes from the render state's stencil mode.
    let depth_stencil = key.has_stencil.then(|| depth_stencil_for(render_state));

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!(
            "noesis_runtime pipeline shader={} state=0x{:02x} fmt={}",
            key.shader, key.render_state, key.vertex_format
        )),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: &module,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[vertex_layout],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            strip_index_format: None,
            front_face: wgpu::FrontFace::Ccw,
            cull_mode: None,
            unclipped_depth: false,
            polygon_mode: wgpu::PolygonMode::Fill,
            conservative: false,
        },
        depth_stencil,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module: &module,
            entry_point: Some("fs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format: target_format,
                blend,
                write_mask,
            })],
        }),
        multiview_mask: None,
        cache: None,
    })
}
