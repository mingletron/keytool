//! Test the global LED mode enable command found in the USB capture.
//!
//! Pattern: [0x03, 0xFE, 0xB0, 0x01, <mode>, 0x00...] + commit
//! Where mode = 0x00..0x05 (LED_Mode_0 through LED_Mode_5)

use std::io::Write;
use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, REPORT_LEN},
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

    // First, clear any existing LED indicator records
    println!("=== Clearing existing LED records ===");
    for layer in 1u8..=3 {
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        let rec = &mut report[1..51];
        rec[0x00] = 0xFE;
        rec[0x01] = 0xB0;
        rec[0x02] = layer;
        rec[0x03] = 5;
        rec[protocol::off::SEQ_LEN] = 1;
        rec[protocol::off::LED_BITMASK] = 0x00; // disable all
        let _ = d.debug_send(&report);
        std::thread::sleep(Duration::from_millis(30));
        let _ = d.debug_send(&protocol::commit_report());
        println!("  Cleared layer {layer}");
    }
    std::thread::sleep(Duration::from_millis(200));

    // Now test the global LED enable command for modes 0-5
    println!("\n=== Testing global LED enable command (0xFE 0xB0 0x01 <mode>) ===");
    println!("Mode 0 = off, 1-5 = LED_Mode_1 through LED_Mode_5");
    println!("Watch the device - layer 2 reactive flash should change or stop.\n");

    for mode in 0u8..=5 {
        println!("--- Testing mode {mode} ---");

        // Send the global LED enable command
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        report[1] = 0xFE;
        report[2] = 0xB0;
        report[3] = 0x01;
        report[4] = mode;

        println!("  Sending: {}", hex(&report[1..10]));
        let _ = d.debug_send(&report);
        std::thread::sleep(Duration::from_millis(30));

        // Send commit
        let _ = d.debug_send(&protocol::commit_report());
        std::thread::sleep(Duration::from_millis(100));

        println!("  >>> Press keys on layer 2 now. What happens? (flash colour? no flash?) <<<");
        println!("  Press Enter to continue to next mode...");
        let _ = std::io::stdin().read_line(&mut String::new());
    }

    // Also test if the mode affects layer indicator (write an LED record then enable)
    println!("\n=== Testing layer indicator with LED mode enabled ===");
    println!("Will write a layer indicator (key 1, red group) then enable mode 1");

    // Write LED record for layer 2, key 1, colour group 1 (red?)
    let mut led_report = [0u8; REPORT_LEN];
    led_report[0] = 0x03;
    let rec = &mut led_report[1..51];
    rec[0x00] = 0xFE;
    rec[0x01] = 0xB0;
    rec[0x02] = 2; // layer 2
    rec[0x03] = 5; // LED mode
    rec[protocol::off::SEQ_LEN] = 1;
    rec[protocol::off::LED_BITMASK] = 0x11; // group 1, key 1
    let _ = d.debug_send(&led_report);
    std::thread::sleep(Duration::from_millis(30));
    let _ = d.debug_send(&protocol::commit_report());
    println!("  LED indicator written (layer 2, key 1, group 1)");

    // Now enable mode 1
    let mut enable_report = [0u8; REPORT_LEN];
    enable_report[0] = 0x03;
    enable_report[1] = 0xFE;
    enable_report[2] = 0xB0;
    enable_report[3] = 0x01;
    enable_report[4] = 1; // mode 1
    let _ = d.debug_send(&enable_report);
    std::thread::sleep(Duration::from_millis(30));
    let _ = d.debug_send(&protocol::commit_report());
    println!("  Global mode 1 enabled");

    println!("  >>> Switch to layer 2. Does key 1 flash? What colour? <<<");
    let _ = std::io::stdin().read_line(&mut String::new());

    println!("\n=== Done ===");
}