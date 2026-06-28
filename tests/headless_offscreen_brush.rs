//! Phase 4.E offscreen-RT diagnostic. Wraps `WgpuRenderDevice` with a
//! `RecordingDevice` and drives a handful of tile-brush / clipping
//! scenes, logging the full op sequence Noesis emits.
//!
//! Findings so far:
//!
//! - **`DrawingBrush` is not implemented by Noesis.** The SDK has no
//!   `DrawingBrush.h`; only `SolidColorBrush`, `ImageBrush`,
//!   `VisualBrush`, `LinearGradientBrush`, `RadialGradientBrush`.
//!   XAML that uses `<DrawingBrush>` (including `09_tiled_pattern.xaml`
//!   and the tiled pieces of `Data/Styles/Windows.xaml`) silently skips
//!   the fill. Not an offscreen-RT bug in our device — a missing
//!   Noesis feature, nothing to chase here.
//!
//! The remaining offscreen observations (e.g. `03_scroll.xaml` content
//! viewport blank under a theme) are different bugs — still open.
//!
//! Runs via `cargo test -- --nocapture` to print the trace to stderr.

#![allow(clippy::too_many_lines)]

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use noesis_runtime::render_device::types::{Batch, DeviceCaps, Tile};
use noesis_runtime::render_device::{
    RenderDevice, RenderTargetBinding, RenderTargetDesc, RenderTargetHandle, TextureBinding,
    TextureDesc, TextureHandle, TextureRect,
};
use noesis_runtime::view::{FrameworkElement, RenderFlag, View};
use noesis_runtime::xaml_provider::XamlProvider;

const RT_SIZE: u32 = 128;
const BYTES_PER_ROW: u32 = 512;

#[derive(Debug, Clone)]
#[allow(dead_code)]
enum Op {
    CreateTexture {
        handle: u64,
        w: u32,
        h: u32,
    },
    UpdateTexture {
        handle: u64,
        level: u32,
        rect: TextureRect,
        len: usize,
    },
    DropTexture {
        handle: u64,
    },
    CreateRenderTarget {
        handle: u64,
        w: u32,
        h: u32,
        needs_stencil: bool,
    },
    CloneRenderTarget {
        handle: u64,
    },
    DropRenderTarget {
        handle: u64,
    },
    BeginOffscreenRender,
    EndOffscreenRender,
    BeginOnscreenRender,
    EndOnscreenRender,
    SetRenderTarget {
        handle: u64,
    },
    BeginTile {
        handle: u64,
        tile: Tile,
    },
    EndTile {
        handle: u64,
    },
    ResolveRenderTarget {
        handle: u64,
        tiles: Vec<Tile>,
    },
    MapVertices {
        bytes: u32,
    },
    UnmapVertices,
    MapIndices {
        bytes: u32,
    },
    UnmapIndices,
    EndUpdatingTextures {
        count: usize,
    },
    DrawBatch {
        shader: u8,
        num_vertices: u32,
        num_indices: u32,
        pattern: Option<u64>,
        ramps: Option<u64>,
        image: Option<u64>,
        glyphs: Option<u64>,
    },
}

struct RecordingDevice<D: RenderDevice> {
    inner: D,
    ops: Arc<Mutex<Vec<Op>>>,
}

impl<D: RenderDevice> RecordingDevice<D> {
    fn new(inner: D) -> (Self, Arc<Mutex<Vec<Op>>>) {
        let ops = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner,
                ops: Arc::clone(&ops),
            },
            ops,
        )
    }

    fn push(&self, op: Op) {
        self.ops.lock().unwrap().push(op);
    }
}

