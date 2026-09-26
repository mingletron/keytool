//! Wire protocol for the 0x1189 USB HID macro keypad.
//!
//! All data is derived from reverse-engineering the original Qt/HID app
//! (see FINDINGS.md). Two things live here:
//!   - the 65-byte HID report framing (report ID 0x03, command bytes), and
//!   - the 50-byte per-key record layout (FINDINGS.md §11).

// ---- Report framing constants -------------------------------------------------

/// USB Vendor ID for the keypad family.
pub const VID: u16 = 0x1189;

/// Supported Product IDs (`PID_GRU` in the original).
pub const PIDS: &[u16] = &[0x8840, 0x8842, 0x8830, 0x8831, 0x8832, 0x8833];

/// Report ID used for every command/response.
pub const REPORT_ID: u8 = 0x03;

/// Total bytes passed to hid_write: report ID + 64-byte payload.
pub const REPORT_LEN: usize = 65;

/// Bytes read back from the device (the report ID is consumed by hidapi on read;
/// the 64-byte payload is what we parse).
pub const READ_LEN: usize = 64;

/// Command markers (payload byte 0, i.e. report byte 1).
pub const CMD_READ_KEY_COUNT: u8 = 0xFB;
pub const CMD_READ_KEY: u8 = 0xFA;
pub const CMD_SET_VERSION: u8 = 0xFC;
/// Write marker for the `0x514C` variant: placed at `record[0]` (= `report[1]`)
/// to tell the device this report is a config write. (On the `0x1189` app the
/// per-key counter occupies that slot as an implicit write marker; the
/// `0x514C` firmware instead requires the explicit `0xFE` byte — `0xFA` there
/// is a read, and a small counter is silently ignored.) Validated in hardware.
pub const CMD_WRITE_KEY: u8 = 0xFE;
/// Commit sentinel written after the config stream: [0x03, 0xFD, 0xFE, 0xFF, ...].
pub const CMD_COMMIT: [u8; 3] = [0xFD, 0xFE, 0xFF];

/// Record length per key slot.
pub const RECORD_LEN: usize = 50;
/// Keys per layer (slot 0 is a reserved header; keys are 1-indexed).
pub const KEYS_PER_LAYER: usize = 60;
/// Layers supported by the device.
pub const LAYER_COUNT: usize = 3;
/// Bytes per layer = KEYS_PER_LAYER * RECORD_LEN.
pub const LAYER_BYTES: usize = KEYS_PER_LAYER * RECORD_LEN;

// ---- Rotary encoder slots (12add3 model: 12 keys + 3 rotary knobs) ----------
//
// The three rotary encoders live at on-wire slots 16..=24 — three per knob,
// laid out in turn-left / press / turn-right order:
//   knob 1: slot 16 = turn left, 17 = press, 18 = turn right
//   knob 2: slot 19 = turn left, 20 = press, 21 = turn right
//   knob 3: slot 22 = turn left, 23 = press, 24 = turn right
// Confirmed by disassembling the original app's `Set_Keyboard_12add3`
// (`QButtonGroup::addButton` id sequence) and empirically: `read_layer_report`
// with `rotary_count = 3` streams exactly these 9 slots. Direction is encoded
// by slot number only — the 50-byte record layout is identical to a key.
//
// Slots 13..=15 are *dead*: hidden 15-key-model keys (`QWidget::hide()`'d in
// `Set_Keyboard_12add3`), never read or written on the 12+3 device.

/// Number of rotary encoders (the "add3" in 12add3).
pub const ROTARY_COUNT: usize = 3;
/// Directions per rotary: turn-left, press, turn-right (in slot order).
pub const ROTARY_DIRS: usize = 3;
/// First on-wire slot of the rotary encoders.
pub const ROTARY_SLOT_BASE: usize = 16;
/// Last rotary slot (inclusive): `ROTARY_SLOT_BASE + ROTARY_COUNT*ROTARY_DIRS - 1`.
pub const ROTARY_SLOT_END: usize = ROTARY_SLOT_BASE + ROTARY_COUNT * ROTARY_DIRS - 1;
/// Total slots per layer: header(0) + keys(1..=12) + dead(13..=15) + rotary(16..=24).
/// A `read_layer` row is always this wide; unstreamed slots stay `KeyRecord::empty`.
pub const LAYER_SLOTS: usize = ROTARY_SLOT_END + 1;

