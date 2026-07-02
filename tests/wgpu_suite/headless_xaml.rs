//! End-to-end test: register a `WgpuRenderDevice`, load a `<Grid Background="Red"/>`,
//! drive one frame, and assert every sampled pixel is solid red.
//! Verifies that the `XamlProvider` / `IView` / `IRenderer` FFI surface is correctly wired.
//!
//! Requires `NOESIS_SDK_DIR` to be set.

use std::collections::HashMap;

use noesis_runtime::view::{FrameworkElement, RenderFlag, View};
use noesis_runtime::xaml_provider::XamlProvider;

const RT_SIZE: u32 = 128;
const BYTES_PER_ROW: u32 = 512; // 128 * 4, wgpu COPY_BYTES_PER_ROW_ALIGNMENT-aligned

const RED: [u8; 4] = [255, 0, 0, 255];

// Owned bytes keep the returned slice valid per the "must outlive parsing" contract on load_xaml.
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

#[test]
fn noesis_drives_wgpu_render_device_to_solid_red() {
    crate::common::claim_noesis_process();
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();

    // Scope so every Noesis-owned object drops before shutdown().
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
            label: Some("headless_xaml device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::downlevel_defaults(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device");
    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("headless_xaml target"),
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

    let mut render_device =
        noesis_bevy::render_device::WgpuRenderDevice::new(device.clone(), queue.clone());
    render_device.set_onscreen_target(target_view, RT_SIZE, RT_SIZE);

    let registered_device = noesis_runtime::render_device::register(render_device);

    let provider = InMemoryXamlProvider {
        bytes: HashMap::from([(
            "test.xaml".to_string(),
            br#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation" Background="Red"/>"#
                .to_vec(),
        )]),
    };
    let _registered_provider = noesis_runtime::xaml_provider::set_xaml_provider(provider);

    let element = FrameworkElement::load("test.xaml").expect("XAML load failed");
    let mut view = View::create(element);
    view.set_size(RT_SIZE, RT_SIZE);
    view.set_flags(RenderFlag::Ppaa as u32);
    // Don't call SetProjectionMatrix: Noesis derives the right matrix from
    // DeviceCaps (clip_space_y_inverted, depth_range_zero_to_one). Supplying
    // an OpenGL-style ortho here makes Noesis's render-tree visibility pass
    // cull child elements; see tests/headless_xaml_nested.rs.

    {
        let mut renderer = view.renderer();
        renderer.init(&registered_device);
    }

    let updated = view.update(0.0);
    assert!(updated, "first View::Update should report changes");

    {
        let mut renderer = view.renderer();
        let _new_tree = renderer.update_render_tree();
        let _off = renderer.render_offscreen();
        renderer.render(false, true);
        renderer.shutdown();
    }

    // WgpuRenderDevice auto-submits in end_onscreen_render; no explicit queue.submit needed.
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

    for (x, y) in [
        (0, 0),
        (RT_SIZE / 2, RT_SIZE / 2),
        (RT_SIZE - 1, RT_SIZE - 1),
        (1, 1),
    ] {
        let px = pixel(x, y);
        assert_eq!(px, RED, "pixel ({x},{y}) should be solid red, got {px:?}");
    }

    drop(data);
    readback.unmap();
}
