//! One bounded, GUI-free "is this instrument acquiring normally?": `health [serial] [--expect-sha
//! HEX] [--slot N]`, five falsifiable claims. **It DRIVES A PIN**: a state change on a shared
//! instrument. Claim 4 is the point — valid CRC proves the PIPE, not a dead or floating front end.

use kdi::{Acquisition, ChMode, Commands, Stream};
use std::time::Duration;

const READ_TIMEOUT: Duration = Duration::from_secs(2);

struct Report {
    fails: Vec<String>,
}

impl Report {
    fn check(&mut self, claim: &str, ok: bool, detail: String) {
        println!(
            "  {:<10} {:<5} {}",
            claim,
            if ok { "ok" } else { "FAIL" },
            detail
        );
        if !ok {
            self.fails.push(format!("{claim}: {detail}"));
        }
    }
}

fn main() -> std::process::ExitCode {
    let mut serial = String::new();
    let mut expect_sha: Option<u32> = None;
    let mut slot: u8 = 0;
    let mut args = std::env::args().skip(1);

    while let Some(a) = args.next() {
        match a.as_str() {
            "--expect-sha" => {
                expect_sha = args
                    .next()
                    .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
                if expect_sha.is_none() {
                    eprintln!("--expect-sha wants 8 hex digits");
                    return std::process::ExitCode::from(2);
                }
            }
            "--slot" => match args.next().and_then(|v| v.parse().ok()) {
                Some(s) if s <= 7 => slot = s,
                _ => {
                    eprintln!("--slot wants 0..7");
                    return std::process::ExitCode::from(2);
                }
            },
            other => serial = other.to_string(),
        }
    }

    let mut dev = match kdi::Device::open_usb3(&serial, None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("open failed: {e}");
            return std::process::ExitCode::from(2);
        }
    };
    // Start from a defined state. Without this the check measures whatever the previous
    // application left behind: measured on silicon, one record per twenty sample periods with the
    // sticky overrun clear, which looks exactly like a healthy stream running slowly.
    if let Err(e) = dev.quiesce() {
        eprintln!("quiesce failed: {e}");
        return std::process::ExitCode::from(2);
    }

    // AFTER the quiesce, never before: quiesce returns the device to UNCONFIGURED (contract 0.5),
    // so a rate set ahead of it is undone and the analogue check reports "rate is not configured"
    // from a host that did configure it. Non-fatal, so pre-0.5 gateware still runs.
    if let Err(e) = dev.set_rate(42, 25) {
        eprintln!("note: rate not configured ({e}) - pre-0.5 gateware");
    }

    let mut r = Report { fails: Vec::new() };

    // 1. identity ---------------------------------------------------------------
    let sha = dev.gateware_sha();
    let (maj, min) = dev.kdi();
    match expect_sha {
        Some(want) => r.check(
            "identity",
            sha == want,
            format!("gateware_sha 0x{sha:08x} (want 0x{want:08x}), contract {maj}.{min}"),
        ),
        // Without --expect-sha this REPORTS rather than gates: a result that cannot say what
        // it ran against is not evidence, so print the sha and let the caller pin it.
        None => r.check(
            "identity",
            true,
            format!(
                "gateware_sha 0x{sha:08x}, contract {maj}.{min} \
                     (not gated: pass --expect-sha)"
            ),
        ),
    }

    // 2. ready ------------------------------------------------------------------
    let ready = dev.wait_ready(Duration::from_secs(5)).is_ok();
    let power = dev.power_status();
    let present = power.as_ref().map(|p| p.present).unwrap_or(0);
    r.check(
        "ready",
        ready && power.is_ok(),
        format!("contract_ready={ready}, present=0x{present:02x}"),
    );

    // 3. transport --------------------------------------------------------------
    // Bounded, not free-running: a bound is the only mode where loss cannot happen INSIDE
    // the data, so "no gaps" is a claim the reader can actually make.
    let want = 64u16;
    let mut ts: Vec<u64> = Vec::new();
    let mut firsts = 0usize;
    let mut lost = 0u64;
    match dev.start(
        Stream::Digital,
        &Acquisition {
            lanes: u32::MAX,
            burst: Some(want),
        },
    ) {
        Ok(mut rd) => {
            while ts.len() < want as usize {
                match rd.next(READ_TIMEOUT) {
                    Ok(Some(rec)) => {
                        ts.push(rec.timestamp());
                        lost += rec.lost_before();
                        if rec.first_of_run() {
                            firsts += 1;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        r.check("transport", false, format!("read error: {e}"));
                        break;
                    }
                }
            }
            let overrun = rd.health().map(|h| h.overrun).unwrap_or(true);
            let stats = rd.stats();
            let _ = rd.stop();
            let mono = ts.windows(2).all(|w| w[1] > w[0]);
            r.check(
                "transport",
                !ts.is_empty()
                    && mono
                    && !overrun
                    && lost == 0
                    && stats.bad_frames == 0
                    && firsts == 1,
                format!(
                    "{} frames, mono={mono}, first_of_run={firsts}, lost={lost}, \
                         overrun={overrun}, bad_frames={}",
                    ts.len(),
                    stats.bad_frames
                ),
            );
        }
        Err(e) => r.check("transport", false, format!("start failed: {e}")),
    }

    // 4. stimulus ---------------------------------------------------------------
    // CH1 drives, CH2 reads through the module tie, both edges. Read via `adio.dout`'s echoed
    // `din`: a driven channel exports 0 on its own TTL lane, so a lane-based check would be wrong.
    let mut stim = String::new();
    let mut stim_ok = present & (1 << slot) != 0;
    if !stim_ok {
        stim = format!("slot {slot} not present - front end UNTESTED");
    } else {
        stim_ok =
            dev.adio_mode(slot, ChMode::Out, ChMode::In).is_ok() && dev.adio_fb(slot, 1).is_ok();
        for level in [1u8, 0, 1, 0] {
            match dev.adio_dout(slot, level) {
                Ok(rep) => {
                    let ch2 = (rep.din >> 1) & 1;
                    if ch2 != level {
                        stim_ok = false;
                        stim = format!("drove {level}, CH2 read {ch2}");
                    }
                }
                Err(e) => {
                    stim_ok = false;
                    stim = format!("adio.dout failed: {e}");
                }
            }
        }
        if stim.is_empty() {
            stim = format!("slot {slot} CH1->CH2 through the tie, 4/4 edges");
        }
        // Back to both-inputs, never `off`: `off` exports 0 and leaves the lanes dead.
        let _ = dev.adio_dout(slot, 0);
        let _ = dev.adio_fb(slot, 0);
        let _ = dev.adio_mode(slot, ChMode::In, ChMode::In);
    }
    r.check("stimulus", stim_ok, stim);

    // 5. analogue ---------------------------------------------------------------
    match dev.start(
        Stream::Samples,
        &Acquisition {
            lanes: 1,
            burst: Some(16),
        },
    ) {
        Ok(mut rd) => {
            let mut n = 0usize;
            while n < 16 {
                match rd.next(READ_TIMEOUT) {
                    Ok(Some(_)) => n += 1,
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            let bad = rd.stats().bad_frames;
            let _ = rd.stop();
            // Frame COUNT only. Row content needs a driven input to mean anything, and a
            // min-max spread across row groups reports "live" almost regardless of the data.
            r.check(
                "analogue",
                n > 0 && bad == 0,
                format!("{n} rhd_matrix frames, bad_frames={bad} (content not asserted)"),
            );
        }
        Err(e) => r.check("analogue", false, format!("start failed: {e}")),
    }

    if r.fails.is_empty() {
        println!("\nRESULT: PASS");
        std::process::ExitCode::SUCCESS
    } else {
        println!("\nRESULT: FAIL ({})", r.fails.join("; "));
        std::process::ExitCode::FAILURE
    }
}
