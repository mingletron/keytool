//! Save/load the full keypad config to a JSON file (gui feature only).
//!
//! Keeps [`crate::protocol`] dependency-free by mirroring each [`KeyRecord`]
//! into a serde-friendly struct here. The on-disk form is human-readable JSON
//! (one record per slot, including a `label` for quick scanning); the `label`
//! is regenerated on save and ignored on load.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::device::label_for;
use crate::protocol::{KeyRecord, LAYER_COUNT, LAYER_SLOTS, RECORD_LEN};

/// File format version. Bump on incompatible schema changes.
const VERSION: u32 = 1;

/// How many slots per layer we serialise (slot 0 = reserved header, then keys
/// 1..=12, dead 13..=15, rotary 16..=24). Matches [`protocol::LAYER_SLOTS`] = 25.
const SLOTS_PER_LAYER: usize = LAYER_SLOTS;

#[derive(Serialize, Deserialize)]
struct ConfigFile {
    version: u32,
    keys: u8,
    layers: u8,
    layers_data: Vec<LayerDump>,
}

#[derive(Serialize, Deserialize)]
struct LayerDump {
    records: Vec<RecordDump>,
}

#[derive(Serialize, Deserialize)]
struct RecordDump {
    key: usize,
    mode: u8,
    delay: u8,
    seq_len: u8,
    raw: Vec<u8>,
    label: String,
}

/// Serialise `layers` (`layers[l][k]`, 1-indexed key `k`, slot 0 header) to
/// `path` as pretty JSON. `keys`/`layers` counts are stored for reference.
pub fn save(layers: &[Vec<KeyRecord>], path: &Path) -> Result<(), String> {
    let layers_data: Vec<LayerDump> = layers
        .iter()
        .map(|row| {
            let records: Vec<RecordDump> = (0..SLOTS_PER_LAYER)
                .map(|k| {
                    let rec = row.get(k).cloned().unwrap_or_else(KeyRecord::empty);
                    let label = label_for(&rec);
                    RecordDump {
                        key: k,
                        mode: rec.mode,
                        delay: rec.delay,
                        seq_len: rec.seq_len,
                        raw: rec.to_bytes().to_vec(),
                        label,
                    }
                })
                .collect();
            LayerDump { records }
        })
        .collect();
    let file = ConfigFile {
        version: VERSION,
        keys: SLOTS_PER_LAYER as u8 - 1, // excluding the header slot
        layers: layers.len() as u8,
        layers_data,
    };
    let json = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

/// Load a config file written by [`save`]. Returns a `Vec<Vec<KeyRecord>>`
/// shaped like the GUI's `empty_layers()` (3 layers × 16 slots). Missing or
/// short rows/slots fall back to [`KeyRecord::empty`], so a hand-edited or
/// truncated file can't panic. Unknown `version` → [`Err`].
pub fn load(path: &Path) -> Result<Vec<Vec<KeyRecord>>, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let file: ConfigFile = serde_json::from_slice(&data).map_err(|e| e.to_string())?;
    if file.version != VERSION {
        return Err(format!(
            "unsupported config file version {} (expected {})",
            file.version, VERSION
        ));
    }

    let mut out: Vec<Vec<KeyRecord>> = (0..LAYER_COUNT)
        .map(|_| vec![KeyRecord::empty(); SLOTS_PER_LAYER])
        .collect();
    for (l, layer_dump) in file.layers_data.iter().enumerate().take(LAYER_COUNT) {
        for rd in &layer_dump.records {
            let k = rd.key;
            if k >= SLOTS_PER_LAYER {
                continue;
            }
            let rec = if rd.raw.len() == RECORD_LEN {
                let mut arr = [0u8; RECORD_LEN];
                arr.copy_from_slice(&rd.raw);
                let mut r = KeyRecord::from_bytes(&arr);
                // Honour the mirrored fields in case the file was hand-edited.
                r.mode = rd.mode;
                r.delay = rd.delay;
                r.seq_len = rd.seq_len;
                r
            } else {
                // Malformed raw — keep mode/delay/seq_len, zero the payload.
                let mut r = KeyRecord::empty();
                r.mode = rd.mode;
                r.delay = rd.delay;
                r.seq_len = rd.seq_len;
                r
            };
            out[l][k] = rec;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{off, Slot};

    fn sample_layers() -> Vec<Vec<KeyRecord>> {
        let mut l0 = vec![KeyRecord::empty(); SLOTS_PER_LAYER];
        let mut rec = KeyRecord::empty();
        rec.mode = 1;
        rec.seq_len = 1;
        rec.set_slot(0, Slot { b0: 0x05, b1: 0x06 }); // Ctrl+c
        l0[1] = rec;
        vec![l0, vec![KeyRecord::empty(); SLOTS_PER_LAYER], vec![KeyRecord::empty(); SLOTS_PER_LAYER]]
    }

    #[test]
    fn save_load_round_trips() {
        let dir = std::env::temp_dir();
        let path = dir.join("keytool_cfg_test.json");
        let original = sample_layers();
        save(&original, &path).expect("save");
        let loaded = load(&path).expect("load");
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.len(), LAYER_COUNT);
        for l in 0..LAYER_COUNT {
            for k in 0..SLOTS_PER_LAYER {
                assert_eq!(
                    loaded[l][k].to_bytes(),
                    original[l][k].to_bytes(),
                    "layer {l} key {k} mismatch"
                );
            }
        }
        // The Ctrl+c slot survived intact.
        assert_eq!(loaded[0][1].raw[off::SEQ_START], 0x05);
        assert_eq!(loaded[0][1].raw[off::SEQ_START + 1], 0x06);
    }

    #[test]
    fn rejects_unknown_version() {
        let dir = std::env::temp_dir();
        let path = dir.join("keytool_cfg_bad.json");
        std::fs::write(&path, r#"{"version":999,"keys":15,"layers":3,"layers_data":[]}"#)
            .unwrap();
        assert!(load(&path).is_err());
        let _ = std::fs::remove_file(&path);
    }
}
