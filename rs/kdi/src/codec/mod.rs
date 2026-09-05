//! KDI format 2 — the self-describing frame container, decoded. Every length, lane identity,
//! element width and cadence is ON THE WIRE (contract.yaml:165-183), so nothing here takes a
//! descriptor; it interprets no payload — row order, encoding, scale are semantics (:317-354).

// INNER attributes, so they cover every descendant and an inner `allow` cannot undo the `forbid`.
// `#![no_std]` did NOT survive the merge — it has no module form. `tests/codec_isolation.rs`
// replaces it and is WEAKER (a re-export can hide a `std` path): do not add `use std::` here.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod spec;
pub use spec::*;

/// Compile-time proof the crate version matches the contract it implements. §12 asks for this;
/// doing it as a const assert needs no build.rs and no descriptor on disk.
const _: () = assert!(version_matches(env!("CARGO_PKG_VERSION"), KDI_VERSION));

/// `pkg` must be `<KDI_VERSION>.<patch>` — the crate version tracks the CONTRACT version, not the
/// repo's release tag (`kdi/rs/Cargo.toml:4-4`), so the major.minor is not free to drift and the
/// patch is the only component this crate may pick.
const fn version_matches(pkg: &str, kdi: &str) -> bool {
    let (p, k) = (pkg.as_bytes(), kdi.as_bytes());
    if p.len() <= k.len() || p[k.len()] != b'.' {
        return false;
    }
    let mut i = 0;
    while i < k.len() {
        if p[i] != k[i] {
            return false;
        }
        i += 1;
    }
    true
}

// ─────────────────────────────────────────────────────────────────────── rejection

/// A rejection plus where it was found. `offset` is bytes from the start of the slice handed to
/// [`Frame::parse`], or from the start of the blob when it came out of a [`Walk`] — a walk over a
/// megabyte read is useless if it can only say "somewhere in here".
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct RejectAt {
    /// Which rule the frame broke, as the contract's own token.
    pub reason: Reject,
    /// Bytes from the start of the slice that was decoded — the frame's own start for
    /// [`Frame::parse`], the blob's for a [`Walk`]. Points at the FIELD that failed, not at the
    /// frame, so a report can name the descriptor rather than the read.
    pub offset: usize,
}

impl RejectAt {
    const fn at(reason: Reject, offset: usize) -> Self {
        Self { reason, offset }
    }
}

/// A singular accessor met a frame that legally carries more than one section of that kind.
/// NOT a `Reject`: the frame is valid (`dup_kinds_ok`, contract.yaml:97) and this is host-API
/// misuse — `ambiguous_kind` is in `host_reject_tokens` (:438-443), never counted against a device.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Ambiguous {
    /// The `kind` code that was asked for.
    pub kind: u8,
    /// How many sections of it the frame actually carries. Always 2 or more — one is not
    /// ambiguous, and zero is `Ok(None)`.
    pub count: u16,
}

impl Ambiguous {
    /// The contract token, so a host can log this in the same vocabulary as a `Reject`.
    pub const fn token(self) -> &'static str {
        "ambiguous_kind"
    }
}

