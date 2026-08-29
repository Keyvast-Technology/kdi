// GENERATED FROM kdi/contract.yaml BY kdi/gen.py — DO NOT EDIT.
//
// Regenerate with `make kdi-gen`. A hand edit is caught by
// kdi/tests/test_end_to_end.py::test_generated_rust_is_current, which re-renders this file and
// compares it byte-for-byte; there is no merge path for a local change.
//
// This file is COMMITTED even though the repo otherwise never tracks generated artifacts, because
// a crates.io tarball carries no Python: a consumer who cannot run the generator has to find the
// contract already compiled in.

// A TRAIT, not an inherent impl: this file is generated, so it may reach the device only through
// the one PUBLIC entry point (`Device::raw_cmd`) — no private field of `Device` and no endpoint
// number may leak into a file no human is allowed to edit, which is the same binding quarantine
// lib.rs opens with. A generated method knows a command NAME and an argument ORDER, nothing else.
//
// Why type them at all: `Device::raw_cmd` is stringly-typed, so `raw_cmd("adio.adc", &["1", "0"])`
// transposes slot and channel with no complaint (the failure kdi/frontpanel.py:80-85 documents),
// an out-of-range slot is learned a round trip later, and a misspelled name is learned on the
// wire. Every range below comes from contract.yaml and is checked BEFORE a byte is sent; the
// refusal is `HostErr::HostUnsafeArg`, which is the contract's own token for "refused by the host
// before it reached the wire" — `host_errors` is a CLOSED set and a conforming library mints none
// of its own (kdi/contract.yaml:90-94).

#![allow(dead_code)]

use std::io::ErrorKind;

use serde_json::Value;

use crate::{io_err, Device, Error, HostErr, Reply};

/// Every published command with its declared argument order. THE ORDER IS THE WIRE
/// (`request.arg_order: declared`, kdi/contract.yaml:770), so a host serialising a
/// positional `Device::raw_cmd` call reads it from here instead of keeping a second copy of
/// the registry — the copy that goes stale is the one that transposes two arguments.
/// The RESERVED session commands are listed too: they have no typed method, but they are
/// still callable through `Device::raw_cmd`.
pub const COMMANDS: &[(&str, &[&str])] = &[
    ("sys.claim", &[]),
    ("sys.release", &[]),
    ("sys.challenge", &[]),
    ("sys.unlock", &["grant", "sig"]),
    ("sys.hello", &[]),
    ("power.status", &[]),
    ("power.up", &[]),
    ("adio.mode", &["slot", "ch1", "ch2"]),
    ("adio.fb", &["slot", "on"]),
    ("adio.dout", &["slot", "mask"]),
    ("adio.adc", &["slot", "ch", "n"]),
];

/// A mode token, 1:1 with the contract's `of:` list. One enum for every argument that
/// takes this value set — `adio.mode` declares it twice and two identical types would be
/// two things to keep in step.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum ChMode {
    /// The contract token `off`.
    Off,

    /// The contract token `in`.
    In,

    /// The contract token `out`.
    Out,

    /// The contract token `adc`.
    Adc,

    /// The contract token `dac`.
    Dac,
}

impl ChMode {
    /// The contract token, 1:1 with contract.yaml.
    pub const fn token(self) -> &'static str {
        match self {
            ChMode::Off => "off",
            ChMode::In => "in",
            ChMode::Out => "out",
            ChMode::Adc => "adc",
            ChMode::Dac => "dac",
        }
    }

    /// Parse a token. `None` is a token this build does not know — from a device on
    /// a newer minor, which is legal and must never be a parse failure.
    pub fn from_token(s: &str) -> Option<Self> {
        Some(match s {
            "off" => ChMode::Off,
            "in" => ChMode::In,
            "out" => ChMode::Out,
            "adc" => ChMode::Adc,
            "dac" => ChMode::Dac,
            _ => return None,
        })
    }
}

/// The reply to `sys.hello`.
///
/// Handshake. fw/gw are the firmware and gateware git shas (8 hex) — equal when both came from one
/// build. The identity REGISTERS carry the same gateware sha pre-boot; the device DNA is not on
/// this command (it is AXI-side, reachable via the human `kv id`).
#[derive(Clone, Debug)]
pub struct SysHello {
    /// The reply's `proto` value — declared `u8`.
    pub proto: u8,

    /// The reply's `cmdset` value — declared `str`.
    pub cmdset: String,

    /// The reply's `kdi` value — declared `str`.
    pub kdi: String,

