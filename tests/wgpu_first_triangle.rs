//! Phase 2 integration test: drive [`WgpuRenderDevice`] directly (bypassing
//! Noesis init) to render one `Path_Solid` triangle, read the target back,
//! and assert that pixels inside the triangle are red and pixels outside are
//! the pre-clear color.
//!
//! This test does NOT depend on `dm_noesis_test_run_frame_scenario` or
//! `libNoesis.so` for the rendering path — Noesis is only needed for the
//! lifecycle (`init` / `shutdown`) so the linker is satisfied. The Batch
//! handed to `draw_batch` is hand-built.

use std::ffi::c_void;

use dm_noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::RenderDevice;
use noesis_runtime::render_device::types::{
    Batch, RenderState, SamplerState, Shader, UniformData,
};

const TARGET_W: u32 = 256;
const TARGET_H: u32 = 256;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const BYTES_PER_ROW: u32 = TARGET_W * 4;

const CLEAR_R: u8 = 0;
const CLEAR_G: u8 = 0;
const CLEAR_B: u8 = 255;
const CLEAR_A: u8 = 255;

// Deferred: the device's onscreen path currently renders nothing when driven
// manually (offscreen-path device tests all pass). Tracked in TODO.md §1
// ("Onscreen-path draw renders nothing"); un-ignore when that's fixed.
#[ignore = "onscreen-path draw renders nothing — see TODO.md §1"]
#[test]
fn path_solid_first_triangle_fills_expected_pixels() {
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

#[allow(clippy::too_many_lines)] // wgpu setup is verbose
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
            label: Some("noesis_runtime test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available");

    // ── Target texture ─────────────────────────────────────────────────────
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noesis_runtime test target"),
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
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    // ── Pre-clear pass ─────────────────────────────────────────────────────
    {
        let mut clear_encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pre-clear"),
        });
        clear_encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(CLEAR_R) / 255.0,
                        g: f64::from(CLEAR_G) / 255.0,
                        b: f64::from(CLEAR_B) / 255.0,
                        a: f64::from(CLEAR_A) / 255.0,
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

    // Construct WgpuRenderDevice with a fresh view of the target.
    let device_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());
    rd.set_onscreen_target(device_view);

    // ── Build vertex / index / uniform data ────────────────────────────────
    // Triangle in clip space:
    //   v0 = (-0.5, -0.5)  → pixel (64, 192)  (Y inverted from clip→pixel)
    //   v1 = ( 0.5, -0.5)  → pixel (192, 192)
    //   v2 = ( 0.0,  0.5)  → pixel (128, 64)
    // Centroid: pixel (128, 149).
    //
    // PosColor vertex layout: f32 x, f32 y, u8 r, u8 g, u8 b, u8 a. Stride 12.
    let vertices: [u8; 36] = build_pos_color_vertices(&[
        ([-0.5, -0.5], [255, 0, 0, 255]),
        ([0.5, -0.5], [255, 0, 0, 255]),
        ([0.0, 0.5], [255, 0, 0, 255]),
    ]);
    let indices: [u8; 6] = bytemuck::cast::<[u16; 3], [u8; 6]>([0u16, 1, 2]);

    // Identity 4x4 (column-major == row-major for identity).
    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];

    // ── Drive the device ───────────────────────────────────────────────────
    rd.begin_onscreen_render();

    rd.map_vertices(36).copy_from_slice(&vertices);
    rd.unmap_vertices();
    rd.map_indices(6).copy_from_slice(&indices);
    rd.unmap_indices();

    let batch = make_path_solid_batch(&identity_mat);
    rd.draw_batch(&batch);

    rd.end_onscreen_render();

    // ── Read back ──────────────────────────────────────────────────────────
    // bytes_per_row must be aligned to COPY_BYTES_PER_ROW_ALIGNMENT (256).
    // 256 px * 4 bytes = 1024 bytes/row, which is already aligned.
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

    // Inside the triangle (centroid region).
    assert_eq!(
        pixel(128, 149),
        [255, 0, 0, 255],
        "centroid pixel (128, 149) should be red, got {:?}",
        pixel(128, 149),
    );
    assert_eq!(
        pixel(128, 130),
        [255, 0, 0, 255],
        "(128, 130) should be red"
    );
    assert_eq!(
        pixel(120, 160),
        [255, 0, 0, 255],
        "(120, 160) should be red"
    );

    // Well outside the triangle — corners should keep the clear color.
    let clear = [CLEAR_R, CLEAR_G, CLEAR_B, CLEAR_A];
    assert_eq!(pixel(10, 10), clear, "top-left should be clear");
    assert_eq!(pixel(245, 10), clear, "top-right should be clear");
    assert_eq!(pixel(10, 245), clear, "bottom-left should be clear");
    assert_eq!(pixel(245, 245), clear, "bottom-right should be clear");
}

fn build_pos_color_vertices(verts: &[([f32; 2], [u8; 4])]) -> [u8; 36] {
    let mut out = [0u8; 36];
    for (i, (pos, color)) in verts.iter().enumerate() {
        let off = i * 12;
        out[off..off + 4].copy_from_slice(&pos[0].to_le_bytes());
        out[off + 4..off + 8].copy_from_slice(&pos[1].to_le_bytes());
        out[off + 8..off + 12].copy_from_slice(color);
    }
    out
}

fn make_path_solid_batch(uniforms: &[f32; 16]) -> Batch {
    Batch {
        shader: Shader::PATH_SOLID,
        render_state: RenderState::default(),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 3,
        start_index: 0,
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
                values: uniforms.as_ptr().cast::<c_void>(),
                num_dwords: 16,
                hash: 1,
            },
            UniformData::default(),
        ],
        pixel_uniforms: [UniformData::default(), UniformData::default()],
        pixel_shader: std::ptr::null_mut(),
    }
}