impl Reject {
    /// Is the declared LENGTH still trustworthy after this rejection? THE DRIFT MECHANISM: one
    /// hand-written exhaustive match, no wildcard arm ever. Only `crc_err`/`bad_length` leave it
    /// unverified; past a CRC, a rescan mints a phantom frame from any `4b 56 44 46` in a sample.
    pub fn resyncable(self) -> bool {
        match self {
            Reject::CrcErr => true,
            Reject::BadLength => true,
            Reject::SectionWords => false,
            Reject::LaneIds => false,
            Reject::ReservedBits => false,
            Reject::TickSane => false,
            Reject::TimestampTop16 => false,
            Reject::FirstOfRunDup => false,
            Reject::DescWords => false,
            Reject::HdrWordsFits => false,
            Reject::BodyFitsFrame => false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────── header

/// Header flags. Reserved bits are rejected rather than masked off (`reserved_reject`,
/// `kdi/contract.yaml:97`), so a `Flags` that exists has only defined bits set.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Flags(pub u16);

impl Flags {
    /// The start of a contiguous segment of THIS stream. Its timestamp need not be 0 and there may
    /// be several per `run_id` — `run_id` is the device-wide epoch (`kdi/contract.yaml:97-97`).
    pub const fn first_of_run(self) -> bool {
        self.0 & FLAG_FIRST_OF_RUN != 0
    }
    /// The device dropped data before this frame. The frame is intact; what is lost is its
    /// contiguity with the one before it.
    pub const fn words_dropped(self) -> bool {
        self.0 & FLAG_WORDS_DROPPED != 0
    }

    /// The sampling cadence changed at this frame. Re-read [`Section::cadence`] rather than
    /// carrying the previous rational forward — the loss oracle is computed from it.
    pub const fn rate_event(self) -> bool {
        self.0 & FLAG_RATE_EVENT != 0
    }
}

/// A frame's fixed header, decoded. Every field is verified by [`Frame::parse`] before a `Header`
/// exists; `frame_words` and `hdr_words` are kept because stepping over a frame is done by length,
/// never by scanning for the next magic.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Header {
    /// Shared-timebase ticks at which this frame was sampled, 48 significant bits. ONE free-running
    /// counter per device, sampled per frame by every section, so aligning two streams is exact
    /// integer subtraction. Deltas JITTER by design: 30 kS/s is 10000/3, so gaps run 3333/3334.
    pub timestamp: u64,
    /// The header flag word, reserved bits already rejected.
    pub flags: Flags,
    /// The DEVICE-WIDE acquisition epoch, not this stream's. Two streams can hold one open, so a
    /// per-stream rule keyed on `run_id` must compare within it and not across it.
    pub run_id: u16,
    /// OPAQUE: a cache key for a derived kind/lane -> channel mapping, never a branch
    /// (`kdi/contract.yaml:83-83`).
    pub layout: u16,
    /// The WHOLE frame in 16-bit words, trailer included. A multiple of 4, and the only way to step
    /// over a frame — including one whose `format` this build does not decode.
    pub frame_words: u32,
    /// Header, descriptors, lane ids and zero padding in 16-bit words. Bodies start at
    /// `hdr_words * 2`; it is NOT `HDR_BYTES / 2`, which covers only the fixed part.
    pub hdr_words: u16,
    /// How many section descriptors follow the fixed header.
    pub n_sections: u16,
    /// The contract MINOR the device emitted this under. Informational: a higher one is legal and
    /// additive, and the frame is self-describing either way.
    pub contract_rev: u16,
    /// The descriptor stride ON THE WIRE. Never assume `DESC_WORDS_MIN` (`desc_stride`,
    /// `kdi/contract.yaml:97`).
    pub desc_words: u16,
}

/// One section's descriptor: what the section is, how big it is, and how fast it is sampled.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct SectionDesc {
    /// The raw `kind` code. [`Section::kind`] resolves it; a code this build does not know is
    /// legal and is skipped by `section_words`, so the raw byte is kept for logging it.
    pub kind: u8,
    /// Lanes in this section. Each has an id in the lane-id array and an index in the body.
    pub n_lanes: u16,
    /// `words_per_lane` on the wire: rows per lane in this frame.
    pub rows: u16,
    /// Decoded from `dflags[2:1]`; 16, 1, 32 or 64.
    pub element_bits: u8,
    /// The AUTHORITATIVE body length in 16-bit words, cross-checked against the geometry above at
    /// parse time. Also how far to skip a section this build cannot interpret.
    pub section_words: u16,
    /// Numerator of the ticks-per-sample rational. See [`Section::cadence`] for why this is not
    /// a rate in Hz.
    pub tick_num: u32,
    /// Denominator of the ticks-per-sample rational. Never 0 — a frame declaring one is rejected
    /// as `tick_sane`, because it divides by zero in the loss oracle.
    pub tick_den: u16,
}

// ─────────────────────────────────────────────────────────────────────── little-endian reads
// Every read is bounds-checked and returns Option. A decoder whose inputs are hostile by default (a
// truncated USB read is the normal case) must have no panicking path: hosts record for hours.

fn rd16(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn rd32(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn rd64(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// Read a descriptor without validating it. Safe to call only on a frame that already passed
/// [`Frame::parse`]; `None` means the index or the frame is out of range.
fn desc_at(f: &[u8], desc_words: u16, i: u16) -> Option<SectionDesc> {
    let off = DESCRIPTORS_AT + i as usize * desc_words as usize * 2;
    let dflags = *f.get(off + DOFF_DFLAGS)?;
    Some(SectionDesc {
        kind: *f.get(off + DOFF_KIND)?,
        n_lanes: rd16(f, off + DOFF_N_LANES)?,
        rows: rd16(f, off + DOFF_WORDS_PER_LANE)?,
        element_bits: ELEM_BITS[(dflags >> 1) as usize & 3],
        section_words: rd16(f, off + DOFF_SECTION_WORDS)?,
        tick_num: rd32(f, off + DOFF_TICK_NUM)?,
        tick_den: rd16(f, off + DOFF_TICK_DEN)?,
    })
}

// ─────────────────────────────────────────────────────────────────────── frame

/// One validated frame, borrowing the blob it was decoded from. Nothing is copied out of the body:
/// a 32-lane rhd_matrix frame is 2 KB and a host reads thousands per second.
#[derive(Copy, Clone)]
pub struct Frame<'a> {
    bytes: &'a [u8],
    hdr: Header,
}

impl<'a> Frame<'a> {
    /// Validate and decode one format-2 frame at the start of `bytes`; trailing bytes are ignored.
    /// `format` is NOT checked here — an unrecognised one must be SKIPPED by `frame_words`
    /// (`unknown_kind`, :395), [`Walk`]'s job, so a caller framing its own reads must gate on it.
    pub fn parse(bytes: &'a [u8]) -> Result<Frame<'a>, RejectAt> {
        // Length first, and before the CRC: the declared length is what says where the CRC even is.
        if bytes.len() < HDR_BYTES {
            return Err(RejectAt::at(Reject::BadLength, 0));
        }
        let frame_words =
            rd32(bytes, OFF_FRAME_WORDS).ok_or(RejectAt::at(Reject::BadLength, OFF_FRAME_WORDS))?;
        let min_words = (HDR_BYTES + CRC_BYTES) as u32 / 2;
        // `alignment`, contract.yaml:97 — frame_words % 4 == 0.
        if frame_words % 4 != 0 || frame_words < min_words {
            return Err(RejectAt::at(Reject::BadLength, OFF_FRAME_WORDS));
        }
        // u64, not `as usize * 2`: `frame_words` is raw wire bounded only by the checks above, so
        // it reaches 0xFFFFFFFC and doubling leaves 32 bits — on a 32-bit host (this crate builds
        // riscv32imc) that WRAPS in release and slices a live frame out of hostile input.
        let total = frame_words as u64 * 2;
        let f = usize::try_from(total)
            .ok()
            .and_then(|t| bytes.get(..t))
            .ok_or(RejectAt::at(Reject::BadLength, OFF_FRAME_WORDS))?;
        let total = f.len();

        // `crc_residue`, contract.yaml:97: the residue form, so no separate slice of the payload.
        if crc32(f) != CRC_RESIDUE {
            return Err(RejectAt::at(Reject::CrcErr, total - CRC_BYTES));
        }

        let ts = rd64(f, OFF_TIMESTAMP).ok_or(RejectAt::at(Reject::BadLength, OFF_TIMESTAMP))?;
        if ts >> 48 != 0 {
            return Err(RejectAt::at(Reject::TimestampTop16, OFF_TIMESTAMP));
        }
        let flags = rd16(f, OFF_FLAGS).ok_or(RejectAt::at(Reject::BadLength, OFF_FLAGS))?;
        if flags & !FLAGS_DEFINED != 0 {
            return Err(RejectAt::at(Reject::ReservedBits, OFF_FLAGS));
        }

        let hdr = Header {
            timestamp: ts,
            flags: Flags(flags),
            run_id: rd16(f, OFF_RUN_ID).ok_or(RejectAt::at(Reject::BadLength, OFF_RUN_ID))?,
            layout: rd16(f, OFF_LAYOUT).ok_or(RejectAt::at(Reject::BadLength, OFF_LAYOUT))?,
            frame_words,
            hdr_words: rd16(f, OFF_HDR_WORDS)
                .ok_or(RejectAt::at(Reject::BadLength, OFF_HDR_WORDS))?,
            n_sections: rd16(f, OFF_N_SECTIONS)
                .ok_or(RejectAt::at(Reject::BadLength, OFF_N_SECTIONS))?,
            contract_rev: rd16(f, OFF_CONTRACT_REV)
                .ok_or(RejectAt::at(Reject::BadLength, OFF_CONTRACT_REV))?,
            desc_words: rd16(f, OFF_DESC_WORDS)
                .ok_or(RejectAt::at(Reject::BadLength, OFF_DESC_WORDS))?,
        };
        if hdr.desc_words < DESC_WORDS_MIN {
            return Err(RejectAt::at(Reject::DescWords, OFF_DESC_WORDS));
        }

        // hdr_words is bounded BOTH sides; the LOWER bound is load-bearing: lane ids come from
        // the descriptor block, bodies from `hdr_words`, so a mismatch (a wrong hdrWords in
        // KdiRhdFrame.scala) decodes right samples under wrong labels (kdi/frame.py:56-56).
        let hdr_bytes = hdr.hdr_words as usize * 2;
        if hdr_bytes > total - CRC_BYTES {
            return Err(RejectAt::at(Reject::HdrWordsFits, OFF_HDR_WORDS));
        }
        // `alignment` again (hdr_words % 4). Rejected under the EXISTING hdr_words_fits token on
        // purpose: a new token is new vocabulary every third-party decoder would have to
        // reproduce, and the frame is rejected either way (kdi/frame.py:56-56).
        if !hdr.hdr_words.is_multiple_of(4) {
            return Err(RejectAt::at(Reject::HdrWordsFits, OFF_HDR_WORDS));
        }
        // LOAD-BEARING: every descriptor offset below is `DESCRIPTORS_AT + i * desc_words * 2`, so
        // this one comparison bounds all of them before a descriptor byte is read. u64 because both
        // factors are raw u16s — 0xFFFF * 0xFFFF * 2 is 8.6e9, which wraps a 32-bit usize.
        let descs_end = DESCRIPTORS_AT as u64 + hdr.n_sections as u64 * hdr.desc_words as u64 * 2;
        if descs_end > hdr_bytes as u64 {
            return Err(RejectAt::at(Reject::HdrWordsFits, OFF_HDR_WORDS));
        }
        let descs_end = descs_end as usize; // <= hdr_bytes, so it fits

        let mut lane_off = descs_end;
        for i in 0..hdr.n_sections {
            let off = DESCRIPTORS_AT + i as usize * hdr.desc_words as usize * 2;
            let dflags = *f
                .get(off + DOFF_DFLAGS)
                .ok_or(RejectAt::at(Reject::HdrWordsFits, off))?;
            // dflags[0] and dflags[7:3] are reserved; only [2:1] is the element width.
            if dflags & !0b110 != 0 {
                return Err(RejectAt::at(Reject::ReservedBits, off + DOFF_DFLAGS));
            }
            // Reject, not ignore: two implementations that differ here diverge permanently the day
            // anything is put in this slot (`reserved_reject`, contract.yaml:97).
            if rd16(f, off + DOFF_PAD).ok_or(RejectAt::at(Reject::HdrWordsFits, off))? != 0 {
                return Err(RejectAt::at(Reject::ReservedBits, off + DOFF_PAD));
            }
            let d = desc_at(f, hdr.desc_words, i).ok_or(RejectAt::at(Reject::HdrWordsFits, off))?;
            // A zero divides by zero in the published loss oracle, in every host
            // (`tick_sane`, contract.yaml:97).
            if d.tick_num < 1 || d.tick_den < 1 {
                return Err(RejectAt::at(Reject::TickSane, off + DOFF_TICK_NUM));
            }
            if d.section_words as u32 != section_words(d.n_lanes, d.rows, d.element_bits) {
                return Err(RejectAt::at(Reject::SectionWords, off + DOFF_SECTION_WORDS));
            }
            // Lane ids ascend and are unique WITHIN a section (contract.yaml:97): descending ids
            // decode to real values, so the samples are right and the labels wrong — undetectable
            // downstream (kdi/golden.py:32-36). Also an overflow guard: `lane_off` wraps 32-bit.
            if lane_off > f.len() {
                return Err(RejectAt::at(Reject::HdrWordsFits, OFF_HDR_WORDS));
            }
            let mut prev: Option<u16> = None;
            for l in 0..d.n_lanes as usize {
                let id = rd16(f, lane_off + l * 2)
                    .ok_or(RejectAt::at(Reject::HdrWordsFits, lane_off + l * 2))?;
                if prev.is_some_and(|p| id <= p) {
                    return Err(RejectAt::at(Reject::LaneIds, lane_off + l * 2));
                }
                prev = Some(id);
            }
            lane_off = lane_off.saturating_add(d.n_lanes as usize * 2);
        }
        // The lane-id block must ALSO land inside the declared header: this is the one place both
        // halves of the header can be reconciled against hdr_words.
        if lane_off > hdr_bytes {
            return Err(RejectAt::at(Reject::HdrWordsFits, OFF_HDR_WORDS));
        }

        let mut body = hdr_bytes;
        for i in 0..hdr.n_sections {
            let off = DESCRIPTORS_AT + i as usize * hdr.desc_words as usize * 2;
            let d = desc_at(f, hdr.desc_words, i).ok_or(RejectAt::at(Reject::HdrWordsFits, off))?;
            body += d.section_words as usize * 2;
            if body > total - CRC_BYTES {
                return Err(RejectAt::at(
                    Reject::BodyFitsFrame,
                    off + DOFF_SECTION_WORDS,
                ));
            }
        }

        Ok(Frame { bytes: f, hdr })
    }

    /// This frame's decoded header.
    pub fn header(&self) -> &Header {
        &self.hdr
    }

    /// Exactly `frame_words * 2` bytes, trailer included — what a CRC or an archive wants.
    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Every section, in descriptor order — including kinds this build does not know.
    pub fn sections(&self) -> SectionIter<'a> {
        SectionIter { frame: *self, i: 0 }
    }

    /// The `i`th section in descriptor order, or `None` past the last.
    pub fn section_at(&self, i: u16) -> Option<Section<'a>> {
        if i >= self.hdr.n_sections {
            return None;
        }
        // Lane ids and bodies are both in DESCRIPTOR ORDER with no index anywhere on the wire, so
        // reaching section i means walking the descriptors before it. O(i) with i <= n_sections,
        // which is 2 on every emitter today; an index array would be the only heap in the crate.
        let mut lane_off =
            DESCRIPTORS_AT + self.hdr.n_sections as usize * self.hdr.desc_words as usize * 2;
        let mut body_off = self.hdr.hdr_words as usize * 2;
        let mut j = 0u16;
        let desc = loop {
            let d = desc_at(self.bytes, self.hdr.desc_words, j)?;
            if j == i {
                break d;
            }
            lane_off += d.n_lanes as usize * 2;
            body_off += d.section_words as usize * 2;
            j += 1;
        };
        Some(Section {
            desc,
            lanes: self
                .bytes
                .get(lane_off..lane_off + desc.n_lanes as usize * 2)?,
            body: self
                .bytes
                .get(body_off..body_off + desc.section_words as usize * 2)?,
        })
    }

    /// `Ok(None)` = absent, `Err` = duplicated. TWO channels for two kinds of absence, because
    /// `dup_kinds_ok` (contract.yaml:97) makes duplicates LEGAL and returning the first silently
    /// hands back half the data.
    pub fn section(&self, kind: u8) -> Result<Option<Section<'a>>, Ambiguous> {
        let count = self.sections_of(kind).count();
        if count > 1 {
            return Err(Ambiguous {
                kind,
                count: count as u16,
            });
        }
        Ok(self.sections_of(kind).next())
    }

    /// Every section of one kind. The plural form, and the answer to an [`Ambiguous`] from
    /// [`Frame::section`].
    pub fn sections_of(&self, kind: u8) -> impl Iterator<Item = Section<'a>> + 'a {
        self.sections().filter(move |s| s.desc.kind == kind)
    }
}

/// Iterator over a frame's sections, in descriptor order. Returned by [`Frame::sections`].
pub struct SectionIter<'a> {
    frame: Frame<'a>,
    i: u16,
}

