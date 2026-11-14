//! Read-only probe: look for a way to read the layer-indicator LED colour back.
//!
//! The LED colour is written to each layer's slot-0 header
//! (`[FE, B0, layer, 08, …, seq=1, 00, <colour>]`), but the normal read stream
//! `[FA, key_count, rotary_count, layer]` only returns slots 1..=key_count and
//! 16..=24. This sends ONLY read requests (`0xFB` and `0xFA` variants) — never a
//! write — and dumps every packet, flagging bytes that look like an LED colour
//! (`0x1_`..`0x7_` with the `4` key-bit low nibble, or the `B0`/`08` markers).
//!
//! Result (2026-11-14, 0x514C unit, layer 1 set to orange): NO way found.
//! - `FB` returns only `03 fb 05`.
//! - `FA 12 3 L` streams exactly slots 1..=12 then 16..=24; no slot-0 packet,
//!   nothing after the record bytes. (The flagged "colour-like" bytes are just
//!   keycodes, e.g. 0x34 = `'`.)
//! - `FA 0 0 L`, and `FA 0 0 1 B0`, return nothing; `FA 1 0 1` returns slot 1 only.
//!
//! The phase-1 loop starts at slot 1, and the Windows app's command set
//! (FA/FB/FC/FD/FE) has no LED read either, so the colour is write-only.
//! Not tried (could read out of bounds or hit unknown firmware paths on a unit
//! that has crashed on bad input before): layer 0, huge key counts, other opcodes.
//!
//! Usage: ledread

use std::time::Duration;

use keytool::{device::Device, protocol::REPORT_LEN};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

/// Send `payload` (after the 0x03 report id) and collect every packet until quiet.
fn ask(d: &Device, payload: &[u8]) -> Vec<[u8; 64]> {
    let mut report = [0u8; REPORT_LEN];
    report[0] = 0x03;
    report[1..1 + payload.len()].copy_from_slice(payload);
    if let Err(e) = d.debug_send(&report) {
        println!("    send error: {e}");
        return vec![];
    }
    let mut out = Vec::new();
    let mut timeout = 500;
    for _ in 0..48 {
        match d.debug_recv(timeout) {
            Ok(Some(p)) => {
                out.push(p);
                timeout = 120;
            }
            Ok(None) => break,
            Err(e) => {
                println!("    recv error: {e}");
                break;
            }
        }
    }
    out
}

fn show(label: &str, packets: &[[u8; 64]]) {
    println!("== {label}: {} packet(s)", packets.len());
    for p in packets {
        let flag = if p.contains(&0xB0) || p.iter().any(|&b| (0x14..=0x74).contains(&b) && b & 0x0F == 0x04) {
            "  <-- B0 / colour-like byte"
        } else {
            ""
        };
        // Trim trailing zeros for readability.
        let end = p.iter().rposition(|&b| b != 0).map(|i| i + 1).unwrap_or(1);
        println!("    {}{flag}", hex(&p[..end]));
    }
}

fn main() {
    let d = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening device: {e}");
            std::process::exit(1);
        }
    };
    let pause = || std::thread::sleep(Duration::from_millis(150));

    show("FB key-count query", &ask(&d, &[0xFB, 0xFB, 0xFB]));
    pause();

    for layer in 1u8..=3 {
        show(&format!("FA 12 3 layer {layer} (normal read)"), &ask(&d, &[0xFA, 12, 3, layer]));
        pause();
    }
    for layer in 1u8..=3 {
        show(&format!("FA 0 0 layer {layer} (no keys, no knobs)"), &ask(&d, &[0xFA, 0, 0, layer]));
        pause();
    }
    show("FA 1 0 layer 1 (one key)", &ask(&d, &[0xFA, 1, 0, 1]));
    pause();
    show("FA 0 0 1 B0 (LED marker in byte 5)", &ask(&d, &[0xFA, 0, 0, 1, 0xB0]));
}
