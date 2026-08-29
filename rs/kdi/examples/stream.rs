//! Acquire from a device and print records. NO HARDWARE NEEDED.
//!
//! This runs against the software device model over the UDP binding. From the repo root, in one
//! terminal:
//!
//! ```text
//! ```
//!
//! and in another:
//!
//! ```text
//! cargo run --example stream                 # first device found
//! cargo run --example stream -- KVDEMO       # that serial specifically
//! ```
//!
//! The server announces itself into `$KDI_DISCOVERY_DIR` (default: `kdi-discovery` under the
//! system temp dir), which is where `kdi::find` looks — so if you set that variable, set it for
//! both processes. Nothing below is transport-specific: point it at a board over `--features
//! usb3` and the same code runs.

use std::time::Duration;

use kdi::{Acquisition, ConnectOpts, Device, Filter, Kind, Stream};

fn main() -> Result<(), kdi::Error> {
    // Devices are found by IDENTITY. An unset field means "any", so a bare `Filter::default()`
    // takes whatever is on the bench.
    let filter = Filter {
        serial: std::env::args().nth(1),
        ..Filter::default()
    };
    // The second half is enumeration failures — an SDK that loaded but did not answer, a
    // discovery directory that could not be read. That is NOT the same observation as an empty
    // bench, so it comes back separately instead of being flattened into "no devices".
    let (found, errs) = kdi::find(&filter);
    for e in &errs {
        eprintln!("enumeration error: {e}");
    }
    let Some(info) = found.first() else {
        eprintln!("no device found - connect an instrument and build with --features usb3");
        return Ok(());
    };
    println!(
        "{} ({}) over {:?}",
        info.serial, info.compatible, info.transport
    );

    // `open` is the whole bind: not-KDI, contract major, `contract_ready`, capabilities, lease.
    let mut dev = Device::open(info, &ConnectOpts::default())?;

    // EVERY HOST OWES THIS WRITE (#131, contract 0.5). An unconfigured device emits nothing on
    // `rhd_matrix`, and before the capability existed it emitted one frame per twenty sample
    // periods while declaring the full cadence. Non-fatal, so this still runs against pre-0.5
    // gateware, where the rate is whatever the last host left.
    if let Err(e) = dev.set_rate(42, 25) {
        eprintln!("note: rate not configured ({e}) - pre-0.5 gateware");
    }

    // `start` stops the stream, arms it and starts it — the falling edge is what flushes the
    // device's buffers, so re-asserting an already-set run bit would inherit the previous run.
    let mut rx = dev.start(Stream::Samples, &Acquisition::default())?;

    for _ in 0..10 {
        // `Ok(None)` is "nothing arrived in time", not a failure. An `Err` means the TRANSPORT
        // failed: a corrupt frame never reaches here, it is counted in `stats()` and resynced past.
        let Some(rec) = rx.next(Duration::from_secs(2))? else {
            eprintln!("no record within 2 s");
            break;
        };
        // `Ok(None)` = this record has no such block; `Err` = it legally carries more than one,
        // and a singular accessor must refuse rather than hand back half the data.
        let amp = rec.block(Kind::RhdMatrix)?.and_then(|b| b.amplifier(0, 0));
        println!(
            "t={:>12}  lost_before={}  ch0={}",
            rec.timestamp(),
            rec.lost_before(),
            amp.map_or_else(|| "-".to_string(), |v| format!("0x{v:04x}")),
        );
    }

    // Everything the reader had to recover from. All zero on a healthy link, which is the
    // assertion worth making.
    println!("{:?}", rx.stats());
    rx.stop()?;
    dev.close()
}