/// One rotary direction. The three slots of each knob are laid out in this
/// order: `Left`, `Press`, `Right` (slots 16/17/18 for knob 1, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotaryDir {
    /// Turn counterclockwise.
    Left,
    /// Push down (click).
    Press,
    /// Turn clockwise.
    Right,
}

impl RotaryDir {
    /// Slot offset within a knob: `Left` = 0, `Press` = 1, `Right` = 2.
    pub const fn offset(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Press => 1,
            Self::Right => 2,
        }
    }

    /// Human-readable direction name ("turn left", "press", "turn right").
    pub const fn name(self) -> &'static str {
        match self {
            Self::Left => "turn left",
            Self::Press => "press",
            Self::Right => "turn right",
        }
    }

    /// All three directions in slot order.
    pub const ALL: [Self; 3] = [Self::Left, Self::Press, Self::Right];
}

/// On-wire slot for `knob` (1-indexed, `1..=ROTARY_COUNT`) and `dir`.
pub const fn rotary_slot(knob: usize, dir: RotaryDir) -> usize {
    ROTARY_SLOT_BASE + (knob - 1) * ROTARY_DIRS + dir.offset()
}

/// Inverse of [`rotary_slot`]: `(knob, dir)` for a rotary slot, or `None` if
/// `slot` is not a rotary encoder slot.
pub fn rotary_slot_parse(slot: usize) -> Option<(usize, RotaryDir)> {
    if !(ROTARY_SLOT_BASE..=ROTARY_SLOT_END).contains(&slot) {
        return None;
    }
    let i = slot - ROTARY_SLOT_BASE;
    let knob = i / ROTARY_DIRS + 1;
    let dir = match i % ROTARY_DIRS {
        0 => RotaryDir::Left,
        1 => RotaryDir::Press,
        _ => RotaryDir::Right,
    };
    Some((knob, dir))
}

/// True if `slot` is a rotary encoder slot (`16..=24` on the 12add3).
pub fn is_rotary_slot(slot: usize) -> bool {
    (ROTARY_SLOT_BASE..=ROTARY_SLOT_END).contains(&slot)
}

// ---- Report builders ----------------------------------------------------------

/// Build a zero-padded 65-byte report: `[REPORT_ID, cmd, fill...]`.
pub fn report(cmd_or_payload: &[u8]) -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = REPORT_ID;
    let n = cmd_or_payload.len().min(REPORT_LEN - 1);
    buf[1..1 + n].copy_from_slice(&cmd_or_payload[..n]);
    buf
}

/// `[0x03, 0xFB, 0xFB, 0xFB, 0...]` — query number of keys / layers.
pub fn read_key_count_report() -> [u8; REPORT_LEN] {
    report(&[CMD_READ_KEY_COUNT, CMD_READ_KEY_COUNT, CMD_READ_KEY_COUNT])
}

/// `[0x03, 0xFA, key_count, rotary_count, layer, 0...]` — request a full
/// layer's config (keys + rotary encoders).
///
/// The device's `read_Hidkey_Data` runs two phases: phase 1 streams slots
/// `1..=key_count` (the physical keys); phase 2 streams slots
/// `ROTARY_SLOT_BASE..=ROTARY_SLOT_END` (the rotary encoders' turn-left /
/// press / turn-right records, `3 * rotary_count` of them). Send
/// `rotary_count = 0` to skip phase 2 (keys only). `layer` is 1-indexed on the
/// wire. Verified against the 12add3 hardware: `key_count = 12, rotary_count =
/// 3` yields 12 + 9 = 21 packets per layer (slots 1..=12 then 16..=24).
pub fn read_layer_report(key_count: u8, rotary_count: u8, layer: u8) -> [u8; REPORT_LEN] {
    report(&[CMD_READ_KEY, key_count, rotary_count, layer])
}

