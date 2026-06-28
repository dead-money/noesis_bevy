//! Phase 4.D.0 de-risk: the smallest possible render-graph node that paints
//! directly into a Bevy camera's `ViewTarget`. No Noesis.
//!
//! Validates the three things Phase 4.D needs to work:
//! 1. The render-app sub-plugin pattern (`app.sub_app_mut(RenderApp)`).
//! 2. Registering a custom node into `Core2d` ordered against stock labels.
//! 3. Painting into the camera's `ViewTarget::main_texture_view()` from
//!    `Node::run` using the `RenderContext`'s command encoder.
//!
//! Expected result: a Bevy window painted solid red. The camera's
//! `clear_color` is set to teal so that if our node fails to run we see
//! teal instead — that's the failure signal.
//!
//! Run with:
//! ```sh
//! cargo run --example bevy_wgpu_bridge
//! ```

use bevy::core_pipeline::core_2d::graph::{Core2d, Node2d};
use bevy::prelude::*;
use bevy_render::{
    RenderApp,
    render_graph::{
        NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
    },
    renderer::RenderContext,
    view::ViewTarget,
};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "bevy_wgpu_bridge — phase 4.D.0".into(),
                resolution: (640u32, 480u32).into(),
                ..default()
            }),
            ..default()
        }))
        .add_plugins(OverpaintPlugin)
        .add_systems(Startup, setup)
        .run();
}

fn setup(mut commands: Commands) {
    commands.spawn((
        Camera2d,
        Camera {
            // Teal so a failed-to-run node is visually obvious.
            clear_color: ClearColorConfig::Custom(Color::srgb(0.0, 0.3, 0.3)),
            ..default()
        },
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// OverpaintPlugin — the render-graph integration under test.
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
struct OverpaintLabel;

struct OverpaintPlugin;

impl Plugin for OverpaintPlugin {
    fn build(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            warn!("RenderApp not present; OverpaintPlugin is a no-op");
            return;
        };

        render_app
            .add_render_graph_node::<ViewNodeRunner<OverpaintNode>>(Core2d, OverpaintLabel)
            .add_render_graph_edges(
                Core2d,
                (
                    Node2d::MainTransparentPass,
                    OverpaintLabel,
                    Node2d::EndMainPass,
                ),
            );
    }
}

#[derive(Default)]
struct OverpaintNode;

impl ViewNode for OverpaintNode {
    type ViewQuery = &'static ViewTarget;

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        view_target: &'w ViewTarget,
        _world: &'w World,
    ) -> Result<(), NodeRunError> {
        let encoder = render_context.command_encoder();
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("OverpaintNode"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: view_target.main_texture_view(),
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        Ok(())
    }
}
