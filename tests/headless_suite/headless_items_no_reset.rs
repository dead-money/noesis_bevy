//! Regression for P1.11: re-applying an unchanged source must not reset selection.
//!
//! A `NoesisItems` mutation re-applies every name in the component. Before the
//! fix, `set_typed` always cleared and repushed the collection, which dropped the
//! control's `SelectedIndex` back to `-1` — so touching one list wiped every
//! other list's selection ("Reset is the enemy"). Here we select an item, feed
//! the *same* items again, and assert the selection survives.
//!
//! Drives Noesis directly (no GPU), same harness as `headless_items_source`.
//!
//!   `cargo test -p noesis_bevy --test headless_items_no_reset -- --nocapture`

use std::collections::HashMap;

use noesis_bevy::items::{ItemValue, ItemsBinding};
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
fn unchanged_source_preserves_selection() {
    crate::common::claim_noesis_process();
    if let Some(lic) = crate::common::noesis_license_from_env() {
        noesis_runtime::set_license(&lic.name, &lic.key);
    }
    noesis_runtime::init();

    {
        let items = [
            ItemValue::Str("Low".into()),
            ItemValue::Str("Medium".into()),
            ItemValue::Str("High".into()),
        ];
        let mut binding = ItemsBinding::new();
        binding.set_typed(&items);

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

        // Select an item, then confirm the control reports it.
        assert!(combo.set_selected_index(1), "set_selected_index failed");
        t += 0.016;
        view.update(t);
        assert_eq!(
            combo.selected_index(),
            Some(1),
            "ComboBox did not take the selection",
        );

        // Re-apply the identical source (what a sibling-list mutation triggers):
        // must be a no-op, leaving the selection intact rather than clearing it.
        binding.set_typed(&items);
        t += 0.016;
        view.update(t);
        assert_eq!(
            combo.selected_index(),
            Some(1),
            "re-applying an unchanged source cleared the selection (Reset is the enemy)",
        );

        drop(combo);
        view.deactivate();
        drop(view);
        drop(binding);
    }

    noesis_runtime::shutdown();
}