impl<'a> Iterator for SectionIter<'a> {
    type Item = Section<'a>;

    fn next(&mut self) -> Option<Section<'a>> {
        let s = self.frame.section_at(self.i)?;
        self.i += 1;
        Some(s)
    }
}

// ─────────────────────────────────────────────────────────────────────── section

/// One section: its descriptor plus borrowed views of its lane-id array and its body.
#[derive(Copy, Clone)]
pub struct Section<'a> {
    desc: SectionDesc,
    lanes: &'a [u8],
    body: &'a [u8],
}

impl<'a> Section<'a> {
    /// This section's descriptor: kind, geometry and cadence.
    pub fn desc(&self) -> &SectionDesc {
        &self.desc
    }

    /// `None` = a kind this build does not know. Legal, and the caller must skip the section by
    /// `section_words` rather than treat it as a fault (`unknown_kind`, contract.yaml:97).
    pub fn kind(&self) -> Option<Kind> {
        Kind::from_code(self.desc.kind)
    }

    /// This section's lane ids, ascending. THE ID IS A LABEL, NOT AN INDEX: every accessor below
    /// takes a lane's position in this iterator, and the id says which physical thing it is.
    pub fn lane_ids(&self) -> impl ExactSizeIterator<Item = u16> + 'a {
        // chunks_exact(2) yields slices of exactly 2, so the indexing cannot panic.
        self.lanes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
    }

    /// The id of lane `i`, or `None` past the last lane.
    pub fn lane_id(&self, i: u16) -> Option<u16> {
        rd16(self.lanes, i as usize * 2)
    }

    /// ROW-MAJOR: `body + (row * n_lanes + lane) * element_bits/8`; a 1-bit body packs LSB =
    /// first lane. NOT observable at rows == 1: a `(lane * rows + row)` decoder yields
    /// plausible data at the wrong channel on rhd_matrix's 35 rows — PR #15, :250-256.
    pub fn element(&self, row: u16, lane: u16) -> Option<u64> {
        if row >= self.desc.rows || lane >= self.desc.n_lanes {
            return None;
        }
        let (row, lane, n) = (row as usize, lane as usize, self.desc.n_lanes as usize);
        if self.desc.element_bits == 1 {
            // ceil(n_lanes/16) little-endian words per row, LSB first: 16 digital lines cost ONE
            // word (`bit_packed`, contract.yaml:97-97).
            let per_row = n.div_ceil(16);
            let w = rd16(self.body, (row * per_row + (lane >> 4)) * 2)?;
            return Some(((w >> (lane & 15)) & 1) as u64);
        }
        let step = self.desc.element_bits as usize / 8;
        let at = (row * n + lane) * step;
        let s = self.body.get(at..at + step)?;
        Some(
            s.iter()
                .enumerate()
                .fold(0u64, |v, (k, b)| v | (*b as u64) << (8 * k)),
        )
    }

    /// One whole row, lane by lane in lane-id order. `None` for a row past `rows`.
    pub fn row(&self, row: u16) -> Option<impl ExactSizeIterator<Item = u64> + 'a> {
        if row >= self.desc.rows {
            return None;
        }
        let s = *self;
        // unwrap_or is unreachable: `row` is bounds-checked above, `lane` runs below n_lanes, and
        // the body slice is exactly section_words * 2 bytes because `section_words` was verified
        // against the geometry at parse time.
        Some((0..self.desc.n_lanes).map(move |l| s.element(row, l).unwrap_or(0)))
    }

    /// Exactly `section_words * 2` bytes.
    pub fn body(&self) -> &'a [u8] {
        self.body
    }

    /// Ticks per sample as an exact rational. RATE IS NEVER NORMATIVE IN THIS CONTRACT, THE
    /// TIMEBASE IS (contract.yaml:123-127): 30 kS/s is 10000/3, and an integer 3333 drifts 1.44 s
    /// over a four-hour recording.
    pub fn cadence(&self) -> (u32, u16) {
        (self.desc.tick_num, self.desc.tick_den)
    }
}

