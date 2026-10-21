//! HarmonyOS keyboard handling.
//!
//! Maps OHOS `KeyCode` values (from `native_key_event.h`) to GPUI `Keystroke`
//! / `Modifiers`, mirroring `gpui_mobile::android::keyboard`.
//!
//! ## Key codes
//!
//! The constants below match the OpenHarmony `KeyCode` enum (the numeric
//! values are stable across API 9+).  They are centralised here so that a
//! change in the platform enum only needs a single-file update.
//!
//! ## Meta state
//!
//! The `XComponent` key-event ABI (`OH_NativeXComponent_KeyEvent`) does not
//! carry a modifier bitmask, so `meta_state` is normally `0`.  The bit flags
//! are defined here for completeness and for callers that can supply them
//! (e.g. an ArkTS bridge that tracks modifiers itself).

use gpui::{Keystroke, Modifiers};

// ── OHOS meta-state flags ────────────────────────────────────────────────────

/// No meta keys pressed.
pub const OHOS_META_NONE: u32 = 0;
/// Shift key is pressed.
pub const OHOS_META_SHIFT_ON: u32 = 0x01;
/// Alt key is pressed.
pub const OHOS_META_ALT_ON: u32 = 0x02;
/// Ctrl key is pressed.
pub const OHOS_META_CTRL_ON: u32 = 0x1000;
/// Meta (Command) key is pressed.
pub const OHOS_META_META_ON: u32 = 0x10000;
/// Function key is pressed.
pub const OHOS_META_FUNCTION_ON: u32 = 0x08;

// ── OHOS key actions ─────────────────────────────────────────────────────────

/// `OH_NativeXComponent_KeyAction::NONE`.
pub const KEY_ACTION_NONE: i32 = -1;
/// `OH_NativeXComponent_KeyAction::DOWN`.
pub const KEY_ACTION_DOWN: i32 = 0;
/// `OH_NativeXComponent_KeyAction::UP`.
pub const KEY_ACTION_UP: i32 = 1;
/// `OH_NativeXComponent_KeyAction::CANCEL`.
pub const KEY_ACTION_CANCEL: i32 = 2;

// ── OHOS key codes (native_key_event.h) ──────────────────────────────────────

pub const KEY_UNKNOWN: i32 = -1;
pub const KEY_HOME: i32 = 1;
pub const KEY_BACK: i32 = 2;
pub const KEY_VOLUME_UP: i32 = 16;
pub const KEY_VOLUME_DOWN: i32 = 17;
pub const KEY_POWER: i32 = 18;

