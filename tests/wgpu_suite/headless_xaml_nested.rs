//! Headless diagnostic for nested-element rendering via a recording `RenderDevice`.
//!
//! Wraps `WgpuRenderDevice` in `RecordingDevice` to capture draw-batch counts and
//! pixel readbacks across XAML variations. The assertions encode two things:
//! `SetProjectionMatrix` culls child elements (captured regression) and omitting it
//! restores correct child rendering.

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
#[allow(dead_code)] // fields read via Debug-print
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
    },
    DropTexture {
        handle: u64,
    },
    CreateRenderTarget {
        handle: u64,
        w: u32,
        h: u32,
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
    MapVertices {
        bytes: u32,
    },
    UnmapVertices,
    VertexBuffer {
        bytes: Vec<u8>,
    },
    MapIndices {
        bytes: u32,
    },
    UnmapIndices,
    DrawBatch {
        shader: u8,
        num_vertices: u32,
        num_indices: u32,
        vertex_offset: u32,
        start_index: u32,
        pattern: Option<u64>,
        ramps: Option<u64>,
        glyphs: Option<u64>,
        vs_uniforms: Vec<Vec<u8>>,
        ps_uniforms: Vec<Vec<u8>>,
    },
}

struct RecordingDevice<D: RenderDevice> {
    inner: D,
    ops: Arc<Mutex<Vec<Op>>>,
    scratch_vertices: Vec<u8>,
    scratch_indices: Vec<u8>,
    pending_vertex_bytes: Option<u32>,
    pending_index_bytes: Option<u32>,
}