// ─────────────────────────────────────────────────────────────────────── walk

/// The three decoder counters (contract.yaml:153-153). They are things a conforming decoder does
/// QUIETLY — a mismatch on the published stream vector is the only way to test that it did.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Counters {
    /// Bytes stepped over while scanning forward for the next magic.
    pub resync_bytes: usize,
    /// Sections whose `kind` is not in this build's registry.
    pub unknown_kind: usize,
    /// Whole frames stepped over by `frame_words` because `format` was unrecognised.
    pub format_skipped: usize,
}

/// Decode every whole frame in a blob. TOTAL and chunk-insensitive: one corrupt frame is ONE `Err`
/// item and the walk carries on — the Python reference raises out of `walk()` and loses every frame
/// already decoded. After `None`, [`Walk::tail`] is the remainder to carry into the next read.
pub struct Walk<'a> {
    blob: &'a [u8],
    pos: usize,
    counters: Counters,
}

impl<'a> Walk<'a> {
    /// Start walking `blob`. Nothing is copied; every frame borrows it.
    pub fn new(blob: &'a [u8]) -> Walk<'a> {
        Walk {
            blob,
            pos: 0,
            counters: Counters::default(),
        }
    }

    /// Carry into the next read. Empty once the walk has resynced to the end of the blob: bytes
    /// that are not the start of a frame are consumed, never carried.
    pub fn tail(&self) -> &'a [u8] {
        &self.blob[self.pos..]
    }

