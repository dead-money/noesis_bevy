//! Exercises the offscreen render-target path end to end.
//!
//! - `create_texture` + `update_texture` + `drop_texture` round-trip.
//! - `create_render_target` (single-sampled, with stencil requested).
//! - `begin_offscreen_render` → `set_render_target` → `begin_tile` (scissor)
//!   → `map_vertices`/`map_indices` → `draw_batch` → `end_tile` →
//!   `resolve_render_target` → `end_offscreen_render` → submit.
//! - Readback confirms tile scissor clipped rendering and two batches in one
//!   submit received distinct uniforms (ring-buffer regression).
//!
//! The onscreen path is covered by `wgpu_first_triangle.rs` and
//! `wgpu_multi_shader.rs`; this test exclusively exercises offscreen.

use std::ffi::c_void;

use noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, RenderState, SamplerState, Shader, StencilMode, TextureFormat, UniformData,
};
use noesis_runtime::render_device::{RenderDevice, RenderTargetDesc, TextureDesc, TextureRect};

const RT_SIZE: u32 = 128;
const BYTES_PER_ROW: u32 = RT_SIZE * 4;

const CLEAR: [u8; 4] = [0, 0, 64, 255];

#[test]
fn offscreen_rt_scissored_draw_matches_expected_region() {
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
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
            label: Some("noesis_runtime offscreen test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available");
    // Offscreen path only; no onscreen target needed.
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    // Sanity-check texture lifecycle (no samplers; just allocation).
    let texel = [0xAB_u8; 4 * 4 * 4]; // 4x4 RGBA dummy
    let data = [&texel[..]];
    let tex_binding = rd.create_texture(TextureDesc {
        label: "lifecycle tex",
        width: 4,
        height: 4,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&data),
    });
    assert!(rd.texture(tex_binding.handle).is_some(), "texture missing");
    rd.update_texture(
        tex_binding.handle,
        0,
        TextureRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        },
        &[0u8; 2 * 2 * 4],
    );
    rd.drop_texture(tex_binding.handle);
    assert!(
        rd.texture(tex_binding.handle).is_none(),
        "texture not dropped"
    );

    let rt = rd.create_render_target(RenderTargetDesc {
        label: "test rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: true,
    });
    assert_eq!(rd.render_target_size(rt.handle), Some((RT_SIZE, RT_SIZE)));

    // RenderDevice has no clear API; separate encoder clears before drawing
    // so readback can distinguish clipped from unclipped pixels.
    {
        let resolve = rd
            .texture(rt.resolve_texture.handle)
            .expect("resolve texture registered");
        let view = resolve.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pre-clear resolve"),
        });
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pre-clear"),
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
            multiview_mask: None,
        });
        queue.submit(Some(enc.finish()));
    }

    // tile_a: red batch, scissored to left half  (x=0,  y=32, w=64, h=64)
    // tile_b: green batch, scissored to right half (x=64, y=32, w=64, h=64)
    // Two batches verify both tiling (scissor) and the ring buffer (distinct uniforms).
    // Tile origin lower-left; device converts to wgpu upper-left.

    let mut vb = Vec::with_capacity(96);
    // Fullscreen quad (clip space -1..1): two triangles, 6 verts, Pos format.
    for v in [
        [-1.0f32, -1.0],
        [1.0, -1.0],
        [-1.0, 1.0],
        [-1.0, 1.0],
        [1.0, -1.0],
        [1.0, 1.0],
    ] {
        vb.extend_from_slice(&v[0].to_le_bytes());
        vb.extend_from_slice(&v[1].to_le_bytes());
    }
    for v in [
        [-1.0f32, -1.0],
        [1.0, -1.0],
        [-1.0, 1.0],
        [-1.0, 1.0],
        [1.0, -1.0],
        [1.0, 1.0],
    ] {
        vb.extend_from_slice(&v[0].to_le_bytes());
        vb.extend_from_slice(&v[1].to_le_bytes());
    }
    assert_eq!(vb.len(), 96);

    let mut ib = Vec::with_capacity(24);
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    assert_eq!(ib.len(), 24);

    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    let red: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    let green: [f32; 4] = [0.0, 1.0, 0.0, 1.0];

    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);

    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    // Left tile: red.
    rd.begin_tile(
        rt.handle,
        noesis_runtime::render_device::types::Tile {
            x: 0,
            y: 32,
            width: 64,
            height: 64,
        },
    );
    let batch_left = make_rgba_batch(0, 0, &identity_mat, &red);
    rd.draw_batch(&batch_left);
    rd.end_tile(rt.handle);

    // Right tile: green.
    rd.begin_tile(
        rt.handle,
        noesis_runtime::render_device::types::Tile {
            x: 64,
            y: 32,
            width: 64,
            height: 64,
        },
    );
    let batch_right = make_rgba_batch(48, 6, &identity_mat, &green);
    rd.draw_batch(&batch_right);
    rd.end_tile(rt.handle);

    rd.resolve_render_target(
        rt.handle,
        &[noesis_runtime::render_device::types::Tile {
            x: 0,
            y: 0,
            width: RT_SIZE,
            height: RT_SIZE,
        }],
    );
    rd.end_offscreen_render();

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(RT_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    {
        let resolve = rd
            .texture(rt.resolve_texture.handle)
            .expect("resolve still registered");
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
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let offset = (y * BYTES_PER_ROW + x * 4) as usize;
        [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]
    };

    // Noesis lower-left vs wgpu upper-left: for this 128-tall RT, y=32..96
    // maps identically (128 - 96 = 32 from top). Pre-clear survives outside
    // [32, 96).
    assert_eq!(pixel(32, 16), CLEAR, "above the tile band should be clear");
    assert_eq!(pixel(32, 112), CLEAR, "below the tile band should be clear");
    assert_eq!(
        pixel(32, 64),
        [255, 0, 0, 255],
        "left half inside tile should be red"
    );
    assert_eq!(
        pixel(96, 64),
        [0, 255, 0, 255],
        "right half inside tile should be green"
    );
    // Column 64 is the seam; 62 = left tile, 66 = right tile.
    assert_eq!(pixel(62, 64), [255, 0, 0, 255], "seam-left should be red");
    assert_eq!(
        pixel(66, 64),
        [0, 255, 0, 255],
        "seam-right should be green"
    );

    drop(data);
    readback.unmap();

    rd.drop_render_target(rt.handle);
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
