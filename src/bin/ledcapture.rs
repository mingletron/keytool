//! Test the exact LED report format captured from the Windows app.
//!
//! Capture shows: 03 fe b0 <layer> 08 | 00 00 00 00 00 | 01 | 00 <bitmask>
//! Where bitmask = (colour group << 4) | key bits
//!
//! From the capture, the user selected:
//!   0x12 = colour group 1, key bits 0x2
//!   0x22 = colour group 2, key bits 0x2
//!   0x72 = colour group 7, key bits 0x2
//!   0x71 = colour group 7, key bits 0x1

use std::io::Write;
use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, REPORT_LEN},
};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn send_led(d: &Device, layer: u8, bitmask: u8, label: &str) {
    let mut report = [0u8; REPORT_LEN];
    report[0] = 0x03;
    report[1] = 0xFE;
    report[2] = 0xB0;
    report[3] = layer;
    report[4] = 0x08;
    report[10] = 0x01; // seq_len
    report[11] = 0x00;
    report[12] = bitmask; // colour group + key bits

    println!("  {label}: {}", hex(&report[1..15]));
    let _ = d.debug_send(&report);
    std::thread::sleep(Duration::from_millis(30));
    let _ = d.debug_send(&protocol::commit_report());
    std::thread::sleep(Duration::from_millis(50));
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

    println!("=== Testing exact capture format (fe b0 <layer> 08 ... 01 00 <bitmask>) ===");
    println!("Watch the device LEDs after each write.\n");

    // First, reproduce the exact capture reports
    println!("--- Reproducing capture reports (layer 1) ---");
    send_led(&d, 1, 0x12, "capture: group 1, key 2");
    send_led(&d, 1, 0x22, "capture: group 2, key 2");
    send_led(&d, 1, 0x72, "capture: group 7, key 2");
    send_led(&d, 1, 0x71, "capture: group 7, key 1");

    println!("\n--- Now test different layers ---");
    println!("Write group 7 (orange?) to layer 2, all keys:");
    send_led(&d, 2, 0x7F, "layer 2, group 7, all keys");

    println!("\n--- Test colour groups 0-15 on layer 2, all keys ---");
    println!("Press keys on layer 2 after each write to see the colour.\n");

    for group in 0u8..=15 {
        let bitmask = (group << 4) | 0x0F;
        send_led(&d, 2, bitmask, &format!("layer 2, group {group}, all keys"));
        println!("    >>> Press a key on layer 2. What colour flash? <<<");
        let _ = std::io::stdin().read_line(&mut String::new());
    }

    println!("\n=== Done ===");
    println!("The capture format: 03 fe b0 <layer> 08 00 00 00 00 00 01 00 <bitmask>");
    println!("bitmask high nibble = colour group, low nibble = which keys.");
}