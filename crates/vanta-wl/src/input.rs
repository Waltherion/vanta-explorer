//! sctk keyboard/pointer events -> egui events.

use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym, Modifiers};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind};

pub fn modifiers(m: Modifiers) -> egui::Modifiers {
    egui::Modifiers { alt: m.alt, ctrl: m.ctrl, shift: m.shift, mac_cmd: false, command: m.ctrl }
}

pub fn keysym_to_key(k: Keysym) -> Option<egui::Key> {
    use egui::Key as K;
    Some(match k {
        Keysym::Escape => K::Escape,
        Keysym::Return | Keysym::KP_Enter => K::Enter,
        Keysym::BackSpace => K::Backspace,
        Keysym::Delete | Keysym::KP_Delete => K::Delete,
        Keysym::Tab | Keysym::ISO_Left_Tab => K::Tab,
        Keysym::space => K::Space,
        Keysym::Left | Keysym::KP_Left => K::ArrowLeft,
        Keysym::Right | Keysym::KP_Right => K::ArrowRight,
        Keysym::Up | Keysym::KP_Up => K::ArrowUp,
        Keysym::Down | Keysym::KP_Down => K::ArrowDown,
        Keysym::Home | Keysym::KP_Home => K::Home,
        Keysym::End | Keysym::KP_End => K::End,
        Keysym::Page_Up | Keysym::KP_Page_Up => K::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => K::PageDown,
        Keysym::Insert => K::Insert,
        Keysym::F1 => K::F1,
        Keysym::F2 => K::F2,
        Keysym::F3 => K::F3,
        Keysym::F4 => K::F4,
        Keysym::F5 => K::F5,
        Keysym::F6 => K::F6,
        Keysym::F7 => K::F7,
        Keysym::F8 => K::F8,
        Keysym::F9 => K::F9,
        Keysym::F10 => K::F10,
        Keysym::F11 => K::F11,
        Keysym::F12 => K::F12,
        _ => return None,
    })
}

/// Translates one key press/release/repeat into egui events: a `Key` event when egui has a
/// name for the key (letters and digits via their uppercase name), plus a `Text` event for
/// printable input on press when no Ctrl/Alt is held.
pub fn key_event(event: &KeyEvent, pressed: bool, repeat: bool, mods: egui::Modifiers) -> Vec<egui::Event> {
    let mut out = Vec::with_capacity(2);
    // Letters and digits by their keysym, which Ctrl leaves alone; the text for Ctrl+Z is a
    // control character (U+001A), so it cannot name the key.
    let letter = |c: char| (c.is_ascii_alphanumeric()).then(|| egui::Key::from_name(&c.to_ascii_uppercase().to_string())).flatten();
    let key = keysym_to_key(event.keysym).or_else(|| event.keysym.key_char().and_then(letter)).or_else(|| {
        event.utf8.as_deref().and_then(|s| {
            let mut it = s.chars();
            match (it.next(), it.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => {
                    egui::Key::from_name(&c.to_ascii_uppercase().to_string())
                }
                _ => None,
            }
        })
    });
    if let Some(key) = key {
        out.push(egui::Event::Key { key, physical_key: None, pressed, repeat, modifiers: mods });
    }
    if pressed && !mods.ctrl && !mods.alt {
        if let Some(text) = event.utf8.as_deref() {
            if !text.is_empty() && !text.chars().any(char::is_control) {
                out.push(egui::Event::Text(text.to_string()));
            }
        }
    }
    out
}

/// Translates one pointer event (surface-local logical coordinates == egui points when
/// `pixels_per_point` is the fractional scale).
pub fn pointer_event(ev: &PointerEvent, mods: egui::Modifiers) -> Option<egui::Event> {
    let pos = egui::pos2(ev.position.0 as f32, ev.position.1 as f32);
    Some(match &ev.kind {
        PointerEventKind::Enter { .. } | PointerEventKind::Motion { .. } => egui::Event::PointerMoved(pos),
        PointerEventKind::Leave { .. } => egui::Event::PointerGone,
        PointerEventKind::Press { button, .. } | PointerEventKind::Release { button, .. } => {
            let pressed = matches!(ev.kind, PointerEventKind::Press { .. });
            let button = match button {
                0x110 => egui::PointerButton::Primary,
                0x111 => egui::PointerButton::Secondary,
                0x112 => egui::PointerButton::Middle,
                0x113 => egui::PointerButton::Extra1,
                0x114 => egui::PointerButton::Extra2,
                _ => return None,
            };
            egui::Event::PointerButton { pos, button, pressed, modifiers: mods }
        }
        PointerEventKind::Axis { horizontal, vertical, .. } => egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(-horizontal.absolute as f32, -vertical.absolute as f32),
            phase: egui::TouchPhase::Move,
            modifiers: mods,
        },
    })
}