    /// What this walk stepped over quietly, cumulative so far. Read it after the iterator is
    /// drained: a healthy link leaves it all zero, and that is the assertion worth making.
    pub fn counters(&self) -> Counters {
        self.counters
    }
}

fn find_magic(b: &[u8], from: usize) -> Option<usize> {
    let m = MAGIC.to_le_bytes();
    let last = b.len().checked_sub(m.len())?;
    (from..=last).find(|&i| b[i..i + m.len()] == m)
}

impl<'a> Iterator for Walk<'a> {
    type Item = Result<Frame<'a>, RejectAt>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Less than a header left: whatever it is, it is the caller's tail.
            if self.pos + HDR_BYTES > self.blob.len() {
                return None;
            }
            if rd32(self.blob, self.pos)? != MAGIC {
                // Resync — the anchor's ONLY job (`magic_is_anchor`, contract.yaml:97).
                match find_magic(self.blob, self.pos + 1) {
                    Some(next) => {
                        self.counters.resync_bytes += next - self.pos;
                        self.pos = next;
                        continue;
                    }
                    None => {
                        self.counters.resync_bytes += self.blob.len() - self.pos;
                        self.pos = self.blob.len();
                        return None;
                    }
                }
            }
            let base = self.pos;
            let f = &self.blob[base..];
            let frame_words = rd32(f, OFF_FRAME_WORDS)?;
            if frame_words % 4 != 0 || frame_words < (HDR_BYTES + CRC_BYTES) as u32 / 2 {
                // Nothing here is trustworthy, so step one byte and let the resync path above count
                // the rest of the wreckage.
                self.pos = base + 1;
                self.counters.resync_bytes += 1;
                return Some(Err(RejectAt::at(Reject::BadLength, base + OFF_FRAME_WORDS)));
            }
            // u64 for the same reason as `Frame::parse`: on a 32-bit usize `base + frame_words * 2`
            // wraps to a SMALL `end`, sails past the bound below and then panics slicing with
            // base > end — four bytes of hostile input, against a no-panicking-path promise.
            let end = base as u64 + frame_words as u64 * 2;
            if end > self.blob.len() as u64 {
                return None; // a partial trailing frame: carry it, do not judge it
            }
            let end = end as usize;
            // An unrecognised format is SKIPPABLE BY LENGTH: magic/format/frame_words are frozen at
            // these offsets for every present and future format, which is what makes this safe
            // (contract.yaml:97-97).
            if rd16(f, OFF_FORMAT)? != FORMAT {
                self.counters.format_skipped += 1;
                self.pos = end;
                continue;
            }
            return Some(match Frame::parse(&self.blob[base..end]) {
                Ok(frame) => {
                    self.counters.unknown_kind +=
                        frame.sections().filter(|s| s.kind().is_none()).count();
                    self.pos = end;
                    Ok(frame)
                }
                Err(e) => {
                    // A CRC-verified length may be stepped over exactly; an unverified one leaves
                    // resync as the only move. See `Reject::resyncable`.
                    if e.reason.resyncable() {
                        self.pos = base + 1;
                        self.counters.resync_bytes += 1;
                    } else {
                        self.pos = end;
                    }
                    Err(RejectAt::at(e.reason, base + e.offset))
                }
            });
        }
    }
}

