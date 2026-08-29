//! The silicon acceptance for #131 (docs/kdi_rate_and_validity.md §7).
//!
//! ```text
//! cargo run --features usb3 --example rate_acceptance -- [serial]
//! ```
//!
//! Every check runs after a `ConfigureFPGA` with **no other host action**, because the whole defect
//! is about what a device does before anyone configures it. A green sim proves nothing here: every
//! failed attempt at this bug passed its sims.
//!
//! Exit 0 = all checks passed.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use kdi::{Acquisition, Cap, Device, Stream};

/// The EXPLICIT table entry for 30 kS/s, not `0, 0`. Both reach the same state -- an off-table pair
/// falls through to the default -- but the fall-through makes "which rate did we ask for" a second
/// question during a measurement that is already about rates, and the first run of this check was
/// read with that ambiguity in it.
const M: u8 = 42;
const D: u8 = 25;

/// ONE LANE, and a bounded burst.
///
/// The first version of this check asked for all 32 lanes free-running and measured a gap of
/// 33750 ticks against a declared 3375 -- a clean factor of ten that looked exactly like a rate
/// defect. It was not: 32 lanes x 35 rows x 2 B at 30 kS/s is ~66 MB/s, which this pipe does not
/// sustain, so the records that survive are spaced ten timesteps apart. The measurement was
/// wrong, not the device.
///
/// `lost_before` is summed and reported for the same reason: an interval measurement over frames
/// that were dropped between reads is an interval between the wrong pair of frames, and if nothing
/// prints the loss it reads as a rate.
const LANES: u32 = 1;

/// One frame's `(timestamp, tick_num, tick_den)`. Named because clippy is right that the tuple was
/// getting hard to read, and because the cadence pair travels WITH the timestamp on purpose: the
/// reference for every ratio here comes off the wire, never from a host-side constant.
type Sample = (u64, u32, u32);

