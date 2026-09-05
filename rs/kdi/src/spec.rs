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

/// The contract THIS HOST implements — the `host` half of a `Skew::Major` and the
/// only version a caller may compare its own expectations against. The DEVICE's is
/// `Device::kdi()`, read off the wire at bind.
pub const KDI_VERSION: &str = "0.5";
/// The contract MAJOR. A device announcing a different one must be refused at bind: majors are not
/// compatible, and the traffic that would follow cannot be trusted.
pub const KDI_MAJOR: u16 = 0;
/// The contract MINOR. ADDITIVE by definition — a device on a HIGHER minor binds normally, and a
/// host must never do version arithmetic beyond the major equality test.
pub const KDI_MINOR: u16 = 5;
/// Every frame carries the contract's MINOR in `contract_rev`.
pub const CONTRACT_REV: u16 = KDI_MINOR;
/// How long a host must be willing to poll `contract_ready` before giving up, in milliseconds. A
/// DEVICE property, published so that a slower-booting board does not turn into a fleet of hosts
/// that each need a patch.
pub const READY_TIMEOUT_MS: u64 = 3000;
/// A pipe read on the usb3 binding must be a multiple of this many bytes. A BINDING
/// property, not a codec constant — an ethernet binding has a different one or none.
pub const USB3_READ_ALIGNMENT: usize = 16;

/// A named bulk stream. Independently enabled: one run bit, one burst bound, one pipe and
/// one sticky overrun each.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Stream {
    /// The `samples` stream: sections of `adio_dig`, `rhd_matrix`. Started by `run_samples`,
    /// bounded by `burst_samples`, lanes selected by `lanes_samples`; depth and sticky overrun on
    /// `stream_status_samples`.
    Samples,

    /// The `digital` stream: sections of `adio_dig`. Started by `run_digital`, bounded by
    /// `burst_digital`; depth and sticky overrun on `stream_status_digital`.
    Digital,
}

impl Stream {
    /// The contract token, 1:1 with contract.yaml.
    pub const fn token(self) -> &'static str {
        match self {
            Stream::Samples => "samples",
            Stream::Digital => "digital",
        }
    }

    /// Parse a token. `None` is a token this build does not know — from a device on
    /// a newer minor, which is legal and must never be a parse failure.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "samples" => Stream::Samples,
            "digital" => Stream::Digital,
            _ => return None,
        })
    }

    /// A GENEROUS read-sizing hint in bytes. ADVISORY — never a validity gate: two
    /// sections of one kind are legal, so a frame may exceed it and must still decode.
    pub const fn max_frame_bytes(self) -> usize {
        match self {
            Stream::Samples => 2408,
            Stream::Digital => 88,
        }
    }
}

/// What one block of a record IS. Append-only and FROZEN: a redefined kind is the one
/// mis-parse nothing on the wire can catch. 0x80-0xff are reserved for private use.
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
    /// which is LEGAL and is skipped rather than faulted on.
    pub fn from_code(c: u8) -> Option<Self> {
        Some(match c {
            0x10 => Kind::AdioDig,
            0x20 => Kind::RhdMatrix,
            _ => return None,
        })
    }
}

