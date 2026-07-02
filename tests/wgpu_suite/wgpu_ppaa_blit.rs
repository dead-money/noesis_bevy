//! Tests the premultiplied composite blit used when `RenderFlag::Ppaa` is on.
//!
//! Drives [`blit_composite_for_test`], which builds the same `BlitPipeline` and
//! premultiplied "over" blend (`One, OneMinusSrcAlpha`) the render-graph nodes
//! use. Each test composites a fractional-alpha premultiplied source over a
//! target pre-cleared to a distinct colour, reads back pixels, and asserts exact
//! premultiplied blend values with no clear-colour bleed.
//!
//! Pure wgpu: no Noesis FFI, no init/license needed.

use noesis_bevy::render::blit_composite_for_test;

const W: u32 = 4;
const H: u32 = 1;
// Rgba8Unorm: no sRGB round-trip, so the premultiplied blend is exact and deterministic.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
// 4 px * 4 bytes = 16, padded up to COPY_BYTES_PER_ROW_ALIGNMENT (256).
const BYTES_PER_ROW: u32 = 256;

// Distinct camera clear colour (a non-saturated blue) so any bleed shows up.
const CLEAR: [u8; 4] = [0, 0, 200, 255];

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
}

async fn gpu() -> Gpu {
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
            label: Some("ppaa blit test device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available");
    Gpu { device, queue }
}

/// Composite a 4-pixel premultiplied source over a target cleared to [`CLEAR`]
/// and return the four resulting RGBA pixels.
fn composite(gpu: &Gpu, src_pixels: [[u8; 4]; 4]) -> [[u8; 4]; 4] {
    let device = &gpu.device;
    let queue = &gpu.queue;

    // Source intermediate (what Noesis paints into): premultiplied RGBA8.
    let src = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("src intermediate"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let mut bytes = vec![0u8; (BYTES_PER_ROW * H) as usize];
    for (x, px) in src_pixels.iter().enumerate() {
        let off = x * 4;
        bytes[off..off + 4].copy_from_slice(px);
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &src,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(BYTES_PER_ROW),
            rows_per_image: Some(H),
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    let src_view = src.create_view(&wgpu::TextureViewDescriptor::default());

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("composite target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("ppaa blit"),
    });
    // Camera clear: production blit runs LoadOp::Load against this.
    // Block scope: drops the pass at ';' to release &mut encoder for the blit.
    {
        let _clear = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("camera clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &target_view,
                resolve_target: None,
                depth_slice: None,
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
    }

    blit_composite_for_test(device, &mut encoder, &src_view, &target_view, FORMAT);

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(H),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
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
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit(Some(encoder.finish()));

    let slice = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        tx.send(r).expect("send");
    });
    let _ = device.poll(wgpu::PollType::wait_indefinitely());
    rx.recv().expect("recv").expect("map");
    let data = slice.get_mapped_range();

    let mut out = [[0u8; 4]; 4];
    for (x, px) in out.iter_mut().enumerate() {
        let off = x * 4;
        px.copy_from_slice(&data[off..off + 4]);
    }
    out
}

/// Reference premultiplied "over" in 8-bit unorm, matching the GPU blend
/// (`src.rgb + dst.rgb * (1 - src.a)`, factor computed in normalized float).
fn premul_over(src: [u8; 4], dst: [u8; 4]) -> [u8; 3] {
    let f = 1.0 - f32::from(src[3]) / 255.0;
    let chan = |s: u8, d: u8| -> u8 {
        let v = f32::from(s) / 255.0 + (f32::from(d) / 255.0) * f;
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [
        chan(src[0], dst[0]),
        chan(src[1], dst[1]),
        chan(src[2], dst[2]),
    ]
}

fn assert_rgb_close(label: &str, got: [u8; 4], want: [u8; 3]) {
    let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).abs();
    assert!(
        d(got[0], want[0]) <= 1 && d(got[1], want[1]) <= 1 && d(got[2], want[2]) <= 1,
        "{label}: got {:?}, want rgb {:?} (+/-1)",
        got,
        want,
    );
}

#[test]
fn ppaa_fractional_edges_composite_premultiplied_no_clear_bleed() {
    let gpu = pollster::block_on(gpu());

    // Premultiplied red at varying coverage (what Noesis emits for a PPAA edge):
    // rgb = colour * coverage, a = coverage.
    let src = [
        [255, 0, 0, 255], // interior: fully opaque red
        [128, 0, 0, 128], // ~50% AA edge
        [64, 0, 0, 64],   // ~25% AA edge
        [0, 0, 0, 0],     // outside the shape: fully transparent
    ];
    let got = composite(&gpu, src);

    for (i, s) in src.iter().enumerate() {
        assert_rgb_close(&format!("px{i}"), got[i], premul_over(*s, CLEAR));
    }

    // 1. Opaque interior: pure UI red, no clear-colour bleed.
    assert_rgb_close("opaque interior", got[0], [255, 0, 0]);
    // 2. The 50% edge keeps full UI red contribution (R=128). A straight-alpha
    //    blend re-multiplies the premultiplied bytes and would drop R to ~64.
    assert!(
        got[1][0] >= 127,
        "50% edge R={} collapsed — premultiplied content was re-multiplied \
         (straight-alpha bug)",
        got[1][0],
    );
    // 3. The 50% edge shows clear colour through the uncovered half (B>0).
    assert!(
        got[1][2] > 0,
        "50% edge B={} — clear colour overwritten instead of composited",
        got[1][2],
    );
    // 4. Fully transparent texels leave the clear colour untouched.
    assert_rgb_close("transparent", got[3], [CLEAR[0], CLEAR[1], CLEAR[2]]);
}

#[test]
fn ppaa_off_hard_edges_preserve_overwrite_behavior() {
    // With PPAA off, Noesis emits only hard alpha (0 or 255). The premultiplied
    // "over" blend reduces to: opaque texels overwrite the target, transparent
    // texels pass through the clear colour unchanged.
    let gpu = pollster::block_on(gpu());

    let src = [
        [255, 0, 0, 255],   // opaque red
        [0, 255, 0, 255],   // opaque green
        [0, 0, 0, 0],       // transparent
        [255, 255, 0, 255], // opaque yellow
    ];
    let got = composite(&gpu, src);

    assert_eq!([got[0][0], got[0][1], got[0][2]], [255, 0, 0], "opaque red");
    assert_eq!(
        [got[1][0], got[1][1], got[1][2]],
        [0, 255, 0],
        "opaque green"
    );
    assert_eq!(
        [got[2][0], got[2][1], got[2][2]],
        [CLEAR[0], CLEAR[1], CLEAR[2]],
        "transparent keeps clear colour",
    );
    assert_eq!(
        [got[3][0], got[3][1], got[3][2]],
        [255, 255, 0],
        "opaque yellow",
    );
}
