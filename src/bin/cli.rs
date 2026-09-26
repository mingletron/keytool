//! Minimal CLI for the macro keypad — proves the HID protocol against hardware.
//!
//! Usage:
//!   keytool list             enumerate matching devices
//!   keytool info             open the device and read its key/layer count
//!   keytool read             read every key and dump a table
//!   keytool label            read every key and print human-readable labels
//!   keytool writetest [key] [layer]   restorable write-modify-read-restore cycle
//!   keytool smoke [key] [layer]       end-to-end test of the GUI's device stack
//!
//! Run with:  cargo run --release --bin keytool -- <command>

use keytool::{device::Device, protocol};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("info");

    match cmd {
        "list" => list(),
        "info" => info(),
        "read" => read_all(true),
        "label" => read_all(false),
        "raw" => raw(),
        "rawkey" => raw_key(),
        "stream" => stream_key(),
        "readlayer" => read_layer_cmd(),
        "dump" => dump_cmd(),
        "writetest" => write_test(),
        "writeprobe" => write_probe(),
        "brutewrite" => brute_write(),
        "setkey" => set_key(),
        "setmedia" => set_media(),
        "set-led" => set_led(),
        "smoke" => smoke_test(),
        other => {
            eprintln!("unknown command: {other}");
            eprintln!(
                "commands: list | info | read | label | raw | rawkey | stream | readlayer | dump | writetest | writeprobe | brutewrite | setkey | setmedia | set-led | smoke"
            );
            std::process::exit(2);
        }
    }
}

