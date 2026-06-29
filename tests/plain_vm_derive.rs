//! End-to-end test for the plain-struct `ViewModel` bridge + `#[derive(NoesisViewModel)]`
//! (`noesis_bevy::plain_vm`, TODO §3/§9 Phase C).
//!
//! Two layers:
//!
//!   1. **Derive correctness (pure).** The macro maps fields to reflected
//!      properties (honoring `#[noesis(skip)]`), and the generated
//!      `noesis_snapshot` / `noesis_apply` round-trip owned values.
//!   2. **Two-way through Noesis (headless).** Using only the derive's public
//!      trait output, register the reflected plain VM, bind a `TextBox.Text` to
//!      `{Binding title, Mode=TwoWay}`, seed it from the struct (Rust→UI), then
//!      edit the control and assert the edit flows back into the struct
//!      (UI→Rust) via `noesis_apply` — exactly what the render-side bridge wires.
//!
//! Drives Noesis directly (no GPU), like the runtime's `plain_vm_twoway.rs`.
//!
//!   `cargo test -p noesis_bevy --test plain_vm_derive -- --nocapture`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use noesis_bevy::plain_vm::PlainVmBuilder;
use noesis_bevy::{NoesisViewModel, PlainType, PlainValue, PlainValueRef};
use noesis_runtime::binding::{Binding, BindingMode, UpdateSourceTrigger, set_binding};
use noesis_runtime::view::{FrameworkElement, View};
use noesis_runtime::xaml_provider::XamlProvider;

#[derive(NoesisViewModel)]
struct DemoVm {
    title: String,
    volume: f32,
    muted: bool,
    quality: i32,
    #[noesis(skip)]
    #[allow(dead_code)]
    internal: u32,
}

const XAML: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="80">
  <TextBox x:Name="Box"/>
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

/// Decode a `TwoWay` writeback value as its declared kind (mirrors the bridge's
/// private `unbox`).
fn decode(kind: PlainType, value: &PlainValueRef) -> PlainValue {
    match kind {
        PlainType::String => value
            .as_str()
            .map(|s| PlainValue::String(s.to_owned()))
            .unwrap_or(PlainValue::Null),
        PlainType::Int32 => value
            .as_i32()
            .map(PlainValue::Int32)
            .unwrap_or(PlainValue::Null),
        PlainType::Double => value
            .as_f64()
            .map(PlainValue::Double)
            .unwrap_or(PlainValue::Null),
        PlainType::Bool => value
            .as_bool()
            .map(PlainValue::Bool)
            .unwrap_or(PlainValue::Null),
        PlainType::BaseComponent => PlainValue::Null,
    }
}

#[test]
fn derive_metadata_and_value_round_trip() {
    // `#[noesis(skip)]` excludes `internal`; the rest map by field order.
    assert_eq!(DemoVm::noesis_type_name(), "DemoVm");
    assert_eq!(
        DemoVm::noesis_properties(),
        &[
            ("title", PlainType::String),
            ("volume", PlainType::Double),
            ("muted", PlainType::Bool),
            ("quality", PlainType::Int32),
        ],
    );

    let mut vm = DemoVm {
        title: "Hi".into(),
        volume: 0.5,
        muted: false,
        quality: 2,
        internal: 99,
    };
    let snap = vm.noesis_snapshot();
    assert!(matches!(snap[0], PlainValue::String(ref s) if s == "Hi"));
    assert!(matches!(snap[1], PlainValue::Double(d) if (d - 0.5).abs() < 1e-9));
    assert!(matches!(snap[2], PlainValue::Bool(false)));
    assert!(matches!(snap[3], PlainValue::Int32(2)));

    vm.noesis_apply(0, &PlainValue::String("Bye".into()));
    vm.noesis_apply(3, &PlainValue::Int32(7));
    // A mismatched variant is ignored.
    vm.noesis_apply(2, &PlainValue::Int32(123));
    assert_eq!(vm.title, "Bye");
    assert_eq!(vm.quality, 7);
    assert!(!vm.muted);
}

#[test]
fn derived_plain_vm_binds_two_way() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
    noesis_runtime::init();

    {
        let vm = Arc::new(Mutex::new(DemoVm {
            title: "Hello".into(),
            volume: 0.5,
            muted: false,
            quality: 2,
            internal: 0,
        }));
        let props = DemoVm::noesis_properties();

        // Register the reflected plain VM from the derive's metadata, wiring the
        // on_set writeback into `noesis_apply` (what the bridge does render-side).
        let vm_for_handler = Arc::clone(&vm);
        let mut builder = PlainVmBuilder::new(DemoVm::noesis_type_name());
        for (name, kind) in props {
            builder.add_property(name, *kind);
        }
        let class = builder
            .on_set(move |idx: u32, value: &PlainValueRef| {
                let kind = props[idx as usize].1;
                let decoded = decode(kind, value);
                vm_for_handler.lock().unwrap().noesis_apply(idx, &decoded);
            })
            .register()
            .expect("plain VM registration failed");
        let instance = class
            .create_instance()
            .expect("create_instance returned None");

        // Seed the controls from the struct (Rust→UI), as `apply_snapshot` does.
        let snapshot = vm.lock().unwrap().noesis_snapshot();
        for (idx, (name, _)) in props.iter().enumerate() {
            let _ = instance.set_and_notify(idx as u32, name, snapshot[idx].clone());
        }

        let mut bytes = HashMap::new();
        bytes.insert("scene.xaml".to_string(), XAML.as_bytes().to_vec());
        let _guard = noesis_runtime::xaml_provider::set_xaml_provider(InMem(bytes));

        let element = FrameworkElement::load("scene.xaml").expect("load_xaml returned None");
        let mut view = View::create(element);
        view.set_size(200, 80);
        view.activate();

        let mut content = view.content().expect("View::content returned None");
        assert!(
            instance.set_data_context(&mut content),
            "set_data_context failed"
        );

        let mut textbox = content.find_name("Box").expect("find_name(Box) failed");
        let binding = Binding::new("title")
            .mode(BindingMode::TwoWay)
            .update_source_trigger(UpdateSourceTrigger::PropertyChanged);
        assert!(
            set_binding(&textbox, "Text", &binding),
            "set_binding failed"
        );

        // Rust → UI: the seeded title reached the TextBox.
        view.update(0.0);
        assert_eq!(textbox.text().as_deref(), Some("Hello"));

        // UI → Rust: editing the control writes back into the struct.
        assert!(textbox.set_text("World"));
        view.update(0.0);
        assert_eq!(
            vm.lock().unwrap().title,
            "World",
            "TwoWay edit did not reach the Rust struct via noesis_apply",
        );

        drop(textbox);
        drop(content);
        view.deactivate();
        drop(view);
        drop(instance);
        drop(class);
    }

    noesis_runtime::shutdown();
}
