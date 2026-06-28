#![allow(clippy::similar_names)] // tr_start / br_start / bl_start are self-documenting
#![allow(clippy::too_many_arguments)] // batch_base mirrors the Batch surface

//! Phase 4 visual smoke test. Drives [`WgpuRenderDevice`] through a handful
//! of scenarios and writes the combined result as a PNG so you can eyeball
//! that the pipeline is producing sensible output. No Bevy or Noesis — just
//! the standalone wgpu path, same as the integration tests.
//!
//! Run with:
//!
//! ```sh
//! NOESIS_SDK_DIR=~/sdks/noesis-3.2.12 \
//!   cargo run --release --example phase4_visual
//! ```
//!
//! Output lands at `target/phase4_visual.png`.
//!
//! The 512×512 output is divided into four 256×256 quadrants:
//!
//! ```text
//!   ┌──────────────────┬──────────────────┐
//!   │  PATH_SOLID       │  PATH_AA_SOLID   │
//!   │  red triangle     │  gradient-alpha  │
//!   │                   │  green triangle  │
//!   ├───────────────────┼──────────────────┤
//!   │  PATH_PATTERN     │  multi-batch     │
//!   │  16×16 checker-   │  with scissor    │
//!   │  gradient         │  tiles           │
//!   └───────────────────┴──────────────────┘
//! ```

use std::ffi::c_void;
use std::path::PathBuf;

use dm_noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, Tile, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderDevice, RenderTargetDesc, TextureDesc};

const RT_SIZE: u32 = 512;
const QUAD: u32 = RT_SIZE / 2;
const BYTES_PER_ROW: u32 = RT_SIZE * 4;
const CLEAR: [u8; 4] = [16, 20, 28, 255]; // near-black slate

fn main() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();
    pollster::block_on(run());
    noesis_runtime::shutdown();
}