impl<D: RenderDevice> RecordingDevice<D> {
    fn new(inner: D) -> (Self, Arc<Mutex<Vec<Op>>>) {
        let ops = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                inner,
                ops: Arc::clone(&ops),
                scratch_vertices: Vec::new(),
                scratch_indices: Vec::new(),
                pending_vertex_bytes: None,
                pending_index_bytes: None,
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

    fn update_texture(
        &mut self,
        handle: TextureHandle,
        level: u32,
        rect: TextureRect,
        data: &[u8],
    ) {
        self.push(Op::UpdateTexture {
            handle: handle.0.get(),
            level,
            rect,
        });
        self.inner.update_texture(handle, level, rect, data);
    }

    fn end_updating_textures(&mut self, textures: &[TextureHandle]) {
        self.inner.end_updating_textures(textures);
    }

    fn drop_texture(&mut self, handle: TextureHandle) {
        self.push(Op::DropTexture {
            handle: handle.0.get(),
        });
        self.inner.drop_texture(handle);
    }

    fn create_render_target(&mut self, desc: RenderTargetDesc<'_>) -> RenderTargetBinding {
        let w = desc.width;
        let h = desc.height;
        let binding = self.inner.create_render_target(desc);
        self.push(Op::CreateRenderTarget {
            handle: binding.handle.0.get(),
            w,
            h,
        });
        binding
    }

    fn clone_render_target(&mut self, label: &str, src: RenderTargetHandle) -> RenderTargetBinding {
        let binding = self.inner.clone_render_target(label, src);
        self.push(Op::CreateRenderTarget {
            handle: binding.handle.0.get(),
            w: binding.resolve_texture.width,
            h: binding.resolve_texture.height,
        });
        binding
    }

    fn drop_render_target(&mut self, handle: RenderTargetHandle) {
        self.push(Op::DropRenderTarget {
            handle: handle.0.get(),
        });
        self.inner.drop_render_target(handle);
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

    fn set_render_target(&mut self, handle: RenderTargetHandle) {
        self.push(Op::SetRenderTarget {
            handle: handle.0.get(),
        });
        self.inner.set_render_target(handle);
    }

    fn begin_tile(&mut self, handle: RenderTargetHandle, tile: Tile) {
        self.push(Op::BeginTile {
            handle: handle.0.get(),
            tile,
        });
        self.inner.begin_tile(handle, tile);
    }

    fn end_tile(&mut self, handle: RenderTargetHandle) {
        self.push(Op::EndTile {
            handle: handle.0.get(),
        });
        self.inner.end_tile(handle);
    }

    fn resolve_render_target(&mut self, handle: RenderTargetHandle, tiles: &[Tile]) {
        self.inner.resolve_render_target(handle, tiles);
    }

    fn map_vertices(&mut self, bytes: u32) -> &mut [u8] {
        self.push(Op::MapVertices { bytes });
        self.pending_vertex_bytes = Some(bytes);
        if self.scratch_vertices.len() < bytes as usize {
            self.scratch_vertices.resize(bytes as usize, 0);
        }
        // scratch buffer; forwarded to the inner device on unmap
        &mut self.scratch_vertices[..bytes as usize]
    }

    fn unmap_vertices(&mut self) {
        let bytes = self
            .pending_vertex_bytes
            .take()
            .expect("unmap_vertices without map_vertices");
        let snapshot = self.scratch_vertices[..bytes as usize].to_vec();
        let dst = self.inner.map_vertices(bytes);
        dst.copy_from_slice(&snapshot);
        self.inner.unmap_vertices();
        self.push(Op::UnmapVertices);
        self.push(Op::VertexBuffer { bytes: snapshot });
    }

    fn map_indices(&mut self, bytes: u32) -> &mut [u8] {
        self.push(Op::MapIndices { bytes });
        self.pending_index_bytes = Some(bytes);
        if self.scratch_indices.len() < bytes as usize {
            self.scratch_indices.resize(bytes as usize, 0);
        }
        &mut self.scratch_indices[..bytes as usize]
    }

    fn unmap_indices(&mut self) {
        let bytes = self
            .pending_index_bytes
            .take()
            .expect("unmap_indices without map_indices");
        let snapshot = self.scratch_indices[..bytes as usize].to_vec();
        let dst = self.inner.map_indices(bytes);
        dst.copy_from_slice(&snapshot);
        self.inner.unmap_indices();
        self.push(Op::UnmapIndices);
    }

    fn draw_batch(&mut self, batch: &Batch) {
        let vs_uniforms = batch
            .vertex_uniforms
            .iter()
            .map(|u| u.as_bytes().to_vec())
            .collect();
        let ps_uniforms = batch
            .pixel_uniforms
            .iter()
            .map(|u| u.as_bytes().to_vec())
            .collect();
        self.push(Op::DrawBatch {
            shader: batch.shader.0,
            num_vertices: batch.num_vertices,
            num_indices: batch.num_indices,
            vertex_offset: batch.vertex_offset,
            start_index: batch.start_index,
            pattern: batch.pattern_handle().map(|h| h.0.get()),
            ramps: batch.ramps_handle().map(|h| h.0.get()),
            glyphs: batch.glyphs_handle().map(|h| h.0.get()),
            vs_uniforms,
            ps_uniforms,
        });
        self.inner.draw_batch(batch);
    }
}

fn as_floats(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
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
                vertex_offset,
                start_index,
                pattern,
                ramps,
                glyphs,
                vs_uniforms,
                ps_uniforms,
            } => {
                eprintln!(
                    "  [{i:3}] DrawBatch shader={shader} v={num_vertices} i={num_indices} \
                     vo={vertex_offset} si={start_index} \
                     pat={pattern:?} ramps={ramps:?} glyphs={glyphs:?}"
                );
                for (slot, u) in vs_uniforms.iter().enumerate() {
                    if !u.is_empty() {
                        eprintln!(
                            "         vs[{slot}] {} dwords: {:?}",
                            u.len() / 4,
                            as_floats(u)
                        );
                    }
                }
                for (slot, u) in ps_uniforms.iter().enumerate() {
                    if !u.is_empty() {
                        eprintln!(
                            "         ps[{slot}] {} dwords: {:?}",
                            u.len() / 4,
                            as_floats(u)
                        );
                    }
                }
            }
            Op::VertexBuffer { bytes } => {
                eprintln!("  [{i:3}] VertexBuffer {} bytes:", bytes.len());
                // vec2 position at offset 0 for most shader variants; remainder varies by format
                for (idx, chunk) in bytes.chunks(8).enumerate() {
                    if chunk.len() == 8 {
                        let x = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        let y = f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                        eprintln!(
                            "         off={:3} pos=({x:>7.2}, {y:>7.2}) raw={:02x?}",
                            idx * 8,
                            chunk
                        );
                    } else {
                        eprintln!("         off={:3} raw={:02x?}", idx * 8, chunk);
                    }
                }
            }
            other => eprintln!("  [{i:3}] {other:?}"),
        }
    }
    eprintln!("──────────────────────────────────────────────────────");
}

struct InMemoryXamlProvider {
    bytes: HashMap<String, Vec<u8>>,
}

impl XamlProvider for InMemoryXamlProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn load_xaml(&mut self, uri: &str) -> Option<&[u8]> {
        self.bytes.get(uri).map(Vec::as_slice)
    }
}

#[derive(Clone, Copy)]
struct ScenarioOptions {
    ppaa: bool,
    update_iterations: u32,
    set_projection: bool,
}

