//! Two-way `ViewModel` / `DataContext` binding test.
//!
//! Verifies Rust→UI (writing a VM property moves the bound control) and
//! UI→Rust (changing the control pushes the value back to the VM, fires
//! [`ViewModelChangeForwarder`], and lands on [`SharedVmChangedQueue`]).
//!
//! Covers `Slider` (`Double`) and `ComboBox` (`Int32`). `CheckBox.IsChecked`
//! is `Nullable<bool>` and cannot be asserted headlessly without a real click.
//!
//! Drives Noesis directly via `View::update`; no render device or GPU needed.
//!
//!   `cargo test -p noesis_bevy --test headless_view_model -- --nocapture`

use std::collections::HashMap;
use std::sync::Arc;

use bevy::prelude::Entity;
use noesis_bevy::viewmodel::{SharedVmChangedQueue, ViewModelChangeForwarder, VmValue};
use noesis_runtime::classes::ClassBuilder;
use noesis_runtime::ffi::{ClassBase, PropType};
use noesis_runtime::view::{FrameworkElement, View};
use noesis_runtime::xaml_provider::XamlProvider;

const SETTINGS_XAML: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="300" Height="200">
  <StackPanel>
    <Slider x:Name="VolumeSlider" Minimum="0" Maximum="1"
            Value="{Binding Foo, Mode=TwoWay}"/>
    <CheckBox x:Name="MuteCheck" IsChecked="{Binding Muted, Mode=TwoWay}"/>
    <ComboBox x:Name="QualityCombo" SelectedIndex="{Binding Quality, Mode=TwoWay}">
      <ComboBoxItem Content="Low"/>
      <ComboBoxItem Content="Medium"/>
      <ComboBoxItem Content="High"/>
    </ComboBox>
  </StackPanel>
</Grid>"##;

struct InMem(HashMap<String, Vec<u8>>);
impl XamlProvider for InMem {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn load_xaml(&mut self, uri: &str) -> Option<&[u8]> {
        self.0.get(uri).map(Vec::as_slice)
    }
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

// Drains the queue and asserts it contained a change for `prop` with `value`.
fn assert_change(queue: &SharedVmChangedQueue, view: Entity, prop: &str, value: &VmValue) {
    let drained = queue.drain();
    assert!(
        drained
            .iter()
            .any(|(e, p, v)| *e == view && p == prop && v == value),
        "expected change ({view:?}, {prop:?}, {value:?}) on the queue, got {drained:?}",
    );
}

#[test]
fn view_model_two_way_binding_round_trips() {
    crate::common::claim_noesis_process();
    if let Some(lic) = crate::common::noesis_license_from_env() {
        noesis_runtime::set_license(&lic.name, &lic.key);
    }
    noesis_runtime::init();

    {
        let queue = SharedVmChangedQueue::default();
        let id = Entity::PLACEHOLDER;
        let prop_names = Arc::new(vec![
            "Foo".to_string(),
            "Muted".to_string(),
            "Quality".to_string(),
        ]);
        let forwarder = ViewModelChangeForwarder::new(id, prop_names, queue.clone());

        let mut builder =
            ClassBuilder::new("Settings.E2E.VM", ClassBase::ContentControl, forwarder);
        let foo = builder.add_property("Foo", PropType::Double);
        let muted = builder.add_property("Muted", PropType::Bool);
        let quality = builder.add_property("Quality", PropType::Int32);
        assert_eq!((foo, muted, quality), (0, 1, 2));
        let registration = builder.register().expect("VM class registration failed");
        let vm = registration
            .create_instance()
            .expect("create_instance returned None");

        let mut bytes = HashMap::new();
        bytes.insert(
            "settings.xaml".to_string(),
            SETTINGS_XAML.as_bytes().to_vec(),
        );
        let _guard = noesis_runtime::xaml_provider::set_xaml_provider(InMem(bytes));

        let element = FrameworkElement::load("settings.xaml").expect("load_xaml returned None");
        let mut view = View::create(element);
        view.set_size(300, 200);
        view.activate();

        let mut content = view.content().expect("View::content returned None");
        assert!(
            content.set_data_context(&vm),
            "set_data_context returned false (root not a FrameworkElement?)",
        );

        // First update settles bindings; drain startup churn before asserting.
        // update() returns "needs redraw", not a binding-success signal.
        let mut t = 0.0_f64;
        t += 0.016;
        view.update(t);
        let _ = queue.drain();

        // ── Slider ──────────────────────────────────────────────────────────
        // RangeBase.Value is f32; 0.75 and 0.25 are exact in both f32 and f64
        // so the round-trip comparison is exact.
        //
        // (a) Rust -> UI
        vm.handle().set_double(foo, 0.75);
        t += 0.016;
        view.update(t);
        let mut slider = content
            .find_name("VolumeSlider")
            .expect("VolumeSlider missing");
        assert!(
            approx(
                f64::from(slider.get_f32("Value").expect("Slider.Value unreadable")),
                0.75,
            ),
            "Rust write did not reach Slider.Value",
        );
        assert_change(&queue, id, "Foo", &VmValue::Double(0.75));

        // (b) UI -> Rust
        assert!(slider.set_f32("Value", 0.25), "Slider.Value not writable");
        t += 0.016;
        view.update(t);
        assert_change(&queue, id, "Foo", &VmValue::Double(0.25));

        // CheckBox.IsChecked is Nullable<bool>; get_bool/set_bool address plain
        // Bool, so the round-trip can't be asserted headlessly without a click.

        // ── ComboBox ────────────────────────────────────────────────────────
        // (a) Rust → UI.
        vm.handle().set_int32(quality, 2);
        t += 0.016;
        view.update(t);
        let mut combo = content
            .find_name("QualityCombo")
            .expect("QualityCombo missing");
        assert_eq!(
            combo.get_i32("SelectedIndex"),
            Some(2),
            "Rust write did not reach ComboBox.SelectedIndex",
        );
        let _ = queue.drain();
        // (b) UI → Rust.
        assert!(
            combo.set_i32("SelectedIndex", 1),
            "SelectedIndex not writable",
        );
        t += 0.016;
        view.update(t);
        assert_change(&queue, id, "Quality", &VmValue::Int32(1));

        // Release element handles before the VM; content holds the DataContext ref.
        drop(slider);
        drop(combo);
        drop(content);
        view.deactivate();
        drop(view);
        drop(vm);
        drop(registration);
    }

    noesis_runtime::shutdown();
}
