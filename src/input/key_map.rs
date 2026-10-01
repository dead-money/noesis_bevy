//! Bevy [`KeyCode`] to Noesis [`Key`] mapping.
//!
//! [`KeyCode`] is a physical key position, so punctuation maps to the Noesis
//! `Oem*` key at the same position on a US layout. Keys with no Noesis
//! equivalent (`Fn`, `Lang1`, media keys) map to [`Key::None`]; the input
//! bridge drops those, and any text they produce still arrives as a `Char`
//! event.

use bevy::input::keyboard::KeyCode;
use noesis_runtime::view::Key;

// A new key also needs a variant in `noesis_runtime::view::Key` and its
// `static_assert` in the runtime's `cpp/noesis_view.cpp`.

/// Translates a Bevy [`KeyCode`] into the Noesis [`Key`] the view expects.
///
/// Returns [`Key::None`] for keys with no Noesis equivalent. Both `Enter` and
/// `NumpadEnter` map to [`Key::Return`].
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn from_bevy(code: KeyCode) -> Key {
    match code {
        KeyCode::KeyA => Key::A,
        KeyCode::KeyB => Key::B,
        KeyCode::KeyC => Key::C,
        KeyCode::KeyD => Key::D,
        KeyCode::KeyE => Key::E,
        KeyCode::KeyF => Key::F,
        KeyCode::KeyG => Key::G,
        KeyCode::KeyH => Key::H,
        KeyCode::KeyI => Key::I,
        KeyCode::KeyJ => Key::J,
        KeyCode::KeyK => Key::K,
        KeyCode::KeyL => Key::L,
        KeyCode::KeyM => Key::M,
        KeyCode::KeyN => Key::N,
        KeyCode::KeyO => Key::O,
        KeyCode::KeyP => Key::P,
        KeyCode::KeyQ => Key::Q,
        KeyCode::KeyR => Key::R,
        KeyCode::KeyS => Key::S,
        KeyCode::KeyT => Key::T,
        KeyCode::KeyU => Key::U,
        KeyCode::KeyV => Key::V,
        KeyCode::KeyW => Key::W,
        KeyCode::KeyX => Key::X,
        KeyCode::KeyY => Key::Y,
        KeyCode::KeyZ => Key::Z,

        KeyCode::Digit0 => Key::D0,
        KeyCode::Digit1 => Key::D1,
        KeyCode::Digit2 => Key::D2,
        KeyCode::Digit3 => Key::D3,
        KeyCode::Digit4 => Key::D4,
        KeyCode::Digit5 => Key::D5,
        KeyCode::Digit6 => Key::D6,
        KeyCode::Digit7 => Key::D7,
        KeyCode::Digit8 => Key::D8,
        KeyCode::Digit9 => Key::D9,

        KeyCode::F1 => Key::F1,
        KeyCode::F2 => Key::F2,
        KeyCode::F3 => Key::F3,
        KeyCode::F4 => Key::F4,
        KeyCode::F5 => Key::F5,
        KeyCode::F6 => Key::F6,
        KeyCode::F7 => Key::F7,
        KeyCode::F8 => Key::F8,
        KeyCode::F9 => Key::F9,
        KeyCode::F10 => Key::F10,
        KeyCode::F11 => Key::F11,
        KeyCode::F12 => Key::F12,
        KeyCode::F13 => Key::F13,
        KeyCode::F14 => Key::F14,
        KeyCode::F15 => Key::F15,
        KeyCode::F16 => Key::F16,
        KeyCode::F17 => Key::F17,
        KeyCode::F18 => Key::F18,
        KeyCode::F19 => Key::F19,
        KeyCode::F20 => Key::F20,
        KeyCode::F21 => Key::F21,
        KeyCode::F22 => Key::F22,
        KeyCode::F23 => Key::F23,
        KeyCode::F24 => Key::F24,

        KeyCode::Escape => Key::Escape,
        KeyCode::Enter => Key::Return,
        KeyCode::Tab => Key::Tab,
        KeyCode::Space => Key::Space,
        KeyCode::Backspace => Key::Back,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,

        KeyCode::ArrowUp => Key::Up,
        KeyCode::ArrowDown => Key::Down,
        KeyCode::ArrowLeft => Key::Left,
        KeyCode::ArrowRight => Key::Right,

        KeyCode::ShiftLeft => Key::LeftShift,
        KeyCode::ShiftRight => Key::RightShift,
        KeyCode::ControlLeft => Key::LeftCtrl,
        KeyCode::ControlRight => Key::RightCtrl,
        KeyCode::AltLeft => Key::LeftAlt,
        KeyCode::AltRight => Key::RightAlt,
        KeyCode::SuperLeft => Key::LWin,
        KeyCode::SuperRight => Key::RWin,
        KeyCode::CapsLock => Key::CapsLock,
        KeyCode::NumLock => Key::NumLock,
        KeyCode::ScrollLock => Key::ScrollLock,
        KeyCode::ContextMenu => Key::Apps,
        KeyCode::Pause => Key::Pause,
        KeyCode::PrintScreen => Key::PrintScreen,
        KeyCode::Help => Key::Help,

        KeyCode::Numpad0 => Key::NumPad0,
        KeyCode::Numpad1 => Key::NumPad1,
        KeyCode::Numpad2 => Key::NumPad2,
        KeyCode::Numpad3 => Key::NumPad3,
        KeyCode::Numpad4 => Key::NumPad4,
        KeyCode::Numpad5 => Key::NumPad5,
        KeyCode::Numpad6 => Key::NumPad6,
        KeyCode::Numpad7 => Key::NumPad7,
        KeyCode::Numpad8 => Key::NumPad8,
        KeyCode::Numpad9 => Key::NumPad9,
        KeyCode::NumpadMultiply => Key::Multiply,
        KeyCode::NumpadAdd => Key::Add,
        KeyCode::NumpadSubtract => Key::Subtract,
        KeyCode::NumpadDecimal => Key::Decimal,
        KeyCode::NumpadDivide => Key::Divide,
        KeyCode::NumpadEnter => Key::Return,

        KeyCode::Semicolon => Key::OemSemicolon,
        KeyCode::Equal => Key::OemPlus,
        KeyCode::Comma => Key::OemComma,
        KeyCode::Minus => Key::OemMinus,
        KeyCode::Period => Key::OemPeriod,
        KeyCode::Slash => Key::OemSlash,
        KeyCode::Backquote => Key::OemTilde,
        KeyCode::BracketLeft => Key::OemOpenBrackets,
        KeyCode::Backslash => Key::OemPipe,
        KeyCode::BracketRight => Key::OemCloseBrackets,
        KeyCode::Quote => Key::OemQuotes,

        _ => Key::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_map() {
        assert_eq!(from_bevy(KeyCode::KeyA), Key::A);
        assert_eq!(from_bevy(KeyCode::KeyZ), Key::Z);
        assert_eq!(from_bevy(KeyCode::Digit0), Key::D0);
        assert_eq!(from_bevy(KeyCode::Digit9), Key::D9);
        assert_eq!(from_bevy(KeyCode::F1), Key::F1);
        assert_eq!(from_bevy(KeyCode::F24), Key::F24);
        assert_eq!(from_bevy(KeyCode::Escape), Key::Escape);
        assert_eq!(from_bevy(KeyCode::Enter), Key::Return);
        assert_eq!(from_bevy(KeyCode::NumpadEnter), Key::Return);
        assert_eq!(from_bevy(KeyCode::Backspace), Key::Back);
        assert_eq!(from_bevy(KeyCode::ArrowLeft), Key::Left);
        assert_eq!(from_bevy(KeyCode::ArrowUp), Key::Up);
        assert_eq!(from_bevy(KeyCode::ShiftLeft), Key::LeftShift);
        assert_eq!(from_bevy(KeyCode::ControlRight), Key::RightCtrl);
        assert_eq!(from_bevy(KeyCode::SuperLeft), Key::LWin);
        assert_eq!(from_bevy(KeyCode::Comma), Key::OemComma);
        assert_eq!(from_bevy(KeyCode::Semicolon), Key::OemSemicolon);
        assert_eq!(from_bevy(KeyCode::Quote), Key::OemQuotes);
    }

    #[test]
    fn unmapped_falls_back_to_none() {
        assert_eq!(from_bevy(KeyCode::Fn), Key::None);
        assert_eq!(from_bevy(KeyCode::Lang1), Key::None);
    }
}
