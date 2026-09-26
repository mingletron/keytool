//! Map colour groups to actual LED colours.
//!
//! Uses the capture format: 03 fe b0 <layer> 08 | 00 00 00 00 00 | 01 | 00 <bitmask>
//! bitmask high nibble = colour group, low nibble = which keys (0xF = all).
//!
//! For each group 0..15, writes the group to a layer and waits for the user
//! to observe the LED colour, then records it.

use std::io::{self, Write};
use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, REPORT_LEN},
};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

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
    std::thread::sleep(Duration::from_millis(30));
    let _ = d.debug_send(&protocol::commit_report());
    std::thread::sleep(Duration::from_millis(50));
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let layer: u8 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(2);
    let start_group: u8 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);

    let d = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening device: {e}");
            std::process::exit(1);
        }
    };
    println!("opened device");
    println!("Mapping colour groups 0-15 on layer {layer}.");
    println!("For each group, press a key on layer {layer} and note the flash colour.");
    println!("Type the colour name (e.g. 'red', 'orange', 'green', 'purple', 'off') then Enter.\n");

    let mut results: Vec<(u8, String)> = Vec::new();

    for group in start_group..=15 {
        let bitmask = (group << 4) | 0x0F; // all keys
        send_led(&d, layer, bitmask);

        print!("group {group:2} (bitmask 0x{bitmask:02X}): flash colour? ");
        io::stdout().flush().unwrap();

        let mut input = String::new();
        io::stdin().read_line(&mut input).unwrap();
        let colour = input.trim().to_string();
        if colour.is_empty() {
            continue;
        }
        results.push((group, colour));
    }

    println!("\n=== Colour group map (layer {layer}) ===");
    for (group, colour) in &results {
        println!("  group {group}: {colour}");
    }

    // Print as a Rust table
    println!("\n=== Copy-paste table ===");
    for (group, colour) in &results {
        println!("    {group} => \"{colour}\",");
    }
}