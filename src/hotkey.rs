//! Global recording hotkeys: a canonical text form ("Ctrl+Alt+R"), parsing to a
//! Windows virtual-key code, and a capture helper for the settings window.

use serde::{Deserialize, Serialize};

const MOD_ALT: u32 = 0x0001;
const MOD_CONTROL: u32 = 0x0002;
const MOD_SHIFT: u32 = 0x0004;
const MOD_WIN: u32 = 0x0008;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Windows virtual-key code.
    pub vk: u32,
}

impl Hotkey {
    pub fn modifiers(&self) -> u32 {
        let mut value = 0;
        if self.alt {
            value |= MOD_ALT;
        }
        if self.ctrl {
            value |= MOD_CONTROL;
        }
        if self.shift {
            value |= MOD_SHIFT;
        }
        if self.win {
            value |= MOD_WIN;
        }
        value
    }

    /// Canonical text form, modifiers in a stable order.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push("Win".into());
        }
        parts.push(key_name(self.vk));
        parts.join("+")
    }
}

/// Parse a stored shortcut. Returns `None` for empty or malformed values, which
/// disables the hotkey instead of failing the app.
pub fn parse(text: &str) -> Option<Hotkey> {
    let mut hotkey = Hotkey {
        ctrl: false,
        alt: false,
        shift: false,
        win: false,
        vk: 0,
    };
    for raw in text.split('+') {
        let token = raw.trim();
        if token.is_empty() {
            return None;
        }
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => hotkey.ctrl = true,
            "alt" => hotkey.alt = true,
            "shift" => hotkey.shift = true,
            "win" | "super" | "meta" => hotkey.win = true,
            _ => {
                if hotkey.vk != 0 {
                    return None;
                }
                hotkey.vk = vk_from_name(token)?;
            }
        }
    }
    (hotkey.vk != 0).then_some(hotkey)
}

fn vk_from_name(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    let chars: Vec<char> = upper.chars().collect();
    if chars.len() == 1 {
        let ch = chars[0];
        if ch.is_ascii_uppercase() {
            return Some(ch as u32);
        }
        if ch.is_ascii_digit() {
            return Some(ch as u32);
        }
        return None;
    }
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(number) = rest.parse::<u32>() {
            if (1..=24).contains(&number) {
                return Some(0x70 + number - 1);
            }
        }
    }
    let vk = match upper.as_str() {
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "BACKSPACE" => 0x08,
        "ESC" | "ESCAPE" => 0x1B,
        "INSERT" => 0x2D,
        "DELETE" => 0x2E,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "LEFT" => 0x25,
        "UP" => 0x26,
        "RIGHT" => 0x27,
        "DOWN" => 0x28,
        "PAUSE" => 0x13,
        _ => return None,
    };
    Some(vk)
}

fn key_name(vk: u32) -> String {
    match vk {
        0x20 => "Space".into(),
        0x0D => "Enter".into(),
        0x09 => "Tab".into(),
        0x08 => "Backspace".into(),
        0x1B => "Esc".into(),
        0x2D => "Insert".into(),
        0x2E => "Delete".into(),
        0x24 => "Home".into(),
        0x23 => "End".into(),
        0x21 => "PageUp".into(),
        0x22 => "PageDown".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x13 => "Pause".into(),
        0x70..=0x87 => format!("F{}", vk - 0x70 + 1),
        0x41..=0x5A | 0x30..=0x39 => char::from_u32(vk).map(|c| c.to_string()).unwrap_or_default(),
        _ => format!("VK{vk:02X}"),
    }
}

/// Virtual-key code for a key captured in the settings window.
pub fn vk_from_egui(key: eframe::egui::Key) -> Option<u32> {
    use eframe::egui::Key;
    let vk = match key {
        Key::A => 0x41,
        Key::B => 0x42,
        Key::C => 0x43,
        Key::D => 0x44,
        Key::E => 0x45,
        Key::F => 0x46,
        Key::G => 0x47,
        Key::H => 0x48,
        Key::I => 0x49,
        Key::J => 0x4A,
        Key::K => 0x4B,
        Key::L => 0x4C,
        Key::M => 0x4D,
        Key::N => 0x4E,
        Key::O => 0x4F,
        Key::P => 0x50,
        Key::Q => 0x51,
        Key::R => 0x52,
        Key::S => 0x53,
        Key::T => 0x54,
        Key::U => 0x55,
        Key::V => 0x56,
        Key::W => 0x57,
        Key::X => 0x58,
        Key::Y => 0x59,
        Key::Z => 0x5A,
        Key::Num0 => 0x30,
        Key::Num1 => 0x31,
        Key::Num2 => 0x32,
        Key::Num3 => 0x33,
        Key::Num4 => 0x34,
        Key::Num5 => 0x35,
        Key::Num6 => 0x36,
        Key::Num7 => 0x37,
        Key::Num8 => 0x38,
        Key::Num9 => 0x39,
        Key::F1 => 0x70,
        Key::F2 => 0x71,
        Key::F3 => 0x72,
        Key::F4 => 0x73,
        Key::F5 => 0x74,
        Key::F6 => 0x75,
        Key::F7 => 0x76,
        Key::F8 => 0x77,
        Key::F9 => 0x78,
        Key::F10 => 0x79,
        Key::F11 => 0x7A,
        Key::F12 => 0x7B,
        Key::Space => 0x20,
        Key::Enter => 0x0D,
        Key::Tab => 0x09,
        Key::Backspace => 0x08,
        Key::Escape => 0x1B,
        Key::Insert => 0x2D,
        Key::Delete => 0x2E,
        Key::Home => 0x24,
        Key::End => 0x23,
        Key::PageUp => 0x21,
        Key::PageDown => 0x22,
        Key::ArrowLeft => 0x25,
        Key::ArrowUp => 0x26,
        Key::ArrowRight => 0x27,
        Key::ArrowDown => 0x28,
        _ => return None,
    };
    Some(vk)
}