fn drain(
    dev: &mut Device,
    s: Stream,
    want: usize,
    budget: Duration,
) -> Result<(Vec<Sample>, u64), String> {
    let mut rd = dev
        .start(
            s,
            &Acquisition {
                lanes: LANES,
                burst: Some(want as u16),
            },
        )
        .map_err(|e| format!("start: {e}"))?;
    let mut out = Vec::new();
    let mut lost = 0u64;
    let t0 = Instant::now();
    while out.len() < want && t0.elapsed() < budget {
        match rd.next(Duration::from_millis(200)) {
            // Cadence is a SECTION property, not a frame one -- the timebase is what the contract
            // makes normative, so the reference for every ratio below comes off the wire.
            Ok(Some(r)) => {
                if !out.is_empty() {
                    lost += r.lost_before();
                }
                let (n, d) = r.blocks().next().map(|b| b.cadence()).unwrap_or((0, 1));
                out.push((r.timestamp(), n, u32::from(d)))
            }
            Ok(None) => {}
            Err(e) => return Err(format!("read: {e}")),
        }
    }
    let _ = rd.stop();
    Ok((out, lost))
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
    let (maj, min) = dev.kdi();
    println!("contract      {maj}.{min}");
    if !dev.caps().has(Cap::RateControl) {
        eprintln!(
            "FAIL: this build does not advertise rate_control -- wrong bitstream for this check"
        );
        return ExitCode::FAILURE;
    }

    let mut fails = 0;
    let mut check = |name: &str, ok: bool, detail: String| {
        println!(
            "  {:<26} {}   {detail}",
            name,
            if ok { "ok  " } else { "FAIL" }
        );
        if !ok {
            fails += 1;
        }
    };

    // 1. UNCONFIGURED EMITS NOTHING. Not "slow frames" -- none.
    let ready0 = dev.rate_ready().unwrap_or(true);
    let silent = drain(&mut dev, Stream::Samples, 4, Duration::from_secs(3));
    match silent {
        // start() is expected to refuse before it ever reaches the pipe; that IS the check.
        Err(e) if e.contains("rate is not configured") => check(
            "unconfigured is silent",
            !ready0,
            format!("rate_ready={ready0}, start refused: named set_rate"),
        ),
        Ok((v, _)) => check(
            "unconfigured is silent",
            !ready0 && v.is_empty(),
            format!("rate_ready={ready0}, {} frames", v.len()),
        ),
        Err(e) => check("unconfigured is silent", false, e),
    }

    // 2. CONFIGURED IS CORRECT, and the frame's OWN declared cadence is the reference -- never a
    //    host-side constant, which is what let #131 stand for so long.
    if let Err(e) = dev.set_rate(M, D) {
        check("set_rate", false, format!("{e}"));
    } else {
        let ready1 = dev.rate_ready().unwrap_or(false);
        match drain(&mut dev, Stream::Samples, 24, Duration::from_secs(10)) {
            Ok((v, lost)) if v.len() >= 8 => {
                let declared = f64::from(v[0].1) / f64::from(v[0].2.max(1));
                let mut gaps: Vec<f64> = v.windows(2).map(|w| (w[1].0 - w[0].0) as f64).collect();
                gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let med = gaps[gaps.len() / 2];
                let ratio = med / declared;
                check("configured is correct", ready1 && lost == 0 && (ratio - 1.0).abs() < 0.02,
                      format!("rate_ready={ready1}, declared {declared:.1}, observed {med:.1}, ratio {ratio:.3}, lost {lost}"));
            }
            Ok((v, _)) => check(
                "configured is correct",
                false,
                format!("only {} frames", v.len()),
            ),
            Err(e) => check("configured is correct", false, e),
        }
    }

    // 3. THE PUBLISHED CADENCE IS THE MEASURED ONE, ACROSS THE TABLE. Each rate is judged against
    //    the cadence THAT FRAME DECLARES, never a host-side constant -- a host-side constant is
    //    what let #131 stand, because the device and the host agreed with each other and both were
    //    wrong. This is the check that would have caught it on day one.
    for (m, d, label) in [
        (7u8, 125u8, "1 kS/s"),
        (14, 25, "10 kS/s"),
        (42, 25, "30 kS/s"),
    ] {
        if let Err(e) = dev.set_rate(m, d) {
            check("sweep", false, format!("{label}: set_rate: {e}"));
            continue;
        }
        match drain(&mut dev, Stream::Samples, 16, Duration::from_secs(20)) {
            Ok((v, lost)) if v.len() >= 8 => {
                let declared = f64::from(v[0].1) / f64::from(v[0].2.max(1));
                let mut gaps: Vec<f64> = v.windows(2).map(|w| (w[1].0 - w[0].0) as f64).collect();
                gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let med = gaps[gaps.len() / 2];
                let ratio = med / declared;
                check(
                    &format!("sweep {label}"),
                    lost == 0 && (ratio - 1.0).abs() < 0.02,
                    format!(
                        "declared {declared:.1}, observed {med:.1}, ratio {ratio:.3}, lost {lost}"
                    ),
                );
            }
            Ok((v, _)) => check(
                &format!("sweep {label}"),
                false,
                format!("only {} frames", v.len()),
            ),
            Err(e) => check(&format!("sweep {label}"), false, e),
        }
    }

    // 4. QUIESCE RETURNS TO UNCONFIGURED, and check 1 must hold again.
    if let Err(e) = dev.quiesce() {
        check("quiesce unconfigures", false, format!("{e}"));
    } else {
        let ready2 = dev.rate_ready().unwrap_or(true);
        check(
            "quiesce unconfigures",
            !ready2,
            format!("rate_ready={ready2}"),
        );
    }

    // 5. THE DIGITAL STREAM IS UNTOUCHED by any of it -- it is self-clocked and was correct
    //    throughout #131, so a change that disturbs it is a regression in the fix.
    match drain(&mut dev, Stream::Digital, 8, Duration::from_secs(5)) {
        Ok((v, _)) if v.len() >= 4 => {
            let declared = f64::from(v[0].1) / f64::from(v[0].2.max(1));
            let mut gaps: Vec<f64> = v.windows(2).map(|w| (w[1].0 - w[0].0) as f64).collect();
            gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let med = gaps[gaps.len() / 2];
            check(
                "digital unaffected",
                (med / declared - 1.0).abs() < 0.05,
                format!(
                    "{} frames, declared {declared:.1}, observed {med:.1}",
                    v.len()
                ),
            );
        }
        Ok((v, _)) => check(
            "digital unaffected",
            false,
            format!("only {} frames", v.len()),
        ),
        Err(e) => check("digital unaffected", false, e),
    }

    let _ = dev.quiesce();
    println!("\nRESULT: {}", if fails == 0 { "PASS" } else { "FAIL" });
    if fails == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
