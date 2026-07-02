//! Headless bridge suite (audit R2): `MinimalPlugins` + `NoesisHeadlessPlugin` bridge tests, plus the direct-Noesis unit tests.
//!
//! Each source file in this directory is one `#[test]` module. The suite
//! links Bevy + Noesis once for all of them instead of once per file, and
//! runs under cargo-nextest so every test still gets its own process (Noesis
//! state is process-global and thread-affine). See tests/README.md.

#[path = "../common/mod.rs"]
mod common;

// The `ecs_ui` example doubles as the fixture for the panel/list/observer tests.
// Loaded once here (loading it per test module trips clippy's duplicate_mod);
// modules reach its items through `crate::ecs_ui::`.
#[allow(dead_code)]
#[path = "../../examples/ecs_ui.rs"]
mod ecs_ui;

mod headless_app_animation;
mod headless_app_binding;
mod headless_app_binding_rebuild;
mod headless_app_bridges;
mod headless_app_brushes;
mod headless_app_commands;
mod headless_app_diagnostics;
mod headless_app_focus_control_drain;
mod headless_app_focus_input;
mod headless_app_focus_predict_remove;
mod headless_app_hot_reload;
mod headless_app_imaging;
mod headless_app_inlines;
mod headless_app_inlines_extend;
mod headless_app_integration;
mod headless_app_matrix3d;
mod headless_app_panel;
mod headless_app_plain_vm;
mod headless_app_plain_vm_two_views;
mod headless_app_props;
mod headless_app_resources;
mod headless_app_resources_chain;
mod headless_app_resources_mixed;
mod headless_app_routed_events;
mod headless_app_shapes;
mod headless_app_styles;
mod headless_app_styles_deep;
mod headless_app_svg;
mod headless_app_transforms;
mod headless_app_transforms3d;
mod headless_app_typed_items;
mod headless_app_typography;
mod headless_app_visual_state;
mod headless_binding_removal_reap;
mod headless_collection_view;
mod headless_component_removal_reap;
mod headless_despawn_teardown;
mod headless_dp_access;
mod headless_ecs_ui_events;
mod headless_ecs_ui_list;
mod headless_ecs_ui_panels;
mod headless_example_scoreboard;
mod headless_items_no_reset;
mod headless_items_source;
mod headless_list_query;
mod headless_list_select;
mod headless_list_teardown;
mod headless_list_two_row_types;
mod headless_observer_click;
mod headless_observer_row;
mod headless_panel_click;
mod headless_panel_deferred_seal;
mod headless_panel_keydown;
mod headless_panel_layout;
mod headless_panel_parse_error;
mod headless_panel_parse_warning;
mod headless_pointer_over_ui_reset;
mod headless_required_bridges;
mod headless_theme_resources;
mod headless_view_model;
mod headless_write_before_scene;
mod plain_vm_derive;
