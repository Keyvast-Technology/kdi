//! Decode a KDI blob and print what THIS codec saw, as JSON — the Rust half of the differential
//! harness: `decode_json [FILE|DIR]...`, or a blob on stdin, one JSON object per line. It calls
//! `kdi::codec` because the claim under test is the NORMATIVE decoder's, and it never judges.

use kdi::codec::{check_run_announcements, Frame, Header, Walk};
use serde_json::{json, Value};
use std::io::{Read, Write};

/// Everything one frame carries, flat enough for a field-by-field diff. `values` goes through
/// [`kdi::codec::Section::row`], never `body()`: row-major order is a property of the ACCESSOR,
/// raw bytes would compare Python's encoding with itself (contract.yaml:184-190).
fn frame_value(f: &Frame) -> Value {
    let h = f.header();
    let sections: Vec<Value> = f
        .sections()
        .map(|s| {
            let d = s.desc();
            // `row` is Some for every r < rows, and `section_words` was verified against the
            // geometry at parse time, so neither unwrap can fire on a frame that parsed.
            let values: Vec<Vec<u64>> = (0..d.rows)
                .map(|r| s.row(r).expect("r < rows").collect())
                .collect();
            json!({
                "kind": d.kind,
                "lane_ids": s.lane_ids().collect::<Vec<u16>>(),
                "rows": d.rows,
                "element_bits": d.element_bits,
                "section_words": d.section_words,
                "tick_num": d.tick_num,
                "tick_den": d.tick_den,
                "values": values,
            })
        })
        .collect();
    json!({
        "timestamp": h.timestamp,
        "flags": h.flags.0,
        "run_id": h.run_id,
        "layout": h.layout,
        "frame_words": h.frame_words,
        "hdr_words": h.hdr_words,
        "n_sections": h.n_sections,
        "contract_rev": h.contract_rev,
        "desc_words": h.desc_words,
        "sections": sections,
    })
}

fn decode_value(path: &str, blob: &[u8]) -> Value {
    let mut walk = Walk::new(blob);
    let mut items: Vec<Value> = Vec::new();
    let mut headers: Vec<Header> = Vec::new();
    for item in walk.by_ref() {
        match item {
            Ok(f) => {
                headers.push(*f.header());
                items.push(json!({ "frame": frame_value(&f) }));
            }
            // The offset is blob-relative, at the FIELD that failed; Python's `walk` raises a
            // `FrameError` with only the token (`kdi/frame.py:4-4`) and loses what it
            // decoded, so `items` holds frames AFTER a reject and difftest.py diffs the first.
            Err(e) => items.push(json!({"reject": {
                "reason": e.reason.token(),
                "offset": e.offset,
            }})),
        }
    }
    let c = walk.counters();
    json!({
        "path": path,
        "items": items,
        "counters": {
            "resync_bytes": c.resync_bytes,
            "unknown_kind": c.unknown_kind,
            "format_skipped": c.format_skipped,
        },
        "tail_bytes": walk.tail().len(),
        // The one CROSS-FRAME rule. Python runs it inside `walk` and raises; here it is a separate
        // call over the accepted headers so a decoder's verdict cannot depend on chunk size
        // (`kdi/rs/kdi/src/codec/mod.rs:474-488`). Reported as a token so the two are comparable.
        "run_announcements": match check_run_announcements(&headers) {
            Ok(()) => "ok",
            Err(r) => r.token(),
        },
    })
}

fn emit(out: &mut impl Write, path: &std::path::Path) -> std::io::Result<()> {
    let blob = std::fs::read(path)?;
    writeln!(out, "{}", decode_value(&path.display().to_string(), &blob))
}

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    if args.is_empty() {
        let mut blob = Vec::new();
        std::io::stdin().read_to_end(&mut blob)?;
        writeln!(out, "{}", decode_value("-", &blob))?;
        return out.flush();
    }
    for a in &args {
        let p = std::path::Path::new(a);
        // A DIRECTORY is one argument for a whole corpus, and that is not a convenience: the
        // harness runs this program inside a container through `sh -c`, where several hundred
        // argv paths have to survive a nested quoting layer. One sorted directory does not.
        if p.is_dir() {
            let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(p)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|f| f.extension().is_some_and(|x| x == "bin"))
                .collect();
            files.sort();
            for f in &files {
                emit(&mut out, f)?;
            }
        } else {
            emit(&mut out, p)?;
        }
    }
    out.flush()
}
