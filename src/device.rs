//! Thin wrapper over the `hidapi` crate — the same C library the original
//! Windows app used — providing enumerate / open / read / write for the keypad.

use std::time::Duration;

use hidapi::{HidApi, HidDevice};

use crate::protocol::{
    self, commit_report, read_key_count_report, read_key_report, read_layer_report,
    write_key_report, KeyCount, KeyRecord, Mode, READ_LEN, RECORD_LEN, REPORT_ID,
};

const DEFAULT_TIMEOUT_MS: i32 = 1000;

/// An open connection to a keypad.
pub struct Device {
    api: HidApi,
    handle: HidDevice,
    pub info: DeviceInfo,
}

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub vendor_id: u16,
    pub product_id: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub path: Vec<u8>,
}

/// VIDs known to be used by this keypad family.
///
/// The original Windows app filters on `0x1189`, but at least one hardware
/// variant enumerates as `0x514C` with the same PID family. We match on the
/// PID family (see [`protocol::PIDS`]) regardless of VID, since `0x883x`/
/// `0x884x` is distinctive.
pub const KNOWN_VIDS: &[u16] = &[0x1189, 0x514C];

/// True if a device looks like one of our keypads: PID in the known family.
///
/// The PID family (`0x883x`/`0x884x`) is distinctive; VID varies across
/// variants (`0x1189` in the original app, `0x514C` on at least one unit),
/// so we match on PID alone. `KNOWN_VIDS` is kept only for display/filtering.
fn looks_like_keypad(_vid: u16, pid: u16) -> bool {
    protocol::PIDS.contains(&pid)
}

/// Errors from the device layer.
#[derive(Debug)]
pub enum DeviceError {
    NotFound,
    Hid(String),
    Io(String),
}

impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no keypad found (PID family 0x883x/0x884x)"),
            Self::Hid(s) => write!(f, "hidapi: {s}"),
            Self::Io(s) => write!(f, "io: {s}"),
        }
    }
}
impl std::error::Error for DeviceError {}

/// Ping a handle with the key-count query (used to pick the right interface
/// of a composite device without constructing a full `Device`).
fn ping(handle: &HidDevice) -> Result<KeyCount, DeviceError> {
    let r = read_key_count_report();
    handle
        .write(&r)
        .map_err(|e| DeviceError::Hid(e.to_string()))?;
    let mut buf = [0u8; READ_LEN];
    for _ in 0..5 {
        match handle.read_timeout(&mut buf, DEFAULT_TIMEOUT_MS) {
            Ok(0) => continue,
            Ok(n) => {
                if let Some(kc) = KeyCount::parse(&buf[..n]) {
                    return Ok(kc);
                }
            }
            Err(e) => return Err(DeviceError::Hid(e.to_string())),
        }
    }
    Err(DeviceError::Io("no key-count response".into()))
}

impl Device {
    /// Enumerate and open the keypad. A composite device may expose several
    /// HID interfaces with the same VID/PID (e.g. a keyboard interface plus the
    /// vendor config interface); we try each match and keep the one that opens
    /// *and* responds to the key-count ping. A handle that opens but does not
    /// respond (a keyboard interface seized by macOS) is closed and skipped.
    pub fn open() -> Result<Self, DeviceError> {
        let mut api = HidApi::new().map_err(|e| DeviceError::Hid(e.to_string()))?;
        api.refresh_devices()
            .map_err(|e| DeviceError::Hid(e.to_string()))?;

        let candidates: Vec<_> = api
            .device_list()
            .filter(|d| looks_like_keypad(d.vendor_id(), d.product_id()))
            .cloned()
            .collect();

        let mut last_err = DeviceError::NotFound;
        for dev_info in &candidates {
            let info = DeviceInfo {
                vendor_id: dev_info.vendor_id(),
                product_id: dev_info.product_id(),
                manufacturer: dev_info.manufacturer_string().map(String::from),
                product: dev_info.product_string().map(String::from),
                serial: dev_info.serial_number().map(String::from),
                path: dev_info.path().to_bytes().to_vec(),
            };
            let handle = match api.open_path(dev_info.path()) {
                Ok(h) => h,
                Err(e) => {
                    last_err = DeviceError::Hid(format!(
                        "open {:04X}:{:04X}: {e}",
                        info.vendor_id, info.product_id
                    ));
                    continue;
                }
            };
            // Keep this handle only if it answers the key-count query;
            // otherwise drop (close) it and try the next interface.
            if ping(&handle).is_ok() {
                return Ok(Self { api, handle, info });
            }
            last_err = DeviceError::Io("opened but no key-count response".into());
            // handle dropped here -> closed, freeing it for the next attempt
        }
        Err(last_err)
    }

