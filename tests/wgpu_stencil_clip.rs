//! Render-device stencil-clip regression test.
//!
//! Drives the onscreen path the way Noesis builds a clip: a stencil-only
//! `MASK` batch (`color_enable=0`, `StencilMode::EqualIncr`) raises the
//! stencil to 1 over the left half, then a full-screen `PATH_SOLID` batch
//! (`StencilMode::EqualKeep`, `stencil_ref=1`) paints red only where the
//! stencil equals 1. The right half must keep its pre-clear blue.
//!
//! Before stencil was wired into the pipelines (`depth_stencil: None`,
//! no attachment), the `EqualKeep` test couldn't gate anything and the
//! content spilled past its clip — the themed-`ScrollViewer` blank/spill
//! bug from TODO §1. This test fails (red everywhere) without the stencil
//! attachment + pipeline state.

use std::ffi::c_void;

use dm_noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::RenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, RenderState, SamplerState, Shader, StencilMode, UniformData,
};

const TARGET_W: u32 = 256;
const TARGET_H: u32 = 256;
const BYTES_PER_ROW: u32 = TARGET_W * 4;

const CLEAR: [u8; 4] = [0, 0, 255, 255]; // blue
const RED: [u8; 4] = [255, 0, 0, 255];

#[test]
fn stencil_clip_gates_content_to_masked_region() {
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
            label: Some("noesis_runtime stencil test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("stencil test target"),
        size: wgpu::Extent3d {
            width: TARGET_W,
            height: TARGET_H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // Pre-clear to blue so unclipped pixels are distinguishable from red fill.
    {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pre-clear"),
        });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 1.0,
                        a: 1.0,
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

    let device_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());
    rd.set_onscreen_target(device_view, TARGET_W, TARGET_H);

    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];

    // Vertex buffer: mask (Pos, 6 verts × 8B = 48B) then content (PosColor,
    // 6 verts × 12B = 72B). Mask covers the left half (clip x ∈ [-1, 0]);
    // content covers the full screen (x ∈ [-1, 1]).
    let mut vb: Vec<u8> = Vec::new();
    let mask_quad = [
        [-1.0f32, -1.0],
        [0.0, -1.0],
        [-1.0, 1.0],
        [-1.0, 1.0],
        [0.0, -1.0],
        [0.0, 1.0],
    ];
    for v in mask_quad {
        vb.extend_from_slice(&v[0].to_le_bytes());
        vb.extend_from_slice(&v[1].to_le_bytes());
    }
    let content_offset = vb.len() as u32;
    let content_quad = [
        [-1.0f32, -1.0],
        [1.0, -1.0],
        [-1.0, 1.0],
        [-1.0, 1.0],
        [1.0, -1.0],
        [1.0, 1.0],
    ];
    for v in content_quad {
        vb.extend_from_slice(&v[0].to_le_bytes());
        vb.extend_from_slice(&v[1].to_le_bytes());
        vb.extend_from_slice(&RED); // u8x4 color
    }

    // Index buffer: 0..6 for the mask, 0..6 for the content (relative to each
    // batch's vertex slice; base_vertex is 0 in draw_batch).
    let mut ib: Vec<u8> = Vec::new();
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }

    rd.begin_onscreen_render();
    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    // 1) Stencil-only mask: raise stencil to 1 over the left half.
    rd.draw_batch(&mask_batch(&identity_mat));
    // 2) Content gated to stencil == 1.
    rd.draw_batch(&content_batch(content_offset, 6, &identity_mat));

    rd.end_onscreen_render();

    // Readback.
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(TARGET_H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback copy"),
        });
        enc.copy_texture_to_buffer(
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
        queue.submit(Some(enc.finish()));
    }

    let slice = readback.slice(..);
    let (sender, receiver) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        sender.send(r).expect("readback send");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver.recv().expect("readback recv").expect("map");

    let data = slice.get_mapped_range();
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let o = (y * BYTES_PER_ROW + x * 4) as usize;
        [data[o], data[o + 1], data[o + 2], data[o + 3]]
    };

    // Left half is inside the stencil mask → red. Right half is outside →
    // pre-clear blue survives.
    assert_eq!(
        pixel(64, 128),
        RED,
        "left half (inside stencil mask) should be red, got {:?}",
        pixel(64, 128),
    );
    assert_eq!(
        pixel(192, 128),
        CLEAR,
        "right half (outside stencil mask) must keep clear blue, got {:?}",
        pixel(192, 128),
    );
    assert_eq!(pixel(10, 10), RED, "top-left inside mask should be red");
    assert_eq!(
        pixel(245, 245),
        CLEAR,
        "bottom-right outside mask should be clear"
    );
}

/// Stencil-only mask draw: `color_enable=0`, `EqualIncr` from ref 0 → writes
/// stencil = 1 over the rasterized region.
fn mask_batch(vs_uniforms: &[f32; 16]) -> Batch {
    Batch {
        shader: Shader::MASK,
        render_state: RenderState::new(false, BlendMode::Src, StencilMode::EqualIncr, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 6,
        start_index: 0,
        num_indices: 6,
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
        pixel_uniforms: [UniformData::default(), UniformData::default()],
        pixel_shader: std::ptr::null_mut(),
    }
}

/// Content draw gated to `stencil == 1` via `EqualKeep` + `stencil_ref=1`.
fn content_batch(vertex_offset: u32, start_index: u32, vs_uniforms: &[f32; 16]) -> Batch {
    Batch {
        shader: Shader::PATH_SOLID,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::EqualKeep, false),
        stencil_ref: 1,
        single_pass_stereo: false,
        vertex_offset,
        num_vertices: 6,
        start_index,
        num_indices: 6,
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
        pixel_uniforms: [UniformData::default(), UniformData::default()],
        pixel_shader: std::ptr::null_mut(),
    }
}