#[allow(clippy::too_many_lines)]
async fn run() {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("no wgpu adapter");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("phase4_visual device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    // This example draws exclusively to an offscreen RT; no onscreen target.
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    // Procedural 16×16 checker-gradient pattern. Four cells in a 2×2 tile,
    // each cell is an 8×8 vertical ramp shifting hue. Produces a visually
    // unmistakable textured look once sampled at 256×256.
    let pattern_texels = build_pattern_16();
    let pattern = rd.create_texture(TextureDesc {
        label: "checker-gradient 16x16",
        width: 16,
        height: 16,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&[&pattern_texels[..]]),
    });

    // Single 512×512 RT covering the whole canvas.
    let rt = rd.create_render_target(RenderTargetDesc {
        label: "phase4_visual canvas",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });

    // Pre-clear directly; Noesis normally does this via Shader::CLEAR.
    {
        let view = rd
            .texture(rt.resolve_texture.handle)
            .expect("resolve")
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pre-clear"),
        });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(CLEAR[0]) / 255.0,
                        g: f64::from(CLEAR[1]) / 255.0,
                        b: f64::from(CLEAR[2]) / 255.0,
                        a: f64::from(CLEAR[3]) / 255.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        queue.submit(Some(enc.finish()));
    }

    // Each quadrant's quad maps through the identity projection to clip
    // space, so we build clip-space coordinates per scene below.
    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];

    // Build the whole frame's vertex / index buffer.
    //
    // Quadrant centres in clip space (RT top-left = (-1, +1); bottom-right =
    // (+1, -1) for wgpu readback orientation):
    //   TL quad: clip x ∈ [-1, 0], y ∈ [ 0, 1]
    //   TR quad: clip x ∈ [ 0, 1], y ∈ [ 0, 1]
    //   BL quad: clip x ∈ [-1, 0], y ∈ [-1, 0]
    //   BR quad: clip x ∈ [ 0, 1], y ∈ [-1, 0]
    let mut vb: Vec<u8> = Vec::new();
    let mut ib: Vec<u8> = Vec::new();
    let mut next_start_index: u32 = 0;

    // TL: PATH_SOLID red triangle in the top-left quadrant.
    // PosColor (12B/vert). Triangle inside the quadrant.
    let tl_vb_offset = vb.len() as u32;
    let tl_start = next_start_index;
    push_pos_color(&mut vb, [-0.8, 0.8], [240, 70, 70, 255]);
    push_pos_color(&mut vb, [-0.2, 0.8], [240, 70, 70, 255]);
    push_pos_color(&mut vb, [-0.5, 0.15], [240, 70, 70, 255]);
    push_u16_indices(&mut ib, &[0, 1, 2]);
    next_start_index += 3;

    // TR: PATH_AA_SOLID green triangle with per-vertex coverage (1.0 center,
    // 0.0 edges). Demonstrates the coverage-AA edge.
    // PosColorCoverage = 16B/vert.
    let tr_vb_offset = vb.len() as u32;
    let tr_start = next_start_index;
    push_pos_color_coverage(&mut vb, [0.2, 0.8], [70, 200, 120, 255], 0.0);
    push_pos_color_coverage(&mut vb, [0.8, 0.8], [70, 200, 120, 255], 0.0);
    push_pos_color_coverage(&mut vb, [0.5, 0.15], [70, 200, 120, 255], 1.0);
    push_u16_indices(&mut ib, &[0, 1, 2]);
    next_start_index += 3;

    // BL: PATH_PATTERN quad sampling the 16×16 procedural pattern.
    // PosTex0 = 16B/vert. Quad covers the bottom-left quadrant with UVs 0..1
    // so each texel ends up as a 16×16 block of output pixels.
    let bl_vb_offset = vb.len() as u32;
    let bl_start = next_start_index;
    push_pos_tex0(&mut vb, [-1.0, -1.0], [0.0, 1.0]); // wgpu V flipped vs clip Y
    push_pos_tex0(&mut vb, [0.0, -1.0], [1.0, 1.0]);
    push_pos_tex0(&mut vb, [-1.0, 0.0], [0.0, 0.0]);
    push_pos_tex0(&mut vb, [-1.0, 0.0], [0.0, 0.0]);
    push_pos_tex0(&mut vb, [0.0, -1.0], [1.0, 1.0]);
    push_pos_tex0(&mut vb, [0.0, 0.0], [1.0, 0.0]);
    push_u16_indices(&mut ib, &[0, 1, 2, 3, 4, 5]);
    next_start_index += 6;

    // BR: two EFFECT_RGBA fullscreen quads; scissor tiles restrict them to
    // stripes inside the bottom-right quadrant. Demonstrates offscreen tile
    // + per-batch uniform isolation (Phase 4.A ring).
    // Pos = 8B/vert.
    let br_vb_offset = vb.len() as u32;
    let br_start = next_start_index;
    for v in [
        [0.0f32, -1.0],
        [1.0, -1.0],
        [0.0, 0.0],
        [0.0, 0.0],
        [1.0, -1.0],
        [1.0, 0.0],
    ] {
        push_pos(&mut vb, v);
    }
    push_u16_indices(&mut ib, &[0, 1, 2, 3, 4, 5]);
    next_start_index += 6;

    // Same six verts reused for the second scissor-clipped batch.
    let br2_start = next_start_index;
    push_u16_indices(&mut ib, &[0, 1, 2, 3, 4, 5]);

    // Nearest-filter sampler for the pattern batch.
    let sampler_state = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Linear,
        MipFilter::Disabled,
    );
    rd.test_set_forced_pattern(Some((pattern.handle, sampler_state)));

    // Drive the offscreen frame.
    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);

    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    // TL — PATH_SOLID (no tile: draws to full RT, but geometry is only in TL).
    rd.begin_tile(rt.handle, whole_rt_tile());
    rd.draw_batch(&path_solid_batch(tl_vb_offset, tl_start, &identity_mat));
    rd.end_tile(rt.handle);

    // TR — PATH_AA_SOLID.
    rd.begin_tile(rt.handle, whole_rt_tile());
    rd.draw_batch(&path_aa_solid_batch(tr_vb_offset, tr_start, &identity_mat));
    rd.end_tile(rt.handle);

    // BL — PATH_PATTERN.
    rd.begin_tile(rt.handle, whole_rt_tile());
    rd.draw_batch(&path_pattern_batch(
        bl_vb_offset,
        bl_start,
        &identity_mat,
        sampler_state,
    ));
    rd.end_tile(rt.handle);

    // BR — two EFFECT_RGBA draws with per-batch ps_uniforms (ring-buffer
    // test). Each clipped to its own scissor stripe inside the BR quadrant.
    // Noesis tile origin is lower-left; BR quadrant spans y ∈ [0, QUAD).
    let stripe_h = QUAD / 4;
    let cyan: [f32; 4] = [0.25, 0.85, 0.95, 1.0];
    let magenta: [f32; 4] = [0.95, 0.30, 0.75, 1.0];

    rd.begin_tile(
        rt.handle,
        Tile {
            x: QUAD,
            y: stripe_h,
            width: QUAD,
            height: stripe_h,
        },
    );
    rd.draw_batch(&rgba_batch(br_vb_offset, br_start, &identity_mat, &cyan));
    rd.end_tile(rt.handle);

    rd.begin_tile(
        rt.handle,
        Tile {
            x: QUAD,
            y: stripe_h * 2,
            width: QUAD,
            height: stripe_h,
        },
    );
    rd.draw_batch(&rgba_batch(
        br_vb_offset,
        br2_start,
        &identity_mat,
        &magenta,
    ));
    rd.end_tile(rt.handle);

    rd.resolve_render_target(rt.handle, &[]);
    rd.end_offscreen_render();
    rd.test_set_forced_pattern(None);

    // Readback.
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(RT_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    {
        let resolve = rd.texture(rt.resolve_texture.handle).expect("resolve");
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback copy"),
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: resolve,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(BYTES_PER_ROW),
                    rows_per_image: Some(RT_SIZE),
                },
            },
            wgpu::Extent3d {
                width: RT_SIZE,
                height: RT_SIZE,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(enc.finish()));
    }

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("readback send");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver
        .recv()
        .expect("readback recv")
        .expect("readback map");

    let data = slice.get_mapped_range();
    let bytes: Vec<u8> = data.to_vec();
    drop(data);
    readback.unmap();

    let out = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("phase4_visual.png");
    image::save_buffer(&out, &bytes, RT_SIZE, RT_SIZE, image::ColorType::Rgba8)
        .expect("PNG write failed");
    println!("wrote {}", out.display());
}