pub const KEY_0: i32 = 2000;
pub const KEY_1: i32 = 2001;
pub const KEY_2: i32 = 2002;
pub const KEY_3: i32 = 2003;
pub const KEY_4: i32 = 2004;
pub const KEY_5: i32 = 2005;
pub const KEY_6: i32 = 2006;
pub const KEY_7: i32 = 2007;
pub const KEY_8: i32 = 2008;
pub const KEY_9: i32 = 2009;
pub const KEY_STAR: i32 = 2010;
pub const KEY_POUND: i32 = 2011;
pub const KEY_DPAD_UP: i32 = 2012;
pub const KEY_DPAD_DOWN: i32 = 2013;
pub const KEY_DPAD_LEFT: i32 = 2014;
pub const KEY_DPAD_RIGHT: i32 = 2015;
pub const KEY_DPAD_CENTER: i32 = 2016;
pub const KEY_A: i32 = 2017;
pub const KEY_B: i32 = 2018;
pub const KEY_C: i32 = 2019;
pub const KEY_D: i32 = 2020;
pub const KEY_E: i32 = 2021;
pub const KEY_F: i32 = 2022;
pub const KEY_G: i32 = 2023;
pub const KEY_H: i32 = 2024;
pub const KEY_I: i32 = 2025;
pub const KEY_J: i32 = 2026;
pub const KEY_K: i32 = 2027;
pub const KEY_L: i32 = 2028;
pub const KEY_M: i32 = 2029;
pub const KEY_N: i32 = 2030;
pub const KEY_O: i32 = 2031;
pub const KEY_P: i32 = 2032;
pub const KEY_Q: i32 = 2033;
pub const KEY_R: i32 = 2034;
pub const KEY_S: i32 = 2035;
pub const KEY_T: i32 = 2036;
pub const KEY_U: i32 = 2037;
pub const KEY_V: i32 = 2038;
pub const KEY_W: i32 = 2039;
pub const KEY_X: i32 = 2040;
pub const KEY_Y: i32 = 2041;
pub const KEY_Z: i32 = 2042;
pub const KEY_COMMA: i32 = 2043;
pub const KEY_PERIOD: i32 = 2044;
pub const KEY_ALT_LEFT: i32 = 2045;
pub const KEY_ALT_RIGHT: i32 = 2046;
pub const KEY_SHIFT_LEFT: i32 = 2047;
pub const KEY_SHIFT_RIGHT: i32 = 2048;
pub const KEY_TAB: i32 = 2049;
pub const KEY_SPACE: i32 = 2050;
pub const KEY_ENTER: i32 = 2054;
pub const KEY_DEL: i32 = 2055; // Backspace
pub const KEY_GRAVE: i32 = 2056;
pub const KEY_MINUS: i32 = 2057;
pub const KEY_EQUALS: i32 = 2058;
pub const KEY_LEFT_BRACKET: i32 = 2059;
pub const KEY_RIGHT_BRACKET: i32 = 2060;
pub const KEY_BACKSLASH: i32 = 2061;
pub const KEY_SEMICOLON: i32 = 2062;
pub const KEY_APOSTROPHE: i32 = 2063;
pub const KEY_SLASH: i32 = 2064;
pub const KEY_AT: i32 = 2065;
pub const KEY_PLUS: i32 = 2066;
pub const KEY_MENU: i32 = 2067;
pub const KEY_PAGE_UP: i32 = 2068;
pub const KEY_PAGE_DOWN: i32 = 2069;
pub const KEY_ESCAPE: i32 = 2070;
pub const KEY_FORWARD_DEL: i32 = 2071;
pub const KEY_CTRL_LEFT: i32 = 2072;
pub const KEY_CTRL_RIGHT: i32 = 2073;
pub const KEY_CAPS_LOCK: i32 = 2074;
pub const KEY_SCROLL_LOCK: i32 = 2075;
pub const KEY_META_LEFT: i32 = 2076;
pub const KEY_META_RIGHT: i32 = 2077;
pub const KEY_FUNCTION: i32 = 2078;
pub const KEY_SYSRQ: i32 = 2079;
pub const KEY_BREAK: i32 = 2080;
pub const KEY_MOVE_HOME: i32 = 2081;
pub const KEY_MOVE_END: i32 = 2082;
pub const KEY_INSERT: i32 = 2083;
pub const KEY_NUM_LOCK: i32 = 2102;
pub const KEY_F1: i32 = 2090;
pub const KEY_F2: i32 = 2091;
pub const KEY_F3: i32 = 2092;
pub const KEY_F4: i32 = 2093;
pub const KEY_F5: i32 = 2094;
pub const KEY_F6: i32 = 2095;
pub const KEY_F7: i32 = 2096;
pub const KEY_F8: i32 = 2097;
pub const KEY_F9: i32 = 2098;
pub const KEY_F10: i32 = 2099;
pub const KEY_F11: i32 = 2100;
pub const KEY_F12: i32 = 2101;

// ── conversion helpers ───────────────────────────────────────────────────────

/// Convert an OHOS meta-state bitmask to GPUI [`Modifiers`].
pub fn ohos_meta_to_modifiers(meta_state: u32) -> Modifiers {
    Modifiers {
        control: meta_state & OHOS_META_CTRL_ON != 0,
        alt: meta_state & OHOS_META_ALT_ON != 0,
        shift: meta_state & OHOS_META_SHIFT_ON != 0,
        platform: meta_state & OHOS_META_META_ON != 0,
        function: meta_state & OHOS_META_FUNCTION_ON != 0,
    }
}

