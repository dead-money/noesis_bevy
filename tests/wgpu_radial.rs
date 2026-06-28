//! Phase 4.E regression test: `PATH_RADIAL` + `PATH_AA_RADIAL` both compile,
//! bind the `ramps` texture at group(2), and compute the radial `u` parameter
//! correctly from `uv0`.
//!
//! Strategy — drive `WgpuRenderDevice` directly (no Noesis), with uniforms
//! picked so `u = sqrt(uv0.x² + uv0.y²)`. Render a full-screen quad whose
//! `uv0` attribute copies clip-space `pos`, then sample a 256×1 ramp where
//! texel R equals its index. The output R channel at each pixel becomes a
//! direct encoding of `u * 255` — easy to pixel-assert.
//!
//! Pipeline shared with `PATH_LINEAR`; the distinguishing bit is the
//! `PAINT_RADIAL` fragment branch we just ported.

use std::ffi::c_void;

use dm_noesis_bevy::render_device::WgpuRenderDevice;
use dm_noesis_runtime::render_device::types::{
    Batch, BlendMode, MinMagFilter, MipFilter, RenderState, SamplerState, Shader, StencilMode,
    TextureFormat, UniformData, WrapMode,
};
use dm_noesis_runtime::render_device::{RenderDevice, TextureDesc};

const TARGET_W: u32 = 128;
const TARGET_H: u32 = 128;
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
const BYTES_PER_ROW: u32 = TARGET_W * 4;

const CLEAR: [u8; 4] = [0, 0, 64, 255];
const RAMP_W: u32 = 256;