/// A capability bit. Branch on these, NEVER on a version comparison.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Cap {
    /// this build emits the format-2 self-describing frame on the declared stream endpoints
    ///
    /// Gates: streams `samples`, `digital`; registers `quiesce`, `run_samples`, `run_digital`,
    /// `lanes_samples`, `burst_samples`, `burst_digital`, `stream_status_samples`,
    /// `stream_status_digital`.
    CleanFrame,

    /// the typed request/response command channel is live (firmware-dependent: it is baked into the
    /// same .bit)
    ///
    /// Gates: commands `sys.hello`, `power.status`, `power.up`, `adio.mode`, `adio.adc`, `adio.fb`,
    /// `adio.dout`, `gnd.eeprom.read`, `sys.claim`, `sys.release`, `sys.challenge`, `sys.unlock`.
    CommandProtocol,

    /// the DDR3 pipe buffer is present and calibrated; without it a stream's depth is the on-chip
    /// FIFO only
    Ddr3,

    /// ADIO analog/digital module I/O is present
    ///
    /// Gates: commands `adio.mode`, `adio.adc`, `adio.fb`, `adio.dout`.
    Adio,

    /// the grounding/module-ID board is present
    ///
    /// Gates: commands `gnd.eeprom.read`.
    Grounding,

    /// TTL inputs are wired to the digital stream's lanes
    TtlIn,

    /// per-slot presence and rail health are readable
    ///
    /// Gates: commands `power.status`.
    SlotHealth,

    /// gateware images can be written over the wire and selected at boot (RESERVED — no command
    /// implements this yet)
    FieldUpdate,

    /// the acquisition rate is host-configurable, and `rhd_matrix` emits NOTHING until it has been
    /// configured (#131). A device with this capability expects `rate_md` + `rate_apply` before
    /// acquisition and reports readiness on `rate_ready`; without it a host cannot know whether the
    /// cadence a frame declares is one the hardware was placed into.
    ///
    /// Gates: registers `rate_md`, `rate_apply`, `rate_ready`.
    RateControl,
}

impl Cap {
    /// The contract token, 1:1 with contract.yaml.
    pub const fn token(self) -> &'static str {
        match self {
            Cap::CleanFrame => "clean_frame",
            Cap::CommandProtocol => "command_protocol",
            Cap::Ddr3 => "ddr3",
            Cap::Adio => "adio",
            Cap::Grounding => "grounding",
            Cap::TtlIn => "ttl_in",
            Cap::SlotHealth => "slot_health",
            Cap::FieldUpdate => "field_update",
            Cap::RateControl => "rate_control",
        }
    }

    /// Parse a token. `None` is a token this build does not know — from a device on
    /// a newer minor, which is legal and must never be a parse failure.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "clean_frame" => Cap::CleanFrame,
            "command_protocol" => Cap::CommandProtocol,
            "ddr3" => Cap::Ddr3,
            "adio" => Cap::Adio,
            "grounding" => Cap::Grounding,
            "ttl_in" => Cap::TtlIn,
            "slot_health" => Cap::SlotHealth,
            "field_update" => Cap::FieldUpdate,
            "rate_control" => Cap::RateControl,
            _ => return None,
        })
    }

    /// This capability's bit position in the `caps` register.
    pub const fn bit(self) -> u32 {
        match self {
            Cap::CleanFrame => 0,
            Cap::CommandProtocol => 1,
            Cap::Ddr3 => 2,
            Cap::Adio => 3,
            Cap::Grounding => 4,
            Cap::TtlIn => 5,
            Cap::SlotHealth => 6,
            Cap::FieldUpdate => 7,
            Cap::RateControl => 8,
        }
    }
}

/// THE closed device error set. A host switches on this; `rc` is a platform errno and is
/// informative only — the same "unknown command" is -38 on Linux and -88 on the target.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum DeviceErr {
    /// argument count, type or range rejected before the handler ran
    ///
    /// Not retryable: the same call will be refused again.
    BadArgs,

    /// no such command in this cmdset
    ///
    /// Not retryable: the same call will be refused again.
    UnknownCmd,

    /// service/factory command refused; no unlock grant in this session
    ///
    /// Not retryable: the same call will be refused again.
    TierLocked,

    /// slot not in the last power-sequence present mask; refusing to drive its pins
    ///
    /// Not retryable: the same call will be refused again.
    NotPresent,

    /// the slot's adio_core did not answer at its AXI page
    ///
    /// Not retryable: the same call will be refused again.
    NoIp,

    /// a Zephyr device backing this command is not ready
    ///
    /// Retryable: another attempt may answer differently.
    NoDevice,

    /// firmware bug — report it with the rc
    ///
    /// Not retryable: the same call will be refused again.
    Internal,

    /// another host holds the device lease; retry after it releases or its lease expires
    ///
    /// Retryable: another attempt may answer differently.
    Busy,

    /// a state-changing command was sent without holding the lease; claim first
    ///
    /// Not retryable: the same call will be refused again.
    NotClaimed,

    /// a destructive command needs a matching `confirm` envelope echo
    ///
    /// Not retryable: the same call will be refused again.
    ConfirmRequired,

    /// the device has not finished boot/calibration; poll contract_ready
    ///
    /// Retryable: another attempt may answer differently.
    NotReady,

    /// a write was addressed to a read-only register
    ///
    /// Not retryable: the same call will be refused again.
    RoRegister,

    /// no register or stream of that name in this binding
    ///
    /// Not retryable: the same call will be refused again.
    NoSuchRegister,

    /// the reply did not fit the device's response buffer and was not sent
    ///
    /// Not retryable: the same call will be refused again.
    ResponseTooLarge,
}

