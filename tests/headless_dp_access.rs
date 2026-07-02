//! Headless test for the dp bridge: writes and reads back `f32`, `bool`, and
//! `i32` dependency properties by `(x:Name, property)` with no binding or GPU.
//!
//!   `cargo test -p noesis_bevy --test headless_dp_access -- --nocapture`

use std::collections::HashMap;

mod common;

use noesis_bevy::dp::{DpKind, DpValue};
use noesis_runtime::view::{FrameworkElement, View};
use noesis_runtime::xaml_provider::XamlProvider;

const DP_XAML: &str = r##"<?xml version="1.0" encoding="utf-8"?>
<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="300" Height="200">
  <StackPanel>
    <Slider x:Name="Vol" Minimum="0" Maximum="1"/>
    <ComboBox x:Name="Quality">
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

#[test]
fn dp_access_writes_and_reads_by_name_and_type() {
    if let Some(lic) = common::noesis_license_from_env() {
        noesis_runtime::set_license(&lic.name, &lic.key);
    }
    noesis_runtime::init();

    {
        let mut bytes = HashMap::new();
        bytes.insert("dp.xaml".to_string(), DP_XAML.as_bytes().to_vec());
        let _guard = noesis_runtime::xaml_provider::set_xaml_provider(InMem(bytes));

        let element = FrameworkElement::load("dp.xaml").expect("load_xaml returned None");
        let mut view = View::create(element);
        view.set_size(300, 200);
        view.activate();
        let content = view.content().expect("View::content returned None");

        let mut t = 0.0_f64;

        let mut slider = content.find_name("Vol").expect("Vol missing");
        assert!(
            DpValue::F32(0.5).write_to(&mut slider, "Value"),
            "write f32 Slider.Value failed",
        );
        t += 0.016;
        view.update(t);
        assert_eq!(
            DpKind::F32.read_from(&slider, "Value"),
            Some(DpValue::F32(0.5)),
            "read-back f32 Slider.Value mismatch",
        );

        // IsEnabled is a plain bool DP; CheckBox.IsChecked is nullable and needs different handling.
        assert!(
            DpValue::Bool(false).write_to(&mut slider, "IsEnabled"),
            "write bool Slider.IsEnabled failed",
        );
        t += 0.016;
        view.update(t);
        assert_eq!(
            DpKind::Bool.read_from(&slider, "IsEnabled"),
            Some(DpValue::Bool(false)),
            "read-back bool Slider.IsEnabled mismatch",
        );

        let mut combo = content.find_name("Quality").expect("Quality missing");
        assert!(
            DpValue::I32(2).write_to(&mut combo, "SelectedIndex"),
            "write i32 ComboBox.SelectedIndex failed",
        );
        t += 0.016;
        view.update(t);
        assert_eq!(
            DpKind::I32.read_from(&combo, "SelectedIndex"),
            Some(DpValue::I32(2)),
            "read-back i32 ComboBox.SelectedIndex mismatch",
        );

        // A type-mismatched read returns None rather than a garbage value.
        assert_eq!(
            DpKind::Bool.read_from(&slider, "Value"),
            None,
            "reading an f32 property as bool should miss, not coerce",
        );

        drop(slider);
        drop(combo);
        drop(content);
        view.deactivate();
        drop(view);
    }

    noesis_runtime::shutdown();
}
