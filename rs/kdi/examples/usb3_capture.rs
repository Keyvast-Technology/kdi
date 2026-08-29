//! Sparse-lane / burst-alignment capture over USB3. Fixed stdout, one line per fact.
//!
//! ```text
//! cargo run --features usb3 --example usb3_capture -- samples [serial]
//! cargo run --features usb3 --example usb3_capture -- digital [serial]
//! ```
//!
//! Does **not** flash. `samples` defaults to `lanes = 1 << 2` and `burst = 6`; `digital` starts
//! with `burst = 1`, which [`kdi::Device::start`] must raise to 2 (88 B × 1 is not 16-byte
//! aligned). These logs are diffed between runs; do not print timestamps.
//!
//! THE LANE MASK IS AN ARGUMENT, because the default is not universal and a wrong one is silent.
//! Which KDI lane carries a headstage depends on the board and on which RHX streams the engine is
//! capturing (`WI_STREAM_EN`). Ask for a lane nothing feeds and every amplifier reads `0xFFFF` —
//! well-formed frames, correct cadence, zero loss, and no error anywhere. On the reference board the
//! chip is on lane 0 with RHX stream 0 enabled (`tools/kdi_rhd_content.py:24-27`), and the `1 << 2`
//! default here read idle for exactly that reason.
//!
//! ```text
//! cargo run --features usb3 --example usb3_capture -- samples [serial] [lane-mask-hex] [free]
//! ```

use std::process::ExitCode;
use std::time::Duration;

