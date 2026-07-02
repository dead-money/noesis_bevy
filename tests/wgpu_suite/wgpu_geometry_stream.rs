//! Regression: two map/unmap geometry cycles in one phase, each draw reading
//! the segment its own unmap wrote. Before the geometry-stream fix, every
//! `unmap_*` wrote to buffer offset 0, so all per-phase uploads landed before
//! the single encoder ran and every recorded draw read the LAST segment's
//! bytes. The per-phase byte cursor + segment base give each draw its own
//! geometry.
//!
//! 256x256 target; left half drawn red, right half green — but unlike the
//! uniform-ring test, the two quads come from two separate map/unmap cycles.
//! If the streams are clobbered, both draws render the last (right) quad and
//! the left half stays at the clear color.

use std::ffi::c_void;

use noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::RenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, RenderState, SamplerState, Shader, StencilMode, UniformData,
};

const TARGET_W: u32 = 256;
const TARGET_H: u32 = 256;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const BYTES_PER_ROW: u32 = TARGET_W * 4;

const CLEAR: [u8; 4] = [0, 0, 64, 255];

#[test]
fn two_geometry_segments_read_distinct_streams_in_one_phase() {
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
            label: Some("noesis_runtime geometry-stream test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("geometry-stream target"),
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
    rd.set_onscreen_target(device_view, TARGET_W, TARGET_H);

    // Vertex format `Pos`: 8 bytes per vertex (two f32). Each quad is uploaded
    // in its OWN map/unmap cycle, so every batch references offset 0 of its own
    // segment.
    let left_vb = quad_bytes([-1.0, 0.0]);
    let right_vb = quad_bytes([0.0, 1.0]);
    let ib = index_bytes();

    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    let red: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    let green: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    rd.begin_onscreen_render();

    // Segment 0: left quad + its draw.
    rd.map_vertices(left_vb.len() as u32)
        .copy_from_slice(&left_vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.draw_batch(&make_rgba_batch(0, 0, &identity_mat, &red));

    // Segment 1: right quad + its draw. With the bug, this overwrites offset 0
    // and the left draw above ends up reading these vertices too.
    rd.map_vertices(right_vb.len() as u32)
        .copy_from_slice(&right_vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.draw_batch(&make_rgba_batch(0, 0, &identity_mat, &green));

    rd.end_onscreen_render();

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

    assert_eq!(
        pixel(64, 128),
        [255, 0, 0, 255],
        "left half should read the left quad's own vertex segment (red)"
    );
    assert_eq!(
        pixel(192, 128),
        [0, 255, 0, 255],
        "right half should read the right quad's own vertex segment (green)"
    );
}

/// Six `Pos` vertices (two triangles) spanning `x ∈ [x0, x1]`, `y ∈ [-1, 1]`.
fn quad_bytes([x0, x1]: [f32; 2]) -> Vec<u8> {
    let mut vb = Vec::with_capacity(48);
    for v in [
        [x0, -1.0],
        [x1, -1.0],
        [x0, 1.0],
        [x0, 1.0],
        [x1, -1.0],
        [x1, 1.0],
    ] {
        vb.extend_from_slice(&v[0].to_le_bytes());
        vb.extend_from_slice(&v[1].to_le_bytes());
    }
    vb
}

/// Indices 0..6, relative to the segment's own six vertices.
fn index_bytes() -> Vec<u8> {
    let mut ib = Vec::with_capacity(12);
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    ib
}

fn make_rgba_batch(
    vertex_offset: u32,
    start_index: u32,
    vs_uniforms: &[f32; 16],
    ps_uniforms: &[f32; 4],
) -> Batch {
    Batch {
        shader: Shader::RGBA,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
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
        pixel_uniforms: [
            UniformData {
                values: ps_uniforms.as_ptr().cast::<c_void>(),
                num_dwords: 4,
                hash: 2,
            },
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}
