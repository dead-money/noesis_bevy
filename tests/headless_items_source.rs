//! Tests the `ItemsSource` bridge: populate a `ComboBox` from Rust and mutate live.
//!
//! Drives Noesis directly (no GPU). Asserts item counts after `set`, `push`,
//! `remove_at`, and `clear`. These assertions are only meaningful if the collection
//! is observable and actually bound.
//! Main/render queue plumbing is covered by unit tests in `src/items.rs`.
//!
//!   `cargo test -p noesis_bevy --test headless_items_source -- --nocapture`

use std::collections::HashMap;

mod common;

use noesis_bevy::items::ItemsBinding;
use noesis_runtime::view::{FrameworkElement, View};
use noesis_runtime::xaml_provider::XamlProvider;

const COMBO_XAML: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<ComboBox xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
          xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
          x:Name="Combo" Width="200" Height="30"/>"##;

struct InMem(HashMap<String, Vec<u8>>);
impl XamlProvider for InMem {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn load_xaml(&mut self, uri: &str) -> Option<&[u8]> {
        self.0.get(uri).map(Vec::as_slice)
    }
}

#[test]
fn items_source_populates_and_mutates_combobox() {
    if let Some(lic) = common::noesis_license_from_env() {
        noesis_runtime::set_license(&lic.name, &lic.key);
    }
    noesis_runtime::init();

    {
        // Calls the same ItemsBinding methods the render-side apply pass uses.
        let mut binding = ItemsBinding::new();
        binding.set(["Low", "Medium", "High"]);

        let mut bytes = HashMap::new();
        bytes.insert("combo.xaml".to_string(), COMBO_XAML.as_bytes().to_vec());
        let _guard = noesis_runtime::xaml_provider::set_xaml_provider(InMem(bytes));

        let element = FrameworkElement::load("combo.xaml").expect("load_xaml returned None");
        let mut view = View::create(element);
        view.set_size(300, 200);
        view.activate();

        let mut combo = view.content().expect("View::content returned None");

        assert!(
            combo.set_items_source(binding.collection()),
            "set_items_source returned false (root not an ItemsControl?)",
        );

        let mut t = 0.0_f64;
        t += 0.016;
        view.update(t);
        assert_eq!(
            combo.items_count(),
            Some(3),
            "ComboBox did not see the 3 items set from Rust",
        );

        // Observable: incremental edits must propagate without a rebuild.
        binding.push("Ultra");
        t += 0.016;
        view.update(t);
        assert_eq!(
            combo.items_count(),
            Some(4),
            "push did not reach the control"
        );

        binding.remove_at(0);
        t += 0.016;
        view.update(t);
        assert_eq!(
            combo.items_count(),
            Some(3),
            "remove_at did not reach the control",
        );

        binding.clear();
        t += 0.016;
        view.update(t);
        assert_eq!(
            combo.items_count(),
            Some(0),
            "clear did not reach the control"
        );

        // Drop view before binding: releases ItemsSource ref first.
        drop(combo);
        view.deactivate();
        drop(view);
        drop(binding);
    }

    noesis_runtime::shutdown();
}
