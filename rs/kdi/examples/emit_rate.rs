//! Does the DEVICE emit sparsely, or does the HOST lose frames? (#131)
//!
//! ```text
//! cargo run --features usb3 --example emit_rate -- [serial]
//! ```
//!
//! The measurement that started #131 read a 20x timestamp gap between delivered records and called
//! it a rate. It is not decidable from that number: `lost_before` is DERIVED FROM TIMESTAMPS, so it
//! restates the gap rather than corroborating any cause, and both stories predict it exactly.
//!
//! This decides it by never reading the pipe. Start the stream, leave the data where it is, and
//! watch the device's own FIFO fill:
//!
//! * emitting at the full rate -> the FIFO saturates almost immediately and `overrun` sets;
//! * emitting one frame in twenty -> it fills about twenty times slower.
//!
//! Host transport cannot influence either number, because the host is not reading.

use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, Instant};

use kdi::{Acquisition, Device, Stream};

/// A BOUNDED burst small enough that the pipe cannot overflow: 16 frames x 110 B = 1760 B against a
/// FIFO measured at 4096 B. The device emits exactly 16 and stops, so no frame can be lost in
/// transport or dropped on the floor -- whatever the timestamps say is the device's own emission
/// cadence, with the host removed from the question entirely.
fn probe(dev: &mut Device, label: &str) -> Result<(), String> {
    const N: u16 = 16;
    let mut rd = dev
        .start(
            Stream::Samples,
            &Acquisition {
                lanes: 1,
                burst: Some(N),
            },
        )
        .map_err(|e| format!("start: {e}"))?;
    println!("  {label}");

    // Let the whole burst land before reading a single byte.
    sleep(Duration::from_millis(600));
    let h = rd.health().map_err(|e| format!("health: {e}"))?;
    println!(
        "    resident before any read: {} B, overrun={}",
        h.readable_bytes, h.overrun
    );

    let mut ts = Vec::new();
    let t0 = Instant::now();
    while ts.len() < usize::from(N) && t0.elapsed() < Duration::from_secs(5) {
        match rd.next(Duration::from_millis(200)) {
            Ok(Some(r)) => ts.push(r.timestamp()),
            Ok(None) => break,
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let _ = rd.stop();

    if ts.len() < 2 {
        println!("    only {} frames", ts.len());
        return Ok(());
    }
    let gaps: Vec<u64> = ts.windows(2).map(|w| w[1] - w[0]).collect();
    let min = gaps.iter().min().unwrap();
    let max = gaps.iter().max().unwrap();
    println!(
        "    {} frames, gap min={min} max={max}  (3375 = every timestep, 67500 = one in twenty)",
        ts.len()
    );
    Ok(())
}

fn main() -> ExitCode {
    let serial = std::env::args().nth(1).unwrap_or_default();
    let mut dev = match Device::open_usb3(&serial, None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("open failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("gateware_sha  0x{:08x}", dev.gateware_sha());

    // One lane at 29,629.6 S/s is 35 rows x 2 B + a 40 B header = 110 B per frame, so a device
    // emitting every timestep produces ~3.3 MB/s. Any FIFO on this board saturates in milliseconds.
    println!("\nBEFORE any DRP reprogram:");
    if let Err(e) = probe(&mut dev, "samples, pipe never read") {
        println!("    {e}");
    }

    println!(
        "\nAFTER programming the rate (m=42 d=25, state 17 = the SAME rate the RTL default gives):"
    );
    match dev.set_rate(42, 25) {
        Ok(()) => {
            if let Err(e) = probe(&mut dev, "samples, pipe never read") {
                println!("    {e}");
            }
        }
        Err(e) => println!("    set_rate failed: {e}"),
    }

    let _ = dev.quiesce();
    ExitCode::SUCCESS
}