/// The one CROSS-FRAME rule, exported separately: [`Walk`] stays a pure function of its blob —
/// a verdict must not depend on chunk size. `first_of_run` stamps strictly increase WITHIN a
/// `run_id`: "must be 0" accepted #78 (31 flagged frames at 0).
pub fn check_run_announcements(headers: &[Header]) -> Result<(), Reject> {
    for (i, h) in headers.iter().enumerate() {
        if !h.flags.first_of_run() {
            continue;
        }
        // The previous announcement in the same run_id. A backward scan rather than a map because
        // this crate has no allocator; announcements are rare, so the quadratic term is over the
        // FLAGGED frames in one read, not over the read.
        let prev = headers[..i]
            .iter()
            .rev()
            .find(|p| p.flags.first_of_run() && p.run_id == h.run_id);
        if prev.is_some_and(|p| h.timestamp <= p.timestamp) {
            return Err(Reject::FirstOfRunDup);
        }
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────── helpers

/// Frames lost between two timestamps of ONE stream: `round(dt * den / num) - 1`. Never across
/// streams (`loss_oracle`, contract.yaml:97; `not_co_sampled`, :413). Reproduces Python's
/// HALF-TO-EVEN tie break: `(x + 0.5) as i64` diverges, and ties are routine at 10000/3.
pub fn lost_frames(dt_ticks: u64, tick_num: u32, tick_den: u16) -> Result<i64, Reject> {
    if tick_num < 1 || tick_den < 1 {
        return Err(Reject::TickSane);
    }
    // u128: a 48-bit dt times a 16-bit den cannot overflow u64 today, but dt_ticks is a u64 in the
    // signature and a caller subtracting stamps in the wrong order must not wrap into nonsense.
    let (n, d) = (dt_ticks as u128 * tick_den as u128, tick_num as u128);
    let (q, r) = (n / d, n % d);
    let q = match (2 * r).cmp(&d) {
        core::cmp::Ordering::Greater => q + 1,
        core::cmp::Ordering::Less => q,
        core::cmp::Ordering::Equal => q + (q & 1), // half to even
    };
    // Saturating: a garbage dt must give a garbage-but-finite answer, not a wrapped negative that
    // reads as "no loss".
    Ok(q.min(i64::MAX as u128) as i64 - 1)
}

/// Body length of a section in 16-bit words: `rows * ceil(n_lanes * element_bits / 16)`.
/// Generalises `n_lanes * rows`, which is only the 16-bit case (`section_words`,
/// contract.yaml:97). SATURATES at `u32::MAX` — see below for why that is the right answer.
pub fn section_words(n_lanes: u16, rows: u16, element_bits: u8) -> u32 {
    // u64 then saturate. All three arguments come STRAIGHT OFF THE WIRE, so 65535 rows of 65535
    // 64-bit lanes is 1.7e10 words: `rows as u32 * ...` panicked in debug and WRAPPED in
    // release. Saturating, not `Option`: a u16 `section_words` can never equal u32::MAX.
    let w = rows as u64 * (n_lanes as u64 * element_bits as u64).div_ceil(16);
    w.min(u32::MAX as u64) as u32
}

/// The largest burst <= `want` whose byte total is a multiple of `alignment`, or the smallest legal
/// one; `None` for a zero argument. `alignment` is a PARAMETER — 16 is a usb3 pipe property (:715).
/// A bounded capture has no next read, so an unaligned burst strands a partial group (:389-390).
pub fn aligned_burst(frame_bytes: u32, want: u32, alignment: u32) -> Option<u32> {
    let step = alignment_step(frame_bytes, want, alignment)?;
    Some(((want / step) * step).max(step))
}

pub(crate) fn aligned_burst_at_least(frame_bytes: u32, want: u32, alignment: u32) -> Option<u32> {
    let step = alignment_step(frame_bytes, want, alignment)?;
    want.div_ceil(step).checked_mul(step)
}

fn alignment_step(frame_bytes: u32, want: u32, alignment: u32) -> Option<u32> {
    if frame_bytes == 0 || want == 0 || alignment == 0 {
        return None;
    }
    Some(alignment / gcd(frame_bytes, alignment))
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// CRC-32/ISO-HDLC (zlib/PKZIP): poly 0x04C11DB7, reflected 0xEDB88320, init 0xFFFFFFFF,
/// refin/refout true, xorout 0xFFFFFFFF; check value 0xCBF43926 over `"123456789"`
/// (`crc_algorithm`, contract.yaml:97-97). Written out because this crate depends on nothing.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for b in data {
        c = CRC_TABLE[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

const CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

// The three helpers the vector bundle does NOT cover — it publishes frames, and these take
// numbers. Expected values produced by the reference implementation itself (`python3 -c "import
// kdi.frame as f; f.lost_frames(...)"`), so still a cross-check against Python.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_frames_ties_go_to_even() {
        // Both are exact .5 ties at the 10000/3 cadence, and they round in OPPOSITE directions:
        // 1.5 -> 2 and 4.5 -> 4. A `(x + 0.5) as i64` gives 2 and 5, and the second is silently
        // one phantom lost frame.
        assert_eq!(lost_frames(5_000, 10_000, 3), Ok(1));
        assert_eq!(lost_frames(15_000, 10_000, 3), Ok(3));
        assert_eq!(lost_frames(10_000, 10_000, 3), Ok(2));
        assert_eq!(lost_frames(6_667, 10_000, 3), Ok(1));
        assert_eq!(lost_frames(3_333, 10_000, 3), Ok(0));
        // No gap at all is -1 frames lost, which is what a caller comparing consecutive stamps of
        // one stream must see: 0 would mean "one frame missing".
        assert_eq!(lost_frames(0, 1, 1), Ok(-1));
        assert_eq!(lost_frames(1, 0, 1), Err(Reject::TickSane));
        assert_eq!(lost_frames(1, 1, 0), Err(Reject::TickSane));
    }

    /// Four hostile bytes must not panic the decoder: `frame_words` sizes the frame upstream of
    /// the CRC, so it arrives as anything up to 0xFFFFFFFC, and on 32-bit usize `as usize * 2`
    /// panicked in debug, WRAPPED in release. Asserts reject-and-carry on any target.
    #[test]
    fn a_hostile_frame_words_is_rejected_not_slice_indexed() {
        let mut b = [0u8; 64];
        b[..4].copy_from_slice(&MAGIC.to_le_bytes());
        for fw in [0xFFFF_FFFCu32, 0x8000_0000, 0x4000_0000] {
            b[OFF_FRAME_WORDS..OFF_FRAME_WORDS + 4].copy_from_slice(&fw.to_le_bytes());
            assert_eq!(
                Frame::parse(&b).map(|_| ()),
                Err(RejectAt::at(Reject::BadLength, OFF_FRAME_WORDS))
            );
            // Walk carries it: a length running past the blob is the straddling case, not a fault.
            let mut w = Walk::new(&b);
            assert!(w.next().is_none());
            assert_eq!(w.tail().len(), b.len());
        }
    }

    /// `run_id` is the DEVICE-WIDE epoch, so the rule is per-run_id, not global (:391-392). No
    /// vector shows it: dropping the `p.run_id == h.run_id` filter passes the whole conformance
    /// suite while rejecting a healthy restart. Interleaved — a run_id-BLIND scan sees 10, 5, 20.
    #[test]
    fn run_announcements_are_scoped_to_their_run_id() {
        fn ann(run_id: u16, timestamp: u64, first: bool) -> Header {
            Header {
                timestamp,
                flags: Flags(if first { FLAG_FIRST_OF_RUN } else { 0 }),
                run_id,
                layout: 0,
                frame_words: 32,
                hdr_words: 28,
                n_sections: 1,
                contract_rev: CONTRACT_REV,
                desc_words: DESC_WORDS_MIN,
            }
        }
        let two_runs = [ann(1, 10, true), ann(2, 5, true), ann(1, 20, true)];
        assert_eq!(check_run_announcements(&two_runs), Ok(()));
        // ...and within ONE run_id the stamps must still strictly increase: 31 consecutive
        // announcements all stamped 0 is the defect this rule exists for (issue #78 in the
        // reference implementation).
        let repeat = [ann(1, 7, true), ann(2, 99, true), ann(1, 7, true)];
        assert_eq!(check_run_announcements(&repeat), Err(Reject::FirstOfRunDup));
        // An unflagged frame is not an announcement, however its stamp compares.
        let quiet = [ann(1, 10, true), ann(1, 3, false), ann(1, 11, true)];
        assert_eq!(check_run_announcements(&quiet), Ok(()));
    }

    /// The crate version tracks the CONTRACT version, and the `const _: () = assert!` at the top of
    /// this file proves nothing if the predicate is vacuously true — neutering `version_matches` to
    /// `if false { .. } true` compiles and passes everything else.
    #[test]
    fn version_matches_needs_a_component_boundary() {
        assert!(version_matches("0.4.0", "0.4"));
        assert!(version_matches("0.4.17", "0.4"));
        // The one the naive prefix test gets wrong: "0.40" starts with "0.4" and is a DIFFERENT
        // minor, so the character after the prefix has to be the separator.
        assert!(!version_matches("0.40", "0.4"));
        assert!(!version_matches("0.5.0", "0.4"));
        assert!(!version_matches("0.4", "0.4")); // no patch component at all
    }

    #[test]
    fn aligned_burst_and_section_words() {
        assert_eq!(aligned_burst(88, 100, 16), Some(100)); // adio_dig: 88 B, step 2
        assert_eq!(aligned_burst(136, 10, 16), Some(10)); // 1-lane rhd_matrix: 136 B, step 2
        assert_eq!(aligned_burst(64, 3, 16), Some(3)); // already aligned: step 1
        assert_eq!(aligned_burst(88, 1, 16), Some(2)); // below the step: the smallest legal burst
        assert_eq!(aligned_burst(88, 100, 0), None);
        assert_eq!(aligned_burst_at_least(88, 3, 16), Some(4));
        assert_eq!(aligned_burst_at_least(88, 0, 16), None);
        assert_eq!(aligned_burst_at_least(88, u32::MAX, 16), None);
        assert_eq!(section_words(20, 1, 1), 2); // 20 bit-packed lanes -> ceil(20/16) words
        assert_eq!(section_words(2, 35, 16), 70);
        assert_eq!(section_words(3, 2, 32), 12);
        // SATURATION asserted in ARITHMETIC, not by a panic: robustness.rs only catches the
        // pre-fix `rows as u32 * ...` because DEBUG checks overflow — measured, that mutant is
        // GREEN under `--release`. Line two wraps to 65534, which a u16 CAN equal.
        assert_eq!(section_words(u16::MAX, u16::MAX, 64), u32::MAX);
        assert_eq!(section_words(32769, u16::MAX, 32), u32::MAX);
    }
}