/// `[0x03, 0xFA, key, layer, seq, 0...]` — legacy single-key request form
/// (kept for compatibility with the original app's per-key read loop).
pub fn read_key_report(key: u8, layer: u8, seq: u8) -> [u8; REPORT_LEN] {
    report(&[CMD_READ_KEY, key, layer, seq])
}

/// Commit sentinel written after streaming all modified key records.
pub fn commit_report() -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = REPORT_ID;
    buf[1..1 + CMD_COMMIT.len()].copy_from_slice(&CMD_COMMIT);
    buf
}

/// `[0x03, 0xFE, keyIndex, layer, <record body>, padding]` — write one key's
/// configuration on the `0x514C` variant.
///
/// `record[0]` is forced to the `0xFE` write marker, `record[1]` to the
/// 1-indexed key, and `record[2]` to the 1-indexed layer; the rest of the
/// record (mode/delay/seq_len/slots) is taken from `record` as-is. The
/// caller passes a 0-indexed `layer`.
pub fn write_key_report(key: u8, layer: u8, record: &KeyRecord) -> [u8; REPORT_LEN] {
    let mut bytes = record.to_bytes();
    bytes[0] = CMD_WRITE_KEY; // 0xFE write marker
    bytes[1] = key; // 1-indexed key
    bytes[2] = layer + 1; // device layers are 1-indexed
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = REPORT_ID;
    buf[1..1 + RECORD_LEN].copy_from_slice(&bytes);
    buf
}

// ---- Parsed key-count response -------------------------------------------------

/// Result of the `0xFB` query: how many physical keys and how many layers.
#[derive(Debug, Clone, Copy)]
pub struct KeyCount {
    pub keys: u8,
    pub layers: u8,
}

/// Strip a leading report-ID byte if present.
///
/// The macOS hidapi backend includes the report ID (`0x03`) as the first byte
/// of every read; the Windows backend does not. Normalising here means every
/// response parser sees a payload that starts with the command echo.
pub fn strip_report_id(resp: &[u8]) -> &[u8] {
    if resp.first() == Some(&REPORT_ID) {
        &resp[1..]
    } else {
        resp
    }
}

impl KeyCount {
    /// Parse a read response. After [`strip_report_id`] the payload begins with
    /// the command echo: `[cmd_echo(0xFB), keys, layers, ...]`.
    pub fn parse(resp: &[u8]) -> Option<Self> {
        let p = strip_report_id(resp);
        if p.len() < 3 {
            return None;
        }
        Some(Self {
            keys: p[1],
            layers: p[2],
        })
    }
}

// ---- 50-byte per-key record (FINDINGS.md §11) ---------------------------------

/// Assignment type stored in `record[0x03]`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Basic = 1,
    Multimedia = 2,
    Mouse = 3,
    Led = 5,
    /// Combination/macro — payload format not fully decoded (FINDINGS.md §12).
    /// Stored/loaded opaquely so it is never corrupted.
    Macro = 8,
}

impl Mode {
    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Basic),
            2 => Some(Self::Multimedia),
            3 => Some(Self::Mouse),
            5 => Some(Self::Led),
            8 => Some(Self::Macro),
            _ => None,
        }
    }
}

/// Offsets within the 50-byte record (FINDINGS.md §11.2).
pub mod off {
    pub const MARKER: usize = 0x00; // 0xFE for LED mode
    pub const LED_SUB: usize = 0x01; // 0xB0
    pub const LED_INDEX: usize = 0x02; // layer+1
    pub const MODE: usize = 0x03;
    pub const DELAY: usize = 0x04;
    pub const AUX: usize = 0x07; // code byte in mode 2
    pub const SUBTYPE: usize = 0x08; // mode 3
    pub const SEQ_LEN: usize = 0x09;
    pub const SEQ_START: usize = 0x0A; // 18 slots × 2 bytes
    pub const SEQ_END: usize = 0x2D; // inclusive: 0x0A..=0x2D
    pub const MOUSE_BTN: usize = 0x0B; // also slot[0].1
    pub const MOUSE_PARAM: usize = 0x0E;
    pub const LED_BITMASK: usize = 0x0B;
}

