//! Exercises `PATH_PATTERN_CLAMP` (`PosTex0Rect`) and `PATH_PATTERN_REPEAT`
//! (`PosTex0RectTile`) in a single frame.
//!
//! Left half: clamped quad with an inner `rect`; fragments inside sample the
//! pattern, outside collapse to transparent black. Right half: repeated quad
//! with `uv0 ∈ [0, 2]` and `tile = (0, 0, 1, 1)`, wrapping the 2×2 pattern
//! into two horizontal tiles.
//!
//! Asserts: CLAMP inside-rect pixel matches expected texel, outside collapses
//! to zero; REPEAT two x-positions at the same v map to the same texel
//! (confirming `fract()` wrap).

use std::ffi::c_void;

use noesis_bevy::render_device::WgpuRenderDevice;
use noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, UniformData, WrapMode,
};
use noesis_runtime::render_device::{RenderDevice, TextureDesc};

const TARGET_W: u32 = 32;
const TARGET_H: u32 = 32;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
// COPY_BYTES_PER_ROW_ALIGNMENT requires 256; TARGET_W*4=128 would break
// copy_texture_to_buffer, so pad to 256 and skip trailing bytes in pixel().
const BYTES_PER_ROW: u32 = 256;

const CLEAR: [u8; 4] = [0, 0, 64, 255];