    /// List all matching devices (for a future device-picker UI).
    pub fn list() -> Result<Vec<DeviceInfo>, DeviceError> {
        let mut api = HidApi::new().map_err(|e| DeviceError::Hid(e.to_string()))?;
        api.refresh_devices()
            .map_err(|e| DeviceError::Hid(e.to_string()))?;
        Ok(api
            .device_list()
            .filter(|d| looks_like_keypad(d.vendor_id(), d.product_id()))
            .map(|d| DeviceInfo {
                vendor_id: d.vendor_id(),
                product_id: d.product_id(),
                manufacturer: d.manufacturer_string().map(String::from),
                product: d.product_string().map(String::from),
                serial: d.serial_number().map(String::from),
                path: d.path().to_bytes().to_vec(),
            })
            .collect())
    }

    /// Send a 65-byte report (report ID as buf[0]).
    fn send(&self, buf: &[u8; protocol::REPORT_LEN]) -> Result<(), DeviceError> {
        debug_assert_eq!(buf[0], REPORT_ID);
        let n = self
            .handle
            .write(buf)
            .map_err(|e| DeviceError::Hid(e.to_string()))?;
        if n != buf.len() {
            return Err(DeviceError::Io(format!(
                "short write: {n}/{} bytes",
                buf.len()
            )));
        }
        Ok(())
    }

    /// Read one 64-byte response payload within the timeout.
    fn recv(&self, timeout_ms: i32) -> Result<[u8; READ_LEN], DeviceError> {
        let mut buf = [0u8; READ_LEN];
        let n = self
            .handle
            .read_timeout(&mut buf, timeout_ms)
            .map_err(|e| DeviceError::Hid(e.to_string()))?;
        if n == 0 {
            return Err(DeviceError::Io(format!(
                "read timed out after {timeout_ms} ms"
            )));
        }
        Ok(buf)
    }

    /// Public raw send (debug/probing): write a 65-byte report as-is.
    pub fn debug_send(&self, buf: &[u8; protocol::REPORT_LEN]) -> Result<(), DeviceError> {
        self.send(buf)
    }