impl Default for ScenarioOptions {
    fn default() -> Self {
        Self {
            ppaa: true,
            update_iterations: 1,
            set_projection: true,
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn nested_child_grid_diagnostic() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        crate::common::claim_noesis_process();
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();

    let scenarios: &[(&str, &[u8], ScenarioOptions)] = &[
        // Sanity check: 1 PATH_AA_SOLID batch expected, whole surface red.
        (
            "leaf-grid-red",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red"/>"#,
            ScenarioOptions::default(),
        ),
        // Nested Grid with explicit Width/Height: expected outer-red + inner-yellow,
        // observed outer-red only.
        (
            "nested-grid-yellow-child",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Same scene, PPAA off. If this draws the inner, PPAA's tessellation
        // / batch reordering is the culprit.
        (
            "nested-grid-yellow-child-no-ppaa",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64"/>
                </Grid>"#,
            ScenarioOptions { ppaa: false, ..ScenarioOptions::default() },
        ),
        // Drive multiple Update cycles. If this draws the inner, layout was
        // not converged after one pass.
        (
            "nested-grid-yellow-child-multi-update",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64"/>
                </Grid>"#,
            ScenarioOptions { update_iterations: 4, ..ScenarioOptions::default() },
        ),
        // Different inner element type. If Rectangle works where Grid doesn't,
        // the issue is Grid-as-child-of-Grid specifically.
        (
            "nested-grid-rectangle-child",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Rectangle Fill="Yellow" Width="64" Height="64" HorizontalAlignment="Center" VerticalAlignment="Center"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Inner Grid stretched to fill (no explicit size, no alignment).
        // Standard WPF Grid default is HorizontalAlignment=Stretch.
        (
            "nested-grid-yellow-child-stretch",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        (
            "border-with-rectangle",
            br#"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Rectangle Fill="Yellow" Width="64" Height="64" HorizontalAlignment="Center" VerticalAlignment="Center"/>
                </Border>"#,
            ScenarioOptions::default(),
        ),
        // Margin-inset: clear inset that should reveal outer red around an
        // inner yellow rectangle. Differentiates "inner not laid out at all"
        // from "inner laid out somewhere unexpected".
        (
            "nested-grid-margin",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Margin="10"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Inner positioned top-left: center reads outer-red, top-left reads inner-yellow.
        // Disentangles "no draw" from "drew at unexpected position".
        (
            "nested-grid-explicit-topleft",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64" HorizontalAlignment="Left" VerticalAlignment="Top"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Inner larger than the view surface, forces overlap regardless of alignment.
        // Yellow here means alignment math is wrong; no yellow means layout drops the inner.
        (
            "nested-grid-larger-inner",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="200" Height="200"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Width/Height + explicit Center alignment. If this paints inner,
        // confirms the bug is "default alignment (Stretch) + explicit Width
        // drops the element from the render tree".
        (
            "nested-grid-explicit-center",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64" HorizontalAlignment="Center" VerticalAlignment="Center"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Just one explicit alignment axis. Helps isolate whether both axes
        // need it or just one.
        (
            "nested-grid-explicit-h-only",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64" HorizontalAlignment="Center"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Canvas + absolute positioning, bypasses alignment entirely.
        (
            "canvas-rectangle-absolute",
            br#"<Canvas xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Rectangle Fill="Yellow" Width="64" Height="64" Canvas.Left="32" Canvas.Top="32"/>
                </Canvas>"#,
            ScenarioOptions::default(),
        ),
        (
            "stackpanel-two-rects",
            br#"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Rectangle Fill="Yellow" Height="32"/>
                  <Rectangle Fill="Lime" Height="32"/>
                </StackPanel>"#,
            ScenarioOptions::default(),
        ),
        // Outer with explicit Width/Height: checks whether the outer element itself renders.
        (
            "outer-explicit-size",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red" Width="64" Height="64"/>"#,
            ScenarioOptions::default(),
        ),
        // Same as `nested-grid-yellow-child` but Right+Bottom alignment.
        (
            "nested-grid-explicit-bottomright",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64" HorizontalAlignment="Right" VerticalAlignment="Bottom"/>
                </Grid>"#,
            ScenarioOptions::default(),
        ),
        // Tests the hypothesis that SetProjectionMatrix causes child culling.
        // These should draw the inner element if the projection call is the trigger.
        (
            "no-projection-nested-yellow",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Grid Background="Yellow" Width="64" Height="64"/>
                </Grid>"#,
            ScenarioOptions { set_projection: false, ..ScenarioOptions::default() },
        ),
        (
            "no-projection-canvas-rect",
            br#"<Canvas xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red">
                  <Rectangle Fill="Yellow" Width="64" Height="64" Canvas.Left="32" Canvas.Top="32"/>
                </Canvas>"#,
            ScenarioOptions { set_projection: false, ..ScenarioOptions::default() },
        ),
        (
            "no-projection-outer-explicit",
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red" Width="64" Height="64"/>"#,
            ScenarioOptions { set_projection: false, ..ScenarioOptions::default() },
        ),
    ];

    let mut summary = Vec::new();

    pollster::block_on(async {
        for (name, xaml, opts) in scenarios {
            let (ops, center, corner) = run_scenario(xaml, *opts).await;
            print_trace(name, &ops);
            let draw_count = ops
                .iter()
                .filter(|o| matches!(o, Op::DrawBatch { .. }))
                .count();
            eprintln!("  → {name}: draws={draw_count} center={center:?} corner={corner:?}\n");
            summary.push((name.to_string(), draw_count, center, corner));
        }
    });

    noesis_runtime::shutdown();

    eprintln!("══ Summary ═══════════════════════════════════════════");
    for (name, draws, center, corner) in &summary {
        eprintln!("  {name:<45} draws={draws} center={center:?} corner={corner:?}");
    }
    eprintln!("══════════════════════════════════════════════════════");

    let by_name = |needle: &str| -> ([u8; 4], [u8; 4]) {
        let entry = summary
            .iter()
            .find(|(n, ..)| n == needle)
            .unwrap_or_else(|| panic!("scenario {needle:?} missing"));
        (entry.2, entry.3)
    };

    let (center, corner) = by_name("no-projection-nested-yellow");
    assert!(
        center[0] > 200 && center[1] > 200 && center[2] < 50,
        "no-projection nested-yellow: center should be inner-Yellow, got {center:?}",
    );
    assert!(
        corner[0] > 200 && corner[1] < 50 && corner[2] < 50,
        "no-projection nested-yellow: corner should be outer-Red, got {corner:?}",
    );

    let (center, corner) = by_name("no-projection-canvas-rect");
    assert!(
        center[0] > 200 && center[1] > 200 && center[2] < 50,
        "no-projection canvas-rect: center should be inner-Yellow, got {center:?}",
    );
    assert!(
        corner[0] > 200 && corner[1] < 50 && corner[2] < 50,
        "no-projection canvas-rect: corner should be outer-Red, got {corner:?}",
    );

    // SetProjectionMatrix culls children in Noesis's visibility pass (captured regression).
    // A fix should flip this assertion.
    let (center, corner) = by_name("nested-grid-yellow-child");
    assert_eq!(
        center,
        [255, 0, 0, 255],
        "nested-grid-yellow-child still exhibits the SetProjectionMatrix culling bug; \
         center should be outer-Red until a real projection fix lands"
    );
    assert_eq!(corner, [255, 0, 0, 255]);
}

#[allow(clippy::too_many_lines, clippy::similar_names)]
async fn run_scenario(xaml: &[u8], opts: ScenarioOptions) -> (Vec<Op>, [u8; 4], [u8; 4]) {
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
            label: Some("headless_xaml_nested device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless_xaml_nested target"),
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
        noesis_bevy::render_device::WgpuRenderDevice::new(device.clone(), queue.clone());
    wgpu_device.set_onscreen_target(target_view, RT_SIZE, RT_SIZE);

    let (recording, ops) = RecordingDevice::new(wgpu_device);
    let registered_device = noesis_runtime::render_device::register(recording);

    let provider = InMemoryXamlProvider {
        bytes: HashMap::from([("test.xaml".to_string(), xaml.to_vec())]),
    };
    let _registered_provider = noesis_runtime::xaml_provider::set_xaml_provider(provider);

    let element = FrameworkElement::load("test.xaml").expect("XAML load failed");
    let mut view = View::create(element);
    view.set_size(RT_SIZE, RT_SIZE);
    if opts.ppaa {
        view.set_flags(RenderFlag::Ppaa as u32);
    } else {
        view.set_flags(0);
    }

    if opts.set_projection {
        #[allow(clippy::cast_precision_loss)]
        let w = RT_SIZE as f32;
        #[allow(clippy::cast_precision_loss)]
        let h = RT_SIZE as f32;
        #[rustfmt::skip]
        let projection: [f32; 16] = [
            2.0 / w, 0.0,     0.0, -1.0,
            0.0,     2.0 / h, 0.0, -1.0,
            0.0,     0.0,     1.0,  0.0,
            0.0,     0.0,     0.0,  1.0,
        ];
        view.set_projection_matrix(&projection);
    }

    {
        let mut renderer = view.renderer();
        renderer.init(&registered_device);
    }

    // multi-tick to let layout converge
    for i in 0..opts.update_iterations {
        let _changed = view.update(f64::from(i) * 0.016);
        let mut renderer = view.renderer();
        let _new_tree = renderer.update_render_tree();
        if i + 1 < opts.update_iterations {
            // only the final tick paints into the target
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
            label: Some("readback copy"),
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

    let center = pixel(RT_SIZE / 2, RT_SIZE / 2);
    let corner = pixel(0, 0);

    drop(data);
    readback.unmap();

    let ops_snapshot = ops.lock().unwrap().clone();
    (ops_snapshot, center, corner)
}