// ---- LED mode (confirmed by USB capture, FINDINGS.md §12.6) -----------------
//
// The global/pe-layer LED mode is written with the report:
//   [0x03, 0xFE, 0xB0, 0x0<layer>, 0x08, 0,0,0,0,0, 0x01, 0x00, 0x0<colour>]
// followed by the commit sentinel. Byte 3 is the layer (1/2/3), byte 4 is the
// mode (0x08 = mode 4 from Dialog3), byte 10 is seq_len (=1), and byte 12 is
// the colour selector: low nibble = key bits (0x4 = "all keys on this layer"),
// high nibble = colour index 1..7 (LED_color_1..7).
//
// Decoded from led2.pcapng (the Windows app onChange of Dialog3):
//   colour byte 0x14 = red,   0x24 = orange, 0x34 = yellow,
//                 0x44 = green, 0x54 = cyan,  0x64 = blue,   0x74 = purple

/// LED mode value written at byte 4 (Dialog3 "LED mode 4").
pub const LED_MODE: u8 = 0x08;
/// Offset of the layer byte within the LED report (report index, after report ID).
pub const LED_REPORT_LAYER: usize = 3;
/// Offset of the mode byte within the LED report.
pub const LED_REPORT_MODE: usize = 4;
/// Offset of seq_len within the LED report.
pub const LED_REPORT_SEQ_LEN: usize = 10;
/// Offset of the colour selector within the LED report.
pub const LED_REPORT_COLOUR: usize = 12;

/// LED colour selector byte: `0x0N4` where N = colour index (1=red .. 7=purple)
/// and the low nibble `4` = "all keys on the layer".
pub const fn led_colour(index: u8) -> u8 {
    (index << 4) | 0x04
}

/// Build a full 65-byte LED-mode write report for a given device layer (1..3)
/// and colour index (1..7). Mirrors the Windows app's Dialog3 writes exactly.
pub fn led_report(layer: u8, colour_idx: u8) -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = REPORT_ID;
    buf[1] = CMD_WRITE_KEY; // 0xFE
    buf[2] = 0xB0; // LED sub-marker
    buf[LED_REPORT_LAYER] = layer;
    buf[LED_REPORT_MODE] = LED_MODE; // 0x08
    buf[LED_REPORT_SEQ_LEN] = 0x01;
    buf[LED_REPORT_COLOUR] = led_colour(colour_idx);
    buf
}

pub const SLOTS: usize = 18; // (0x2D - 0x0A + 1) / 2

/// One slot in the sequence region: `[byte0, byte1]` at `0x0A + 2*i`.
///
/// Interpretation depends on `mode` (see `Mode` docs and FINDINGS.md §11.4):
///   - Basic:      (modifier_bitmask, hid_keycode)
///   - Multimedia:  (consumer_code, usage_page)
///   - Mouse/LED:   repurposed marker/button fields.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Slot {
    pub b0: u8,
    pub b1: u8,
}

/// A parsed 50-byte key record. Field meanings are mode-dependent; we preserve
/// every byte so any mode round-trips losslessly.
#[derive(Debug, Clone)]
pub struct KeyRecord {
    pub mode: u8,
    pub delay: u8,
    pub seq_len: u8,
    pub raw: [u8; RECORD_LEN],
}

impl Default for KeyRecord {
    fn default() -> Self {
        Self::empty()
    }
}

impl KeyRecord {
    /// An all-zero record (mode 0 / unassigned).
    pub fn empty() -> Self {
        Self {
            mode: 0,
            delay: 0,
            seq_len: 0,
            raw: [0u8; RECORD_LEN],
        }
    }

    /// Parse from the 50 bytes the device returns (response[1..] in `read_Hidkey_Data`).
    pub fn from_bytes(b: &[u8; RECORD_LEN]) -> Self {
        Self {
            mode: b[off::MODE],
            delay: b[off::DELAY],
            seq_len: b[off::SEQ_LEN],
            raw: *b,
        }
    }

