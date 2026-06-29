//! End-to-end test for the collection-view navigation bridge
//! (`noesis_bevy::items`): drive a bound `ListBox`'s default
//! `ICollectionView` current item and assert the exact `CurrentPosition` /
//! `CurrentItem` the engine reports back.
//!
//! Drives Noesis directly (no GPU), like `headless_items_source.rs`: the
//! bridge's [`ItemsBinding`] owns the `ObservableCollection` + the
//! `CollectionViewSource`/`CollectionView` over it (the same default view the
//! control synchronizes against once `set_items_source` shares the collection).
//! We bind a typed list to a `ListBox`, then walk the cursor with
//! [`ItemsBinding::navigate`] and assert `current_position()` /
//! `current_item_value()` after each move.
//!
//! Bluff-resistance — the *un-driven default* and the off-the-end states are
//! negative controls:
//!
//!   * before any move, the freshly populated view's current item is **not**
//!     `Some(I32(30))` (the value we later land on), so a stuck read-back would
//!     fail the post-move assertion;
//!   * `MoveCurrentToNext` past the end yields `current_position == count` and
//!     `current_item_value() == None` (cursor *after last*), while a valid
//!     position yields `Some(exact typed value)` — proving the read-back tracks
//!     real cursor state rather than always returning a value;
//!   * the items round-trip as boxed `i32`s: a string list would unbox to
//!     `Str`/`None`, never `I32(20)`.
//!
//! Sorting / filtering / grouping are a genuine Noesis SDK limitation (no
//! programmatic `SortDescription`/`Filter` is exposed), so they are not tested
//! here — see `noesis_runtime::collection_view`.
//!
//!   `cargo test -p noesis_bevy --test headless_collection_view -- --nocapture`

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
        // The bridge's per-element binding, exactly as the render systems own it.
        // Typed i32 items so the read-back proves real boxing, not the Rust copy.
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

        // Negative control: the un-driven view must not already sit on the value
        // we later land on, or the post-move assertions would be vacuous.
        assert_ne!(
            binding.current_item_value(),
            Some(ItemValue::I32(30)),
            "fresh view unexpectedly already at the last item",
        );

        // MoveCurrentToFirst -> position 0, value 10.
        assert!(binding.navigate(CollectionViewOp::First));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 0, "First -> position 0");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(10)),
            "First -> item 10",
        );

        // MoveCurrentToNext -> position 1, value 20.
        assert!(binding.navigate(CollectionViewOp::Next));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 1, "Next -> position 1");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(20)),
            "Next -> item 20",
        );

        // MoveCurrentToLast -> position 2, value 30.
        assert!(binding.navigate(CollectionViewOp::Last));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 2, "Last -> position 2");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(30)),
            "Last -> item 30",
        );

        // MoveCurrentToNext past the end -> cursor after last: position == count,
        // no current item. The off-the-end negative control.
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

        // MoveCurrentToPrevious -> back onto the last item.
        assert!(binding.navigate(CollectionViewOp::Previous));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 2, "Previous -> position 2");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(30)),
            "Previous -> item 30",
        );

        // MoveCurrentToPosition(0) -> jump to the first item by ordinal.
        assert!(binding.navigate(CollectionViewOp::To(0)));
        tick(&mut view, &mut t);
        assert_eq!(binding.current_position(), 0, "To(0) -> position 0");
        assert_eq!(
            binding.current_item_value(),
            Some(ItemValue::I32(10)),
            "To(0) -> item 10",
        );

        // MoveCurrentToPosition(-1) -> before first: another off-the-end control.
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

        // Teardown: drop the control's ItemsSource ref before the collection.
        drop(list);
        view.deactivate();
        drop(view);
        drop(binding);
    }

    noesis_runtime::shutdown();
}