/// Convert an OHOS key code to a GPUI key-name string.
///
/// Returns `None` for codes that should be ignored (pure modifiers, volume,
/// power, etc.).
pub fn ohos_keycode_to_key(key_code: i32) -> Option<String> {
    let key = match key_code {
        // Letters
        KEY_A => "a",
        KEY_B => "b",
        KEY_C => "c",
        KEY_D => "d",
        KEY_E => "e",
        KEY_F => "f",
        KEY_G => "g",
        KEY_H => "h",
        KEY_I => "i",
        KEY_J => "j",
        KEY_K => "k",
        KEY_L => "l",
        KEY_M => "m",
        KEY_N => "n",
        KEY_O => "o",
        KEY_P => "p",
        KEY_Q => "q",
        KEY_R => "r",
        KEY_S => "s",
        KEY_T => "t",
        KEY_U => "u",
        KEY_V => "v",
        KEY_W => "w",
        KEY_X => "x",
        KEY_Y => "y",
        KEY_Z => "z",

        // Number row
        KEY_0 => "0",
        KEY_1 => "1",
        KEY_2 => "2",
        KEY_3 => "3",
        KEY_4 => "4",
        KEY_5 => "5",
        KEY_6 => "6",
        KEY_7 => "7",
        KEY_8 => "8",
        KEY_9 => "9",

        // Punctuation / symbols
        KEY_COMMA => ",",
        KEY_PERIOD => ".",
        KEY_SPACE => " ",
        KEY_GRAVE => "`",
        KEY_MINUS => "-",
        KEY_EQUALS => "=",
        KEY_LEFT_BRACKET => "[",
        KEY_RIGHT_BRACKET => "]",
        KEY_BACKSLASH => "\\",
        KEY_SEMICOLON => ";",
        KEY_APOSTROPHE => "'",
        KEY_SLASH => "/",
        KEY_AT => "@",
        KEY_STAR => "*",
        KEY_POUND => "#",
        KEY_PLUS => "+",

        // Navigation
        KEY_ENTER => "enter",
        KEY_ESCAPE => "escape",
        KEY_DEL => "backspace",
        KEY_FORWARD_DEL => "delete",
        KEY_TAB => "tab",
        KEY_DPAD_UP => "up",
        KEY_DPAD_DOWN => "down",
        KEY_DPAD_LEFT => "left",
        KEY_DPAD_RIGHT => "right",
        KEY_DPAD_CENTER => "enter",
        KEY_MOVE_HOME => "home",
        KEY_MOVE_END => "end",
        KEY_INSERT => "insert",
        KEY_PAGE_UP => "pageup",
        KEY_PAGE_DOWN => "pagedown",

        // Function keys
        KEY_F1 => "f1",
        KEY_F2 => "f2",
        KEY_F3 => "f3",
        KEY_F4 => "f4",
        KEY_F5 => "f5",
        KEY_F6 => "f6",
        KEY_F7 => "f7",
        KEY_F8 => "f8",
        KEY_F9 => "f9",
        KEY_F10 => "f10",
        KEY_F11 => "f11",
        KEY_F12 => "f12",

        // Menu / back
        KEY_MENU => "menu",
        KEY_BACK => "escape",

        // Ignore pure modifiers, volume, power, locks
        KEY_SHIFT_LEFT | KEY_SHIFT_RIGHT | KEY_CTRL_LEFT | KEY_CTRL_RIGHT | KEY_ALT_LEFT
        | KEY_ALT_RIGHT | KEY_META_LEFT | KEY_META_RIGHT | KEY_FUNCTION | KEY_CAPS_LOCK
        | KEY_NUM_LOCK | KEY_SCROLL_LOCK | KEY_VOLUME_UP | KEY_VOLUME_DOWN | KEY_POWER
        | KEY_HOME | KEY_SYSRQ | KEY_BREAK => return None,

        // Unknown / unmapped
        _ => return None,
    };

    Some(key.to_string())
}

