//! Pipeline cache keyed on `(Shader, RenderState, VertexFormat)` plus the
//! lazy build path. Pipelines are constructed on first `draw_batch` for a
//! key and reused thereafter.
//!
//! `RenderState` and `VertexFormat` are part of the key so the same `Shader`
//! can produce multiple pipelines when batches differ in blend mode, stencil
//! mode, color-write mask, wireframe flag, or vertex stride.

use std::collections::HashMap;

use dm_noesis_runtime::render_device::types::{
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
    pub shader: u8,
    pub render_state: u8,
    pub vertex_format: u8,
}

impl PipelineKey {
    #[must_use]
    pub fn from_batch(batch: &Batch) -> Self {
        let vshader = VERTEX_FOR_SHADER[batch.shader.0 as usize];
        let vfmt = FORMAT_FOR_VERTEX[vshader as usize];
        Self {
            shader: batch.shader.0,
            render_state: batch.render_state.0,
            vertex_format: vfmt,
        }
    }
}

/// Lazy pipeline cache. Holds the bits needed to build a new pipeline when a
/// fresh `PipelineKey` arrives at `draw_batch`: the wgpu device, the layout,
/// and the target color format.
///
/// The pipeline layout binds three groups: `group(0)` vs uniforms,
/// `group(1)` ps uniforms, `group(2)` pattern texture + sampler. Shaders
/// that don't use the pattern bindings still share this layout — the Rust
/// side binds a dummy bind group at group(2) for those draws since wgpu
/// requires every declared group to be set before a draw.
pub struct PipelineCache {
    device: wgpu::Device,
    pipeline_layout: wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    cache: HashMap<PipelineKey, wgpu::RenderPipeline>,
}

impl PipelineCache {
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
    /// Returns nothing — pair with [`Self::get`] to actually fetch the
    /// pipeline; the split lets `draw_batch` borrow other fields of
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
/// `None` means "no blending" (the wgpu default) — the source value
/// overwrites the destination. That's `BlendMode::Src`. Every other variant
/// returns `Some` with the appropriate factor / op pair.
///
/// `SrcOverDual` requires dual-source blending (extra `@location(0)
/// @blend_src(1)` fragment output) for SDF LCD subpixel rendering and lands
/// with the SDF shader matrix in a later phase.
fn blend_state_for(blend_mode_raw: u8) -> Option<wgpu::BlendState> {
    let comp = |src, dst| wgpu::BlendComponent {
        src_factor: src,
        dst_factor: dst,
        operation: wgpu::BlendOperation::Add,
    };
    let src_over_alpha = comp(wgpu::BlendFactor::One, wgpu::BlendFactor::OneMinusSrcAlpha);

    match blend_mode_raw {
        0 => None, // BlendMode::Src — straight overwrite
        1 => Some(wgpu::BlendState {
            // BlendMode::SrcOver: cs + cd*(1-as), as + ad*(1-as) — premultiplied alpha
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
        5 => panic!("BlendMode::SrcOverDual needs dual-source blending — Phase 6 (SDF LCD)"),
        other => panic!("unknown BlendMode raw value: {other}"),
    }
}

fn build_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    target_format: wgpu::TextureFormat,
    key: PipelineKey,
) -> wgpu::RenderPipeline {
    let defines = defines_for_shader(dm_noesis_runtime::render_device::types::Shader(key.shader));
    let source = preprocess(NOESIS_WGSL, &defines);

    let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(&format!("dm_noesis_runtime Shader({})", key.shader)),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });

    let attrs = attributes_for_format(key.vertex_format);
    let vertex_layout = wgpu::VertexBufferLayout {
        array_stride: stride_for_format(key.vertex_format),
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &attrs,
    };

    // BlendMode comes from RenderState bits 1–3. ColorEnable gates color
    // writes — Noesis emits stencil-only MASK draws with `color_enable=0`,
    // and those write `vec4(1.0)` from the fragment shader; without
    // honoring the flag here, those white pixels land in the color
    // attachment and obscure subsequent draws (manifested for hommlet's
    // dev console as a white panel covering the log surface on the
    // second open). StencilMode / wireframe wiring lands in later
    // sub-phases when stencil clipping earns its keep.
    let render_state = RenderState(key.render_state);
    let blend = blend_state_for(render_state.blend_mode_raw());
    let write_mask = if render_state.color_enable() {
        wgpu::ColorWrites::ALL
    } else {
        wgpu::ColorWrites::empty()
    };

    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(&format!(
            "dm_noesis_runtime pipeline shader={} state=0x{:02x} fmt={}",
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
        depth_stencil: None,
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
        multiview: None,
        cache: None,
    })
}