#[test]
fn radial_variants_sample_ramp_at_computed_radius() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        dm_noesis_runtime::set_license(&name, &key);
    }
    dm_noesis_runtime::init();
    pollster::block_on(run_test());
    dm_noesis_runtime::shutdown();
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
            label: Some("dm_noesis_runtime radial test device"),
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
        label: Some("radial target"),
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

    // ── 256×1 ramp: R channel = texel index, full alpha. ───────────────────
    // Sampling with NEAREST at u ∈ [0, 1] picks texel floor(u * 256), so
    // output R ≈ round(u * 255) for u ∈ [0, 1]; clamped beyond.
    let mut ramp_texels = Vec::with_capacity(RAMP_W as usize * 4);
    for i in 0..RAMP_W {
        ramp_texels.push(i as u8); // R
        ramp_texels.push(0); // G
        ramp_texels.push(0); // B
        ramp_texels.push(255); // A
    }
    let level_data = [&ramp_texels[..]];
    let ramp_binding = rd.create_texture(TextureDesc {
        label: "ramp 256x1",
        width: RAMP_W,
        height: 1,
        num_levels: 1,
        format: TextureFormat::Rgba8,
        data: Some(&level_data),
    });

    // ── Geometry: one full-screen PosTex0 quad for PATH_RADIAL, plus a
    //    smaller PosTex0Coverage quad for PATH_AA_RADIAL so both variants
    //    exercise their pipelines. ────────────────────────────────────────
    //
    // uv0 copies clip-space xy so the radial math reduces to
    //   u = sqrt(uv0.x² + uv0.y²) = distance-from-screen-centre in clip units.
    //
    // PosTex0 layout: pos.xy (F32x2, 8B) + tex0.xy (F32x2, 8B) = 16B
    let full_quad: [f32; 4 * 4] = [
        -1.0, -1.0, -1.0, -1.0, 1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
    ];
    let mut vb = Vec::with_capacity(full_quad.len() * 4);
    for v in full_quad {
        vb.extend_from_slice(&v.to_le_bytes());
    }
    assert_eq!(vb.len(), 64); // 4 verts × 16 bytes

    // PosTex0Coverage layout: pos.xy (F32x2, 8B) + tex0.xy (F32x2, 8B) + cov (F32, 4B) = 20B.
    // Place the AA quad inside a small corner of the target; coverage = 1.0
    // so it behaves as a fully-covered path.
    let aa_quad: [f32; 4 * 5] = [
        -0.9, -0.9, -0.9, -0.9, 1.0, -0.7, -0.9, -0.7, -0.9, 1.0, -0.9, -0.7, -0.9, -0.7, 1.0,
        -0.7, -0.7, -0.7, -0.7, 1.0,
    ];
    for v in aa_quad {
        vb.extend_from_slice(&v.to_le_bytes());
    }
    // 4 vertices × 20 bytes = 80
    assert_eq!(vb.len(), 64 + 80);

    // Indices — two triangle-list quads share the same [0,1,2, 1,3,2] layout.
    let quad_idx = [0u16, 1, 2, 1, 3, 2];
    let mut ib = Vec::with_capacity(24);
    for _ in 0..2 {
        for i in quad_idx {
            ib.extend_from_slice(&i.to_le_bytes());
        }
    }

    // ── Uniforms. ──────────────────────────────────────────────────────────
    // Identity projection: pos.xy → clip.xy (wgpu's internal clip space).
    let identity_mat: [f32; 16] = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    // ps_uniforms0 layout (matches WGSL `values: array<vec4, 2>`):
    //   values[0] = (cb[0], cb[1], cb[2], cb[3]) — u coefs, opacity
    //   values[1] = (cb[4], cb[5], cb[6], _)     — dd coefs, ramp row
    //
    // With cb[0..2] = (0, 0, 1) and cb[4..5] = (0, 0), `u` collapses to
    //   u = sqrt(uv0.x² + uv0.y²)
    // and `dd` is zero. cb[6] = 0.5 → ramp row at the centre of the 1-tall
    // atlas (NEAREST / 1-row texture → picks the only row either way).
    let ps_uniform0: [f32; 8] = [0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 0.5, 0.0];

    // Nearest / clamp / no mips — so we can pin down the sampled R value.
    let sampler_state = SamplerState::new(
        WrapMode::ClampToEdge,
        MinMagFilter::Nearest,
        MipFilter::Disabled,
    );
    rd.test_set_forced_pattern(Some((ramp_binding.handle, sampler_state)));

    // ── Drive the device. ──────────────────────────────────────────────────
    rd.begin_onscreen_render();

    rd.map_vertices(vb.len() as u32).copy_from_slice(&vb);
    rd.unmap_vertices();
    rd.map_indices(ib.len() as u32).copy_from_slice(&ib);
    rd.unmap_indices();

    let radial = make_radial_batch(
        Shader::PATH_RADIAL,
        /* vertex_offset */ 0,
        /* start_index */ 0,
        /* num_vertices */ 4,
        /* num_indices */ 6,
        &identity_mat,
        &ps_uniform0,
        sampler_state,
    );
    rd.draw_batch(&radial);

    let aa_radial = make_radial_batch(
        Shader::PATH_AA_RADIAL,
        /* vertex_offset */ 64,
        /* start_index */ 6,
        /* num_vertices */ 4,
        /* num_indices */ 6,
        &identity_mat,
        &ps_uniform0,
        sampler_state,
    );
    rd.draw_batch(&aa_radial);

    rd.end_onscreen_render();
    rd.test_set_forced_pattern(None);

    // ── Read back. ─────────────────────────────────────────────────────────
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

    // Target pixel → clip coordinate → uv0:
    //   clip.x = (px + 0.5) / W * 2 - 1,  clip.y = 1 - (py + 0.5) / H * 2
    //   uv0 = clip  (by our vertex setup)
    //   u   = sqrt(uv0.x² + uv0.y²)
    //
    // Ramp R ≈ round(u * 255), clamped.

    // Centre pixel → u ≈ 0 → R ≈ 0 (also G = B = 0 from the ramp).
    let c = pixel(TARGET_W / 2, TARGET_H / 2);
    assert!(
        c[0] <= 4,
        "centre pixel R should be near 0 (u≈0), got {c:?}",
    );

    // Half-radius along +X: pixel ≈ (W * 3/4, H/2) → uv0 ≈ (0.5, 0) → u ≈ 0.5
    // → R ≈ 128. Allow ±8 for nearest-sampler rounding + the 0.5 pixel bias.
    let mid = pixel(TARGET_W * 3 / 4, TARGET_H / 2);
    assert!(
        (120..=136).contains(&mid[0]),
        "mid-radius R should be ~128 (u≈0.5), got {mid:?}",
    );

    // Corner → uv0 ≈ (±1, ±1) → u ≈ √2 → clamped → R = 255.
    let corner = pixel(0, 0);
    assert_eq!(
        corner[0], 255,
        "corner R should clamp to 255, got {corner:?}"
    );
    assert_eq!(corner[3], 255, "alpha passthrough from ramp");

    // PATH_AA_RADIAL quad covers pixel range roughly corresponding to clip
    // [-0.9, -0.7] × [-0.9, -0.7]. Picking a point inside: pixel (9, 112).
    // uv0 ≈ (-0.86, -0.86), u ≈ 1.21 → clamped → R = 255. Confirms the AA
    // variant compiled, bound group(2), and interpolated coverage=1.0 without
    // killing the paint.
    let aa = pixel(9, 112);
    assert_eq!(
        aa[0], 255,
        "AA-radial quad should paint clamped R=255, got {aa:?}",
    );
    assert_eq!(aa[3], 255, "AA-radial alpha = ramp.a (×coverage=1)");

    drop(data);
    readback.unmap();
}

#[allow(clippy::too_many_arguments)]
fn make_radial_batch(
    shader: Shader,
    vertex_offset: u32,
    start_index: u32,
    num_vertices: u32,
    num_indices: u32,
    vs_uniforms: &[f32; 16],
    ps_uniforms: &[f32; 8],
    ramps_sampler: SamplerState,
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
        pattern: std::ptr::null_mut(),
        // Non-null so any accidental null-check fires loudly; handle
        // resolution goes through `test_set_forced_pattern` and never
        // dereferences this pointer.
        ramps: std::ptr::dangling_mut(),
        image: std::ptr::null_mut(),
        glyphs: std::ptr::null_mut(),
        shadow: std::ptr::null_mut(),
        pattern_sampler: SamplerState::default(),
        ramps_sampler,
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
                num_dwords: 8,
                hash: 2,
            },
            UniformData::default(),
        ],
        pixel_shader: std::ptr::null_mut(),
    }
}
