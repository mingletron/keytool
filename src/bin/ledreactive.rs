//! Probe the reactive LED mode (on-press/on-release flash).
//!
//! The device shows green flash on every key press/release on layer 2.
//! This probes the per-key reactive LED settings to understand the mapping.
//!
//! Per FINDINGS.md §11.3: mode 5 records with bitmask control LEDs.
//! The bitmask format is: high nibble = colour group, low nibble = key bits.
//!
//! This tool:
//! 1. Reads current slot 0 (LED header) for each layer to see current settings
//! 2. Tries different colour groups on a single key
//! 3. Tests the reactive vs layer-indicator modes

use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, off, KeyRecord, REPORT_LEN},
};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() {
    let d = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening device: {e}");
            std::process::exit(1);
        }
    };
    println!("opened device\n");

    // Read current LED settings (slot 0) for each layer
    println!("=== Reading current LED slot-0 records ===");
    for layer in 0..3 {
        match d.read_layer(12, 3, layer) {
            Ok(row) => {
                let slot0 = &row[0];
                println!(
                    "  layer {}: slot0 = mode={} raw={}",
                    layer + 1,
                    slot0.mode,
                    hex(&slot0.to_bytes()[..12])
                );
                if slot0.mode == 5 {
                    println!(
                        "         LED bitmask = 0x{:02X} (colour group {}, key bits 0x{:01X})",
                        slot0.raw[off::LED_BITMASK],
                        slot0.raw[off::LED_BITMASK] >> 4,
                        slot0.raw[off::LED_BITMASK] & 0x0F
                    );
                }
            }
            Err(e) => println!("  layer {}: read error: {e}", layer + 1),
        }
    }

    println!("\n=== Current reactive behaviour ===");
    println!("Layer 2 keys flash GREEN on press and release.");
    println!("Let's probe what controls this.\n");

    // Probe: change the LED bitmask for layer 2 and observe
    println!("=== Probing layer 2 LED settings ===");
    println!("Will write different colour groups to layer 2's LED record.");
    println!("After each write, press any key on layer 2 and note the flash colour.\n");

    let args: Vec<String> = std::env::args().collect();
    let interactive = args.get(1).map(|s| s.as_str()) == Some("-i");

    if interactive {
        for group in 0u8..=15 {
            let bitmask = (group << 4) | 0x0F; // all keys, this colour group
            let mut report = [0u8; REPORT_LEN];
            report[0] = 0x03;
            let rec = &mut report[1..51];
            rec[0x00] = 0xFE;
            rec[0x01] = 0xB0;
            rec[0x02] = 2; // layer 2 (1-indexed = 3)
            rec[0x03] = 5; // mode LED
            rec[off::SEQ_LEN] = 1;
            rec[off::LED_BITMASK] = bitmask;

            println!("--- Writing colour group {group} (bitmask=0x{bitmask:02X}) ---");
            let _ = d.debug_send(&report);
            std::thread::sleep(Duration::from_millis(30));
            let _ = d.debug_send(&protocol::commit_report());
            println!("Press any key on layer 2 now. What colour does it flash?");
            println!("Press Enter to continue to next group...");
            let _ = std::io::stdin().read_line(&mut String::new());
        }
    } else {
        println!("Run with -i for interactive colour group sweep.");
        println!("\n=== Writing test LED records ===");

        // Write an LED record for layer 2 with all keys, group 0
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        let rec = &mut report[1..51];
        rec[0x00] = 0xFE;
        rec[0x01] = 0xB0;
        rec[0x02] = 3; // layer 3 (device is 1-indexed, so layer 2 = 3)
        rec[0x03] = 5;
        rec[off::SEQ_LEN] = 1;
        rec[off::LED_BITMASK] = 0x0F; // group 0, all keys

        println!("Writing to layer 3 (host layer 2): group 0, all keys");
        let _ = d.debug_send(&report);
        std::thread::sleep(Duration::from_millis(30));
        let _ = d.debug_send(&protocol::commit_report());

        // Read it back
        std::thread::sleep(Duration::from_millis(100));
        match d.read_layer(12, 3, 2) {
            Ok(row) => {
                let slot0 = &row[0];
                println!(
                    "Read back: slot0 = mode={} raw={}",
                    slot0.mode,
                    hex(&slot0.to_bytes()[..12])
                );
            }
            Err(e) => println!("Read back error: {e}"),
        }
    }

    println!("\n=== Done ===");
}
