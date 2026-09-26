//! Try the exact LED mode pattern from Dialog3 disassembly.
//!
//! Dialog3::on_changeButtonGroup stores 0xb008 at ebx+0x1c.
//! This tries sending that pattern directly as a HID report.

use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, REPORT_LEN},
};

fn try_report(d: &Device, report: &[u8; REPORT_LEN], label: &str) {
    match d.debug_send(report) {
        Ok(()) => {
            println!("  {label}: OK");
            println!("    raw: {}", hex::encode(&report[1..12]));
        }
        Err(e) => println!("  {label}: ERR {e}"),
    }
    std::thread::sleep(Duration::from_millis(100));
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

    // First write an LED indicator so we have something to enable
    println!("=== Writing test LED record (layer 1, key 1, colour group 0) ===");
    let mut led = [0u8; REPORT_LEN];
    led[0] = 0x03;
    led[1] = 0xFE;
    led[2] = 0xB0;
    led[3] = 1;      // layer 1
    led[4] = 5;      // mode = LED
    led[9] = 1;      // seq_len
    led[0x0B] = 0x01; // bitmask: group 0, key 1
    let _ = d.debug_send(&led);
    std::thread::sleep(Duration::from_millis(30));
    let _ = d.debug_send(&protocol::commit_report());
    println!("LED record written\n");

    // Try the exact pattern from Dialog3: 0xB0 followed by mode value
    println!("=== Trying 0xB0 <mode> patterns (from Dialog3) ===");
    for mode in 0u8..=5 {
        // Pattern 1: Just 0xB0 + mode
        let mut r1 = [0u8; REPORT_LEN];
        r1[0] = 0x03;
        r1[1] = 0xB0;
        r1[2] = mode;
        try_report(&d, &r1, &format!("0xB0 mode={mode}"));

        // Pattern 2: 0xFE 0xB0 mode (like LED record but to "slot 0")
        let mut r2 = [0u8; REPORT_LEN];
        r2[0] = 0x03;
        r2[1] = 0xFE;
        r2[2] = 0xB0;
        r2[3] = mode; // mode in position 3
        r2[4] = 0;    // layer 0
        try_report(&d, &r2, &format!("0xFE 0xB0 mode={mode} layer=0"));

        // Pattern 3: 0xB0 0x08 <mode> (the 0x08 from the disassembly)
        let mut r3 = [0u8; REPORT_LEN];
        r3[0] = 0x03;
        r3[1] = 0xB0;
        r3[2] = 0x08;
        r3[3] = mode;
        try_report(&d, &r3, &format!("0xB0 0x08 mode={mode}"));
    }

    // Try 0xB0 0x00 through 0xB0 0xFF
    println!("\n=== Trying 0xB0 <byte> patterns ===");
    for byte in [0x00, 0x01, 0x02, 0x08, 0x10, 0x20, 0x50, 0x80, 0xFF] {
        let mut r = [0u8; REPORT_LEN];
        r[0] = 0x03;
        r[1] = 0xB0;
        r[2] = byte;
        try_report(&d, &r, &format!("0xB0 0x{byte:02X}"));
    }

    // Try with commit after each
    println!("\n=== Trying 0xB0 mode with commit ===");
    for mode in 0u8..=5 {
        let mut r = [0u8; REPORT_LEN];
        r[0] = 0x03;
        r[1] = 0xB0;
        r[2] = mode;
        let _ = d.debug_send(&r);
        std::thread::sleep(Duration::from_millis(30));
        let _ = d.debug_send(&protocol::commit_report());
        println!("  0xB0 mode={mode} + commit: sent");
        std::thread::sleep(Duration::from_millis(100));
    }

    println!("\n=== Done ===");
    println!("Watch the device LEDs during this probe.");
}

mod hex {
    pub fn encode(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
}
