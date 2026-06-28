//! End-to-end test for the `ItemsSource` bridge (`dm_noesis_bevy::items`,
//! TODO §3): populate a `ComboBox`'s items from Rust and mutate them live.
//!
//! Drives Noesis directly (no GPU), like the runtime's `binding.rs` /
//! `observable_collection.rs`: the bridge's [`ItemsBinding`] wraps an
//! `ObservableCollection`, and the safe
//! `FrameworkElement::set_items_source(&ObservableCollection)` accessor binds it
//! to a named `ItemsControl`. We assert the control sees the right item count
//! after the initial `set` and after each incremental `push` / `remove` /
//! `clear` — which only tracks if the collection is observable and actually
//! bound. The plugin's main↔render queue plumbing is covered by the unit tests
//! in `src/items.rs`.
//!
//!   `cargo test -p dm_noesis_bevy --test headless_items_source -- --nocapture`

use std::collections::HashMap;

use dm_noesis_bevy::items::ItemsBinding;
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
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();

    {
        // The bridge's per-element items list, exactly as the render systems own
        // it. The app would drive this through `NoesisItemsSources`; here we call
        // the same `ItemsBinding` methods the render-side apply pass calls.
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

        // Bind the Rust-owned collection as the ComboBox's ItemsSource — the
        // safe accessor added for unsafe-free consumers.
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

        // Incremental edits after binding must track live (the collection is
        // observable, so the control's view of it updates without a rebuild).
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

        // Teardown: drop the view (releases the control's ItemsSource ref) before
        // the binding's collection.
        drop(combo);
        view.deactivate();
        drop(view);
        drop(binding);
    }

    noesis_runtime::shutdown();
}