fn whole_rt_tile() -> Tile {
    Tile {
        x: 0,
        y: 0,
        width: RT_SIZE,
        height: RT_SIZE,
    }
}

fn build_pattern_16() -> Vec<u8> {
    let mut out = Vec::with_capacity(16 * 16 * 4);
    for y in 0..16u32 {
        for x in 0..16u32 {
            let cell_x = x / 8;
            let cell_y = y / 8;
            // Four cells with distinct hue ramps along Y inside each cell.
            let t = f32::from((y % 8) as u8) / 7.0;
            let lo = 40;
            let hi = 230;
            let ramp = lo + ((t * f32::from(hi - lo)) as u8);
            let (r, g, b) = match (cell_x, cell_y) {
                (0, 0) => (ramp, lo, lo), // red
                (1, 0) => (lo, ramp, lo), // green
                (0, 1) => (lo, lo, ramp), // blue
                _ => (ramp, ramp, lo),    // yellow
            };
            out.extend_from_slice(&[r, g, b, 255]);
        }
    }
    out
}

fn push_pos(vb: &mut Vec<u8>, pos: [f32; 2]) {
    vb.extend_from_slice(&pos[0].to_le_bytes());
    vb.extend_from_slice(&pos[1].to_le_bytes());
}
fn push_pos_color(vb: &mut Vec<u8>, pos: [f32; 2], color: [u8; 4]) {
    push_pos(vb, pos);
    vb.extend_from_slice(&color);
}
fn push_pos_color_coverage(vb: &mut Vec<u8>, pos: [f32; 2], color: [u8; 4], coverage: f32) {
    push_pos_color(vb, pos, color);
    vb.extend_from_slice(&coverage.to_le_bytes());
}
fn push_pos_tex0(vb: &mut Vec<u8>, pos: [f32; 2], uv: [f32; 2]) {
    push_pos(vb, pos);
    vb.extend_from_slice(&uv[0].to_le_bytes());
    vb.extend_from_slice(&uv[1].to_le_bytes());
}
fn push_u16_indices(ib: &mut Vec<u8>, indices: &[u16]) {
    // All batches use 0-based indices within their vertex buffer slice; the
    // slice is selected per batch via `vertex_offset`.
    for i in indices {
        ib.extend_from_slice(&i.to_le_bytes());
    }
}