    /// The reply's `board_id` value — declared `u32`.
    pub board_id: u32,

    /// The reply's `fw` value — declared `str`.
    pub fw: String,

    /// The reply's `gw` value — declared `str`.
    pub gw: String,
}

impl SysHello {
    fn parse(r: &Reply) -> Result<Self, Error> {
        Ok(Self {
            proto: uint(r, "proto")?,
            cmdset: text(r, "cmdset")?,
            kdi: text(r, "kdi")?,
            board_id: uint(r, "board_id")?,
            fw: text(r, "fw")?,
            gw: text(r, "gw")?,
        })
    }
}

/// The reply to `power.status`.
///
/// Read the power tree, writing nothing. present = module-present bitmask from the most recent
/// sequence pass (a raw detect read is NOT equivalent — after a pass those bits are outputs driving
/// the DCDC enables). reverify = the periodic re-verify thread is enabled.
#[derive(Clone, Debug)]
pub struct PowerStatus {
    /// The reply's `present` value — declared `u8`.
    pub present: u8,

    /// The reply's `reverify` value — declared `bool`.
    pub reverify: bool,
}

impl PowerStatus {
    fn parse(r: &Reply) -> Result<Self, Error> {
        Ok(Self {
            present: uint(r, "present")?,
            reverify: flag(r, "reverify")?,
        })
    }
}

/// The reply to `power.up`.
///
/// Run the rail sequence. Stim stays off. Level-set: safe to retry.
#[derive(Clone, Debug)]
pub struct PowerUp {
    /// The reply's `ok` value — declared `bool`.
    pub ok: bool,

    /// The reply's `present` value — declared `u8`.
    pub present: u8,
}

impl PowerUp {
    fn parse(r: &Reply) -> Result<Self, Error> {
        Ok(Self {
            ok: flag(r, "ok")?,
            present: uint(r, "present")?,
        })
    }
}

/// The reply to `adio.mode`.
///
/// Set a slot's two channel modes (I2C mux + CH_MODE, kept coherent).
#[derive(Clone, Debug)]
pub struct AdioMode {
    /// The reply's `slot` value — declared `u8`.
    pub slot: u8,

    /// The reply's `ch_mode` value — declared `u16`.
    pub ch_mode: u16,
}

impl AdioMode {
    fn parse(r: &Reply) -> Result<Self, Error> {
        Ok(Self {
            slot: uint(r, "slot")?,
            ch_mode: uint(r, "ch_mode")?,
        })
    }
}

/// The reply to `adio.fb`.
///
/// Cross-channel loopback tie. Independent of either channel's DIRECTION and of CH_MODE, and re-
/// asserted with the expander, so setting a mode never silently drops a tie. The muxes make it a
/// SELECT, not a strap: with the tie on, a channel routes to the other channel instead of to its
/// jack.
#[derive(Clone, Debug)]
pub struct AdioFb {
    /// The reply's `slot` value — declared `u8`.
    pub slot: u8,

    /// The reply's `fb` value — declared `u8`.
    pub fb: u8,
}

impl AdioFb {
    fn parse(r: &Reply) -> Result<Self, Error> {
        Ok(Self {
            slot: uint(r, "slot")?,
            fb: uint(r, "fb")?,
        })
    }
}

/// The reply to `adio.dout`.
///
/// Drive the digital outputs (bit0 CH1, bit1 CH2) and echo the RAW pad readback, so one call is
/// both a stimulus and its measurement. A non-zero mask is refused on an absent slot: driving a pin
/// high there back-powers the module rail. Note the pin only moves if that channel's CH_MODE is
/// out, and a driven channel reads 0 on its own TTL lane by design -- read the OTHER channel,
/// through the tie.
#[derive(Clone, Debug)]
pub struct AdioDout {
    /// The reply's `slot` value — declared `u8`.
    pub slot: u8,

    /// The reply's `dout` value — declared `u8`.
    pub dout: u8,

    /// The reply's `din` value — declared `u8`.
    pub din: u8,
}

impl AdioDout {
    fn parse(r: &Reply) -> Result<Self, Error> {
        Ok(Self {
            slot: uint(r, "slot")?,
            dout: uint(r, "dout")?,
            din: uint(r, "din")?,
        })
    }
}

/// The reply to `adio.adc`.
///
/// n is range-checked, never silently clamped (the human `kv adio adc` clamps). valid\[i\] mirrors
/// each sample's valid bit — a code with valid=false is meaningless.
#[derive(Clone, Debug)]
pub struct AdioAdc {
    /// The reply's `slot` value — declared `u8`.
    pub slot: u8,