impl<D: RenderDevice> RenderDevice for RecordingDevice<D> {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn caps(&self) -> DeviceCaps {
        self.inner.caps()
    }
    fn create_texture(&mut self, desc: TextureDesc<'_>) -> TextureBinding {
        let w = desc.width;
        let h = desc.height;
        let binding = self.inner.create_texture(desc);
        self.push(Op::CreateTexture {
            handle: binding.handle.0.get(),
            w,
            h,
        });
        binding
    }
    fn update_texture(&mut self, h: TextureHandle, level: u32, rect: TextureRect, data: &[u8]) {
        self.push(Op::UpdateTexture {
            handle: h.0.get(),
            level,
            rect,
            len: data.len(),
        });
        self.inner.update_texture(h, level, rect, data);
    }
    fn end_updating_textures(&mut self, t: &[TextureHandle]) {
        self.push(Op::EndUpdatingTextures { count: t.len() });
        self.inner.end_updating_textures(t);
    }
    fn drop_texture(&mut self, h: TextureHandle) {
        self.push(Op::DropTexture { handle: h.0.get() });
        self.inner.drop_texture(h);
    }
    fn create_render_target(&mut self, desc: RenderTargetDesc<'_>) -> RenderTargetBinding {
        let w = desc.width;
        let h = desc.height;
        let needs_stencil = desc.needs_stencil;
        let binding = self.inner.create_render_target(desc);
        self.push(Op::CreateRenderTarget {
            handle: binding.handle.0.get(),
            w,
            h,
            needs_stencil,
        });
        binding
    }
    fn clone_render_target(&mut self, label: &str, src: RenderTargetHandle) -> RenderTargetBinding {
        let binding = self.inner.clone_render_target(label, src);
        self.push(Op::CloneRenderTarget {
            handle: binding.handle.0.get(),
        });
        binding
    }
    fn drop_render_target(&mut self, h: RenderTargetHandle) {
        self.push(Op::DropRenderTarget { handle: h.0.get() });
        self.inner.drop_render_target(h);
    }
    fn begin_offscreen_render(&mut self) {
        self.push(Op::BeginOffscreenRender);
        self.inner.begin_offscreen_render();
    }
    fn end_offscreen_render(&mut self) {
        self.push(Op::EndOffscreenRender);
        self.inner.end_offscreen_render();
    }
    fn begin_onscreen_render(&mut self) {
        self.push(Op::BeginOnscreenRender);
        self.inner.begin_onscreen_render();
    }
    fn end_onscreen_render(&mut self) {
        self.push(Op::EndOnscreenRender);
        self.inner.end_onscreen_render();
    }
    fn set_render_target(&mut self, h: RenderTargetHandle) {
        self.push(Op::SetRenderTarget { handle: h.0.get() });
        self.inner.set_render_target(h);
    }
    fn begin_tile(&mut self, h: RenderTargetHandle, tile: Tile) {
        self.push(Op::BeginTile {
            handle: h.0.get(),
            tile,
        });
        self.inner.begin_tile(h, tile);
    }
    fn end_tile(&mut self, h: RenderTargetHandle) {
        self.push(Op::EndTile { handle: h.0.get() });
        self.inner.end_tile(h);
    }
    fn resolve_render_target(&mut self, h: RenderTargetHandle, tiles: &[Tile]) {
        self.push(Op::ResolveRenderTarget {
            handle: h.0.get(),
            tiles: tiles.to_vec(),
        });
        self.inner.resolve_render_target(h, tiles);
    }
    fn map_vertices(&mut self, bytes: u32) -> &mut [u8] {
        self.push(Op::MapVertices { bytes });
        self.inner.map_vertices(bytes)
    }
    fn unmap_vertices(&mut self) {
        self.push(Op::UnmapVertices);
        self.inner.unmap_vertices();
    }
    fn map_indices(&mut self, bytes: u32) -> &mut [u8] {
        self.push(Op::MapIndices { bytes });
        self.inner.map_indices(bytes)
    }
    fn unmap_indices(&mut self) {
        self.push(Op::UnmapIndices);
        self.inner.unmap_indices();
    }
    fn draw_batch(&mut self, batch: &Batch) {
        self.push(Op::DrawBatch {
            shader: batch.shader.0,
            num_vertices: batch.num_vertices,
            num_indices: batch.num_indices,
            pattern: batch.pattern_handle().map(|h| h.0.get()),
            ramps: batch.ramps_handle().map(|h| h.0.get()),
            image: batch.image_handle().map(|h| h.0.get()),
            glyphs: batch.glyphs_handle().map(|h| h.0.get()),
        });
        self.inner.draw_batch(batch);
    }
}

struct InMemoryXamlProvider(HashMap<String, Vec<u8>>);
impl XamlProvider for InMemoryXamlProvider {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn load_xaml(&mut self, uri: &str) -> Option<&[u8]> {
        self.0.get(uri).map(Vec::as_slice)
    }
}