use kdi::{Acquisition, Aux, Kind, Stream};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (stream, default_lanes, want) = match args.next().as_deref() {
        Some("samples") | None => (Stream::Samples, 1 << 2, 6),
        Some("digital") => (Stream::Digital, 0, 1),
        Some(other) => {
            eprintln!("usage: usb3_capture [samples|digital] [serial]");
            eprintln!("unknown stream {other}");
            return ExitCode::FAILURE;
        }
    };
    let serial = args.next().unwrap_or_default();
    // An explicit mask overrides the default; `0x1` is lane 0.
    let lanes = args
        .next()
        .and_then(|a| u32::from_str_radix(a.trim_start_matches("0x"), 16).ok())
        .unwrap_or(default_lanes);
    // A fourth argument `free` drops the burst bound and nothing else, so the lane mask stays an
    // independent axis. That matters: `Acquisition::default()` changes BOTH at once (`lanes: !0,
    // burst: None`), and the first version of this arm used `default()` verbatim, which reported
    // SUSPECT without saying whether the free run or the 32-lane mask caused it. Pass
    // `ffffffff free` to reproduce `default()` exactly; vary one at a time to attribute a result.
    //
    // Free-running exercises two things a bounded burst cannot: the free-running gateware path,
    // and `stop()` on a stream that is still producing -- bounded, the stream has already stopped
    // itself by the time we ask, so that falling-edge flush is never tested.
    let free = args.next().as_deref() == Some("free");

    let mut dev = match kdi::Device::open_usb3(&serial, None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("could not open the board: {e}");
            return ExitCode::FAILURE;
        }
    };
    // EVERY HOST OWES THIS WRITE (#131, contract 0.5). An unconfigured device emits nothing on
    // `rhd_matrix`, and before the capability existed it emitted one frame per twenty sample
    // periods while declaring the full cadence. Non-fatal, so this still runs against pre-0.5
    // gateware, where the rate is whatever the last host left.
    if let Err(e) = dev.set_rate(42, 25) {
        eprintln!("note: rate not configured ({e}) - pre-0.5 gateware");
    }

    let (major, minor) = dev.kdi();
    println!("gateware_sha    {:08x}", dev.gateware_sha());
    println!("contract        {major}.{minor}");
    let acq = Acquisition {
        burst: if free { None } else { Some(want) },
        lanes,
    };
    let mut rx = match dev.start(stream, &acq) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("start failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut n_records = 0u32;
    let mut lost = 0u64;
    let mut lane_ids = String::from("-");
    let mut amp0_lane2 = String::from("-");
    let mut amp0_lane0 = String::from("-");
    // ONE SAMPLE PROVES NOTHING ABOUT THE ANALOGUE PATH. A lane nothing feeds returns 0xFFFF on
    // every row -- well-formed frames, right cadence, zero loss -- and a single reading cannot tell
    // that from a quiet channel. What separates them needs no known value: a lane nothing feeds is
    // CONSTANT, so its per-row variability is zero, while a lane carrying a headstage moves. That
    // is the claim this makes, and the verdict below states its limits.
    //
    // The statistic is a PER-ROW STANDARD DEVIATION, and the row it is taken over is why. A min-max
    // spread pooled over all 32 amplifier channels and compared against a single aux row is biased
    // by construction -- a range grows with the size of the pool -- so it reported a healthy
    // ordering on data that does not have one. Per-row stddev compares like with like.
    //
    // Rows 0..31 are the amplifier channels, 32 and 33 the two aux rows.
    let (mut sum, mut sumsq) = ([0f64; 34], [0f64; 34]);
    let mut sampled = 0u32;
    // A free-running stream never returns `Ok(None)`, so the wall clock is the only bound. No
    // elapsed time is printed -- these logs are diffed.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if free && std::time::Instant::now() >= deadline {
            break;
        }
        match rx.next(Duration::from_secs(10)) {
            Ok(Some(rec)) => {
                n_records += 1;
                lost = lost.saturating_add(rec.lost_before());
                if let Ok(Some(b)) = rec.block(Kind::RhdMatrix) {
                    if n_records == 1 {
                        let ids: Vec<String> = b.lanes().map(|id| id.to_string()).collect();
                        lane_ids = ids.join(",");
                        amp0_lane2 = format!("{:?}", b.amplifier(0, 2));
                        amp0_lane0 = format!("{:?}", b.amplifier(0, 0));
                    }
                    if let Some(lane) = b.lanes().next() {
                        for ch in 0..32u8 {
                            if let Some(v) = b.amplifier(ch, lane) {
                                let v = f64::from(v);
                                sum[usize::from(ch)] += v;
                                sumsq[usize::from(ch)] += v * v;
                                sampled += 1;
                            }
                        }
                        for (i, which) in [Aux::Temp, Aux::Supply].into_iter().enumerate() {
                            if let Some(v) = b.aux(which, lane) {
                                let v = f64::from(v);
                                sum[32 + i] += v;
                                sumsq[32 + i] += v * v;
                            }
                        }
                    }
                }
            }
            Ok(None) => break,
            Err(e) => {
                eprintln!("read failed: {e}");
                let _ = rx.stop();
                return ExitCode::FAILURE;
            }
        }
    }
    if let Err(e) = rx.stop() {
        eprintln!("stop: {e}");
        return ExitCode::FAILURE;
    }

    println!("n_records       {n_records}");
    println!("lane_ids        {lane_ids}");
    println!("amp0_lane2      {amp0_lane2}");
    println!("amp0_lane0      {amp0_lane0}");
    println!("lost_before     {lost}");
    if sampled > 0 {
        let n = f64::from(n_records);
        let sd = |i: usize| (sumsq[i] / n - (sum[i] / n).powi(2)).max(0.0).sqrt();
        let amp_sd = (0..32).map(sd).sum::<f64>() / 32.0;
        let (temp_sd, supply_sd) = (sd(32), sd(33));
        println!("amp_sd          {amp_sd:.1}   ({sampled} readings)");
        println!("aux_sd          {temp_sd:.1} temp, {supply_sd:.1} supply");
        // WHAT THIS DOES AND DOES NOT ESTABLISH. A lane nothing feeds reads a constant 0xffff, so
        // its stddev is 0 -- that separation is sound whatever the input is doing, and it is the
        // bug that actually shipped: a capture of a dead lane, well-formed and zero-loss.
        //
        // ROW ALIGNMENT IS NOT DECIDED HERE, and an earlier version of this example claimed it was.
        // The premise is that amplifier rows move more than the two slow aux rows, which holds only
        // for a DRIVEN input; on a bench with floating inputs both are noise and the comparison
        // decides nothing. It read LIVE anyway because it compared min-max spread pooled over 32
        // amplifier channels against 1 aux row, and a min-max range grows with the size of the pool
        // -- so it passed for a structural reason, not a measurement. Per-row stddev removes that
        // bias, and with it removed this bench's temp row is NOISIER than the mean amplifier row
        // (measured 5062 vs 4266 free-running, 4022 vs 1897 bounded; `tools/kdi_rhd_content.py`
        // independently reads the same shape, which is also what rules out a decoder fault).
        //
        // The authority on row order is the golden chip-model sim, which drives known values
        // through a modelled headstage (docs/rhd_chip_model.md, rhd/RhdCore.scala:236-247). The
        // numbers above are reported so a user with a driven input can apply the test themselves.
        println!(
            "verdict         {}",
            if amp_sd <= 5.0 {
                "IDLE - the amplifier rows do not move; this lane carries no headstage"
            } else {
                "LIVE - the amplifier rows move; this lane carries a headstage"
            }
        );
    }

    if let Err(e) = dev.close() {
        eprintln!("close: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
