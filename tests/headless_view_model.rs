//! End-to-end test for the `ViewModel` / `DataContext` binding bridge
//! (`dm_noesis_bevy::viewmodel`, TODO §3).
//!
//! Proves the two-way data flow a Rust-owned settings menu needs, against real
//! `Slider` (`Double`) and `ComboBox` (`Int32`) controls bound
//! `{Binding …, Mode=TwoWay}` to a Rust view model:
//!
//!   (a) **Rust → UI** — writing a VM dependency property updates the bound
//!       control (`Slider.Value`, `ComboBox.SelectedIndex`).
//!   (b) **UI → Rust** — changing the control pushes the new value back through
//!       the binding onto the VM, fires the bridge's [`ViewModelChangeForwarder`],
//!       and lands on the shared change queue the plugin drains into a
//!       `NoesisViewModelChanged` message.
//!
//! (A `CheckBox.IsChecked` two-way bind to a bool VM property works identically;
//! it's left out of the assertions because `IsChecked` is `Nullable<bool>` and
//! the generic test accessors can't read/write it without a real click — see
//! the in-body note.)
//!
//! Like the runtime's `tests/binding.rs`, this drives Noesis directly: data
//! binding and layout settle inside `View::update`, with no render device or
//! GPU needed. That keeps the test hermetic while still exercising the genuine,
//! risky code paths — the safe `FrameworkElement::set_data_context(&ClassInstance)`
//! accessor and the real `ViewModelChangeForwarder` + `SharedVmChangedQueue` +
//! `VmValue` decoding the plugin installs render-side. The plugin's main↔render
//! queue plumbing is covered by the unit tests in `src/viewmodel.rs`.
//!
//!   `cargo test -p dm_noesis_bevy --test headless_view_model -- --nocapture`

use std::collections::HashMap;
use std::sync::Arc;

use dm_noesis_bevy::viewmodel::{
    SharedVmChangedQueue, ViewModelChangeForwarder, ViewModelId, VmValue,
};
use dm_noesis_runtime::classes::ClassBuilder;
use dm_noesis_runtime::ffi::{ClassBase, PropType};
use dm_noesis_runtime::view::{FrameworkElement, View};
use dm_noesis_runtime::xaml_provider::XamlProvider;

/// `Slider.Value` (`Double`), `CheckBox.IsChecked` (`Bool`), and
/// `ComboBox.SelectedIndex` (`Int32`) each two-way bound to a VM property.
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

/// Assert the queue captured a change for `prop` with `value` (draining it).
fn assert_change(queue: &SharedVmChangedQueue, id: ViewModelId, prop: &str, value: &VmValue) {
    let drained = queue.drain();
    assert!(
        drained
            .iter()
            .any(|(i, p, v)| *i == id && p == prop && v == value),
        "expected change ({id:?}, {prop:?}, {value:?}) on the queue, got {drained:?}",
    );
}

#[test]
fn view_model_two_way_binding_round_trips() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        dm_noesis_runtime::set_license(&name, &key);
    }
    dm_noesis_runtime::init();

    {
        // The bridge's change sink + forwarder, wired exactly as the render-side
        // plugin wires them. `id` is the stable handle the plugin would hand back
        // from `NoesisViewModels::register`.
        let queue = SharedVmChangedQueue::default();
        let id = ViewModelId(0);
        let prop_names = Arc::new(vec![
            "Foo".to_string(),
            "Muted".to_string(),
            "Quality".to_string(),
        ]);
        let forwarder = ViewModelChangeForwarder::new(id, prop_names, queue.clone());

        // A Rust-backed view model: Foo(Double), Muted(Bool), Quality(Int32) —
        // indices 0/1/2, matching `prop_names` above.
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
        let _guard = dm_noesis_runtime::xaml_provider::set_xaml_provider(InMem(bytes));

        let element = FrameworkElement::load("settings.xaml").expect("load_xaml returned None");
        let mut view = View::create(element);
        view.set_size(300, 200);
        view.activate();

        let mut content = view.content().expect("View::content returned None");
        // The safe accessor added for unsafe-free consumers — the heart of the
        // bridge's attach step.
        assert!(
            content.set_data_context(&vm),
            "set_data_context returned false (root not a FrameworkElement?)",
        );

        // First pass settles the bindings; clear any startup churn. `update`
        // returns "needs redraw", which goes false once settled — not a
        // binding-success signal — so we advance time and ignore the result.
        let mut t = 0.0_f64;
        t += 0.016;
        view.update(t);
        let _ = queue.drain();

        // ── Slider ──────────────────────────────────────────────────────────
        // Noesis is a float engine: `RangeBase.Value` is an `f32` DP, so we read
        // it with `get_f32`. The VM's `Foo` is `Double`, and the binding converts
        // across — we use values exact in both `f32` and `f64` (0.75, 0.25) so the
        // round-trip compares exactly.
        //
        // (a) Rust → UI: writing the VM property moves the bound Slider, and the
        //     forwarder reports the VM-side change.
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

        // (b) UI → Rust: moving the Slider pushes back through the TwoWay binding
        //     onto the VM, raising a change for the main world to observe.
        assert!(slider.set_f32("Value", 0.25), "Slider.Value not writable");
        t += 0.016;
        view.update(t);
        assert_change(&queue, id, "Foo", &VmValue::Double(0.25));

        // (Note: a `CheckBox.IsChecked` two-way binding to the VM's `Muted`
        // bool works the same way, but `IsChecked` is `Nullable<bool>` in
        // Noesis and the generic `get_bool`/`set_bool` accessors are typed for
        // plain `Bool`, so a headless test can't read/write it to assert the
        // round-trip without driving a real pointer click. The bridge itself is
        // value-type agnostic — see the `VmValue::Bool` unit coverage.)

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

        // Teardown: release element handles + view (drops the DataContext ref)
        // before the VM and registration.
        drop(slider);
        drop(combo);
        drop(content);
        view.deactivate();
        drop(view);
        drop(vm);
        drop(registration);
    }

    dm_noesis_runtime::shutdown();
}