// ── Batch constructors ──────────────────────────────────────────────────────

fn path_solid_batch(vertex_offset: u32, start_index: u32, vs: &[f32; 16]) -> Batch {
    batch_base(
        Shader::PATH_SOLID,
        vertex_offset,
        3,
        start_index,
        3,
        vs,
        None,
        SamplerState::default(),
    )
}
fn path_aa_solid_batch(vertex_offset: u32, start_index: u32, vs: &[f32; 16]) -> Batch {
    batch_base(
        Shader::PATH_AA_SOLID,
        vertex_offset,
        3,
        start_index,
        3,
        vs,
        None,
        SamplerState::default(),
    )
}
fn path_pattern_batch(
    vertex_offset: u32,
    start_index: u32,
    vs: &[f32; 16],
    sampler: SamplerState,
) -> Batch {
    // ps_uniforms0.values[0].x is opacity (1.0 here).
    static PS: [f32; 4] = [1.0, 0.0, 0.0, 0.0];
    let mut b = batch_base(
        Shader::PATH_PATTERN,
        vertex_offset,
        6,
        start_index,
        6,
        vs,
        Some(&PS),
        sampler,
    );
    // Non-null pattern pointer — the forced-pattern test hook resolves the
    // actual handle; the pointer is never dereferenced.
    b.pattern = std::ptr::dangling_mut();
    b
}
fn rgba_batch(vertex_offset: u32, start_index: u32, vs: &[f32; 16], color: &[f32; 4]) -> Batch {
    batch_base(
        Shader::RGBA,
        vertex_offset,
        6,
        start_index,
        6,
        vs,
        Some(color),
        SamplerState::default(),
    )
}

fn batch_base(
    shader: Shader,
    vertex_offset: u32,
    num_vertices: u32,
    start_index: u32,
    num_indices: u32,
    vs_uniforms: &[f32; 16],
    ps_uniforms: Option<&[f32; 4]>,
    pattern_sampler: SamplerState,
) -> Batch {
    Batch {
        shader,
        render_state: RenderState::new(true, BlendMode::SrcOver, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset,
        num_vertices,
        start_index,
        num_indices,
        pattern: std::ptr::null_mut(),
        ramps: std::ptr::null_mut(),
        image: std::ptr::null_mut(),
        glyphs: std::ptr::null_mut(),
        shadow: std::ptr::null_mut(),
        pattern_sampler,
        ramps_sampler: SamplerState::default(),
        image_sampler: SamplerState::default(),
        glyphs_sampler: SamplerState::default(),
        shadow_sampler: SamplerState::default(),
        vertex_uniforms: [
            UniformData {
                values: vs_uniforms.as_ptr().cast::<c_void>(),
                num_dwords: 16,
                hash: 1,
            },
            UniformData::default(),
        ],
        pixel_uniforms: [
            ps_uniforms.map_or_else(UniformData::default, |u| UniformData {
                values: u.as_ptr().cast::<c_void>(),
                num_dwords: 4,
                hash: 2,
            }),
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}
