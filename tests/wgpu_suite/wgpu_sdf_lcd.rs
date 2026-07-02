//! Tests dual-source blending for `SDF_LCD_SOLID` at coverage limit cases (0, 1).
//! No SDK GL/VK reference exists for the LCD path, so limit cases are used
//! instead of pinning subpixel constants. Broken `@blend_src` wiring or a
//! failed pipeline compile causes the blend factors to disagree and the pixel
//! assertions fail. Skips without `DUAL_SOURCE_BLENDING`.

use std::ffi::c_void;

use noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderDevice, RenderTargetDesc, TextureDesc};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256;
const RT_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];
// uv1 spans 0..1 so `st1 = uv1 * glyph_size` gives ~1 texel/px gradient,
// keeping the SDF AA window narrow enough that limit-case distances saturate.
const GLYPH_SIZE: [f32; 2] = [4.0, 4.0];
const GREEN_BG: wgpu::Color = wgpu::Color {
    r: 0.0,
    g: 1.0,
    b: 0.0,
    a: 1.0,
};

#[test]
fn sdf_lcd_subpixel_dual_source() {
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
        .expect("no wgpu adapter");

    if !adapter
        .features()
        .contains(wgpu::Features::DUAL_SOURCE_BLENDING)
    {
        eprintln!("skipping: adapter lacks DUAL_SOURCE_BLENDING");
        return;
    }

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis_runtime sdf-lcd test device"),
            required_features: wgpu::Features::DUAL_SOURCE_BLENDING,
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());

    let nearest = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );

    // 1×1 glyph "fully inside" (r = 255 → distance ≫ AA window → coverage 1).
    let inside_px: [u8; 1] = [255];
    let inside_levels = [&inside_px[..]];
    let inside = rd.create_texture(TextureDesc {
        label: "glyph inside r=1",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::R8,
        data: Some(&inside_levels),
    });
    // 1×1 glyph "fully outside" (r = 0 → distance ≪ AA window → coverage 0).
    let outside_px: [u8; 1] = [0];
    let outside_levels = [&outside_px[..]];
    let outside = rd.create_texture(TextureDesc {
        label: "glyph outside r=0",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::R8,
        data: Some(&outside_levels),
    });

    let red = [255u8, 0, 0, 255];
    let covered = run_lcd_draw(&device, &queue, &mut rd, inside.handle, nearest, red).await;
    assert_close(
        covered,
        [255, 0, 0, 255],
        2,
        "lcd fully-inside → text color",
    );

    let uncovered = run_lcd_draw(&device, &queue, &mut rd, outside.handle, nearest, red).await;
    assert_close(
        uncovered,
        [0, 255, 0, 255],
        2,
        "lcd fully-outside → background",
    );
}

async fn run_lcd_draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &mut WgpuRenderDevice,
    glyph: noesis_runtime::render_device::TextureHandle,
    sampler: SamplerState,
    color: [u8; 4],
) -> [u8; 4] {
    let rt = rd.create_render_target(RenderTargetDesc {
        label: "lcd rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });

    // draw_batch loads the color attachment, so pre-fill with the green background.
    clear_rt(device, queue, rd, rt.resolve_texture.handle, GREEN_BG);

    let vb = lcd_quad(color);
    let ib = quad_indices();

    rd.test_set_forced_pattern(Some((glyph, sampler)));
    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);
    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt.handle, full_tile());
    rd.draw_batch(&lcd_batch());
    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[]);
    rd.end_offscreen_render();
    rd.test_set_forced_pattern(None);

    read_pixel(device, queue, rd, rt.resolve_texture.handle, 2, 2).await
}