fn shader_name(s: u8) -> &'static str {
    match s {
        0 => "RGBA",
        1 => "MASK",
        2 => "CLEAR",
        3 => "PATH_SOLID",
        4 => "PATH_LINEAR",
        5 => "PATH_RADIAL",
        6 => "PATH_PATTERN",
        7 => "PATH_PATTERN_CLAMP",
        8 => "PATH_PATTERN_REPEAT",
        9 => "PATH_PATTERN_MIRROR_U",
        10 => "PATH_PATTERN_MIRROR_V",
        11 => "PATH_PATTERN_MIRROR",
        12 => "PATH_AA_SOLID",
        13 => "PATH_AA_LINEAR",
        14 => "PATH_AA_RADIAL",
        15 => "PATH_AA_PATTERN",
        16 => "PATH_AA_PATTERN_CLAMP",
        17 => "PATH_AA_PATTERN_REPEAT",
        18 => "PATH_AA_PATTERN_MIRROR_U",
        19 => "PATH_AA_PATTERN_MIRROR_V",
        20 => "PATH_AA_PATTERN_MIRROR",
        21 => "SDF_SOLID",
        39 => "OPACITY_SOLID",
        48 => "UPSAMPLE",
        49 => "DOWNSAMPLE",
        50 => "SHADOW",
        51 => "BLUR",
        _ => "<unknown>",
    }
}

fn print_trace(label: &str, ops: &[Op]) {
    eprintln!(
        "── {label} ({} ops) ─────────────────────────────────",
        ops.len()
    );
    for (i, op) in ops.iter().enumerate() {
        match op {
            Op::DrawBatch {
                shader,
                num_vertices,
                num_indices,
                pattern,
                ramps,
                image,
                glyphs,
            } => {
                eprintln!(
                    "  [{i:3}] DrawBatch {:<25} v={num_vertices} i={num_indices} \
                     pat={pattern:?} ramps={ramps:?} image={image:?} glyphs={glyphs:?}",
                    shader_name(*shader),
                );
            }
            other => eprintln!("  [{i:3}] {other:?}"),
        }
    }
    eprintln!("──────────────────────────────────────────────────────");
}

