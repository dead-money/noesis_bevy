//! Tests `ItemsBinding` collection-view navigation: bind a typed i32 list to a
//! `ListBox`, walk the cursor via [`ItemsBinding::navigate`], and assert
//! `current_position()` / `current_item_value()` after each move.
//!
//! Bluff-resistance controls: the un-driven fresh view, the after-last cursor
//! (position == count, value == None), and the before-first cursor (position == -1,
//! value == None) each prove the read-back tracks real state. Typed i32 items ensure
//! values round-trip as `I32(n)`, not `Str`/`None`.
//!
//! Sorting, filtering, and grouping are not tested: the Noesis SDK exposes no
//! programmatic `SortDescription` or `Filter`.

use std::collections::HashMap;

use noesis_bevy::ItemValue;
use noesis_bevy::items::{CollectionViewOp, ItemsBinding};
use noesis_runtime::view::{FrameworkElement, View};
use noesis_runtime::xaml_provider::XamlProvider;

const LIST_XAML: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<ListBox xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
         xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
         x:Name="List" Width="200" Height="120"/>"##;

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
fn collection_view_navigates_current_item() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();

    {
        // i32 items: read-back must return I32(n), not Str/None.
        let mut binding = ItemsBinding::new();
        binding.set_typed(&[ItemValue::I32(10), ItemValue::I32(20), ItemValue::I32(30)]);

        let mut bytes = HashMap::new();
        bytes.insert("list.xaml".to_string(), LIST_XAML.as_bytes().to_vec());
        let _guard = noesis_runtime::xaml_provider::set_xaml_provider(InMem(bytes));

        let element = FrameworkElement::load("list.xaml").expect("load_xaml returned None");
        let mut view = View::create(element);
        view.set_size(300, 200);
        view.activate();

        let mut list = view.content().expect("View::content returned None");
        assert!(
            list.set_items_source(binding.collection()),
            "set_items_source returned false (root not an ItemsControl?)",
        );

        let mut t = 0.0_f64;
        let tick = |view: &mut View, t: &mut f64| {
            *t += 0.016;
            view.update(*t);
        };
        tick(&mut view, &mut t);
        assert_eq!(
            list.items_count(),
            Some(3),
            "ListBox did not see the 3 typed items",
        );

        // Negative control: if already at last item, post-move assertions are vacuous.
        assert_ne!(
            binding.current_item_value(),
            Some(ItemValue::I32(30)),
            "fresh view unexpectedly already at the last item",
        );

        assert!(binding.navigate(CollectionViewOp::First));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 0, "First -> position 0");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(10)),
            "First -> item 10",
        );

        assert!(binding.navigate(CollectionViewOp::Next));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 1, "Next -> position 1");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(20)),
            "Next -> item 20",
        );

        assert!(binding.navigate(CollectionViewOp::Last));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 2, "Last -> position 2");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(30)),
            "Last -> item 30",
        );

        binding.navigate(CollectionViewOp::Next);
        tick(&mut view, &mut t);
        assert_eq!(
            binding.current_position(),
            3,
            "Next past end -> position == count (after last)",
        );
        assert_eq!(
            binding.current_item_value(),
            None,
            "after-last cursor has no current item",
        );

        assert!(binding.navigate(CollectionViewOp::Previous));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 2, "Previous -> position 2");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(30)),
            "Previous -> item 30",
        );

        assert!(binding.navigate(CollectionViewOp::To(0)));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 0, "To(0) -> position 0");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(10)),
            "To(0) -> item 10",
        );

        binding.navigate(CollectionViewOp::To(-1));
        tick(&mut view, &mut t);
        assert_eq!(
            binding.current_position(),
            -1,
            "To(-1) -> position -1 (before first)",
        );
        assert_eq!(
            binding.current_item_value(),
            None,
            "before-first cursor has no current item",
        );

        // Drop the control's ItemsSource ref before the collection.
        drop(list);
        view.deactivate();
        drop(view);
        drop(binding);
    }

    noesis_runtime::shutdown();
}
