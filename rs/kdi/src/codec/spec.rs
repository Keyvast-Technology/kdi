// GENERATED FROM kdi/contract.yaml BY kdi/gen.py — DO NOT EDIT.
//
// Regenerate with `make kdi-gen`. A hand edit is caught by
// kdi/tests/test_end_to_end.py::test_generated_rust_is_current, which re-renders this file and
// compares it byte-for-byte; there is no merge path for a local change.
//
// This file is COMMITTED even though the repo otherwise never tracks generated artifacts, because
// a crates.io tarball carries no Python: a consumer who cannot run the generator has to find the
// contract already compiled in.

#![allow(dead_code)]

/// The contract version this crate implements, `major.minor`. The crate's own version is this plus
/// a patch component and nothing else, asserted at COMPILE TIME — so a published crate cannot
/// disagree with the contract it claims to implement.
pub const KDI_VERSION: &str = "0.6";
/// The contract MAJOR. A device announcing a different one must be refused at bind: majors are not
/// compatible, and the traffic that would follow cannot be trusted.
pub const KDI_MAJOR: u16 = 0;
/// The contract MINOR. ADDITIVE by definition — a device on a HIGHER minor binds normally, and a
/// host must never do version arithmetic beyond the major equality test.
pub const KDI_MINOR: u16 = 6;
/// Every frame carries the contract's MINOR in `contract_rev`. Derived here rather
/// than restated, which is the whole point: it was once a literal in three places
/// at once and a contract bump left all three reading the old revision.
pub const CONTRACT_REV: u16 = KDI_MINOR;

/// b"KVDF" little-endian. A RESYNC ANCHOR ONLY — never a validity test.
pub const MAGIC: u32 = 0x4644564B;
/// The only frame `format` this decoder understands. A frame declaring any other value is SKIPPED
/// by its own `frame_words`, never rejected — magic, format and frame_words are frozen at their
/// offsets for every present and future format, and that is what makes the skip safe.
pub const FORMAT: u16 = 2;
/// crc32 over a frame INCLUDING its own trailer. CRC-32/ISO-HDLC; see the
/// `crc_algorithm` invariant for the full parameter set.
pub const CRC_RESIDUE: u32 = 0x2144DF1C;
/// Bytes of FIXED header — the part whose field offsets are compiled in. The whole header,
/// descriptors and lane ids included, is `hdr_words * 2` and is only knowable from the wire.
pub const HDR_BYTES: usize = 0x20;
/// Byte offset of the first section descriptor, immediately after the fixed header.
pub const DESCRIPTORS_AT: usize = 0x20;
/// A decoder takes the real stride from `desc_words` on the wire; this is the floor
/// below which a frame is rejected.
pub const DESC_WORDS_MIN: u16 = 8;
/// Bytes of CRC trailer every frame ends with. Included in `frame_words`, so the body ends this
/// many bytes before the frame does.
pub const CRC_BYTES: usize = 4;

/// `dflags[2:1]` -> bits per element. Index 0 is 16 so `dflags == 0` is the common case.
pub const ELEM_BITS: [u8; 4] = [16, 1, 32, 64];

// Byte offsets within the fixed header, from `streams.*.header`.
/// Byte offset of `magic` (u32) in the fixed header.
pub const OFF_MAGIC: usize = 0x00;
/// Byte offset of `format` (u16) in the fixed header.
pub const OFF_FORMAT: usize = 0x04;
/// Byte offset of `flags` (u16) in the fixed header.
pub const OFF_FLAGS: usize = 0x06;
/// Byte offset of `timestamp` (u64) in the fixed header.
pub const OFF_TIMESTAMP: usize = 0x08;
/// Byte offset of `frame_words` (u32, words16) in the fixed header.
pub const OFF_FRAME_WORDS: usize = 0x10;
/// Byte offset of `layout` (u16) in the fixed header. Opaque section-arrangement id; a cache key,
/// never a branch.
pub const OFF_LAYOUT: usize = 0x14;
/// Byte offset of `hdr_words` (u16, words16) in the fixed header.
pub const OFF_HDR_WORDS: usize = 0x16;
/// Byte offset of `n_sections` (u16) in the fixed header.
pub const OFF_N_SECTIONS: usize = 0x18;
/// Byte offset of `run_id` (u16) in the fixed header.
pub const OFF_RUN_ID: usize = 0x1A;
/// Byte offset of `contract_rev` (u16) in the fixed header.
pub const OFF_CONTRACT_REV: usize = 0x1C;
/// Byte offset of `desc_words` (u16, words16) in the fixed header.
pub const OFF_DESC_WORDS: usize = 0x1E;

