//! Tests SHADOW (50) and BLUR (51) effect shaders in `WgpuRenderDevice`.
//!
//! Both shaders co-bind a `shadow` texture with `image` (group(3) bindings 2/3)
//! plus `cbuffer1_ps` (group(1) binding(1)).
//!
//! Shader formulas (used to derive the expected pixel values):
//!   SHADOW: (img + (1-img.a) * (shadowColor * alpha)) * (opacity * paint.a),
//!           alpha = mix(image(uv-offset).a, shadow(uv-offset).a, cb1[6]).
//!   BLUR:   mix(image(uv1), shadow(uv1), cb1[0]) * (opacity * paint.a).
//!
//! Drives `WgpuRenderDevice` directly without Noesis, using `test_set_forced_image`
//! and `test_set_forced_shadow` to point the two group(3) slots at solid 1x1
//! textures; constants chosen so the algebra collapses to assertable values.

use std::ffi::c_void;

use noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderDevice, RenderTargetDesc, TextureDesc};

const RT_SIZE: u32 = 4;
const BYTES_PER_ROW: u32 = 256; // wgpu COPY_BYTES_PER_ROW_ALIGNMENT

const IDENTITY: [f32; 16] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0, //
    0.0, 0.0, 0.0, 1.0,
];

#[test]
fn shadow_and_blur_effects() {
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
            label: Some("noesis_runtime shadow/blur test device"),
            required_features: wgpu::Features::empty(),
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

    // BLUR: mix(red image, green shadow, 0.75)
    // cbuffer1_ps[0] = 0.75 crossfade; paint = opaque white vertex color so
    // opacity*paint.a = 1. Expect 0.25*red + 0.75*green ≈ (64, 191, 0, 255).
    let red_px: [u8; 4] = [255, 0, 0, 255];
    let green_px: [u8; 4] = [0, 255, 0, 255];
    let red_levels = [&red_px[..]];
    let green_levels = [&green_px[..]];
    let image_tex = rd.create_texture(TextureDesc {
        label: "blur image 1x1 red",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&red_levels),
    });
    let shadow_tex = rd.create_texture(TextureDesc {
        label: "blur shadow 1x1 green",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&green_levels),
    });

    let rt = rd.create_render_target(RenderTargetDesc {
        label: "blur rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });

    let ib = quad_indices();
    // PosColorTex1 quad (stride 20): white opaque color, uv1 = (0.5, 0.5).
    let blur_vb = pos_color_tex1_quad([255, 255, 255, 255], [0.5, 0.5]);
    // cbuffer1_ps: only [0] is read (= crossfade 0.75).
    let blur_cb1: [f32; 8] = [0.75, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];

    rd.test_set_forced_image(Some((image_tex.handle, nearest)));
    rd.test_set_forced_shadow(Some((shadow_tex.handle, nearest)));
    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);
    rd.map_vertices(blur_vb.len() as u32)
        .copy_from_slice(&blur_vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt.handle, full_tile());
    rd.draw_batch(&effect_batch(Shader::BLUR, &blur_cb1));
    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[]);
    rd.end_offscreen_render();

    let blur = read_pixel(&device, &queue, &rd, rt.resolve_texture.handle, 2, 2).await;
    assert_close(blur, [64, 191, 0, 255], 2, "blur mix(image, shadow, 0.75)");

    // SHADOW: transparent layer over an opaque-alpha shadow
    // Make `image` fully transparent (img.a = 0) so the formula reduces to
    //   shadowColor * alpha, with alpha = mix(image.a=0, shadow.a=1, cb1[6]=1)
    //   = 1. shadowColor = (0, 0, 1, 1) (blue). offset = 0, rect = whole. So
    // output = shadowColor = blue, scaled by opacity*paint.a = 1.
    let clear_px: [u8; 4] = [0, 0, 0, 0]; // transparent image
    let opaque_px: [u8; 4] = [255, 255, 255, 255]; // shadow alpha = 1
    let clear_levels = [&clear_px[..]];
    let opaque_levels = [&opaque_px[..]];
    let s_image = rd.create_texture(TextureDesc {
        label: "shadow image 1x1 transparent",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&clear_levels),
    });
    let s_shadow = rd.create_texture(TextureDesc {
        label: "shadow shadow 1x1 opaque",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&opaque_levels),
    });

    let rt2 = rd.create_render_target(RenderTargetDesc {
        label: "shadow rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });

    // PosColorTex1Rect quad (stride 28): white opaque color, uv1 = (0.5,0.5),
    // rect = whole [0,1]² (Unorm16x4, so 0xFFFF == 1.0).
    let shadow_vb = pos_color_tex1_rect_quad([255, 255, 255, 255], [0.5, 0.5]);
    // cbuffer1_ps: shadowColor=(0,0,1,1) at [0..3], offset=(0,0) at [4..5],
    // blend factor cb1[6] = 1.0 (take the shadow texture's alpha).
    let shadow_cb1: [f32; 8] = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0];

    rd.test_set_forced_image(Some((s_image.handle, nearest)));
    rd.test_set_forced_shadow(Some((s_shadow.handle, nearest)));
    rd.begin_offscreen_render();
    rd.set_render_target(rt2.handle);
    rd.map_vertices(shadow_vb.len() as u32)
        .copy_from_slice(&shadow_vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt2.handle, full_tile());
    rd.draw_batch(&effect_batch(Shader::SHADOW, &shadow_cb1));
    rd.end_tile(rt2.handle);
    rd.resolve_render_target(rt2.handle, &[]);
    rd.end_offscreen_render();
    rd.test_set_forced_image(None);
    rd.test_set_forced_shadow(None);

    let shadow = read_pixel(&device, &queue, &rd, rt2.resolve_texture.handle, 2, 2).await;
    // (transparent_img + (1-0)*(blue * 1)) * 1 = blue.
    assert_close(shadow, [0, 0, 255, 255], 2, "shadow over transparent layer");
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

const QUAD_POS: [[f32; 2]; 6] = [
    [-1.0, -1.0],
    [1.0, -1.0],
    [-1.0, 1.0],
    [-1.0, 1.0],
    [1.0, -1.0],
    [1.0, 1.0],
];

fn pos_color_tex1_quad(color: [u8; 4], uv1: [f32; 2]) -> Vec<u8> {
    let mut vb = Vec::new();
    for p in QUAD_POS {
        vb.extend_from_slice(&p[0].to_le_bytes());
        vb.extend_from_slice(&p[1].to_le_bytes());
        vb.extend_from_slice(&color);
        vb.extend_from_slice(&uv1[0].to_le_bytes());
        vb.extend_from_slice(&uv1[1].to_le_bytes());
    }
    vb
}

fn pos_color_tex1_rect_quad(color: [u8; 4], uv1: [f32; 2]) -> Vec<u8> {
    let mut vb = Vec::new();
    for p in QUAD_POS {
        vb.extend_from_slice(&p[0].to_le_bytes());
        vb.extend_from_slice(&p[1].to_le_bytes());
        vb.extend_from_slice(&color);
        vb.extend_from_slice(&uv1[0].to_le_bytes());
        vb.extend_from_slice(&uv1[1].to_le_bytes());
        // rect = (0, 0, 1, 1) as Unorm16x4.
        vb.extend_from_slice(&0u16.to_le_bytes());
        vb.extend_from_slice(&0u16.to_le_bytes());
        vb.extend_from_slice(&u16::MAX.to_le_bytes());
        vb.extend_from_slice(&u16::MAX.to_le_bytes());
    }
    vb
}

fn effect_batch(shader: Shader, cb1: &[f32; 8]) -> Batch {
    Batch {
        shader,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 6,
        start_index: 0,
        num_indices: 6,
        // Non-null so draw_batch's null-checks pass; resolution goes through
        // the test-only forced image/shadow hooks.
        pattern: std::ptr::null_mut(),
        ramps: std::ptr::null_mut(),
        image: std::ptr::dangling_mut(),
        glyphs: std::ptr::null_mut(),
        shadow: std::ptr::dangling_mut(),
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
            UniformData::default(),
        ],
        pixel_uniforms: [
            UniformData::default(),
            UniformData {
                values: cb1.as_ptr().cast::<c_void>(),
                num_dwords: 8,
                hash: 2,
            },
        ],
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
