//! Render-device effects-resolve test: the `DOWNSAMPLE` (49) and `UPSAMPLE`
//! (48) shaders, the two halves of Noesis's separable-blur resolve chain that
//! `Effects.xaml` / `Transform3D.xaml` previously panicked on
//! (`Shader(49)=DOWNSAMPLE`).
//!
//! - `DOWNSAMPLE`: the vertex shader spreads `uv0 ± uv1` into four taps; the
//!   fragment box-filters them from the group(2) source. Pointed at a 2×2
//!   red/green/blue/yellow source with the taps on the four texel centres, the
//!   output is their average.
//! - `UPSAMPLE`: `mix(image(uv1), pattern(uv0), color.a)`. With a red `image`
//!   (group 3), a green `pattern` (group 2) and `color.a = 0.5`, the output is
//!   the half-and-half blend.

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
fn downsample_and_upsample_resolve_chain() {
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
            label: Some("noesis_runtime effects test device"),
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

    // ── DOWNSAMPLE: 2×2 red/green/blue/yellow → average ───────────────────
    let src_texels: [u8; 2 * 2 * 4] = [
        255, 0, 0, 255, // (0,0) red
        0, 255, 0, 255, // (1,0) green
        0, 0, 255, 255, // (0,1) blue
        255, 255, 0, 255, // (1,1) yellow
    ];
    let src_levels = [&src_texels[..]];
    let src = rd.create_texture(TextureDesc {
        label: "downsample src 2x2",
        width: 2,
        height: 2,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&src_levels),
    });

    let rt = rd.create_render_target(RenderTargetDesc {
        label: "downsample rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });

    // PosTex0Tex1 quad (stride 24): pos, uv0=tap centre (0.5,0.5),
    // uv1=±offset (0.25,0.25) so the four taps hit the four texel centres
    // (0.25/0.75). Constant across the quad → every output pixel is the mean.
    let ds_vb = pos_tex0_tex1_quad([0.5, 0.5], [0.25, 0.25]);
    let ib = quad_indices();

    rd.test_set_forced_pattern(Some((src.handle, nearest)));
    rd.begin_offscreen_render();
    rd.set_render_target(rt.handle);
    rd.map_vertices(ds_vb.len() as u32).copy_from_slice(&ds_vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt.handle, full_tile());
    rd.draw_batch(&effect_batch(Shader::DOWNSAMPLE));
    rd.end_tile(rt.handle);
    rd.resolve_render_target(rt.handle, &[]);
    rd.end_offscreen_render();
    rd.test_set_forced_pattern(None);

    let avg = read_pixel(&device, &queue, &rd, rt.resolve_texture.handle, 2, 2).await;
    // Mean of red/green/blue/yellow = (127.5, 127.5, 63.75, 255).
    assert_close(avg, [128, 128, 64, 255], 2, "downsample 4-tap average");

    // ── UPSAMPLE: mix(red image, green pattern, 0.5) ──────────────────────
    let red_px: [u8; 4] = [255, 0, 0, 255];
    let green_px: [u8; 4] = [0, 255, 0, 255];
    let red_levels = [&red_px[..]];
    let green_levels = [&green_px[..]];
    let image_tex = rd.create_texture(TextureDesc {
        label: "upsample image 1x1 red",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&red_levels),
    });
    let pattern_tex = rd.create_texture(TextureDesc {
        label: "upsample pattern 1x1 green",
        width: 1,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&green_levels),
    });

    let rt2 = rd.create_render_target(RenderTargetDesc {
        label: "upsample rt",
        width: RT_SIZE,
        height: RT_SIZE,
        sample_count: 1,
        needs_stencil: false,
    });

    // PosColorTex0Tex1 quad (stride 28). color.a = 128/255 ≈ 0.5 is the
    // mix weight; uv0/uv1 = (0.5, 0.5) sample the solid 1×1 textures.
    let us_vb = pos_color_tex0_tex1_quad([0, 0, 0, 128], [0.5, 0.5], [0.5, 0.5]);

    rd.test_set_forced_pattern(Some((pattern_tex.handle, nearest)));
    rd.test_set_forced_image(Some((image_tex.handle, nearest)));
    rd.begin_offscreen_render();
    rd.set_render_target(rt2.handle);
    rd.map_vertices(us_vb.len() as u32).copy_from_slice(&us_vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();
    rd.begin_tile(rt2.handle, full_tile());
    rd.draw_batch(&effect_batch(Shader::UPSAMPLE));
    rd.end_tile(rt2.handle);
    rd.resolve_render_target(rt2.handle, &[]);
    rd.end_offscreen_render();
    rd.test_set_forced_pattern(None);
    rd.test_set_forced_image(None);

    let blend = read_pixel(&device, &queue, &rd, rt2.resolve_texture.handle, 2, 2).await;
    // mix(red, green, ~0.5) = (~127, ~128, 0, 255).
    assert_close(blend, [128, 128, 0, 255], 2, "upsample mix");
}

// ── Geometry helpers ────────────────────────────────────────────────────

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

/// Full-screen quad in `PosTex0Tex1` format (pos, uv0, uv1), constant uvs.
fn pos_tex0_tex1_quad(uv0: [f32; 2], uv1: [f32; 2]) -> Vec<u8> {
    let pos = [
        [-1.0f32, -1.0],
        [1.0, -1.0],
        [-1.0, 1.0],
        [-1.0, 1.0],
        [1.0, -1.0],
        [1.0, 1.0],
    ];
    let mut vb = Vec::new();
    for p in pos {
        vb.extend_from_slice(&p[0].to_le_bytes());
        vb.extend_from_slice(&p[1].to_le_bytes());
        vb.extend_from_slice(&uv0[0].to_le_bytes());
        vb.extend_from_slice(&uv0[1].to_le_bytes());
        vb.extend_from_slice(&uv1[0].to_le_bytes());
        vb.extend_from_slice(&uv1[1].to_le_bytes());
    }
    vb
}

/// Full-screen quad in `PosColorTex0Tex1` format, constant color/uvs.
fn pos_color_tex0_tex1_quad(color: [u8; 4], uv0: [f32; 2], uv1: [f32; 2]) -> Vec<u8> {
    let pos = [
        [-1.0f32, -1.0],
        [1.0, -1.0],
        [-1.0, 1.0],
        [-1.0, 1.0],
        [1.0, -1.0],
        [1.0, 1.0],
    ];
    let mut vb = Vec::new();
    for p in pos {
        vb.extend_from_slice(&p[0].to_le_bytes());
        vb.extend_from_slice(&p[1].to_le_bytes());
        vb.extend_from_slice(&color);
        vb.extend_from_slice(&uv0[0].to_le_bytes());
        vb.extend_from_slice(&uv0[1].to_le_bytes());
        vb.extend_from_slice(&uv1[0].to_le_bytes());
        vb.extend_from_slice(&uv1[1].to_le_bytes());
    }
    vb
}

fn effect_batch(shader: Shader) -> Batch {
    Batch {
        shader,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset: 0,
        num_vertices: 6,
        start_index: 0,
        num_indices: 6,
        // Non-null so draw_batch's null-check passes; resolution goes through
        // the test-only forced pattern/image hooks.
        pattern: std::ptr::dangling_mut(),
        ramps: std::ptr::null_mut(),
        image: std::ptr::dangling_mut(),
        glyphs: std::ptr::null_mut(),
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
            UniformData::default(),
        ],
        pixel_uniforms: [UniformData::default(), UniformData::default()],
        pixel_shader: std::ptr::null_mut(),
    }
}

// ── Readback ────────────────────────────────────────────────────────────

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
