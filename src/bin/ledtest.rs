//! Empirical LED probe v6 — colour-group sweep for the layer indicator.
//!
//! Confirmed: per-key LEDs are LAYER INDICATORS (brief flash on switch to a
//! layer). The LED record (mode 5, slot 0 per layer, record[1]=0xB0) controls
//! it: bitmask low nibble = which key flashes, high nibble = colour group.
//! Writing a record changes the indicator (proved: default layer2 key1-green
//! became nothing when overwritten). Group 5 = no visible colour.
//!
//! This sweeps colour groups 0..15 for a fixed layer+key, holding each, so the
//! user can cycle to that layer per step and note the flash colour → maps
//! group→colour. No auto-restore between steps (each overwrites the last);
//! clears at the end.
//!
//! Usage:  ledtest <layer> <keybit> [hold_secs]
//!   layer   1..3
//!   keybit  0x01=key1, 0x02=key2, 0x04=key3 … (low nibble)
//!   hold    default 12
//!
//! Example: ledtest 2 0x01 12   → layer 2, key 1, sweep colour groups 0..15.

use std::io::Write;

use keytool::{
    device::Device,
    protocol::{self, off, REPORT_LEN},
};

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

fn led_report(layer: u8, bitmask: u8) -> [u8; REPORT_LEN] {
    let mut buf = [0u8; REPORT_LEN];
    buf[0] = 0x03;
    let rec = &mut buf[1..1 + 50];
    rec[0x00] = 0xFE;
    rec[0x01] = 0xB0;
    rec[0x02] = layer;
    rec[0x03] = 5;
    rec[off::SEQ_LEN] = 1;
    rec[off::LED_BITMASK] = bitmask; // high nibble = group, low = keybit
    buf
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `ledtest clear <layer>` → write bitmask 0 (no indicator) for that layer.
    if args.get(1).map(|s| s.as_str()) == Some("clear") {
        let layer: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(2);
        let d = match Device::open() {
            Ok(d) => d,
            Err(e) => {
                eprintln!("error opening device: {e}");
                std::process::exit(1);
            }
        };
        let rep = led_report(layer, 0x00);
        let _ = d.debug_send(&rep);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _ = d.debug_send(&protocol::commit_report());
        println!("cleared layer {layer} indicator (bitmask=0).");
        return;
    }
    let layer: u8 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2);
    let keybit: u8 = args
        .get(2)
        .and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x01);
    let hold_secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(12);
    let hold = std::time::Duration::from_secs(hold_secs);

    let d = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening device: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "opened. sweep colour groups 0..15 for layer {layer} keybit 0x{keybit:02X}. hold {hold_secs}s/step."
    );
    println!("→ after EACH step, cycle to layer {layer} and note that key's flash colour (or none).");

    let mut out = std::io::stdout();
    let commit = protocol::commit_report();
    for group in 0u8..=15 {
        let bitmask = (group << 4) | (keybit & 0x0f);
        let rep = led_report(layer, bitmask);
        println!(
            "── group {group:2}  bitmask=0x{bitmask:02X}  raw={}",
            hex(&rep[1..51])
        );
        if let Err(e) = d.debug_send(&rep) {
            eprintln!("  send failed: {e} — aborting");
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        if let Err(e) = d.debug_send(&commit) {
            eprintln!("  commit failed: {e} — aborting");
            break;
        }
        println!("OBSERVE group {group}: cycle to layer {layer}, note flash colour. ({hold_secs}s)");
        let _ = out.flush();
        std::thread::sleep(hold);
        println!();
    }

    // clear
    let off_rep = led_report(layer, 0x00);
    println!("clearing layer {layer} indicator (bitmask=0)…");
    let _ = d.debug_send(&off_rep);
    std::thread::sleep(std::time::Duration::from_millis(30));
    let _ = d.debug_send(&commit);
    println!("done.");
}
