//! Tests `PATH_PATTERN` sampling from a registered `wgpu::Texture` via the group(2)
//! bind group. Setup: 4×4 RT, 2×2 pattern texture (red/green/blue/yellow), full-screen
//! quad, nearest sampler. Expected readback (clip `y=+1` maps to row 0):
//! ```
//!   row 0 | red   green
//!   row 3 | blue  yellow
//! ```

use std::ffi::c_void;

use noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderDevice, RenderTargetDesc, TextureDesc};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT

const CLEAR: [u8; 4] = [0, 0, 64, 255];

#[test]
fn path_pattern_samples_from_registered_texture() {
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
        .expect("no wgpu adapter");
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis_runtime pattern test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    // Row-major Rgba8: (0,0) red, (1,0) green, (0,1) blue, (1,1) yellow
    let pattern_texels: [u8; 2 * 2 * 4] = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let level_data = [&pattern_texels[..]];
    let pattern_binding = rd.create_texture(TextureDesc {
        label: "pattern 2x2",
        width: 2,
        height: 2,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&level_data),
    });

    let rt = rd.create_render_target(RenderTargetDesc {
        label: "pattern test rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });

    // Noesis would issue Shader::CLEAR; the test harness doesn't drive Noesis.
    {
        let resolve = rd.texture(rt.resolve_texture.handle).expect("resolve");
        let view = resolve.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("pre-clear"),
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
        });
        queue.submit(Some(enc.finish()));
    }

    // wgpu: clip Y=+1 → row 0; V=0 → top row. With nearest filter on a 4×4 output
    // over a 2×2 texture, pixel centres (x+0.5)/4 land at u=0.125/0.375 (texel 0)
    // or u=0.625/0.875 (texel 1), so each 2×2 quadrant samples one texel exactly.
    let vertices: [f32; 6 * 4] = [
        // x, y, u, v
        -1.0, -1.0, 0.0, 1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 0.0, 0.0, -1.0, 1.0, 0.0, 0.0, 1.0,
        -1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.0,
    ];
    let mut vb = Vec::with_capacity(vertices.len() * 4);
    for v in vertices {
        vb.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(vb.len(), 96);

    let mut ib = Vec::with_capacity(12);
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }

    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    // PAINT_PATTERN reads opacity from ps_uniforms0.values[0].x.
    let ps_uniform0: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

    let sampler_state = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );

    // Can't produce a Noesis-owned Texture* in standalone wgpu tests; pattern
    // resolution goes through test_set_forced_pattern. The non-null pointer
    // satisfies shader_uses_pattern assertions but is never dereferenced.
    rd.test_set_forced_pattern(Some((pattern_binding.handle, sampler_state)));

    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);

    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    rd.begin_tile(
        rt.handle,
        noesis_runtime::render_device::types::Tile {
            x: 0,
            y: 0,
            width: RT_SIZE,
            height: RT_SIZE,
        },
    );

    let batch = make_pattern_batch(&identity_mat, &ps_uniform0, sampler_state);
    rd.draw_batch(&batch);

    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[]);
    rd.end_offscreen_render();
    rd.test_set_forced_pattern(None);

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
    let pixel = |x: u32, y: u32| -> [u8; 4] {
        let offset = (y * BYTES_PER_ROW + x * 4) as usize;
        [
            data[offset],
            data[offset + 1],
            data[offset + 2],
            data[offset + 3],
        ]
    };

    assert_eq!(pixel(0, 0), [255, 0, 0, 255], "top-left quadrant = red");
    assert_eq!(
        pixel(1, 1),
        [255, 0, 0, 255],
        "top-left quadrant inner = red"
    );
    assert_eq!(pixel(2, 0), [0, 255, 0, 255], "top-right quadrant = green");
    assert_eq!(
        pixel(3, 1),
        [0, 255, 0, 255],
        "top-right quadrant inner = green"
    );
    assert_eq!(pixel(0, 2), [0, 0, 255, 255], "bottom-left quadrant = blue");
    assert_eq!(
        pixel(1, 3),
        [0, 0, 255, 255],
        "bottom-left quadrant inner = blue"
    );
    assert_eq!(
        pixel(2, 2),
        [255, 255, 0, 255],
        "bottom-right quadrant = yellow"
    );
    assert_eq!(
        pixel(3, 3),
        [255, 255, 0, 255],
        "bottom-right quadrant inner = yellow"
    );

    drop(data);
    readback.unmap();
}

fn make_pattern_batch(
    vs_uniforms: &[f32; 16],
    ps_uniforms: &[f32; 4],
    pattern_sampler: SamplerState,
) -> Batch {
    Batch {
        shader: Shader::PATH_PATTERN,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 6,
        start_index: 0,
        num_indices: 6,
        // Non-null so shader_uses_pattern assertions pass; never dereferenced
        // (test_set_forced_pattern handles actual resolution).
        pattern: std::ptr::dangling_mut(),
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