    /// Serialize back to the 50-byte on-wire form.
    pub fn to_bytes(&self) -> [u8; RECORD_LEN] {
        let mut b = self.raw;
        b[off::MODE] = self.mode;
        b[off::DELAY] = self.delay;
        b[off::SEQ_LEN] = self.seq_len;
        b
    }

    /// The 18 2-byte slots in the sequence region.
    pub fn slots(&self) -> [Slot; SLOTS] {
        let mut s = [Slot::default(); SLOTS];
        for i in 0..SLOTS {
            s[i] = Slot {
                b0: self.raw[off::SEQ_START + 2 * i],
                b1: self.raw[off::SEQ_START + 2 * i + 1],
            };
        }
        s
    }

    /// Set a slot (writes through to `raw`).
    pub fn set_slot(&mut self, i: usize, s: Slot) {
        if i < SLOTS {
            self.raw[off::SEQ_START + 2 * i] = s.b0;
            self.raw[off::SEQ_START + 2 * i + 1] = s.b1;
        }
    }

    /// Parsed mode, if recognised.
    pub fn mode_enum(&self) -> Option<Mode> {
        Mode::from_byte(self.mode)
    }
}

/// The full in-memory configuration: 3 layers, each 60 slots (slot 0 reserved).
#[derive(Debug, Clone)]
pub struct Config {
    pub layers: [Vec<KeyRecord>; LAYER_COUNT],
}

impl Default for Config {
    fn default() -> Self {
        Self::empty(15, LAYER_COUNT as u8)
    }
}

impl Config {
    /// Empty config sized for `keys` physical keys + header slot.
    pub fn empty(keys: usize, layers: u8) -> Self {
        let layer = || vec![KeyRecord::empty(); KEYS_PER_LAYER];
        let layers_arr = [layer(), layer(), layer()];
        // only first `layers` are meaningful
        let _ = keys;
        let _ = layers;
        Self {
            layers: layers_arr,
        }
    }