// Byte offsets within one section descriptor, from `streams.*.descriptor.fields`.
/// Byte offset of `kind` (u8) in one section descriptor.
pub const DOFF_KIND: usize = 0x00;
/// Byte offset of `dflags` (u8) in one section descriptor.
pub const DOFF_DFLAGS: usize = 0x01;
/// Byte offset of `n_lanes` (u16) in one section descriptor.
pub const DOFF_N_LANES: usize = 0x02;
/// Byte offset of `words_per_lane` (u16) in one section descriptor.
pub const DOFF_WORDS_PER_LANE: usize = 0x04;
/// Byte offset of `section_words` (u16) in one section descriptor.
pub const DOFF_SECTION_WORDS: usize = 0x06;
/// Byte offset of `tick_num` (u32) in one section descriptor.
pub const DOFF_TICK_NUM: usize = 0x08;
/// Byte offset of `tick_den` (u16) in one section descriptor.
pub const DOFF_TICK_DEN: usize = 0x0C;
/// Byte offset of `pad` (u16) in one section descriptor.
pub const DOFF_PAD: usize = 0x0E;

// Header flag bits, from the `flags` field's declared bit assignment.
/// The frame starts a contiguous segment of ITS OWN stream. Its timestamp need not be 0 and there
/// may be several per `run_id`, which is the device-wide epoch — so this is where a timestamp gap
/// stops meaning loss, and nothing else marks that seam.
pub const FLAG_FIRST_OF_RUN: u16 = 1 << 0;
/// The device dropped data before this frame. The frame itself is intact; what is lost is the
/// contiguity with the one before it.
pub const FLAG_WORDS_DROPPED: u16 = 1 << 1;
/// The sampling cadence changed at this frame. Re-read the per-section `tick_num`/`tick_den` rather
/// than carrying the previous rational forward.
pub const FLAG_RATE_EVENT: u16 = 1 << 2;
/// Every DEFINED flag bit. A header with any other bit set is rejected rather than masked
/// (`reserved_reject`), so a `flags` word that survives parsing has only these in it.
pub const FLAGS_DEFINED: u16 = FLAG_FIRST_OF_RUN | FLAG_WORDS_DROPPED | FLAG_RATE_EVENT;

/// A section kind. Append-only and FROZEN: a redefined kind is the one mis-parse nothing on
/// the wire can catch. 0x80-0xff are reserved for private use.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Kind {
    /// `adio_dig` = code 0x10, up to 16 lanes, 1 row of 1-bit elements per frame.
    ///
    /// ADIO digital INPUT levels, one bit-packed lane per physical line: 16 lines cost ONE word and
    /// every bit is named by its own lane id, so no host needs a slot-to-bit formula. The level is
    /// latched at FRAME START, so it belongs to its own frame's timestamp instant (fixed). LEVELS,
    /// not edges: a pulse shorter than one sample period can be missed and an edge is located only
    /// to +-1 sample.
    AdioDig,

    /// `rhd_matrix` = code 0x20, up to 32 lanes, 35 rows of 16-bit elements per frame.
    ///
    /// One RHD acquisition lane per lane id. ROW ORDER IS ROTATED BY ONE and this is a property of
    /// the hardware, not a choice: the RHD SPI returns a command's result during the NEXT command,
    /// so row k carries the capture from command k-1 (RhdCore.scala:216-218, hardware-caught in PR
    /// #15). Concretely:
    ///
    /// ```text
    ///   row 0        the PREVIOUS timestep's aux2 (aux_adc) — note the lag
    ///   rows 1..32   amplifier channels 0..31, ascending
    ///   rows 33,34   this timestep's aux0 (temp) and aux1 (supply)
    /// ```
    ///
    /// A host that assumes rows 0..31 are the amplifier reads channel n at row n and gets channel
    /// n-1, with row 0 pure garbage — plausible-looking neural data at the wrong index, which is
    /// exactly the failure PR #15 shipped once already. Amplifier codes are offset binary around
    /// 0x8000, NOT two's complement. The volts-per-code scale is a property of the chip profile and
    /// is deliberately NOT published here: a profile swap is a MAJOR bump a host must refuse to
    /// bind, never silently rescale.
    RhdMatrix,
}

impl Kind {
    /// The contract token, 1:1 with contract.yaml.
    pub const fn token(self) -> &'static str {
        match self {
            Kind::AdioDig => "adio_dig",
            Kind::RhdMatrix => "rhd_matrix",
        }
    }

    /// Parse a token. `None` is a token this build does not know — from a device on
    /// a newer minor, which is legal and must never be a parse failure.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "adio_dig" => Kind::AdioDig,
            "rhd_matrix" => Kind::RhdMatrix,
            _ => return None,
        })
    }

    /// The code this kind travels as in a section descriptor's `kind` byte.
    pub const fn code(self) -> u8 {
        match self {
            Kind::AdioDig => 0x10,
            Kind::RhdMatrix => 0x20,
        }
    }

    /// Decode a descriptor's `kind` byte. `None` is a kind this build does not know,
    /// which is LEGAL: skip the section by `section_words` rather than fault.
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0x10 => Kind::AdioDig,
            0x20 => Kind::RhdMatrix,
            _ => return None,
        })
    }
}