impl DeviceErr {
    /// The contract token, 1:1 with contract.yaml.
    pub const fn token(self) -> &'static str {
        match self {
            DeviceErr::BadArgs => "bad_args",
            DeviceErr::UnknownCmd => "unknown_cmd",
            DeviceErr::TierLocked => "tier_locked",
            DeviceErr::NotPresent => "not_present",
            DeviceErr::NoIp => "no_ip",
            DeviceErr::NoDevice => "no_device",
            DeviceErr::Internal => "internal",
            DeviceErr::Busy => "busy",
            DeviceErr::NotClaimed => "not_claimed",
            DeviceErr::ConfirmRequired => "confirm_required",
            DeviceErr::NotReady => "not_ready",
            DeviceErr::RoRegister => "ro_register",
            DeviceErr::NoSuchRegister => "no_such_register",
            DeviceErr::ResponseTooLarge => "response_too_large",
        }
    }

    /// Parse a token. `None` is a token this build does not know — from a device on
    /// a newer minor, which is legal and must never be a parse failure.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "bad_args" => DeviceErr::BadArgs,
            "unknown_cmd" => DeviceErr::UnknownCmd,
            "tier_locked" => DeviceErr::TierLocked,
            "not_present" => DeviceErr::NotPresent,
            "no_ip" => DeviceErr::NoIp,
            "no_device" => DeviceErr::NoDevice,
            "internal" => DeviceErr::Internal,
            "busy" => DeviceErr::Busy,
            "not_claimed" => DeviceErr::NotClaimed,
            "confirm_required" => DeviceErr::ConfirmRequired,
            "not_ready" => DeviceErr::NotReady,
            "ro_register" => DeviceErr::RoRegister,
            "no_such_register" => DeviceErr::NoSuchRegister,
            "response_too_large" => DeviceErr::ResponseTooLarge,
            _ => return None,
        })
    }
}

/// The closed HOST-side set. A conforming library reports these and mints no others — the
/// Python reference minted `overrun` and `timeout`, which are in neither published set.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum HostErr {
    /// no reply / no data within the caller's deadline
    HostTimeout,

    /// the transport returned fewer bytes than the framing declared
    HostShortRead,

    /// this stream's sticky overrun was set: the frames just read may not be contiguous
    HostOverflow,

    /// an argument was refused by the host before it reached the wire (see arg_charset)
    HostUnsafeArg,
}

impl HostErr {
    /// The contract token, 1:1 with contract.yaml.
    pub const fn token(self) -> &'static str {
        match self {
            HostErr::HostTimeout => "host_timeout",
            HostErr::HostShortRead => "host_short_read",
            HostErr::HostOverflow => "host_overflow",
            HostErr::HostUnsafeArg => "host_unsafe_arg",
        }
    }

    /// Parse a token. `None` is a token this build does not know — from a device on
    /// a newer minor, which is legal and must never be a parse failure.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "host_timeout" => HostErr::HostTimeout,
            "host_short_read" => HostErr::HostShortRead,
            "host_overflow" => HostErr::HostOverflow,
            "host_unsafe_arg" => HostErr::HostUnsafeArg,
            _ => return None,
        })
    }
}

