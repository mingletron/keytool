//! USB HID keyboard usage codes + consumer-page codes + display names.
//!
//! The key-code lookup table in the original app is the `.data+0x14` array
//! (FINDINGS.md §11.4), a standard USB HID Keyboard/Keypad usage table. The
//! modifier byte is the standard HID modifier bitmap (bit0=LCtrl ... bit7=RGui).

/// Standard USB HID keyboard modifier bits (the byte at record[0x0A + 2*i]).
pub const MOD_LCTRL: u8 = 0x01;
pub const MOD_LSHIFT: u8 = 0x02;
pub const MOD_LALT: u8 = 0x04;
pub const MOD_LGUI: u8 = 0x08;
pub const MOD_RCTRL: u8 = 0x10;
pub const MOD_RSHIFT: u8 = 0x20;
pub const MOD_RALT: u8 = 0x40;
pub const MOD_RGUI: u8 = 0x80;

/// Short modifier label string for a modifier byte, e.g. `0x05` -> "Ctrl+Alt+".
pub fn mod_name(m: u8) -> String {
    let mut s = String::new();
    let bits: &[(u8, &str)] = &[
        (MOD_LCTRL | MOD_RCTRL, "Ctrl+"),
        (MOD_LSHIFT | MOD_RSHIFT, "Shift+"),
        (MOD_LALT | MOD_RALT, "Alt+"),
        (MOD_LGUI | MOD_RGUI, "Win+"),
    ];
    for (mask, label) in bits {
        if m & mask != 0 {
            s.push_str(label);
        }
    }
    s
}

/// Human-readable name for a USB HID keyboard usage code.
pub fn key_name(code: u8) -> &'static str {
    // Standard USB HID Keyboard/Keypad page (0x07) usage IDs.
    match code {
        0x04 => "a", 0x05 => "b", 0x06 => "c", 0x07 => "d", 0x08 => "e",
        0x09 => "f", 0x0A => "g", 0x0B => "h", 0x0C => "i", 0x0D => "j",
        0x0E => "k", 0x0F => "l", 0x10 => "m", 0x11 => "n", 0x12 => "o",
        0x13 => "p", 0x14 => "q", 0x15 => "r", 0x16 => "s", 0x17 => "t",
        0x18 => "u", 0x19 => "v", 0x1A => "w", 0x1B => "x", 0x1C => "y",
        0x1D => "z",
        0x1E => "1", 0x1F => "2", 0x20 => "3", 0x21 => "4", 0x22 => "5",
        0x23 => "6", 0x24 => "7", 0x25 => "8", 0x26 => "9", 0x27 => "0",
        0x28 => "Enter", 0x29 => "Esc", 0x2A => "Backspace", 0x2B => "Tab",
        0x2C => "Space", 0x2D => "-", 0x2E => "=", 0x2F => "[", 0x30 => "]",
        0x31 => "\\", 0x33 => ";", 0x34 => "'", 0x35 => "`", 0x36 => ",",
        0x37 => ".", 0x38 => "/",
        0x39 => "CapsLock",
        0x3A => "F1", 0x3B => "F2", 0x3C => "F3", 0x3D => "F4", 0x3E => "F5",
        0x3F => "F6", 0x40 => "F7", 0x41 => "F8", 0x42 => "F9", 0x43 => "F10",
        0x44 => "F11", 0x45 => "F12",
        0x46 => "PrintScreen", 0x47 => "ScrollLock", 0x48 => "Pause",
        0x49 => "Insert", 0x4A => "Home", 0x4B => "PageUp", 0x4C => "Delete",
        0x4D => "End", 0x4E => "PageDown",
        0x4F => "Right", 0x50 => "Left", 0x51 => "Down", 0x52 => "Up",
        0x53 => "NumLock",
        0x54 => "Num/", 0x55 => "Num*", 0x56 => "Num-", 0x57 => "Num+",
        0x58 => "NumEnter",
        0x59 => "Num1", 0x5A => "Num2", 0x5B => "Num3", 0x5C => "Num4",
        0x5D => "Num5", 0x5E => "Num6", 0x5F => "Num7", 0x60 => "Num8",
        0x61 => "Num9", 0x62 => "Num0",
        0x63 => "Num.",
        0x65 => "App", 0x66 => "Power", 0x67 => "Num=",
        0xE0 => "Left Ctrl", 0xE1 => "Left Shift", 0xE2 => "Left Alt",
        0xE3 => "Left Win", 0xE4 => "Right Ctrl", 0xE5 => "Right Shift",
        0xE6 => "Right Alt", 0xE7 => "Right Win",
        _ => "?",
    }
}