#[test]
fn drawingbrush_tile_offscreen_trace() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();

    // (scenario_name, xaml, ticks). Baselines first, then zoom in on
    // ScrollViewer — the only real offscreen case we still need to
    // explain (blank content viewport with theme loaded).
    let scenarios: &[(&str, &[u8], u32)] = &[
        // Baseline: solid fill confirms layout + basic path works.
        (
            "solid-fill-baseline",
            br##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                       Background="#FF101418">
                <Rectangle Width="96" Height="96" Fill="#FF3AA0FF"
                           HorizontalAlignment="Center" VerticalAlignment="Center"/>
            </Grid>"##,
            1,
        ),
        // DrawingBrush — confirmed no-op because Noesis has no
        // `DrawingBrush.h`. Kept as the negative reference.
        (
            "drawingbrush-unsupported",
            br##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                       Background="#FF101418">
                <Rectangle Width="96" Height="96"
                           HorizontalAlignment="Center" VerticalAlignment="Center">
                    <Rectangle.Fill>
                        <DrawingBrush TileMode="Tile">
                            <DrawingBrush.Drawing>
                                <GeometryDrawing Brush="#FF3AA0FF">
                                    <GeometryDrawing.Geometry>
                                        <RectangleGeometry Rect="0,0,16,16"/>
                                    </GeometryDrawing.Geometry>
                                </GeometryDrawing>
                            </DrawingBrush.Drawing>
                        </DrawingBrush>
                    </Rectangle.Fill>
                </Rectangle>
            </Grid>"##,
            1,
        ),
        // VisualBrush — existence confirmed in `NsGui/VisualBrush.h`.
        // Should round-trip into an offscreen RT + pattern draw.
        (
            "visualbrush-rectangle",
            br##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                       Background="#FF101418">
                <Rectangle Width="96" Height="96"
                           HorizontalAlignment="Center" VerticalAlignment="Center">
                    <Rectangle.Fill>
                        <VisualBrush TileMode="Tile"
                                     Viewport="0,0,16,16" ViewportUnits="Absolute"
                                     Viewbox="0,0,16,16" ViewboxUnits="Absolute">
                            <VisualBrush.Visual>
                                <Rectangle Width="16" Height="16" Fill="#FF3AA0FF"/>
                            </VisualBrush.Visual>
                        </VisualBrush>
                    </Rectangle.Fill>
                </Rectangle>
            </Grid>"##,
            1,
        ),
        // ScrollViewer — the real remaining mystery. No custom Style:
        // if a theme isn't loaded the ScrollViewer template resolves
        // to nothing and we probably see no clip-induced offscreen at
        // all; if one IS loaded (see `-with-theme` scenario below)
        // we'd expect offscreen draws for the content clip.
        (
            "scrollviewer-no-theme",
            br##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
                       Background="#FF101418">
                <ScrollViewer Width="100" Height="80"
                              HorizontalAlignment="Center" VerticalAlignment="Center">
                    <StackPanel>
                        <Rectangle Height="40" Fill="#FF3AA0FF"/>
                        <Rectangle Height="40" Fill="#FFFF8A3C"/>
                        <Rectangle Height="40" Fill="#FF7AD47A"/>
                    </StackPanel>
                </ScrollViewer>
            </Grid>"##,
            1,
        ),
    ];

    pollster::block_on(async {
        for (name, xaml, ticks) in scenarios {
            let (ops, center, corner) = run_scenario(xaml, *ticks).await;
            print_trace(name, &ops);
            let offscreen_phases = ops
                .iter()
                .filter(|o| matches!(o, Op::BeginOffscreenRender))
                .count();
            let rt_creates = ops
                .iter()
                .filter(|o| matches!(o, Op::CreateRenderTarget { .. }))
                .count();
            let tiles = ops
                .iter()
                .filter(|o| matches!(o, Op::BeginTile { .. }))
                .count();
            let repeat_draws = ops
                .iter()
                .filter(|o| matches!(o, Op::DrawBatch { shader: 8 | 17, .. }))
                .count();
            eprintln!(
                "  → {name}: center={center:?} corner={corner:?} \
                 offscreen_phases={offscreen_phases} rt_creates={rt_creates} \
                 tiles={tiles} repeat_draws={repeat_draws}\n",
            );
        }
    });

    noesis_runtime::shutdown();
}

async fn run_scenario(xaml: &[u8], ticks: u32) -> (Vec<Op>, [u8; 4], [u8; 4]) {
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
            label: Some("offscreen-brush diag device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen-brush target"),
        size: wgpu::Extent3d {
            width: RT_SIZE,
            height: RT_SIZE,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());

    let mut wgpu_device =
        dm_noesis_bevy::render_device::WgpuRenderDevice::new(device.clone(), queue.clone());
    wgpu_device.set_onscreen_target(target_view);

    let (recording, ops) = RecordingDevice::new(wgpu_device);
    let registered_device = noesis_runtime::render_device::register(recording);

    let provider = InMemoryXamlProvider(HashMap::from([("test.xaml".to_string(), xaml.to_vec())]));
    let _registered_provider = noesis_runtime::xaml_provider::set_xaml_provider(provider);

    let element = FrameworkElement::load("test.xaml").expect("XAML load failed");
    let mut view = View::create(element);
    view.set_size(RT_SIZE, RT_SIZE);
    view.set_flags(RenderFlag::Ppaa as u32);

    {
        let mut renderer = view.renderer();
        renderer.init(&registered_device);
    }

    for i in 0..ticks {
        let t = f64::from(i) * 0.016;
        let _changed = view.update(t);
        let mut renderer = view.renderer();
        let _tree = renderer.update_render_tree();
        if i + 1 < ticks {
            continue;
        }
        let _off = renderer.render_offscreen();
        renderer.render(false, true);
    }

    {
        let mut renderer = view.renderer();
        renderer.shutdown();
    }
    drop(view);

    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(BYTES_PER_ROW) * u64::from(RT_SIZE),
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
        sender.send(r).expect("readback send");
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
    let center = pixel(RT_SIZE / 2, RT_SIZE / 2);
    let corner = pixel(2, 2);
    drop(data);
    readback.unmap();

    let ops_snapshot = ops.lock().unwrap().clone();
    (ops_snapshot, center, corner)
}
