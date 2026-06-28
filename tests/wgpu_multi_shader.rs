//! Phase 3.B integration test: render three shader variants (`Path_Solid`,
//! `Path_AA_Solid`, `RGBA`) into one frame and read back the target.
//!
//! - Each batch uses a different vertex format (`PosColor`,
//!   `PosColorCoverage`, `Pos`), driving the `vertex_layout` dispatch.
//! - All three pipelines come out of `PipelineCache` lazily on first draw.
//! - `RGBA` exercises the `ps_uniforms0` bind-group path with a yellow fill.
//! - All vertex / index data is packed into one `map_vertices` /
//!   `map_indices` pair; per-batch `vertex_offset` / `start_index` slice the
//!   right region. Per-batch uniforms are isolated via the ring buffer +
//!   dynamic-offset bind groups landed in Phase 4.A (see
//!   `tests/wgpu_uniform_ring.rs` for the direct regression test).

use std::ffi::c_void;

use dm_noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::RenderDevice;
use noesis_runtime::render_device::types::Batch;
use noesis_runtime::render_device::types::{
    BlendMode, RenderState, SamplerState, Shader, StencilMode, UniformData,
};

const TARGET_W: u32 = 256;
const TARGET_H: u32 = 256;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const BYTES_PER_ROW: u32 = TARGET_W * 4;

const CLEAR: [u8; 4] = [0, 0, 64, 255]; // dark blue

#[test]
fn three_shader_variants_render_into_distinct_quadrants() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();
    pollster::block_on(run_test());
    noesis_runtime::shutdown();
}

#[allow(clippy::too_many_lines)]
async fn run_test() {
    // ── wgpu init ──────────────────────────────────────────────────────────
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("no wgpu adapter available");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis_runtime multi-shader test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available");

    // ── Target + pre-clear ─────────────────────────────────────────────────
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("multi-shader target"),
        size: wgpu::Extent3d {
            width: TARGET_W,
            height: TARGET_H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: TARGET_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    {
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut clear_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
        clear_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
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
        queue.submit(Some(clear_encoder.finish()));
    }

    let device_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());
    rd.set_onscreen_target(device_view);

    // ── Pack all vertex data into one buffer ───────────────────────────────
    // Layout:
    //   bytes  0..36   PosColor  ×3   Path_Solid (red)         upper-left
    //   bytes 36..84   PosColorCoverage ×3  Path_AA_Solid (green) upper-right
    //   bytes 84..108  Pos       ×3   RGBA (yellow via ps_uniforms0) lower-left
    let mut vb_data = Vec::with_capacity(108);
    vb_data.extend_from_slice(&pos_color_verts(&[
        ([-0.7, 0.7], [255, 0, 0, 255]),
        ([-0.3, 0.7], [255, 0, 0, 255]),
        ([-0.5, 0.3], [255, 0, 0, 255]),
    ]));
    vb_data.extend_from_slice(&pos_color_coverage_verts(&[
        ([0.3, 0.7], [0, 255, 0, 255], 1.0),
        ([0.7, 0.7], [0, 255, 0, 255], 1.0),
        ([0.5, 0.3], [0, 255, 0, 255], 1.0),
    ]));
    vb_data.extend_from_slice(&pos_only_verts(&[[-0.7, -0.3], [-0.3, -0.3], [-0.5, -0.7]]));
    assert_eq!(vb_data.len(), 108);

    // All three batches use [0, 1, 2] indices; we just bump start_index per batch.
    let mut ib_data = Vec::with_capacity(18);
    for _ in 0..3 {
        ib_data.extend_from_slice(&[0u16, 1, 2].map(u16::to_le_bytes).concat());
    }
    assert_eq!(ib_data.len(), 18);

    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    // RGBA batch's color, fed in via ps_uniforms0.values[0].
    let rgba_color: [f32; 4] = [1.0, 1.0, 0.0, 1.0]; // yellow

    // ── Drive the device ───────────────────────────────────────────────────
    rd.begin_onscreen_render();

    rd.map_vertices(vb_data.len() as u32)
        .copy_from_slice(&vb_data);
    rd.unmap_vertices();
    rd.map_indices(ib_data.len() as u32)
        .copy_from_slice(&ib_data);
    rd.unmap_indices();

    let path_solid = make_batch(
        Shader::PATH_SOLID,
        /* vertex_offset */ 0,
        /* start_index */ 0,
        &identity_mat,
        None,
    );
    let path_aa_solid = make_batch(Shader::PATH_AA_SOLID, 36, 3, &identity_mat, None);
    let rgba = make_batch(Shader::RGBA, 84, 6, &identity_mat, Some(&rgba_color));

    rd.draw_batch(&path_solid);
    rd.draw_batch(&path_aa_solid);
    rd.draw_batch(&rgba);

    rd.end_onscreen_render();

    // ── Read back ──────────────────────────────────────────────────────────
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(TARGET_H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    {
        let mut copy_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback copy"),
        });
        copy_encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(BYTES_PER_ROW),
                    rows_per_image: Some(TARGET_H),
                },
            },
            wgpu::Extent3d {
                width: TARGET_W,
                height: TARGET_H,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(copy_encoder.finish()));
    }

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("readback channel send failed");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver
        .recv()
        .expect("readback channel recv failed")
        .expect("readback map failed");

    let data = slice.get_mapped_range();
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let offset = (y * BYTES_PER_ROW + x * 4) as usize;
        [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]
    };

    // Centroid (clip space) → pixel:
    //   Path_Solid (red):    clip ≈ (-0.5,  0.566) → pixel ≈ (64, 56)
    //   Path_AA_Solid (grn): clip ≈ ( 0.5,  0.566) → pixel ≈ (192, 56)
    //   RGBA (yellow):       clip ≈ (-0.5, -0.433) → pixel ≈ (64, 183)
    assert_eq!(
        pixel(64, 60),
        [255, 0, 0, 255],
        "Path_Solid centroid should be red"
    );
    assert_eq!(
        pixel(192, 60),
        [0, 255, 0, 255],
        "Path_AA_Solid centroid should be green"
    );
    assert_eq!(
        pixel(64, 183),
        [255, 255, 0, 255],
        "RGBA centroid should be yellow"
    );

    // Quadrants outside any triangle should still be the clear color.
    assert_eq!(pixel(200, 200), CLEAR, "lower-right corner should be clear");
    assert_eq!(pixel(10, 10), CLEAR, "top-left corner should be clear");
}