fn list() {
    match Device::list() {
        Ok(devs) if devs.is_empty() => println!("no keypads found (VID 0x{:04X})", protocol::VID),
        Ok(devs) => {
            for d in devs {
                println!(
                    "VID 0x{:04X} PID 0x{:04X}  {:?}  {:?}",
                    d.vendor_id, d.product_id, d.product, d.manufacturer
                );
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn open_device() -> Device {
    match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

fn info() {
    let d = open_device();
    println!(
        "opened VID 0x{:04X} PID 0x{:04X}",
        d.info.vendor_id, d.info.product_id
    );
    if let Some(p) = &d.info.product {
        println!("product: {p}");
    }
    if let Some(m) = &d.info.manufacturer {
        println!("manufacturer: {m}");
    }
    match d.ping() {
        Ok(kc) => {
            println!("keys: {}", kc.keys);
            println!("layers: {}", kc.layers);
        }
        Err(e) => {
            eprintln!("read_key_count failed: {e}");
            std::process::exit(1);
        }
    }
}

fn read_all(hex: bool) {
    let d = open_device();
    let kc = match d.ping() {
        Ok(kc) => kc,
        Err(e) => {
            eprintln!("read_key_count failed: {e}");
            std::process::exit(1);
        }
    };
    println!("device: {} keys, {} layers", kc.keys, kc.layers);

    match d.read_all(kc.keys, kc.layers) {
        Ok(layers) => {
            for (li, row) in layers.iter().enumerate() {
                println!("\n== layer {li} ==");
                for (ki, rec) in row.iter().enumerate() {
                    if ki == 0 {
                        continue; // reserved header slot
                    }
                    if hex {
                        println!(
                            "  key {ki:2}: mode={} delay={:3} len={:2} {}",
                            rec.mode,
                            rec.delay,
                            rec.seq_len,
                            hex::encode(rec.to_bytes())
                        );
                    } else {
                        println!("  key {ki:2}: {}", keytool::device::label_for(rec));
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("read_all failed: {e}");
            std::process::exit(1);
        }
    }
}

fn raw() {
    let d = open_device();
    match d.read_key_count_raw() {
        Ok(resp) => {
            println!("key-count raw response (64 bytes):");
            print!("{}", keytool::device::pretty_hex(&resp));
            println!("\nbyte[0]={:#04x} byte[1]={:#04x} byte[2]={:#04x} byte[3]={:#04x}",
                resp[0], resp[1], resp[2], resp[3]);
        }
        Err(e) => {
            eprintln!("read_key_count_raw failed: {e}");
            std::process::exit(1);
        }
    }
}

fn raw_key() {
    let d = open_device();
    let args: Vec<String> = std::env::args().collect();
    // Probe form: `rawkey b c counter` sends [03, FA, b, c, counter].
    // Per FINDINGS §5.4: b=keyCount, c=layer, counter=keyIndex(1..b).
    let b: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(12);
    let c: u8 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let counter: u8 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    match d.read_key_raw_args(b, c, counter) {
        Ok(resp) => {
            println!("read_key(b={b}, c={c}, counter={counter}) raw response (64 bytes):");
            print!("{}", keytool::device::pretty_hex(&resp));
            let p = keytool::protocol::strip_report_id(&resp);
            println!("\nstripped payload ({}) — first 20 bytes:", p.len());
            let mut s = String::new();
            for (i, x) in p.iter().take(20).enumerate() {
                s.push_str(&format!("[{i}]={x:02x} "));
            }
            println!("{s}");
        }
        Err(e) => {
            eprintln!("read_key_raw failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Low-level streaming-read probe: `stream <keyCount> <rotaryCount> <layer>`.
/// Sends `[03 FA keyCount rotaryCount layer]` raw and drains every packet.
/// NOTE `layer` here is the **1-indexed device layer** (the raw `byte[4]`),
/// unlike `readlayer`/`setkey`/`setmedia`/`dump` which take a 0-indexed layer
/// and add `+1` internally. So `stream 12 3 1` reads host layer 0 (== `readlayer 0`),
/// `stream 12 3 2` reads host layer 1, etc. `stream 12 3 0` hits device layer 0
/// (invalid) and times out.
fn stream_key() {
    let d = open_device();
    let args: Vec<String> = std::env::args().collect();
    let b: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(12);
    let c: u8 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let counter: u8 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    match d.read_key_stream(b, c, counter) {
        Ok(packets) => {
            println!("stream(b={b}, c={c}, counter={counter}) → {} packets:", packets.len());
            for (i, p) in packets.iter().enumerate() {
                println!("--- packet {i} ---");
                print!("{}", keytool::device::pretty_hex(p));
            }
        }
        Err(e) => {
            eprintln!("stream failed: {e}");
            std::process::exit(1);
        }
    }
}

fn read_layer_cmd() {
    let d = open_device();
    let layer: u8 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    match d.read_layer(12, 3, layer) {
        Ok(row) => {
            let live = row.iter().skip(1).filter(|r| r.mode != 0).count();
            println!("layer {layer}: {} non-empty keys", live);
            for (ki, rec) in row.iter().enumerate() {
                if ki == 0 {
                    continue;
                }
                println!("  key {ki:2}: {}", keytool::device::label_for(rec));
            }
        }
        Err(e) => {
            eprintln!("read_layer failed: {e}");
            std::process::exit(1);
        }
    }
}

/// Force-read 12 keys × 3 layers and dump each record as hex + label, regardless
/// of what `0xFB` reports (the mode switch can make the device report 2/0 while
/// the full config is still stored). Used as a backup snapshot.
fn dump_cmd() {
    let d = open_device();
    let keys: u8 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(12);
    let layers: u8 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(3);
    for layer in 0..layers {
        println!("== device layer {layer} (readlayer {layer}) ==");
        match d.read_layer(keys, 3, layer) {
            Ok(row) => {
                for (ki, rec) in row.iter().enumerate() {
                    if ki == 0 {
                        continue;
                    }
                    println!(
                        "  key {ki:2}: {} | {}",
                        keytool::device::label_for(rec),
                        hex::encode(rec.to_bytes())
                    );
                }
            }
            Err(e) => eprintln!("  read_layer {layer} failed: {e}"),
        }
    }
}

/// Restorable write-modify-read-restore round-trip on one key.
///
/// Proves the write path (`0xFE` marker + `0xFD` commit, validated on the
/// `0x514C` unit): read the key, change slot 0 to 'z' (0x1D — absent from the
/// config), write+commit, read back to verify the change, then restore the
/// original and verify it. `key` is 1-indexed; `layer` is 0-indexed (device
/// layer = layer+1). A full backup is at `config-backup.txt`.
fn write_test() {
    use keytool::protocol::{off, Slot};

    let d = open_device();
    let key: u8 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let layer: u8 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    println!("round-trip test: key {key}, device layer {}", layer + 1);

    // 1. Read the current record (the original we must restore). read_layer
    //    forces 12 keys regardless of the 0xFB report (the mode switch can make
    //    it report 2/0 while the config persists).
    let row = d.read_layer(12, 3, layer).expect("read layer");
    let original = row[key as usize].clone();
    println!(
        "ORIGINAL  key {key} layer {layer}: {} | {}",
        keytool::device::label_for(&original),
        hex::encode(original.to_bytes())
    );

    // 2. Build a modified copy: basic mode, single slot = 'z' (0x1D).
    let mut modified = original.clone();
    modified.mode = 1;
    modified.seq_len = 1;
    modified.set_slot(0, Slot { b0: 0x00, b1: 0x1D });
    println!(
        "MODIFIED  key {key} layer {layer}: {} | {}",
        keytool::device::label_for(&modified),
        hex::encode(modified.to_bytes())
    );

    // 3. Write + commit (0xFE marker + key/layer addressing encoded by write_key_report).
    d.write_and_commit(key, layer, &modified).expect("write");
    println!("WRITE: sent (0xFE record + 0xFD commit)");
    std::thread::sleep(std::time::Duration::from_millis(120));

    // 4. Read back and verify the change took effect at the targeted key.
    let row2 = d.read_layer(12, 3, layer).expect("re-read layer");
    let after = &row2[key as usize];
    println!(
        "AFTER     key {key} layer {layer}: {} | {}",
        keytool::device::label_for(after),
        hex::encode(after.to_bytes())
    );
    let write_worked = after.mode == 1
        && after.seq_len == 1
        && after.raw[off::SEQ_START + 1] == 0x1D;
    println!("WRITE {}!", if write_worked { "VERIFIED" } else { "did NOT take effect" });

    // 5. Restore the original, regardless of outcome.
    println!("RESTORING original…");
    d.write_and_commit(key, layer, &original).expect("restore write");
    std::thread::sleep(std::time::Duration::from_millis(120));

    // 6. Verify restore.
    let row3 = d.read_layer(12, 3, layer).expect("re-read after restore");
    let restored = &row3[key as usize];
    println!(
        "RESTORED  key {key} layer {layer}: {} | {}",
        keytool::device::label_for(restored),
        hex::encode(restored.to_bytes())
    );
    let restore_ok = restored.to_bytes() == original.to_bytes();
    println!(
        "RESTORE {}!",
        if restore_ok {
            "VERIFIED"
        } else {
            "MISMATCH — original differs, check manually"
        }
    );
    let _ = write_worked;
}

/// Probe whether the device acknowledges a write / commit at all.
///
/// Sends the write report for `key`/`layer` (with slot 0 = 'z'), drains any
/// response, sends the commit sentinel, drains again, then reads the key back.
/// If the device is write-protected (physical lock switch) or doesn't
/// recognise the write report, the post-write reads will be empty AND the key
/// will be unchanged. If writes are merely gated on a handshake we haven't
/// found, this surfaces any ACK/error bytes the device emits.
fn write_probe() {
    use keytool::protocol::{Slot, off};

    let d = open_device();
    let kc = d.ping().expect("ping");
    let key: u8 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let layer: u8 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1);

    let row = d.read_layer(kc.keys, 3, layer).expect("read layer");
    let original = row[key as usize].clone();
    println!(
        "ORIGINAL  key {key} layer {layer}: {} | {}",
        keytool::device::label_for(&original),
        hex::encode(original.to_bytes())
    );

    let mut modified = original.clone();
    modified.mode = 1;
    modified.seq_len = 1;
    modified.set_slot(0, Slot { b0: 0x00, b1: 0x1D }); // 'z'

    // Send the write report (0xFE marker + key/layer addressing) and capture any response.
    let write_report = keytool::protocol::write_key_report(key, layer, &modified);
    println!("sending write report (65 bytes)…");
    d.debug_send(&write_report).expect("send write");
    drain_and_print(&d, "after write");

    // Send the commit sentinel and capture any response.
    let commit = keytool::protocol::commit_report();
    println!("sending commit sentinel…");
    d.debug_send(&commit).expect("send commit");
    drain_and_print(&d, "after commit");

    std::thread::sleep(std::time::Duration::from_millis(150));
    let row2 = d.read_layer(kc.keys, 3, layer).expect("re-read");
    let after = &row2[key as usize];
    println!(
        "AFTER     key {key} layer {layer}: {} | {}",
        keytool::device::label_for(after),
        hex::encode(after.to_bytes())
    );
    let worked = after.mode == 1 && after.seq_len == 1 && after.raw[off::SEQ_START + 1] == 0x1D;
    println!("WRITE {}!", if worked { "VERIFIED" } else { "did NOT take effect" });

    // Restore.
    println!("RESTORING original…");
    let _ = d.write_and_commit(key, layer, &original);
    std::thread::sleep(std::time::Duration::from_millis(150));
    let row3 = d.read_layer(kc.keys, 3, layer).expect("re-read after restore");
    let restored = &row3[key as usize];
    println!(
        "RESTORED  key {key} layer {layer}: {} | {}",
        keytool::device::label_for(restored),
        hex::encode(restored.to_bytes())
    );
    let _ = worked;
}

/// Read every available response packet (until quiet) and print it.
fn drain_and_print(d: &keytool::device::Device, label: &str) {
    let mut got = 0usize;
    let mut timeout = 200;
    for _ in 0..20 {
        match d.debug_recv(timeout) {
            Ok(Some(buf)) => {
                got += 1;
                println!("  [{label}] packet {got}: {}", keytool::device::pretty_hex(&buf));
                timeout = 80;
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("  [{label}] recv error: {e}");
                break;
            }
        }
    }
    if got == 0 {
        println!("  [{label}] (no response — device silent)");
    }
}

/// Write a basic-mode key record directly: `setkey <key> <layer> <hidcode0>
/// [hidcode1] ...` where hidcodes are USB HID keyboard usage IDs (e.g. 0x04='a',
/// 0x0c='i', 0x1d='z', 0x3e='F5'). `key` is 1-indexed, `layer` 0-indexed. Builds a
/// mode-1 record with the given slots (no modifiers), writes + commits, reads
/// back. Used to restore individual keys from the backup.
fn set_key() {
    use keytool::protocol::Slot;

    let d = open_device();
    let args: Vec<String> = std::env::args().collect();
    let key: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let layer: u8 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let codes: Vec<u8> = args[4..]
        .iter()
        .filter_map(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .collect();
    if codes.is_empty() {
        eprintln!("usage: setkey <key> <layer> <hidcode0> [hidcode1 ...]");
        std::process::exit(2);
    }

    let mut rec = keytool::protocol::KeyRecord::empty();
    rec.mode = 1;
    rec.seq_len = codes.len() as u8;
    for (i, &c) in codes.iter().enumerate() {
        rec.set_slot(i, Slot { b0: 0x00, b1: c });
    }
    println!(
        "SETTING key {key} device layer {}: {} | {}",
        layer + 1,
        keytool::device::label_for(&rec),
        hex::encode(rec.to_bytes())
    );
    d.write_and_commit(key, layer, &rec).expect("write");
    std::thread::sleep(std::time::Duration::from_millis(120));

    let row = d.read_layer(12, 3, layer).expect("re-read");
    let after = &row[key as usize];
    println!(
        "AFTER    key {key} layer {layer}: {} | {}",
        keytool::device::label_for(after),
        hex::encode(after.to_bytes())
    );
}

/// Write a multimedia-mode (mode 2) key record directly: `setmedia <key>
/// <layer> <consumer_code>` where the consumer code is a HID Consumer Page
/// usage (e.g. 0xE2 Mute, 0xE9 Vol+, 0xEA Vol−). `key` is 1-indexed, `layer`
/// 0-indexed. Builds a mode-2 record (seq_len=1, slot0=(consumer,0)), writes +
/// commits, reads back. Used to restore rotary multimedia slots (knob1 left =
/// Vol+ at slot 16, knob1 right = Vol− at slot 18).
fn set_media() {
    use keytool::protocol::Slot;

    let d = open_device();
    let args: Vec<String> = std::env::args().collect();
    let key: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let layer: u8 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(0);
    let code: u8 = args
        .get(4)
        .and_then(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0xE2);
    let mut rec = keytool::protocol::KeyRecord::empty();
    rec.mode = 2;
    rec.seq_len = 1;
    rec.set_slot(0, Slot { b0: code, b1: 0x00 });
    println!(
        "SETTING key {key} device layer {}: {} | {}",
        layer + 1,
        keytool::device::label_for(&rec),
        hex::encode(rec.to_bytes())
    );
    d.write_and_commit(key, layer, &rec).expect("write");
    std::thread::sleep(std::time::Duration::from_millis(120));
    let row = d.read_layer(12, 3, layer).expect("re-read");
    let after = &row[key as usize];
    println!(
        "AFTER    key {key} layer {layer}: {} | {}",
        keytool::device::label_for(after),
        hex::encode(after.to_bytes())
    );
}

/// Set the layer's LED colour. Format confirmed by USB capture of the Windows
/// app's Dialog3 (FINDINGS.md §12.6).
///
/// Usage: `keytool set-led <layer> <colour>` where:
///   layer   1..3
///   colour  name: red|orange|yellow|green|cyan|blue|purple
///            or index 1..7
fn set_led() {
    use keytool::protocol;

    let d = open_device();
    let args: Vec<String> = std::env::args().collect();
    let layer: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let colour_arg = args.get(3).map(String::as_str).unwrap_or("");

    if !(1..=3).contains(&layer) {
        eprintln!("layer must be 1..3, got {layer}");
        std::process::exit(2);
    }

    let colour_idx = match colour_arg {
        "red" | "1" => 1,
        "orange" | "2" => 2,
        "yellow" | "3" => 3,
        "green" | "4" => 4,
        "cyan" | "5" => 5,
        "blue" | "6" => 6,
        "purple" | "7" => 7,
        _ => {
            eprintln!(
                "unknown colour '{colour_arg}' — use a name (red|orange|yellow|green|cyan|blue|purple) or index 1..7"
            );
            std::process::exit(2);
        }
    };

    let report = protocol::led_report(layer, colour_idx);
    println!(
        "SETTing LED  layer {layer} colour {colour_arg} (idx {colour_idx}) [{}]",
        hex::encode_slice(&report[1..14])
    );
    d.debug_send(&report).expect("send LED report");
    std::thread::sleep(std::time::Duration::from_millis(30));
    d.debug_send(&protocol::commit_report())
        .expect("send commit");
    println!("done");
}

/// End-to-end smoke test of the *GUI's* device stack.
///
/// Instead of rendering a window, this drives the same [`keytool::worker`]
/// (background thread + `Cmd`/`Event` channels) the egui app uses, exercising
/// the real I/O path against the hardware: Connect → Read → modify one key
/// in-memory (exactly as the editor would) → Write → read back & verify →
/// restore → verify. This is the same code the GUI runs; only the pixel layer
/// is skipped. `key` is 1-indexed, `layer` 0-indexed.
fn smoke_test() {
    use keytool::protocol::{off, Slot};
    use keytool::worker::{Cmd, Event, Worker};

    let key: u8 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let layer: u8 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(1);

    println!("== GUI stack smoke test: key {key}, device layer {} ==", layer + 1);
    let mut worker = Worker::spawn();

    // helper to wait for the next event with a timeout; on timeout, stop the
    // worker and abort. (Not a closure capturing `worker` — that would fight
    // the immutable borrow of `event_rx` below.)
    macro_rules! wait {
        ($label:expr, $ms:expr) => {
            match worker.event_rx.recv_timeout(std::time::Duration::from_millis($ms)) {
                Ok(ev) => ev,
                Err(_) => {
                    eprintln!("TIMEOUT waiting for {}", $label);
                    worker.stop();
                    std::process::exit(1);
                }
            }
        };
    }

    // 1. Connect (as the GUI's Connect button does).
    print!("Connect… ");
    worker.send(Cmd::Connect);
    match wait!("Connect", 3000) {
        Event::Connected(kc) => println!(
            "OK ({} keys, {} layers reported)",
            if kc.keys == 0 { 12 } else { kc.keys },
            if kc.layers == 0 { 3 } else { kc.layers }
        ),
        Event::Error(e) => {
            println!("FAILED: {e}");
            worker.stop();
            std::process::exit(1);
        }
        ev => {
            println!("UNEXPECTED: {ev:?}");
            worker.stop();
            std::process::exit(1);
        }
    }

    // The GUI auto-reads after connecting; the smoke test does the same.
    print!("Read (auto after connect)… ");
    worker.send(Cmd::Read);
    let mut layers = match wait!("Read", 8000) {
        Event::Config(l) => {
            println!("OK ({} layers)", l.len());
            l
        }
        ev => {
            println!("UNEXPECTED: {ev:?}");
            worker.stop();
            std::process::exit(1);
        }
    };
    // swallow the optional DegenerateKeyCount note
    while worker.event_rx.try_recv().is_ok() {}

    let original = layers[layer as usize][key as usize].clone();
    println!(
        "ORIGINAL  key {key} layer {}: {} | {}",
        layer + 1,
        keytool::device::label_for(&original),
        hex::encode(original.to_bytes())
    );

    // 2. Edit in-memory exactly as the editor does: basic mode, slot 0 = 'z'.
    let mut modified = original.clone();
    modified.mode = 1;
    modified.seq_len = 1;
    modified.set_slot(0, Slot { b0: 0x00, b1: 0x1D });
    println!(
        "MODIFIED  key {key} layer {}: {} | {}",
        layer + 1,
        keytool::device::label_for(&modified),
        hex::encode(modified.to_bytes())
    );

    // 3. Write (as the GUI's Write button does): send the dirty record.
    print!("Write… ");
    worker.send(Cmd::Write(vec![(key, layer, modified.clone())]));
    match wait!("Write", 5000) {
        Event::Written => println!("OK"),
        Event::Error(e) => {
            println!("FAILED: {e}");
            // try to restore before exiting
            worker.send(Cmd::Write(vec![(key, layer, original.clone())]));
            let _ = worker.event_rx.recv_timeout(std::time::Duration::from_millis(3000));
            worker.stop();
            std::process::exit(1);
        }
        ev => {
            println!("UNEXPECTED: {ev:?}");
            worker.stop();
            std::process::exit(1);
        }
    }

    // 4. Re-read and verify the write took effect at the targeted key.
    print!("Read back… ");
    worker.send(Cmd::Read);
    layers = match wait!("re-read", 5000) {
        Event::Config(l) => {
            println!("OK");
            l
        }
        ev => {
            println!("UNEXPECTED: {ev:?}");
            worker.stop();
            std::process::exit(1);
        }
    };
    let after = &layers[layer as usize][key as usize];
    println!(
        "AFTER     key {key} layer {}: {} | {}",
        layer + 1,
        keytool::device::label_for(after),
        hex::encode(after.to_bytes())
    );
    let write_ok = after.mode == 1 && after.seq_len == 1 && after.raw[off::SEQ_START + 1] == 0x1D;
    println!("WRITE {}!", if write_ok { "VERIFIED" } else { "did NOT take effect" });

    // 5. Restore the original via the GUI write path.
    print!("Restore… ");
    worker.send(Cmd::Write(vec![(key, layer, original.clone())]));
    match wait!("restore", 5000) {
        Event::Written => println!("OK"),
        Event::Error(e) => println!("restore FAILED: {e}"),
        ev => println!("UNEXPECTED: {ev:?}"),
    }

    // 6. Final read and verify the config matches the original.
    print!("Final read… ");
    worker.send(Cmd::Read);
    let final_layers = match wait!("final read", 5000) {
        Event::Config(l) => {
            println!("OK");
            l
        }
        ev => {
            println!("UNEXPECTED: {ev:?}");
            worker.stop();
            std::process::exit(1);
        }
    };
    let restored = &final_layers[layer as usize][key as usize];
    println!(
        "RESTORED  key {key} layer {}: {} | {}",
        layer + 1,
        keytool::device::label_for(restored),
        hex::encode(restored.to_bytes())
    );
    let restore_ok = restored.to_bytes() == original.to_bytes();

    worker.stop();

    println!();
    if write_ok && restore_ok {
        println!("SMOKE TEST PASSED: GUI device stack reads, writes, and restores cleanly.");
    } else if write_ok && !restore_ok {
        println!("SMOKE TEST PARTIAL: write worked but restore mismatch — check config.");
    } else {
        println!("SMOKE TEST FAILED: write did not take effect. Protocol may differ.");
    }
}

/// Snapshot all 12 keys × 3 layers as raw 50-byte records (slot 0 skipped).
fn snapshot(d: &keytool::device::Device) -> Vec<Vec<[u8; keytool::protocol::RECORD_LEN]>> {
    (0..3)
        .map(|layer| {
            d.read_layer(12, 3, layer)
                .unwrap_or_default()
                .into_iter()
                .map(|r| r.to_bytes())
                .collect()
        })
        .collect()
}

/// Brute-force the `0x514C` write command byte.
///
/// The `0x1189` app puts the per-key counter at `record[0]` (= `report[1]`) as an
/// implicit write marker, but this device silently ignores that. We try a
/// curated list of candidate command bytes at `record[0]`, each time sending a
/// record that carries the unique keycode `0x1D` ('z' — absent from the config)
/// to an already-altered target key, plus the `0xFD` commit. After every
/// candidate we re-read the whole config and diff against the pre-run snapshot;
/// the first candidate that changes ANY key is reported and we stop (so at most
/// one extra key can be affected). A full backup is at `config-backup.txt`.
fn brute_write() {
    use keytool::protocol::{off, Slot, RECORD_LEN, REPORT_ID};

    let d = open_device();
    let key: u8 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let layer: u8 = std::env::args().nth(3).and_then(|s| s.parse().ok()).unwrap_or(0);

    println!("brute-forcing write command (target key {key}, device layer {})…", layer + 1);
    println!("snapshotting current config…");
    let before = snapshot(&d);
    println!("snapshot taken ({} layers)", before.len());

    // Curated candidate list for record[0] (= report[1]): likely write markers
    // near the read cmd 0xFA, plus small numbers (the 0x1189 counter form).
    // 0xFA/0xFB/0xFC/0xFD are known (read/keycount/setver/commit) — skipped.
    let candidates: Vec<u8> = [
        0xF9u8, 0xF8, 0xF7, 0xF6, 0xF5, 0xF4, 0xF3, 0xF2, 0xF1, 0xF0, 0xEF, 0xEE,
        0xED, 0xEC, 0xEB, 0xEA, 0xE9, 0xE8, 0xFE, 0xFF, 0x01, 0x02, 0x03, 0x04,
        0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x00,
    ]
    .to_vec();

    for &c in &candidates {
        // Build the write record: record[0]=candidate cmd, record[1]=key,
        // record[2]=layer (1-indexed), mode=1, seq_len=1, slot0=(0, 0x1D 'z').
        let mut rec = keytool::protocol::KeyRecord::empty();
        rec.mode = 1;
        rec.seq_len = 1;
        rec.set_slot(0, Slot { b0: 0x00, b1: 0x1D });
        let mut bytes = rec.to_bytes();
        bytes[0] = c;
        bytes[off::MODE] = 1;
        bytes[off::SEQ_LEN] = 1;
        bytes[off::SEQ_START] = 0x00;
        bytes[off::SEQ_START + 1] = 0x1D;
        // addressing bytes
        bytes[1] = key;
        bytes[2] = layer + 1; // device layers are 1-indexed

        let mut report = [0u8; keytool::protocol::REPORT_LEN];
        report[0] = REPORT_ID;
        report[1..1 + RECORD_LEN].copy_from_slice(&bytes);

        if let Err(e) = d.debug_send(&report) {
            println!("candidate {c:#04x}: send error {e} — skipping");
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(3));
        // commit
        let _ = d.debug_send(&keytool::protocol::commit_report());
        std::thread::sleep(std::time::Duration::from_millis(60));
        // drain any response so it can't contaminate the next read
        for _ in 0..16 {
            if d.debug_recv(40).ok().flatten().is_none() {
                break;
            }
        }

        // Re-read and diff.
        let after = snapshot(&d);
        let mut diffs = Vec::new();
        for (l, (bl, al)) in before.iter().zip(after.iter()).enumerate() {
            for (k, (b, a)) in bl.iter().zip(al.iter()).enumerate() {
                if k == 0 {
                    continue;
                }
                if b != a {
                    diffs.push((l, k, *b, *a));
                }
            }
        }
        if diffs.is_empty() {
            println!("candidate {c:#04x}: no effect");
        } else {
            println!("*** candidate {c:#04x} CHANGED {n} key(s) ***", n = diffs.len());
            for (l, k, b, a) in &diffs {
                println!(
                    "  layer {l} key {k}: {} -> {}",
                    hex::encode(*b),
                    hex::encode(*a)
                );
            }
            println!("STOPPING — candidate {c:#04x} is a live write command.");
            println!("Re-snapshot to confirm, then restore the backup once the write is understood.");
            return;
        }
    }
    println!("no candidate took effect. The write marker is outside the tried set, or the write report framing differs from [0x03, <record>].");
}

// minimal hex encoder (avoid pulling a dependency just for the CLI dump)
mod hex {
    pub fn encode(b: [u8; keytool::protocol::RECORD_LEN]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for byte in b {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }

    pub fn encode_slice(b: &[u8]) -> String {
        let mut s = String::with_capacity(b.len() * 2);
        for byte in b {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }
}