/// The usb3 binding's register map: (name, kind, address, low bit, width).
/// Resolved by NAME at run time; the numbers live only here, generated.
pub(crate) const USB3_REG: &[(&str, &str, u8, Option<u8>, u8)] = &[
    ("contract_version", "wireout", 0x35, None, 32),
    ("caps", "wireout", 0x36, None, 32),
    ("gateware_sha", "wireout", 0x37, None, 32),
    ("contract_ready", "wireout", 0x31, Some(1), 1),
    ("quiesce", "wirein", 0x00, Some(0), 1),
    ("run_samples", "wirein", 0x11, Some(1), 1),
    ("run_digital", "wirein", 0x11, Some(0), 1),
    ("lanes_samples", "wirein", 0x12, None, 32),
    ("burst_digital", "wirein", 0x13, Some(0), 16),
    ("burst_samples", "wirein", 0x13, Some(16), 16),
    ("stream_status_samples", "wireout", 0x39, None, 32),
    ("stream_status_digital", "wireout", 0x38, None, 32),
    ("occupancy", "wireout", 0x20, None, 32),
    ("overflow", "wireout", 0x31, Some(0), 1),
    ("console_tx_drop", "wireout", 0x31, Some(2), 1),
    ("rate_md", "wirein", 0x03, None, 32),
    ("rate_apply", "triggerin", 0x40, Some(0), 1),
    ("rate_ready", "wireout", 0x3B, Some(0), 1),
];

/// Which registers control which stream, from `streams.*.registers`:
/// (stream, run, burst, status, lanes). This replaced an f-string convention only the
/// Python reference could know. `lanes` is `None` for a stream that declares no lane
/// mask — `digital` has none, and a host that wrote `lanes_digital` anyway would be
/// inventing a register the contract does not have.
#[rustfmt::skip]
pub(crate) const STREAM_REGS: &[(&str, &str, &str, &str, Option<&str>)] = &[
    ("samples", "run_samples", "burst_samples", "stream_status_samples", Some("lanes_samples")),
    ("digital", "run_digital", "burst_digital", "stream_status_digital", None),
];

/// The usb3 stream endpoints: (stream, pipe kind, address).
pub(crate) const USB3_STREAM: &[(&str, &str, u8)] = &[
    ("samples", "okPipeOut", 0xA3),
    ("digital", "okPipeOut", 0xA2),
];

/// The vUART endpoints carrying the message channel: (role, kind, address, bit).
/// Published in 0.4 — they were host-side constants before, the one part of the
/// contract a host could not resolve by name.
pub(crate) const USB3_MSG: &[(&str, &str, u8, Option<u8>)] = &[
    ("status", "wireout", 0x30, None),
    ("tx_pipe", "okPipeOut", 0xA1, None),
    ("rx_data", "wirein", 0x0F, None),
    ("rx_count", "wirein", 0x10, None),
    ("rx_push", "triggerin", 0x43, Some(0)),
];

/// Every request token must match this, or a host MUST refuse it WITHOUT sending a
/// byte: the vUART is shared with an ungated human shell, so a CR is command
/// injection and a raw 0x1e forges a reply.
pub const ARG_CHARSET: &str = "[A-Za-z0-9_.-]+";
/// The byte that opens a reply frame on the usb3 message channel. The wire it shares with the human
/// console carries anything, so this — not a newline — is what a reply is found by.
pub const RESP_SENTINEL: u8 = 0x1e;
/// Lowercase ASCII hex digits of body length following the sentinel. EXACTLY this many: a short
/// count is not padded, so a reader that scans for a delimiter instead reads garbage.
pub const RESP_LEN_DIGITS: usize = 3;
/// What every request line on the usb3 message channel starts with, so the shared console can tell
/// a KDI request from something a human typed.
pub const REQ_TAG: &str = "kdi ";
/// The device's reply buffer, in bytes. A reply that does not fit is NOT SENT — pair a missing
/// reply with the sticky `console_tx_drop` register rather than reading it as a device that
/// answered short.
pub const RESP_MAX_BODY: usize = 480;