/// Build a GPUI [`Keystroke`] from an OHOS key event.
///
/// Returns `None` if the key code should be ignored.
pub fn ohos_key_to_keystroke(
    key_code: i32,
    meta_state: u32,
    unicode_char: u32,
) -> Option<Keystroke> {
    let key = ohos_keycode_to_key(key_code)?;
    let modifiers = ohos_meta_to_modifiers(meta_state);

    let key_char = if unicode_char != 0 {
        char::from_u32(unicode_char).map(|c| c.to_string())
    } else if key.len() == 1 {
        let ch = key.chars().next().unwrap();
        if modifiers.shift && ch.is_ascii_alphabetic() {
            Some(ch.to_ascii_uppercase().to_string())
        } else {
            Some(key.clone())
        }
    } else {
        None
    };

    Some(Keystroke {
        modifiers,
        key,
        key_char,
    })
}

/// Create a [`Keystroke`] for a single character of software-keyboard text.
pub fn character_to_keystroke(c: char) -> Keystroke {
    Keystroke {
        modifiers: Modifiers::default(),
        key: c.to_lowercase().to_string(),
        key_char: Some(c.to_string()),
    }
}

/// Create a backspace [`Keystroke`].
pub fn backspace_keystroke() -> Keystroke {
    Keystroke {
        modifiers: Modifiers::default(),
        key: "backspace".to_string(),
        key_char: None,
    }
}

// ── keyboard layout ──────────────────────────────────────────────────────────

/// HarmonyOS keyboard layout descriptor.
#[derive(Clone, Debug)]
pub struct OhosKeyboardLayout {
    name: String,
}

impl OhosKeyboardLayout {
    /// Create a new keyboard layout with the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl gpui::PlatformKeyboardLayout for OhosKeyboardLayout {
    fn id(&self) -> &str {
        &self.name
    }

    fn name(&self) -> &str {
        &self.name
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_to_modifiers() {
        let m = ohos_meta_to_modifiers(OHOS_META_CTRL_ON | OHOS_META_SHIFT_ON);
        assert!(m.control);
        assert!(m.shift);
        assert!(!m.alt);
        assert!(!m.platform);
    }

    #[test]
    fn letter_keycodes() {
        assert_eq!(ohos_keycode_to_key(KEY_A), Some("a".to_string()));
        assert_eq!(ohos_keycode_to_key(KEY_Z), Some("z".to_string()));
    }

    #[test]
    fn special_keycodes() {
        assert_eq!(ohos_keycode_to_key(KEY_ENTER), Some("enter".to_string()));
        assert_eq!(ohos_keycode_to_key(KEY_DEL), Some("backspace".to_string()));
        assert_eq!(ohos_keycode_to_key(KEY_SPACE), Some(" ".to_string()));
    }

    #[test]
    fn modifier_keys_ignored() {
        assert_eq!(ohos_keycode_to_key(KEY_SHIFT_LEFT), None);
        assert_eq!(ohos_keycode_to_key(KEY_CTRL_LEFT), None);
        assert_eq!(ohos_keycode_to_key(KEY_CAPS_LOCK), None);
    }

    #[test]
    fn volume_keys_ignored() {
        assert_eq!(ohos_keycode_to_key(KEY_VOLUME_UP), None);
        assert_eq!(ohos_keycode_to_key(KEY_VOLUME_DOWN), None);
    }

    #[test]
    fn keystroke_from_event() {
        let ks = ohos_key_to_keystroke(KEY_A, OHOS_META_NONE, 0).unwrap();
        assert_eq!(ks.key, "a");
        assert_eq!(ks.key_char, Some("a".to_string()));
    }

    #[test]
    fn keystroke_with_unicode() {
        let ks = ohos_key_to_keystroke(KEY_E, OHOS_META_NONE, 'é' as u32).unwrap();
        assert_eq!(ks.key, "e");
        assert_eq!(ks.key_char, Some("é".to_string()));
    }

    #[test]
    fn keyboard_layout_name() {
        let layout = OhosKeyboardLayout::new("en-US");
        assert_eq!(gpui::PlatformKeyboardLayout::id(&layout), "en-US");
    }
}