    /// The reply's `ch` value — declared `u8`.
    pub ch: u8,

    /// The reply's `codes` value — declared `u16[]`.
    pub codes: Vec<u16>,

    /// The reply's `valid` value — declared `bool[]`.
    pub valid: Vec<bool>,
}

impl AdioAdc {
    fn parse(r: &Reply) -> Result<Self, Error> {
        Ok(Self {
            slot: uint(r, "slot")?,
            ch: uint(r, "ch")?,
            codes: uints(r, "codes")?,
            valid: flags(r, "valid")?,
        })
    }
}

/// Every published command, typed. Implemented for `Device` below.
pub trait Commands {
    /// Handshake. fw/gw are the firmware and gateware git shas (8 hex) — equal when both came from
    /// one build. The identity REGISTERS carry the same gateware sha pre-boot; the device DNA is
    /// not on this command (it is AXI-side, reachable via the human `kv id`).
    fn sys_hello(&mut self) -> Result<SysHello, Error>;

    /// Read the power tree, writing nothing. present = module-present bitmask from the most recent
    /// sequence pass (a raw detect read is NOT equivalent — after a pass those bits are outputs
    /// driving the DCDC enables). reverify = the periodic re-verify thread is enabled.
    fn power_status(&mut self) -> Result<PowerStatus, Error>;

    /// Run the rail sequence. Stim stays off. Level-set: safe to retry.
    fn power_up(&mut self) -> Result<PowerUp, Error>;

    /// Set a slot's two channel modes (I2C mux + CH_MODE, kept coherent).
    fn adio_mode(&mut self, slot: u8, ch1: ChMode, ch2: ChMode) -> Result<AdioMode, Error>;

    /// Cross-channel loopback tie. Independent of either channel's DIRECTION and of CH_MODE, and
    /// re-asserted with the expander, so setting a mode never silently drops a tie. The muxes make
    /// it a SELECT, not a strap: with the tie on, a channel routes to the other channel instead of
    /// to its jack.
    fn adio_fb(&mut self, slot: u8, on: u8) -> Result<AdioFb, Error>;

    /// Drive the digital outputs (bit0 CH1, bit1 CH2) and echo the RAW pad readback, so one call is
    /// both a stimulus and its measurement. A non-zero mask is refused on an absent slot: driving a
    /// pin high there back-powers the module rail. Note the pin only moves if that channel's
    /// CH_MODE is out, and a driven channel reads 0 on its own TTL lane by design -- read the OTHER
    /// channel, through the tie.
    fn adio_dout(&mut self, slot: u8, mask: u8) -> Result<AdioDout, Error>;

    /// n is range-checked, never silently clamped (the human `kv adio adc` clamps). valid\[i\]
    /// mirrors each sample's valid bit — a code with valid=false is meaningless.
    fn adio_adc(&mut self, slot: u8, ch: u8, n: Option<u8>) -> Result<AdioAdc, Error>;
}

impl Commands for Device {
    fn sys_hello(&mut self) -> Result<SysHello, Error> {
        let reply = checked(self.raw_cmd("sys.hello", &[])?)?;
        SysHello::parse(&reply)
    }

    fn power_status(&mut self) -> Result<PowerStatus, Error> {
        let reply = checked(self.raw_cmd("power.status", &[])?)?;
        PowerStatus::parse(&reply)
    }

    fn power_up(&mut self) -> Result<PowerUp, Error> {
        let reply = checked(self.raw_cmd("power.up", &[])?)?;
        PowerUp::parse(&reply)
    }

