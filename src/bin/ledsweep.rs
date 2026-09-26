//! Sweep colour groups with a hold time so the user can observe each one.
//!
//! Usage: ledsweep <layer> <start_group> <end_group> <hold_secs>
//!   layer       1..3
//!   start_group 0..15
//!   end_group   0..15
//!   hold_secs   seconds to hold each group (default 8)
//!
//! For each group, writes the LED config and holds. The user should press a
//! key on the target layer during each hold and note the flash colour.
//!
//! The report format (from USB capture):
//!   03 fe b0 <layer> 08 | 00 00 00 00 00 | 01 | 00 <bitmask>
//!   bitmask = (colour group << 4) | key bits (0xF = all keys)

use std::thread::sleep;
use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, REPORT_LEN},
};

fn send_led(d: &Device, layer: u8, bitmask: u8) {
    let mut report = [0u8; REPORT_LEN];
    report[0] = 0x03;
    report[1] = 0xFE;
    report[2] = 0xB0;
    report[3] = layer;
    report[4] = 0x08;
    report[10] = 0x01; // seq_len
    report[11] = 0x00;
    report[12] = bitmask;

    let _ = d.debug_send(&report);
    sleep(Duration::from_millis(30));
    let _ = d.debug_send(&protocol::commit_report());
    sleep(Duration::from_millis(50));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let layer: u8 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2);
    let start: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let end: u8 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(15);
    let hold: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(8);

    let d = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening device: {e}");
            std::process::exit(1);
        }
    };
    println!("opened device");
    println!("Sweeping colour groups {start}..{end} on layer {layer}, hold {hold}s each.");
    println!("For each group, press a key on layer {layer} and note the flash colour.\n");

    for group in start..=end {
        let bitmask = (group << 4) | 0x0F;
        send_led(&d, layer, bitmask);
        println!(
            "── group {group:2}  bitmask=0x{bitmask:02X}  (hold {hold}s)  → observe flash colour"
        );
        sleep(Duration::from_secs(hold));
    }

    println!("\n=== Done ===");
    println!("Note down the colour you saw for each group.");
}