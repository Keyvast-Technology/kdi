//! Sparse-lane capture over USB3; no flash. `usb3_capture [samples|digital] [serial]
//! [lane-mask-hex] [free]`; `digital`'s burst 1 raises to 2 (88 B is not 16-B aligned). LANE
//! MASK: the chip is on lane 0, so `1 << 2` idles (`tools/kdi_rhd_content.py:4-4`).

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
    // independent axis (`Acquisition::default()` changes BOTH: `lanes: !0, burst: None`; pass
    // `ffffffff free` for it). Only free-running tests `stop()` on a stream still producing.
    let free = args.next().as_deref() == Some("free");

    let mut dev = match kdi::Device::open_usb3(&serial, None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("could not open the board: {e}");
            return ExitCode::FAILURE;
        }
    };
    // EVERY HOST OWES THIS WRITE (#131, contract 0.5). An unconfigured device emits nothing on
    // `rhd_matrix`; before the capability existed it emitted one frame per twenty sample periods
    // while declaring the full cadence. Non-fatal: pre-0.5 gateware keeps whatever rate was left.
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
    // ONE SAMPLE PROVES NOTHING: a lane nothing feeds returns 0xFFFF on every row -- well-formed,
    // right cadence, zero loss -- so the test is variability, not value. PER-ROW stddev, because a
    // min-max pooled over 32 channels is biased by pool size. Rows 0..31 amplifier, 32/33 aux.
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
        // ROW ALIGNMENT IS NOT DECIDED HERE: the premise needs a DRIVEN input; here the temp
        // row is NOISIER than the mean amplifier row (5062 vs 4266 free, 4022 vs 1897 bounded).
        // Row order's authority is the chip-model sim (rhd/RhdCore.scala:146-153).
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