    fn adio_mode(&mut self, slot: u8, ch1: ChMode, ch2: ChMode) -> Result<AdioMode, Error> {
        if slot > 7 {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        let args: Vec<String> = vec![
            slot.to_string(),
            ch1.token().to_string(),
            ch2.token().to_string(),
        ];
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let reply = checked(self.raw_cmd("adio.mode", &args)?)?;
        AdioMode::parse(&reply)
    }

    fn adio_fb(&mut self, slot: u8, on: u8) -> Result<AdioFb, Error> {
        if slot > 7 {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        if on > 1 {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        let args: Vec<String> = vec![slot.to_string(), on.to_string()];
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let reply = checked(self.raw_cmd("adio.fb", &args)?)?;
        AdioFb::parse(&reply)
    }

    fn adio_dout(&mut self, slot: u8, mask: u8) -> Result<AdioDout, Error> {
        if slot > 7 {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        if mask > 3 {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        let args: Vec<String> = vec![slot.to_string(), mask.to_string()];
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let reply = checked(self.raw_cmd("adio.dout", &args)?)?;
        AdioDout::parse(&reply)
    }

    fn adio_adc(&mut self, slot: u8, ch: u8, n: Option<u8>) -> Result<AdioAdc, Error> {
        if slot > 7 {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        if ch > 1 {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        if let Some(n) = n {
            if !(1..=16).contains(&n) {
                return Err(Error::Host(HostErr::HostUnsafeArg));
            }
        }
        let mut args: Vec<String> = vec![slot.to_string(), ch.to_string()];
        if let Some(n) = n {
            args.push(n.to_string());
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let reply = checked(self.raw_cmd("adio.adc", &args)?)?;
        AdioAdc::parse(&reply)
    }
}

// RESERVED, and deliberately without typed methods: `sys.claim`, `sys.release`, `sys.challenge`,
// `sys.unlock`. Every one is `scope: session`, pending P3b — the grant canonicalisation and the
// signature scheme are NOT published, so a host must not depend on the shape of what it signs, and
// a build may legally answer `unknown_cmd` to any of them (kdi/contract.yaml:551-569). A typed
// method here would claim a settled shape. They stay in `COMMANDS` above, so `Device::raw_cmd` can
// still drive one positionally.

// ───────────────────────────────────────────────────────────────────── reply decoding
//
// A missing or ill-typed key is an ERROR, never a default. A device on a NEWER minor may ADD keys
// — that is what makes a minor additive, and nothing here rejects an unknown one — but a reply
// missing a key this contract declares is not from the device this build thinks it is, and
// `unwrap_or_default` would hand the bench a present-mask of 0: "no modules fitted",
// indistinguishable from the truth.

fn bad(key: &str, want: &str) -> Error {
    io_err(
        ErrorKind::InvalidData,
        format!("reply key `{key}` is not {want}"),
    )
}

fn at<'a>(r: &'a Reply, key: &str) -> Result<&'a Value, Error> {
    r.get(key).ok_or_else(|| bad(key, "present in the reply"))
}

/// Width-checked, never truncating: the device declares `present` as a u8 and a `v as u8` on a
/// wider value would silently report a different power tree than the one that answered.
fn uint<T: TryFrom<u64>>(r: &Reply, key: &str) -> Result<T, Error> {
    let v = at(r, key)?
        .as_u64()
        .ok_or_else(|| bad(key, "an unsigned integer"))?;
    T::try_from(v).map_err(|_| bad(key, "in range for its declared width"))
}

fn flag(r: &Reply, key: &str) -> Result<bool, Error> {
    at(r, key)?.as_bool().ok_or_else(|| bad(key, "a bool"))
}

fn text(r: &Reply, key: &str) -> Result<String, Error> {
    Ok(at(r, key)?
        .as_str()
        .ok_or_else(|| bad(key, "a string"))?
        .to_string())
}

fn uints<T: TryFrom<u64>>(r: &Reply, key: &str) -> Result<Vec<T>, Error> {
    at(r, key)?
        .as_array()
        .ok_or_else(|| bad(key, "an array"))?
        .iter()
        .map(|v| {
            let n = v
                .as_u64()
                .ok_or_else(|| bad(key, "an array of unsigned integers"))?;
            T::try_from(n).map_err(|_| bad(key, "in range for its declared width"))
        })
        .collect()
}

fn flags(r: &Reply, key: &str) -> Result<Vec<bool>, Error> {
    at(r, key)?
        .as_array()
        .ok_or_else(|| bad(key, "an array"))?
        .iter()
        .map(|v| v.as_bool().ok_or_else(|| bad(key, "an array of bools")))
        .collect()
}

fn object(r: &Reply, key: &str) -> Result<Value, Error> {
    Ok(at(r, key)?.clone())
}

/// A refusal reaches a TYPED caller as `Err`, while `Device::raw_cmd` still returns it as data
/// (lib.rs:12-15). The difference is what was asked for: `raw_cmd` hands back a `Reply` whose `rc`
/// the caller is obliged to read, but a reply that says `not_present` contains no `AdioAdc`, and
/// parsing one out of the absent keys would invent codes that were never sampled.
fn checked(r: Reply) -> Result<Reply, Error> {
    if r.ok() {
        Ok(r)
    } else {
        Err(Error::Device(r))
    }
}