// ────────────────────────────────────────────────────────────────────────────
// Vertex builders + Batch builder
// ────────────────────────────────────────────────────────────────────────────

fn pos_color_verts(verts: &[([f32; 2], [u8; 4])]) -> Vec<u8> {
    let mut out = Vec::with_capacity(verts.len() * 12);
    for (pos, color) in verts {
        out.extend_from_slice(&pos[0].to_le_bytes());
        out.extend_from_slice(&pos[1].to_le_bytes());
        out.extend_from_slice(color);
    }
    out
}

fn pos_color_coverage_verts(verts: &[([f32; 2], [u8; 4], f32)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(verts.len() * 16);
    for (pos, color, coverage) in verts {
        out.extend_from_slice(&pos[0].to_le_bytes());
        out.extend_from_slice(&pos[1].to_le_bytes());
        out.extend_from_slice(color);
        out.extend_from_slice(&coverage.to_le_bytes());
    }
    out
}

fn pos_only_verts(verts: &[[f32; 2]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(verts.len() * 8);
    for pos in verts {
        out.extend_from_slice(&pos[0].to_le_bytes());
        out.extend_from_slice(&pos[1].to_le_bytes());
    }
    out
}

fn make_batch(
    shader: Shader,
    vertex_offset: u32,
    start_index: u32,
    vs_uniforms: &[f32; 16],
    ps_uniforms: Option<&[f32; 4]>,
) -> Batch {
    let render_state = RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false);
    Batch {
        shader,
        render_state,
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset,
        num_vertices: 3,
        start_index,
        num_indices: 3,
        pattern: std::ptr::null_mut(),
        ramps: std::ptr::null_mut(),
        image: std::ptr::null_mut(),
        glyphs: std::ptr::null_mut(),
        shadow: std::ptr::null_mut(),
        pattern_sampler: SamplerState::default(),
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
            ps_uniforms
                .map(|u| UniformData {
                    values: u.as_ptr().cast::<c_void>(),
                    num_dwords: 4,
                    hash: 2,
                })
                .unwrap_or_default(),
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}