    /// Public raw read (debug/probing): one 64-byte response or `None` on
    /// timeout. Unlike [`recv`], a timeout is returned as `Ok(None)` so callers
    /// can drain until quiet.
    pub fn debug_recv(&self, timeout_ms: i32) -> Result<Option<[u8; READ_LEN]>, DeviceError> {
        match self.recv(timeout_ms) {
            Ok(buf) => Ok(Some(buf)),
            Err(DeviceError::Io(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Query the device for number of keys and layers (`0xFB`).
    pub fn read_key_count(&self) -> Result<KeyCount, DeviceError> {
        let resp = self.read_key_count_raw()?;
        KeyCount::parse(&resp).ok_or_else(|| {
            DeviceError::Io(format!(
                "unparseable key-count response: {}",
                pretty_hex(&resp)
            ))
        })
    }

    /// Raw 64-byte response to the `0xFB` key-count query (for debugging
    /// byte offsets across hidapi backends).
    pub fn read_key_count_raw(&self) -> Result<[u8; READ_LEN], DeviceError> {
        self.send(&read_key_count_report())?;
        for _ in 0..5 {
            match self.recv(DEFAULT_TIMEOUT_MS) {
                Ok(resp) => return Ok(resp),
                Err(DeviceError::Io(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(DeviceError::Io("no key-count response after retries".into()))
    }

    /// Read one key's 50-byte record (1-indexed key, 0-indexed layer).
    ///
    /// Implemented via [`read_layer`]: the device streams all keys for a layer
    /// in response to one request, so we read the whole layer and pick the
    /// packet whose key index (response byte[2]) matches `key`.
    pub fn read_key(&self, key: u8, layer: u8, key_count: u8) -> Result<KeyRecord, DeviceError> {
        let row = self.read_layer(key_count, protocol::ROTARY_COUNT as u8, layer)?;
        row.get(key as usize)
            .cloned()
            .ok_or_else(|| DeviceError::Io(format!("key {key} not in layer {layer} stream")))
    }

    /// Read a whole layer: send `[0x03,0xFA,key_count,rotary_count,layer]` and
    /// collect the streamed response packets. Returns a vector of
    /// [`protocol::LAYER_SLOTS`] entries indexed by 1-indexed slot (slot 0 is a
    /// reserved empty header; slots 13..=15 are dead and stay empty).
    ///
    /// The device streams two phases: `1..=key_count` (the keys), then
    /// `16..=24` (the rotary encoders, when `rotary_count > 0`). Each packet is
    /// `[reportID, cmd_echo, keyIndex, layer, MODE, DELAY, …, SEQ_LEN, slots…]`;
    /// the 50-byte record is `packet[1..51]`, so the 1-indexed slot is at raw
    /// response byte[2] and lands at `record[1]`.
    pub fn read_layer(&self, key_count: u8, rotary_count: u8, layer: u8) -> Result<Vec<KeyRecord>, DeviceError> {
        // device layers are 1-indexed in the request (byte[4]); host layers are 0-indexed
        let report = read_layer_report(key_count, rotary_count, layer + 1);
        self.send(&report)?;
        // Drain the streamed response into a buffer with a tight read loop
        // (no per-packet parsing) — the device emits all packets in a burst and
        // the macOS HID queue can drop reports if reads fall behind.
        let total = key_count as usize + rotary_count as usize * 3;
        let mut packets: Vec<[u8; READ_LEN]> = Vec::with_capacity(total);
        let mut timeout = 500;
        for _ in 0..(total + 4) {
            match self.recv(timeout) {
                Ok(resp) => {
                    packets.push(resp);
                    timeout = 120; // tighten after the first packet to detect end-of-stream
                }
                Err(DeviceError::Io(_)) => break,
                Err(e) => return Err(e),
            }
        }
        if packets.is_empty() {
            return Err(DeviceError::Io(format!("no response reading layer {layer}")));
        }
        // Now parse each packet. After stripping the report ID the payload is
        // `[cmd_echo, keyIndex, layer, MODE, …]`, so the 1-indexed slot is at
        // payload byte[1] (= raw response byte[2]). Rotary packets carry slot
        // indices 16..=24 here, so the row must be LAYER_SLOTS wide.
        let mut row = vec![KeyRecord::empty(); protocol::LAYER_SLOTS];
        for resp in &packets {
            let record = parse_record_packet(resp)?;
            let key_index = protocol::strip_report_id(resp)[1] as usize;
            if key_index < row.len() {
                row[key_index] = record;
            }
        }
        Ok(row)
    }

    /// Raw 64-byte response to a `0xFA` read-key query (for debugging the
    /// record's start offset within the response across hidapi backends).
    pub fn read_key_raw(&self, key: u8, layer: u8, seq: u8) -> Result<[u8; READ_LEN], DeviceError> {
        let report = read_key_report(key, layer, seq);
        self.send(&report)?;
        for _ in 0..5 {
            match self.recv(DEFAULT_TIMEOUT_MS) {
                Ok(resp) => return Ok(resp),
                Err(DeviceError::Io(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(DeviceError::Io("no read-key response after retries".into()))
    }

    /// Send an arbitrary `[0x03, 0xFA, b, c, counter, 0…]` read-key report and
    /// return the raw 64-byte response. Used to pin down the request/response
    /// byte mapping against the real device.
    pub fn read_key_raw_args(
        &self,
        b: u8,
        c: u8,
        counter: u8,
    ) -> Result<[u8; READ_LEN], DeviceError> {
        let mut report = [0u8; protocol::REPORT_LEN];
        report[0] = REPORT_ID;
        report[1] = 0xFA;
        report[2] = b;
        report[3] = c;
        report[4] = counter;
        self.send(&report)?;
        for _ in 0..5 {
            match self.recv(DEFAULT_TIMEOUT_MS) {
                Ok(resp) => return Ok(resp),
                Err(DeviceError::Io(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(DeviceError::Io("no read-key response after retries".into()))
    }

    /// Send an arbitrary `[0x03, 0xFA, b, c, counter]` read-key report and
    /// read back EVERY 64-byte packet the device emits until it goes quiet
    /// (short timeout). Returns all packets in order — used to tell whether
    /// the device streams a multi-packet response per request.
    pub fn read_key_stream(
        &self,
        b: u8,
        c: u8,
        counter: u8,
    ) -> Result<Vec<[u8; READ_LEN]>, DeviceError> {
        let mut report = [0u8; protocol::REPORT_LEN];
        report[0] = REPORT_ID;
        report[1] = 0xFA;
        report[2] = b;
        report[3] = c;
        report[4] = counter;
        self.send(&report)?;
        let mut packets = Vec::new();
        // first packet: generous timeout; subsequent: short to detect end-of-stream
        let mut timeout = 500;
        for _ in 0..40 {
            match self.recv(timeout) {
                Ok(resp) => {
                    packets.push(resp);
                    timeout = 80; // tighten after the first packet
                }
                Err(DeviceError::Io(_)) => break,
                Err(e) => return Err(e),
            }
        }
        if packets.is_empty() {
            return Err(DeviceError::Io("no read-key response".into()));
        }
        Ok(packets)
    }

    /// Read every key across all layers (one streamed request per layer).
    pub fn read_all(&self, keys: u8, layers: u8) -> Result<Vec<Vec<KeyRecord>>, DeviceError> {
        let mut out = Vec::with_capacity(layers as usize);
        for layer in 0..layers {
            out.push(self.read_layer(keys, protocol::ROTARY_COUNT as u8, layer)?);
        }
        Ok(out)
    }

    /// Write one key's record (1-indexed key, 0-indexed layer) and send the
    /// commit sentinel. The `0x514C` write marker `0xFE`, key index and layer
    /// are encoded into the report by [`write_key_report`].
    pub fn write_key(&self, key: u8, layer: u8, record: &KeyRecord) -> Result<(), DeviceError> {
        self.send(&write_key_report(key, layer, record))?;
        std::thread::sleep(Duration::from_millis(2));
        self.send(&commit_report())
    }

    /// Write a single record for `key`/`layer` then send the commit sentinel.
    pub fn write_and_commit(&self, key: u8, layer: u8, record: &KeyRecord) -> Result<(), DeviceError> {
        self.send(&write_key_report(key, layer, record))?;
        std::thread::sleep(Duration::from_millis(2));
        self.send(&commit_report())
    }

    /// Stream a whole layer's records (one report per key, 1-indexed) then
    /// send the commit sentinel. Each record carries its own key index; the
    /// layer is the same for all. Use this to write a full layer at once.
    pub fn write_layer_and_commit(
        &self,
        layer: u8,
        row: &[KeyRecord],
        key_count: u8,
    ) -> Result<(), DeviceError> {
        for key in 1..=key_count {
            let Some(rec) = row.get(key as usize) else { continue };
            self.send(&write_key_report(key, layer, rec))?;
            std::thread::sleep(Duration::from_millis(2));
        }
        self.send(&commit_report())
    }

    /// Write an LED layer-indicator record + commit. Mirrors the Windows app's
    /// Dialog3 exact on-wire format (capture-confirmed: `03 fe b0 <layer> 08 ... 01 00 <colour>`
    /// + commit `03 fd fe ff`). Layer is 1-indexed device layer (1..3). Colour index
    /// is 1..7 (red..purple); low nibble `0x04` = all-keys bitmask (from the
    /// app's sweep in led2.pcapng).
    pub fn write_led_and_commit(&self, layer: u8, colour_idx: u8) -> Result<(), DeviceError> {
        self.send(&protocol::led_report(layer, colour_idx))?;
        std::thread::sleep(Duration::from_millis(2));
        self.send(&protocol::commit_report())
    }

    /// Send the `0xFC` set-version report `[0x03, 0xFC, 0xFC, 0x02, 0x00]`.
    ///
    /// The original app sends this after the `0xFB` key-count query so firmware
    /// knows which key matrix to expect (FINDINGS §5.6/§191). It may also be a
    /// prerequisite for accepting config writes. The report is the fixed form
    /// from FINDINGS §5.4 (the version index selects a host-side setup routine,
    /// not a byte in this report).
    pub fn set_version(&self) -> Result<(), DeviceError> {
        let mut buf = [0u8; protocol::REPORT_LEN];
        buf[0] = REPORT_ID;
        buf[1] = protocol::CMD_SET_VERSION;
        buf[2] = protocol::CMD_SET_VERSION;
        buf[3] = 0x02;
        // leave byte[4]=0 (version index slot, unused in the fixed form)
        self.send(&buf)
    }

    /// Stream all modified records and send the commit sentinel.
    pub fn write_config(
        &self,
        layers: &[Vec<KeyRecord>],
        dirty: &[(u8, u8)], // (key, layer) pairs that changed
    ) -> Result<(), DeviceError> {
        for &(key, layer) in dirty {
            let Some(row) = layers.get(layer as usize) else {
                continue;
            };
            let Some(rec) = row.get(key as usize) else {
                continue;
            };
            self.write_key(key, layer, rec)?;
            // small pacing gap, mirroring the original's per-key round-trip
            std::thread::sleep(Duration::from_millis(2));
        }
        self.send(&commit_report())?;
        Ok(())
    }

    /// Convenience: a no-op ping used by the CLI to confirm the link.
    pub fn ping(&self) -> Result<KeyCount, DeviceError> {
        self.read_key_count()
    }

    // keep `api` alive (the handle borrows the hidapi context internally on some backends)
    #[allow(dead_code)]
    fn _keep_api(&self) -> &HidApi {
        &self.api
    }
}

/// Parse one streamed read-key packet into a 50-byte [`KeyRecord`].
///
/// Packet layout: `[reportID, cmd_echo, keyIndex, layer, MODE, DELAY, …,
/// SEQ_LEN, slots…]`. The record is `packet[1..51]`; the command echo at
/// `packet[1]` occupies `record[0]` (the host-managed marker), so we
/// normalise `record[0]` to `0` (or `0xFE` for LED mode) rather than trust it.
fn parse_record_packet(resp: &[u8]) -> Result<KeyRecord, DeviceError> {
    if resp.len() < 1 + RECORD_LEN {
        return Err(DeviceError::Io(format!(
            "short read-key packet: {} bytes",
            resp.len()
        )));
    }
    let mut record = [0u8; RECORD_LEN];
    record.copy_from_slice(&resp[1..1 + RECORD_LEN]);
    // record[0] is the host-managed marker; the device puts the cmd echo there.
    let mode = record[protocol::off::MODE];
    record[0] = if mode == Mode::Led as u8 { 0xFE } else { 0x00 };
    Ok(KeyRecord::from_bytes(&record))
}

/// Hex-dump a fixed buffer, 16 bytes per row with offset prefixes.
pub fn pretty_hex(b: &[u8]) -> String {
    let mut s = String::new();
    for (i, chunk) in b.chunks(16).enumerate() {
        s.push_str(&format!("{:04x}: ", i * 16));
        for x in chunk {
            s.push_str(&format!("{x:02x} "));
        }
        s.push('\n');
    }
    s
}

/// Human-readable label for a parsed key record, used by both CLI and GUI.
pub fn label_for(rec: &KeyRecord) -> String {
    match rec.mode_enum() {
        Some(Mode::Basic) => {
            let slots = rec.slots();
            let mut parts = Vec::new();
            for i in 0..rec.seq_len as usize {
                let s = slots[i];
                let mods = crate::hid_codes::mod_name(s.b0);
                let key = crate::hid_codes::key_name(s.b1);
                // mod_name already ends with "+" (e.g. "Alt+"), so just concatenate.
                if mods.is_empty() {
                    parts.push(key.to_string());
                } else {
                    parts.push(format!("{mods}{key}"));
                }
            }
            if parts.is_empty() {
                "(empty)".into()
            } else {
                parts.join(" ")
            }
        }
        Some(Mode::Multimedia) => {
            let s = rec.slots()[0];
            format!("Media:{:#04X}/{:#04X}", s.b0, s.b1)
        }
        Some(Mode::Mouse) => {
            let btn = rec.raw[protocol::off::MOUSE_BTN];
            format!("Mouse btn={btn:#04X}")
        }
        Some(Mode::Led) => format!("LED mask={:#04X}", rec.raw[protocol::off::LED_BITMASK]),
        Some(Mode::Macro) => "Macro (opaque)".into(),
        None => format!("Mode={} (unknown)", rec.mode),
    }
}