fn clear_rt(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &WgpuRenderDevice,
    handle: noesis_runtime::render_device::TextureHandle,
    color: wgpu::Color,
) {
    let tex = rd.texture(handle).expect("rt texture registered");
    let view = tex.create_view(&wgpu::TextureViewDescriptor {
        format: Some(RT_COLOR_FORMAT),
        ..Default::default()
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("lcd bg clear"),
    });
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("lcd bg clear pass"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(color),
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

fn full_tile() -> noesis_runtime::render_device::types::Tile {
    noesis_runtime::render_device::types::Tile {
        x: 0,
        y: 0,
        width: RT_SIZE,
        height: RT_SIZE,
    }
}

fn quad_indices() -> Vec<u8> {
    let mut ib = Vec::with_capacity(12);
    for i in 0u16..6 {
        ib.extend_from_slice(&i.to_le_bytes());
    }
    ib
}

// uv1 tracks pos (0..1) so `st1 = uv1 * glyph_size` gives the nonzero gradient
// the SDF AA window requires.
fn lcd_quad(color: [u8; 4]) -> Vec<u8> {
    let verts = [
        ([-1.0f32, -1.0], [0.0f32, 0.0]),
        ([1.0, -1.0], [1.0, 0.0]),
        ([-1.0, 1.0], [0.0, 1.0]),
        ([-1.0, 1.0], [0.0, 1.0]),
        ([1.0, -1.0], [1.0, 0.0]),
        ([1.0, 1.0], [1.0, 1.0]),
    ];
    let mut vb = Vec::new();
    for (pos, uv1) in verts {
        vb.extend_from_slice(&pos[0].to_le_bytes());
        vb.extend_from_slice(&pos[1].to_le_bytes());
        vb.extend_from_slice(&color);
        vb.extend_from_slice(&uv1[0].to_le_bytes());
        vb.extend_from_slice(&uv1[1].to_le_bytes());
    }
    vb
}

fn lcd_batch() -> Batch {
    Batch {
        shader: Shader::SDF_LCD_SOLID,
        render_state: RenderState::new(true, BlendMode::SrcOverDual, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 6,
        start_index: 0,
        num_indices: 6,
        pattern: std::ptr::dangling_mut(),
        ramps: std::ptr::null_mut(),
        image: std::ptr::null_mut(),
        glyphs: std::ptr::dangling_mut(),
        shadow: std::ptr::null_mut(),
        pattern_sampler: SamplerState::default(),
        ramps_sampler: SamplerState::default(),
        image_sampler: SamplerState::default(),
        glyphs_sampler: SamplerState::default(),
        shadow_sampler: SamplerState::default(),
        vertex_uniforms: [
            UniformData {
                values: IDENTITY.as_ptr().cast::<c_void>(),
                num_dwords: 16,
                hash: 1,
            },
            UniformData {
                values: GLYPH_SIZE.as_ptr().cast::<c_void>(),
                num_dwords: 2,
                hash: 2,
            },
        ],
        pixel_uniforms: [UniformData::default(), UniformData::default()],
        pixel_shader: std::ptr::null_mut(),
    }
}

async fn read_pixel(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    rd: &WgpuRenderDevice,
    handle: noesis_runtime::render_device::TextureHandle,
    x: u32,
    y: u32,
) -> [u8; 4] {
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(RT_SIZE),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    {
        let resolve = rd.texture(handle).expect("resolve registered");
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback"),
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
    slice.map_async(wgpu::MapMode::Read, move |r| {
        sender.send(r).expect("send");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver.recv().expect("recv").expect("map");
    let data = slice.get_mapped_range();
    let o = (y * BYTES_PER_ROW + x * 4) as usize;
    [data[o], data[o + 1], data[o + 2], data[o + 3]]
}

fn assert_close(got: [u8; 4], want: [u8; 4], tol: u8, what: &str) {
    for i in 0..4 {
        let d = got[i].abs_diff(want[i]);
        assert!(
            d <= tol,
            "{what}: channel {i} = {} expected ~{} (tol {tol}); got {got:?} want {want:?}",
            got[i],
            want[i],
        );
    }
}