#[test]
fn path_pattern_clamp_and_repeat_draw_their_variants() {
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
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis_runtime pattern-wrap test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pattern-wrap target"),
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
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
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
            multiview_mask: None,
        });
        queue.submit(Some(enc.finish()));
    }

    let device_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut rd = WgpuRenderDevice::new(device.clone(), queue.clone());
    rd.set_onscreen_target(device_view, TARGET_W, TARGET_H);

    // 2×2 RGBA: (0,0)=red (1,0)=green / (0,1)=blue (1,1)=yellow
    let pattern_texels: [u8; 16] = [
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
    ];
    let level_data = [&pattern_texels[..]];
    let pattern_binding = rd.create_texture(TextureDesc {
        label: "2x2 pattern",
        width: 2,
        height: 2,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&level_data),
    });

    let sampler_state = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );
    rd.test_set_forced_pattern(Some((pattern_binding.handle, sampler_state)));

    // CLAMP quad (PosTex0Rect, stride 24): clip x ∈ [-1, 0], uv0 ∈ [0,1].
    //   rect=(0.25,0.25,0.75,0.75) carves a 50%×50% window; outside goes to
    //   (0,0,0,0) via the inside-mask.
    // REPEAT quad (PosTex0RectTile, stride 40): clip x ∈ [0, 1],
    //   uv0 ∈ [0,2]×[0,1], tile=(0,0,1,1), producing 2 horizontal tiles.
    // wgpu clip y=+1 is row 0, so top verts use v=0, bottom use v=1
    // (aligns with pattern top-row=red, bottom-row=blue).

    let mut vb: Vec<u8> = Vec::new();

    // CLAMP verts: 4 × 24 bytes = 96 bytes at offset 0.
    push_pos_tex0_rect(&mut vb, [-1.0, -1.0], [0.0, 1.0], [0.25, 0.25, 0.75, 0.75]);
    push_pos_tex0_rect(&mut vb, [0.0, -1.0], [1.0, 1.0], [0.25, 0.25, 0.75, 0.75]);
    push_pos_tex0_rect(&mut vb, [-1.0, 1.0], [0.0, 0.0], [0.25, 0.25, 0.75, 0.75]);
    push_pos_tex0_rect(&mut vb, [0.0, 1.0], [1.0, 0.0], [0.25, 0.25, 0.75, 0.75]);
    assert_eq!(vb.len(), 96);

    // REPEAT verts: 4 × 40 bytes = 160 bytes at offset 96.
    push_pos_tex0_rect_tile(
        &mut vb,
        [0.0, -1.0],
        [0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    );
    push_pos_tex0_rect_tile(
        &mut vb,
        [1.0, -1.0],
        [2.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    );
    push_pos_tex0_rect_tile(
        &mut vb,
        [0.0, 1.0],
        [0.0, 0.0],
        [0.0, 0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    );
    push_pos_tex0_rect_tile(
        &mut vb,
        [1.0, 1.0],
        [2.0, 0.0],
        [0.0, 0.0, 1.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
    );
    assert_eq!(vb.len(), 96 + 160);

    let quad_idx = [0u16, 1, 2, 1, 3, 2];
    let mut ib = Vec::with_capacity(24);
    for _ in 0..2 {
        for i in quad_idx {
            ib.extend_from_slice(&i.to_le_bytes());
        }
    }

    // Identity projection: pos.xy passes through to clip.xy.
    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    // Pattern shaders read opacity from ps_uniforms0.values[0].x; rest is 0.
    let ps_uniform0: [f32; 4] = [1.0, 0.0, 0.0, 0.0];

    rd.begin_onscreen_render();

    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    let clamp = make_pattern_batch(
        Shader::PATH_PATTERN_CLAMP,
        /* vertex_offset */ 0,
        /* start_index */ 0,
        /* num_vertices */ 4,
        /* num_indices */ 6,
        &identity_mat,
        &ps_uniform0,
        sampler_state,
    );
    rd.draw_batch(&clamp);

    let repeat = make_pattern_batch(
        Shader::PATH_PATTERN_REPEAT,
        /* vertex_offset */ 96,
        /* start_index */ 6,
        /* num_vertices */ 4,
        /* num_indices */ 6,
        &identity_mat,
        &ps_uniform0,
        sampler_state,
    );
    rd.draw_batch(&repeat);

    rd.end_onscreen_render();
    rd.test_set_forced_pattern(None);

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(TARGET_H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("readback"),
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
    slice.map_async(wgpu::MapMode::Read, move |result| {
        sender.send(result).expect("readback send");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    receiver.recv().expect("readback recv").expect("map");

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

    // CLAMP half (left 16 px).
    // uv = ((x+0.5)/16, (y+0.5)/32); inside rect when uv ∈ [0.25, 0.75]²
    //   → pixel x ∈ [3.5, 11.5] (x = 4..11), y ∈ [7.5, 23.5] (y = 8..23).
    //
    // Inside: pattern sampled at nearest texel.
    //   uv=(0.344, 0.328) at pixel (5, 10) → texel (0,0) = red.
    //   uv=(0.656, 0.641) at pixel (10, 20) → texel (1,1) = yellow.
    //   uv=(0.531, 0.328) at pixel (8, 10)  → texel (1,0) = green.
    let top_left = pixel(5, 10);
    assert_eq!(
        top_left,
        [255, 0, 0, 255],
        "CLAMP inside-rect top-left quadrant = red; got {top_left:?}",
    );
    let bottom_right = pixel(10, 20);
    assert_eq!(
        bottom_right,
        [255, 255, 0, 255],
        "CLAMP inside-rect bottom-right quadrant = yellow; got {bottom_right:?}",
    );
    let top_right = pixel(8, 10);
    assert_eq!(
        top_right,
        [0, 255, 0, 255],
        "CLAMP inside-rect top-right quadrant = green; got {top_right:?}",
    );

    // Outside rect, inside quad → paint = 0 via the inside-mask.
    //   uv=(0.156, 0.078) at pixel (2, 2)   → outside → (0,0,0,0).
    //   uv=(0.781, 0.859) at pixel (12, 27) → outside → (0,0,0,0).
    let outside_nw = pixel(2, 2);
    assert_eq!(
        outside_nw,
        [0, 0, 0, 0],
        "CLAMP outside-rect → transparent black; got {outside_nw:?}",
    );
    let outside_se = pixel(12, 27);
    assert_eq!(
        outside_se,
        [0, 0, 0, 0],
        "CLAMP outside-rect → transparent black; got {outside_se:?}",
    );

    // REPEAT half (right 16 px).
    // uv = ((x-16+0.5)/16 * 2, (y+0.5)/32). fract(uv.x) picks a tile.
    //   pixel (17, 8):  uv ≈ (0.188, 0.266) → texel (0,0) = red.
    //   pixel (25, 8):  uv ≈ (1.188, 0.266), fract=0.188 → same texel → red.
    //   pixel (17, 24): uv ≈ (0.188, 0.766) → texel (0,1) = blue.
    let tile0_top = pixel(17, 8);
    let tile1_top = pixel(25, 8);
    assert_eq!(
        tile0_top,
        [255, 0, 0, 255],
        "REPEAT tile 0 top-left = red; got {tile0_top:?}",
    );
    assert_eq!(
        tile0_top, tile1_top,
        "REPEAT: two x-offsets into the same tile-local coord must match. \
         got tile0={tile0_top:?}, tile1={tile1_top:?}",
    );
    let tile0_bot = pixel(17, 24);
    assert_eq!(
        tile0_bot,
        [0, 0, 255, 255],
        "REPEAT tile 0 bottom-left = blue; got {tile0_bot:?}",
    );

    drop(data);
    readback.unmap();
}

/// `PosTex0Rect`: pos (F32x2, 8B) + tex0 (F32x2, 8B) + rect (Unorm16x4, 8B) = 24B.
fn push_pos_tex0_rect(out: &mut Vec<u8>, pos: [f32; 2], uv0: [f32; 2], rect: [f32; 4]) {
    out.extend_from_slice(&pos[0].to_le_bytes());
    out.extend_from_slice(&pos[1].to_le_bytes());
    out.extend_from_slice(&uv0[0].to_le_bytes());
    out.extend_from_slice(&uv0[1].to_le_bytes());
    for v in rect {
        let q = (v.clamp(0.0, 1.0) * f32::from(u16::MAX)).round() as u16;
        out.extend_from_slice(&q.to_le_bytes());
    }
}

/// `PosTex0RectTile`: `PosTex0Rect` (24B) + tile (F32x4, 16B) = 40B.
fn push_pos_tex0_rect_tile(
    out: &mut Vec<u8>,
    pos: [f32; 2],
    uv0: [f32; 2],
    rect: [f32; 4],
    tile: [f32; 4],
) {
    push_pos_tex0_rect(out, pos, uv0, rect);
    for v in tile {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

#[allow(clippy::too_many_arguments)]
fn make_pattern_batch(
    shader: Shader,
    vertex_offset: u32,
    start_index: u32,
    num_vertices: u32,
    num_indices: u32,
    vs_uniforms: &[f32; 16],
    ps_uniforms: &[f32; 4],
    pattern_sampler: SamplerState,
) -> Batch {
    Batch {
        shader,
        render_state: RenderState::new(true, BlendMode::Src, StencilMode::Disabled, false),
        stencil_ref: 0,
        single_pass_stereo: false,
        vertex_offset,
        num_vertices,
        start_index,
        num_indices,
        // Non-null so any accidental null-check fires loudly; handle
        // resolution goes through `test_set_forced_pattern` instead.
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