    pub fn record(&self, layer: usize, key: usize) -> Option<&KeyRecord> {
        self.layers.get(layer)?.get(key)
    }
    pub fn record_mut(&mut self, layer: usize, key: usize) -> Option<&mut KeyRecord> {
        self.layers.get_mut(layer)?.get_mut(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_key_count_report_shape() {
        let r = read_key_count_report();
        assert_eq!(r[0], REPORT_ID);
        assert_eq!(&r[1..4], &[0xFB, 0xFB, 0xFB]);
        assert_eq!(r[4..], [0u8; REPORT_LEN - 4]);
    }

    #[test]
    fn commit_report_shape() {
        let r = commit_report();
        assert_eq!(r[0], REPORT_ID);
        assert_eq!(&r[1..4], &[0xFD, 0xFE, 0xFF]);
    }

    #[test]
    fn read_key_report_carries_params() {
        let r = read_key_report(7, 2, 3);
        assert_eq!(r[0], REPORT_ID);
        assert_eq!(r[1], 0xFA);
        assert_eq!(r[2], 7);
        assert_eq!(r[3], 2);
        assert_eq!(r[4], 3);
    }

    #[test]
    fn write_key_report_round_trips_record() {
        let mut rec = KeyRecord::empty();
        rec.mode = 1;
        rec.delay = 12;
        rec.seq_len = 2;
        rec.raw[off::SEQ_START] = 0x05; // LGui
        rec.raw[off::SEQ_START + 1] = 0x06; // 'c'
        let r = write_key_report(3, 1, &rec);
        assert_eq!(r[0], REPORT_ID);
        assert_eq!(r[1], CMD_WRITE_KEY); // 0xFE write marker
        assert_eq!(r[2], 3); // key index
        assert_eq!(r[3], 2); // layer (1-indexed: 1+1)
        assert_eq!(r[1 + off::MODE], 1);
        assert_eq!(r[1 + off::DELAY], 12);
        assert_eq!(r[1 + off::SEQ_LEN], 2);
        assert_eq!(r[1 + off::SEQ_START], 0x05);
    }

    #[test]
    fn record_round_trips_losslessly_for_all_modes() {
        for mode in [1u8, 2, 3, 5, 8] {
            let mut raw = [0u8; RECORD_LEN];
            // sprinkle recognisable bytes
            for (i, b) in raw.iter_mut().enumerate() {
                *b = ((i as u8) ^ mode).wrapping_add(0x11);
            }
            raw[off::MODE] = mode;
            raw[off::DELAY] = 0x44;
            raw[off::SEQ_LEN] = 0x03;
            let rec = KeyRecord::from_bytes(&raw);
            assert_eq!(rec.mode, mode);
            assert_eq!(rec.delay, 0x44);
            assert_eq!(rec.seq_len, 0x03);
            assert_eq!(rec.to_bytes(), raw, "mode {mode} must round-trip exactly");
        }
    }

    #[test]
    fn key_count_parse() {
        // Real macOS hidapi response: [reportID=0x03, cmd_echo=0xFB, keys, layers].
        let resp = [0x03, 0xFB, 0x0C, 0x03];
        let kc = KeyCount::parse(&resp).unwrap();
        assert_eq!(kc.keys, 12);
        assert_eq!(kc.layers, 3);
    }

    #[test]
    fn key_count_parse_without_report_id() {
        // Windows-style response (no leading report ID): [cmd_echo, keys, layers].
        let resp = [0xFB, 0x0C, 0x03];
        let kc = KeyCount::parse(&resp).unwrap();
        assert_eq!(kc.keys, 12);
        assert_eq!(kc.layers, 3);
    }

    #[test]
    fn slots_read_and_write() {
        let mut rec = KeyRecord::empty();
        rec.set_slot(0, Slot { b0: 0x01, b1: 0x06 });
        rec.set_slot(17, Slot { b0: 0xFF, b1: 0xAA });
        let s = rec.slots();
        assert_eq!(s[0], Slot { b0: 0x01, b1: 0x06 });
        assert_eq!(s[17], Slot { b0: 0xFF, b1: 0xAA });
    }

    #[test]
    fn read_layer_report_carries_rotary_count() {
        let r = read_layer_report(12, 3, 1);
        assert_eq!(r[0], REPORT_ID);
        assert_eq!(r[1], CMD_READ_KEY);
        assert_eq!(r[2], 12); // key_count
        assert_eq!(r[3], 3); // rotary_count
        assert_eq!(r[4], 1); // layer (1-indexed)
        // rotary_count = 0 -> byte[3] = 0 (keys-only; phase 2 skipped).
        let r0 = read_layer_report(12, 0, 1);
        assert_eq!(r0[3], 0);
    }

    #[test]
    fn rotary_slot_mapping() {
        assert_eq!(ROTARY_SLOT_BASE, 16);
        assert_eq!(ROTARY_SLOT_END, 24);
        assert_eq!(LAYER_SLOTS, 25);
        // knob 1: left/press/right = 16/17/18
        assert_eq!(rotary_slot(1, RotaryDir::Left), 16);
        assert_eq!(rotary_slot(1, RotaryDir::Press), 17);
        assert_eq!(rotary_slot(1, RotaryDir::Right), 18);
        // knob 2: 19/20/21; knob 3: 22/23/24
        assert_eq!(rotary_slot(2, RotaryDir::Right), 21);
        assert_eq!(rotary_slot(3, RotaryDir::Left), 22);
        assert_eq!(rotary_slot(3, RotaryDir::Right), 24);
        // inverse
        assert_eq!(rotary_slot_parse(16), Some((1, RotaryDir::Left)));
        assert_eq!(rotary_slot_parse(17), Some((1, RotaryDir::Press)));
        assert_eq!(rotary_slot_parse(24), Some((3, RotaryDir::Right)));
        // non-rotary slots
        assert_eq!(rotary_slot_parse(12), None); // key
        assert_eq!(rotary_slot_parse(15), None); // dead slot
        assert_eq!(rotary_slot_parse(25), None); // out of range
        assert!(is_rotary_slot(16));
        assert!(!is_rotary_slot(15));
    }
}