/// A frame carrying one of these MUST be rejected, with exactly this token. The published
/// vector set carries a negative case for every variant.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Reject {
    /// The frame's CRC-32 residue is wrong. Its declared length is therefore unverified too, so a
    /// decoder must RESYNC to the next magic rather than step over it by `frame_words`.
    CrcErr,

    /// `frame_words` is not a multiple of 4, is below the header-plus-trailer minimum, or runs past
    /// the bytes on hand. Nothing else in the frame can be located until this is right.
    BadLength,

    /// A descriptor's `section_words` disagrees with the body length its own geometry implies
    /// (`rows * ceil(n_lanes * element_bits / 16)`).
    SectionWords,

    /// A section's lane ids do not strictly ascend. Descending ids decode to real values under a
    /// decoder that does not check — right samples under wrong channel labels, which is the class
    /// of defect nothing downstream can detect.
    LaneIds,

    /// A reserved bit or pad word is not zero. Rejected rather than masked off: two implementations
    /// that differ here diverge permanently the day anything is put in the slot.
    ReservedBits,

    /// A descriptor declares `tick_num` or `tick_den` as 0, which divides by zero in the published
    /// loss oracle in every host that applies it.
    TickSane,

    /// The timestamp's top 16 bits are set. Only 48 are significant, so a non-zero high half is a
    /// mis-decode, not a very distant frame.
    TimestampTop16,

    /// Two `first_of_run` frames of one `run_id` whose timestamps do not strictly increase. A
    /// CROSS-FRAME rule, so it is checked over a sequence of headers rather than by parsing one.
    FirstOfRunDup,

    /// The frame declares a descriptor stride below the format's floor, so the descriptors cannot
    /// be the shape the format defines.
    DescWords,

    /// `hdr_words` does not agree with the header it describes: it exceeds the frame, is not a
    /// multiple of 4, or does not cover the descriptor block and the lane ids that follow it. The
    /// device-reachable form leaves the SAMPLES right and the LANE IDS read out of the body —
    /// correct data under wrong channel labels.
    HdrWordsFits,

    /// The section bodies, laid end to end from `hdr_words`, run past the frame's own trailer.
    BodyFitsFrame,
}

impl Reject {
    /// The contract token, 1:1 with contract.yaml.
    pub const fn token(self) -> &'static str {
        match self {
            Reject::CrcErr => "crc_err",
            Reject::BadLength => "bad_length",
            Reject::SectionWords => "section_words",
            Reject::LaneIds => "lane_ids",
            Reject::ReservedBits => "reserved_bits",
            Reject::TickSane => "tick_sane",
            Reject::TimestampTop16 => "timestamp_top16",
            Reject::FirstOfRunDup => "first_of_run_dup",
            Reject::DescWords => "desc_words",
            Reject::HdrWordsFits => "hdr_words_fits",
            Reject::BodyFitsFrame => "body_fits_frame",
        }
    }

    /// Parse a token. `None` is a token this build does not know — from a device on
    /// a newer minor, which is legal and must never be a parse failure.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "crc_err" => Reject::CrcErr,
            "bad_length" => Reject::BadLength,
            "section_words" => Reject::SectionWords,
            "lane_ids" => Reject::LaneIds,
            "reserved_bits" => Reject::ReservedBits,
            "tick_sane" => Reject::TickSane,
            "timestamp_top16" => Reject::TimestampTop16,
            "first_of_run_dup" => Reject::FirstOfRunDup,
            "desc_words" => Reject::DescWords,
            "hdr_words_fits" => Reject::HdrWordsFits,
            "body_fits_frame" => Reject::BodyFitsFrame,
            _ => return None,
        })
    }

    /// Every variant, in contract order. The `reject_tokens` coverage check reads
    /// this, so adding a token to contract.yaml immediately demands a vector for it.
    pub const ALL: [Reject; 11] = [
        Reject::CrcErr,
        Reject::BadLength,
        Reject::SectionWords,
        Reject::LaneIds,
        Reject::ReservedBits,
        Reject::TickSane,
        Reject::TimestampTop16,
        Reject::FirstOfRunDup,
        Reject::DescWords,
        Reject::HdrWordsFits,
        Reject::BodyFitsFrame,
    ];
}