// ---- Consumer page (usage page 0x0C) -----------------------------------------
// The keypad's Multimedia mode stores the consumer usage ID in slot.b0.
// These are standard USB HID Consumer Page usage IDs (HID Usage Tables 1.12).

/// (consumer usage code, display name) for the common media keys this keypad
/// uses. Ordered as typically wanted in a picker.
pub fn consumer_presets() -> &'static [(u8, &'static str)] {
    &[
        (0xCD, "Play/Pause"),
        (0xB5, "Next Track"),
        (0xB6, "Previous Track"),
        (0xB7, "Stop"),
        (0xE2, "Mute"),
        (0xE9, "Volume Up"),
        (0xEA, "Volume Down"),
        (0xB8, "Eject"),
        (0xB4, "Rewind"),
        (0xB3, "Fast Forward"),
        (0xB0, "Play"),
        (0xB1, "Pause"),
        (0x9F, "Browser Home"),
        (0xA6, "Browser Back"),
        (0xA7, "Browser Forward"),
        (0xB2, "Record"),
    ]
}

/// Human-readable name for a consumer-page code, or `"?"` if unknown.
pub fn consumer_name(code: u8) -> &'static str {
    for (c, name) in consumer_presets() {
        if *c == code {
            return name;
        }
    }
    "?"
}

// ---- Categorized key presets for the Basic-mode picker ----------------------
// Each group is a (header, &[(code, name)]) pair. `name` reuses `key_name`
// values so what the picker shows matches the label the button displays.

/// Letters a–z (0x04..=0x1D).
pub fn key_presets() -> &'static [(&'static str, &'static [(u8, &'static str)])] {
    &[
        ("Letters", &[
            (0x04, "a"), (0x05, "b"), (0x06, "c"), (0x07, "d"), (0x08, "e"),
            (0x09, "f"), (0x0A, "g"), (0x0B, "h"), (0x0C, "i"), (0x0D, "j"),
            (0x0E, "k"), (0x0F, "l"), (0x10, "m"), (0x11, "n"), (0x12, "o"),
            (0x13, "p"), (0x14, "q"), (0x15, "r"), (0x16, "s"), (0x17, "t"),
            (0x18, "u"), (0x19, "v"), (0x1A, "w"), (0x1B, "x"), (0x1C, "y"),
            (0x1D, "z"),
        ]),
        ("Digits & symbols", &[
            (0x1E, "1"), (0x1F, "2"), (0x20, "3"), (0x21, "4"), (0x22, "5"),
            (0x23, "6"), (0x24, "7"), (0x25, "8"), (0x26, "9"), (0x27, "0"),
            (0x2D, "-"), (0x2E, "="), (0x2F, "["), (0x30, "]"), (0x31, "\\"),
            (0x33, ";"), (0x34, "'"), (0x35, "`"), (0x36, ","), (0x37, "."),
            (0x38, "/"), (0x2C, "Space"),
        ]),
        ("Function keys", &[
            (0x3A, "F1"), (0x3B, "F2"), (0x3C, "F3"), (0x3D, "F4"),
            (0x3E, "F5"), (0x3F, "F6"), (0x40, "F7"), (0x41, "F8"),
            (0x42, "F9"), (0x43, "F10"), (0x44, "F11"), (0x45, "F12"),
        ]),
        ("Navigation", &[
            (0x4F, "Right"), (0x50, "Left"), (0x51, "Down"), (0x52, "Up"),
            (0x49, "Insert"), (0x4A, "Home"), (0x4B, "PageUp"),
            (0x4C, "Delete"), (0x4D, "End"), (0x4E, "PageDown"),
        ]),
        ("Editing", &[
            (0x28, "Enter"), (0x29, "Esc"), (0x2A, "Backspace"), (0x2B, "Tab"),
            (0x39, "CapsLock"),
        ]),
        ("Modifiers", &[
            (0xE0, "Left Ctrl"), (0xE1, "Left Shift"), (0xE2, "Left Alt"),
            (0xE3, "Left Win"), (0xE4, "Right Ctrl"), (0xE5, "Right Shift"),
            (0xE6, "Right Alt"), (0xE7, "Right Win"),
        ]),
    ]
}
