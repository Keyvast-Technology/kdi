//! KDI host library — identity-first discovery, the bind handshake, acquisition as decoded
//! records, the typed command channel. Everything physical is quarantined in a PRIVATE `Link`
//! enum: no public `trait Transport`, and no endpoint number outside a binding module.

// A `pub` item with no doc comment is a support ticket. This is deny rather than warn because a
// warning in a crate that already builds clean is a warning nobody sees.
#![deny(missing_docs)]
// `doc(cfg(..))` labels a feature-gated item in the rendered docs instead of letting it vanish from
// a default-feature build. NIGHTLY, so the gate and its uses sit behind `cfg(docsrs)` — set only by
// `rustdoc-args` in Cargo.toml, never by a normal build, which still compiles on stable.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod commands;
// The release manifest a bitstream is published with, and the judgement made against an image
// before it is flashed. No transport, no board: it is the offline half of the bind rule.
mod release;
mod spec;
mod stream;

pub use commands::*;
pub use release::*;
pub use spec::*;
pub(crate) use spec::{STREAM_REGS, USB3_REG};
#[cfg(feature = "usb3")]
pub(crate) use spec::{USB3_MSG, USB3_STREAM};
pub use stream::*;

/// The decoder, quarantined: `#[doc(hidden)]`, and NOT `pub(crate)` because its vector, robustness
/// and differential harnesses compile from outside — private costs `KDI_FUZZ_SOAK`'s own `--test`
/// target or `allow(dead_code)` over 56 items. `tests/layering.rs` keeps it out of the public API.
#[doc(hidden)]
pub mod codec;

/// The USB3 device driver this crate ships, and where it came from. The provenance table compiles
/// in with or without `--features bundled`: Cargo packages the whole source, so the bytes travel
/// either way. No gateware image here — [`Device::open_usb3_configured`] takes the caller's.
pub mod bundled;

mod udp;
#[cfg(feature = "usb3")]
mod usb3;
// The Verilator harness, `--features sim`. NOT a KDI transport binding — a raw-poke wire to the
// elaborated fabric, and the only thing checking this crate's register addresses against RTL rather
// than the descriptor they came from. Its module docs say what it misses (driver FFI, stream, cmd).
#[cfg(feature = "sim")]
mod simlink;

use std::collections::HashMap;
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

#[cfg(feature = "usb3")]
#[cfg_attr(docsrs, doc(cfg(feature = "usb3")))]
pub use usb3::SdkErr;

// ─────────────────────────────────────────────────────────────────────────── identity

/// A device's IDENTITY, plus a hint about where it answered. What [`find`] hands back and what
/// [`Device::open`] takes: a caller names a board by what it is, never by an address.
#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// The board's serial. The one field that identifies exactly one instrument, and the field a
    /// [`Filter`] normally selects on.
    pub serial: String,
    /// Who made it. Empty when the binding cannot answer without gateware.
    pub vendor: String,
    /// The hardware model string — what a host branches on to decide it can drive this board at
    /// all. Empty when the binding cannot answer without gateware; see [`Device::open_usb3`].
    pub compatible: String,
    /// The device's own board id, when it announced one.
    pub board_id: Option<u32>,
    /// `(major, minor)` as ANNOUNCED. Advisory only — the binding handshake reads
    /// `contract_version` off the device and refuses on its own terms; an announce file is not a
    /// device.
    pub kdi: Option<(u16, u16)>,
    /// Which binding reached it.
    pub transport: TransportKind,
    /// Where it answered — resolved from its identity, never part of it.
    pub addr: Addr,
}

/// Which binding a device was found through. `usb3` exists only in a build with `--features usb3`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum TransportKind {
    /// The software reference transport: datagrams to a software device model. No hardware.
    Udp,
    /// A real instrument, over its USB3 device driver.
    Usb3,
    /// The elaborated gateware under Verilator (`make kdi-sim`). Reported as its own kind rather
    /// than as `Usb3` because it is NOT the driver path: no driver, no board, no FFI.
    #[cfg(feature = "sim")]
    #[cfg_attr(docsrs, doc(cfg(feature = "sim")))]
    Sim,
}

impl TransportKind {
    fn token(self) -> &'static str {
        match self {
            TransportKind::Udp => "udp",
            TransportKind::Usb3 => "usb3",
            #[cfg(feature = "sim")]
            TransportKind::Sim => "sim",
        }
    }
}

/// Where the device answers. A HINT resolved from its identity, never part of its name
/// (the Python reference host).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum Addr {
    /// A UDP peer.
    Socket(SocketAddr),
    /// A board serial the USB3 driver opens by.
    Serial(String),
}

/// What to look for. EVERY FIELD UNSET MEANS ANY — a default `Filter` matches everything [`find`]
/// can reach, and each `Some` narrows it.
#[derive(Clone, Default, Debug)]
pub struct Filter {
    /// Exactly this serial.
    pub serial: Option<String>,
    /// Exactly this model string. Note that a binding which cannot read it without gateware
    /// reports an empty one, so this drops those devices rather than matching them.
    pub compatible: Option<String>,
    /// Exactly this board id.
    pub board_id: Option<u32>,
    /// Only devices reached through this binding.
    pub transport: Option<TransportKind>,
}

impl Filter {
    fn matches(&self, i: &DeviceInfo) -> bool {
        // Written out rather than folded into combinators: every field is "unset means any", and
        // that asymmetry is the whole semantics of a filter.
        if let Some(s) = &self.serial {
            if *s != i.serial {
                return false;
            }
        }
        if let Some(c) = &self.compatible {
            if *c != i.compatible {
                return false;
            }
        }
        if let Some(b) = self.board_id {
            if Some(b) != i.board_id {
                return false;
            }
        }
        if let Some(t) = self.transport {
            if t != i.transport {
                return false;
            }
        }
        true
    }
}

/// Discover devices by IDENTITY across every binding this build has, and report what failed: a
/// broken enumerator (an SDK that loads but does not answer, an unreadable discovery dir) is NOT
/// an empty bench. NOT errors: a probe that fails (it IS gone), or a transport this build lacks.
pub fn find(f: &Filter) -> (Vec<DeviceInfo>, Vec<Error>) {
    let mut found = Vec::new();
    let mut errs = Vec::new();

    // The software transport's enumeration analog: one JSON file per announcing device
    // (`kdi/transport.py:24-52`).
    let dir = std::env::var_os("KDI_DISCOVERY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("kdi-discovery"));
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for e in entries {
                match e {
                    Ok(e) if e.path().extension().is_some_and(|x| x == "json") => {
                        match announced(&e.path()) {
                            Ok(Some(info)) => found.push(info),
                            Ok(None) => {} // stale announcement, or a transport we cannot open
                            Err(err) => errs.push(err),
                        }
                    }
                    Ok(_) => {}
                    Err(err) => errs.push(Error::Io(err)),
                }
            }
        }
        // No directory at all is the ordinary "nothing announced" case, not a failure.
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => errs.push(Error::Io(e)),
    }

    #[cfg(feature = "usb3")]
    match usb3::enumerate() {
        Ok(list) => found.extend(list),
        Err(e) => errs.push(e),
    }

    found.retain(|i| f.matches(i));
    (found, errs)
}

/// Read one announcement and probe it. `Ok(None)` = the file describes nothing this build can
/// reach, or the device stopped answering; `Err` = the file itself is unreadable, which is a fault
/// on THIS machine and must reach the caller.
fn announced(path: &Path) -> Result<Option<DeviceInfo>, Error> {
    let text = std::fs::read_to_string(path).map_err(Error::Io)?;
    let v: Value = serde_json::from_str(&text).map_err(|e| {
        io_err(
            io::ErrorKind::InvalidData,
            format!("{}: {e}", path.display()),
        )
    })?;
    if v.get("transport").and_then(Value::as_str) != Some(TransportKind::Udp.token()) {
        return Ok(None);
    }
    let (Some(host), Some(port)) = (
        v.get("host").and_then(Value::as_str),
        v.get("port").and_then(Value::as_u64),
    ) else {
        return Ok(None);
    };
    let addr: SocketAddr = match format!("{host}:{port}").parse() {
        Ok(a) => a,
        Err(e) => return Err(io_err(io::ErrorKind::InvalidData, e.to_string())),
    };
    // The announcement is a file, not a device: probe before reporting it, or a crashed server
    // stays "found" until something unlinks its file (`kdi/transport.py:43-47`).
    match udp::probe(addr, Duration::from_millis(300)) {
        Ok(live) => Ok(info_from(&live, addr)),
        Err(Error::Host(HostErr::HostTimeout)) => Ok(None),
        Err(e) => Err(e),
    }
}

fn info_from(v: &Value, addr: SocketAddr) -> Option<DeviceInfo> {
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    Some(DeviceInfo {
        serial: match v.get("serial").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return None,
        },
        vendor: s("vendor"),
        compatible: s("compatible"),
        board_id: v.get("board_id").and_then(Value::as_u64).map(|b| b as u32),
        kdi: parse_kdi(&s("kdi")),
        transport: TransportKind::Udp,
        addr: Addr::Socket(addr),
    })
}

fn parse_kdi(s: &str) -> Option<(u16, u16)> {
    let (a, b) = s.split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

// ─────────────────────────────────────────────────────────────────────────── connect

/// How to bind. The default is "no capability requirement, the device's own published ready
/// window", which is what a caller who has nothing to say should pass.
#[derive(Clone)]
pub struct ConnectOpts {
    /// Capabilities the bind must REFUSE without, reported together as [`Skew::MissingCaps`].
    /// Gate on these, never on a version comparison: a minor is additive, so a version says
    /// nothing about which features a build actually has.
    pub need_caps: Vec<Cap>,
    /// How long to poll `contract_ready` before giving up. Defaults to the device's own published
    /// [`READY_TIMEOUT_MS`].
    pub ready_timeout: Duration,
}

impl Default for ConnectOpts {
    fn default() -> Self {
        Self {
            need_caps: Vec::new(),
            // A DEVICE property, published so a slower boot does not turn into a fleet of hosts
            // that each need a patch (`ready_timeout_ms`, kdi/contract.yaml:66-74).
            ready_timeout: Duration::from_millis(READY_TIMEOUT_MS),
        }
    }
}

/// What this host's LEASE request did at bind. `Unsupported` IS NOT A FAILURE: `sys.claim` is
/// `scope: session`, and a build without it answers `unknown_cmd` — treat that as "no such
/// facility" and proceed (contract.yaml:196-202). No `NotHeld`: `busy` is refused at bind.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Lease {
    /// This session holds the device. Every `attended` command is available.
    Held,
    /// This build implements no session command, so there is no lease to hold — and, per the
    /// contract, no error either. Today's firmware answers here; the software device model
    /// answers [`Lease::Held`].
    Unsupported,
}

/// An opaque, caller-unique lease tag: `host-<pid>-<hex>`. NOT A SECRET — the device compares it
/// for EQUALITY against the current holder, so it needs no crypto RNG and no new dependency, only
/// non-collision: pid separates local processes, nanos xor a stack address separate two machines.
fn mint_token() -> String {
    let here = 0u8;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let entropy = nanos ^ (&here as *const u8 as usize as u64);
    format!("host-{}-{entropy:x}", std::process::id())
}

/// How many times [`Device::set_rate`] programs before giving up. More than one because the first
/// DRP burst after configuration is not reliably applied on current gateware (#131).
const RATE_APPLY_TRIES: u32 = 4;

/// A bound device: the link, the identity it was opened as, the capability word read at bind, the
/// session's lease token, and a shadow of every WireIn word this host has written — the shadow is
/// why an unmasked field write cannot silently disarm a neighbouring stream (`write_field`).
pub struct Device {
    link: Link,
    info: DeviceInfo,
    caps: Caps,
    gateware_sha: u32,
    kdi: (u16, u16),
    wire_in: HashMap<u8, u32>,
    next_id: u32,
    /// This session's lease tag, sent as an ENVELOPE key on every request. Private: it is a
    /// session property, not something a caller composes a command out of.
    token: String,
    lease: Lease,
}

impl Device {
    /// Open a device [`find`] reported, and BIND it: check it speaks KDI, check the contract major,
    /// wait for `contract_ready`, read the capability word, take the lease. [`Error::Skew`] when
    /// this host may not drive it ([`Skew::Busy`] = another host has it); [`Error::Io`] if unbound.
    pub fn open(info: &DeviceInfo, opts: &ConnectOpts) -> Result<Device, Error> {
        match (&info.addr, info.transport) {
            (Addr::Socket(a), TransportKind::Udp) => {
                let link = Link::Udp(udp::Udp::connect(*a, opts.ready_timeout)?);
                Device::bind(link, info.clone(), opts)
            }
            #[cfg(feature = "usb3")]
            (Addr::Serial(s), TransportKind::Usb3) => {
                let link = Link::Usb3(usb3::Usb3::open(s, None, None)?);
                Device::bind(link, info.clone(), opts)
            }
            _ => Err(io_err(
                io::ErrorKind::Unsupported,
                format!(
                    "no binding for transport {} in this build (usb3 needs --features usb3)",
                    info.transport.token()
                ),
            )),
        }
    }

    /// Bind a UDP device by address, skipping discovery. `timeout` is both the RPC deadline and
    /// the `contract_ready` window.
    pub fn connect_udp(addr: SocketAddr, timeout: Duration) -> Result<Device, Error> {
        let mut link = Link::Udp(udp::Udp::connect(addr, timeout)?);
        let ident = link.discover()?;
        let info = info_from(&ident, addr).unwrap_or(DeviceInfo {
            serial: String::new(),
            vendor: String::new(),
            compatible: String::new(),
            board_id: None,
            kdi: None,
            transport: TransportKind::Udp,
            addr: Addr::Socket(addr),
        });
        Device::bind(
            link,
            info,
            &ConnectOpts {
                need_caps: Vec::new(),
                ready_timeout: timeout,
            },
        )
    }

    /// Open a board over the USB3 driver by serial (empty = the first found). **BINDS WHAT IS
    /// RUNNING, never flashes** — safe on a shared board; `open_usb3_configured` loads an image.
    /// Search order: `driver_dir`, `$KDI_DRIVER_DIR`, `bundled`, the OS. NOT EXERCISED BY CI.
    #[cfg(feature = "usb3")]
    #[cfg_attr(docsrs, doc(cfg(feature = "usb3")))]
    pub fn open_usb3(serial: &str, driver_dir: Option<&Path>) -> Result<Device, Error> {
        Device::usb3(serial, driver_dir, None)
    }

    /// Load `image` — **the** release bitstream (`make all`), not a copy this crate vendors — then
    /// bind. VOLATILE (no flash, lost on a power cycle) and STATE-CHANGING on a shared board:
    /// whatever ran is gone. A failed configure is an `Error::Sdk`; compare `gateware_sha` after.
    #[cfg(feature = "usb3")]
    #[cfg_attr(docsrs, doc(cfg(feature = "usb3")))]
    pub fn open_usb3_configured(serial: &str, image: &[u8]) -> Result<Device, Error> {
        Device::usb3(serial, None, Some(image))
    }

    /// The body both of the above share: open, optionally configure, then run the bind sequence.
    #[cfg(feature = "usb3")]
    fn usb3(
        serial: &str,
        driver_dir: Option<&Path>,
        image: Option<&[u8]>,
    ) -> Result<Device, Error> {
        let link = Link::Usb3(usb3::Usb3::open(serial, driver_dir, image)?);
        let info = DeviceInfo {
            serial: serial.to_string(),
            // EMPTY, NOT GUESSED. `device.vendor` / `device.compatible` are in contract.yaml but
            // not in the generated `spec.rs`, so a literal here would be a second source of truth.
            // Reported as a generator gap; until it closes, `Filter::compatible` drops usb3 boards.
            vendor: String::new(),
            compatible: String::new(),
            board_id: None,
            kdi: None,
            transport: TransportKind::Usb3,
            addr: Addr::Serial(serial.to_string()),
        };
        Device::bind(link, info, &ConnectOpts::default())
    }

    /// THE BIND SEQUENCE, in the one correct order (the Python reference host's
    /// `identity_registers`): not-KDI, major, ready, caps, LEASE. A `contract_version` of 0 is cut
    /// BEFORE the major compare — an unmapped WireOut reads 0, so a non-KDI board would bind clean.
    fn bind(link: Link, info: DeviceInfo, opts: &ConnectOpts) -> Result<Device, Error> {
        let mut d = Device {
            link,
            info,
            caps: Caps(0),
            gateware_sha: 0,
            kdi: (0, 0),
            wire_in: HashMap::new(),
            next_id: 0,
            token: mint_token(),
            // Overwritten by `claim()` at the end of this function; a device that never got that
            // far has no lease, which is what this says.
            lease: Lease::Unsupported,
        };
        let cv = d.read_reg("contract_version")?;
        if cv == 0 {
            return Err(Error::Skew(Skew::NotKdi));
        }
        let (major, minor) = ((cv >> 16) as u16, cv as u16);
        // Major equality and NOTHING ELSE. A device minor higher than the host's is always fine —
        // a minor is additive by definition (kdi/contract.yaml:63-63).
        if major != KDI_MAJOR {
            return Err(Error::Skew(Skew::Major {
                device: major,
                host: KDI_MAJOR,
            }));
        }
        d.kdi = (major, minor);
        d.info.kdi = Some((major, minor));
        d.wait_ready(opts.ready_timeout)?;
        d.caps = Caps(d.read_reg("caps")?);
        let missing: Vec<Cap> = opts
            .need_caps
            .iter()
            .copied()
            .filter(|c| !d.caps.has(*c))
            .collect();
        if !missing.is_empty() {
            return Err(Error::Skew(Skew::MissingCaps(missing)));
        }
        d.gateware_sha = d.read_reg("gateware_sha")?;
        // LAST, and only when the device advertises a command channel. Without that capability
        // there is nothing to claim; with it, every non-RO command is refused `not_claimed` until
        // the request carries the holder's token (`kdi/device.py:214-214`).
        d.lease = if d.caps.has(Cap::CommandProtocol) {
            d.claim()?
        } else {
            Lease::Unsupported
        };
        Ok(d)
    }

    /// Take the device lease, if this build has one. `rc == 0` held; `unknown_cmd` = no session
    /// command, not an error (see [`Lease`]); anything else, `busy` first, REFUSES THE BIND — a
    /// second host writes the same WireIns from a zero shadow and looks like a gateware regression.
    fn claim(&mut self) -> Result<Lease, Error> {
        // No message channel at all (the Verilator harness runs no firmware) is Unsupported, not a
        // failure. A transport error on a binding that HAS one is a bind failure — a dead vUART is
        // not "this build has no lease".
        if !self.link.has_message() {
            return Ok(Lease::Unsupported);
        }
        match self.raw_cmd("sys.claim", &[]) {
            Ok(r) if r.ok() => Ok(Lease::Held),
            Ok(r) if r.err == Some(DeviceErr::UnknownCmd) => Ok(Lease::Unsupported),
            Ok(r) => Err(Error::Skew(Skew::Busy(r))),
            Err(e) => Err(e),
        }
    }

    /// The identity this device was opened as, with `kdi` filled in from the wire rather than from
    /// an announcement. Pass it back to [`Device::open`] to reconnect to the same board.
    pub fn info(&self) -> &DeviceInfo {
        &self.info
    }

    /// Whether this session holds the device lease — and, if not, whether that is because the
    /// build has no lease to hold. A `busy` device never reaches here: it is refused at bind.
    pub fn lease(&self) -> Lease {
        self.lease
    }

    /// The DEVICE's contract `(major, minor)`, read off the wire at bind. The major equals
    /// [`KDI_MAJOR`] or the bind would have failed; the minor may be HIGHER than [`KDI_MINOR`],
    /// which is legal and additive.
    pub fn kdi(&self) -> (u16, u16) {
        self.kdi
    }

    /// The capability word read at bind. Branch on [`Caps::has`], never on [`Device::kdi`].
    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// Low 32 bits of the gateware's git sha, read from the identity registers before the CPU
    /// boots. This is what says WHICH bitstream is on the board — the answer to "did my flash
    /// actually take", which no other reading gives.
    pub fn gateware_sha(&self) -> u32 {
        self.gateware_sha
    }

    /// Poll `contract_ready`. The `init_calib` scar made explicit: acquiring before calibration
    /// silently drops beats, worst at one lane, and nothing else sees it (contract.yaml:63-63;
    /// `tools/rhd_term.py:82-114` is the blind 2 s sleep this replaces).
    pub fn wait_ready(&mut self, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.read_reg("contract_ready")? != 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::Skew(Skew::NotReady));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Send a command by NAME, args POSITIONAL in declared order (contract.yaml:263). `rc != 0` is
    /// `Ok(Reply)` — a device error is DATA. An escape hatch for a newer minor; prefer the typed
    /// [`Commands`]. A token off `ARG_CHARSET` is `HostUnsafeArg`, unsent: a CR appends a `kv` cmd.
    pub fn raw_cmd(&mut self, name: &str, args: &[&str]) -> Result<Reply, Error> {
        self.next_id = self.next_id.wrapping_add(1);
        let id = format!("{:x}", self.next_id);
        if !safe_token(name) || !args.iter().all(|a| safe_token(a)) {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        debug_assert!(safe_token(&id));
        // The lease token rides on EVERY request, claim included, exactly as the reference does
        // (`kdi/client.py:142`). It is an envelope key and never an argument, so it is not subject
        // to the charset gate above — but `mint_token` emits `[A-Za-z0-9-]` anyway.
        self.link.message(&id, name, args, &self.token)
    }

    /// Raise this session's tier: `sys.challenge`, then `sys.unlock` carrying `sign`'s Ed25519
    /// signature over [`grant_bytes`]. THE KEY NEVER ENTERS THIS CRATE — `sign` is the station's.
    /// `Ok` is the accepting reply (`tier` is the tier reached); a refusal is [`Error::Device`]
    /// with `tier_locked`, and a build with no unlock answers `unknown_cmd`. The grant is bound to
    /// the device's own DNA; the wildcard (`dna` 0) is the same three calls on [`Device::raw_cmd`].
    pub fn unlock_with(
        &mut self,
        tier: &str,
        sign: impl FnOnce(&[u8]) -> [u8; 64],
    ) -> Result<Reply, Error> {
        let ch = self.raw_cmd("sys.challenge", &[])?;
        if !ch.ok() {
            return Err(Error::Device(ch));
        }
        let int = |k: &str| ch.get(k).and_then(Value::as_u64);
        let (nonce, dna) = match (int("nonce").and_then(|n| u32::try_from(n).ok()), int("dna")) {
            (Some(n), Some(d)) => (n, d),
            _ => {
                let why = format!("sys.challenge: no u32 `nonce` + u64 `dna` in {}", ch.body());
                return Err(io_err(io::ErrorKind::InvalidData, why));
            }
        };
        let sig: String = sign(&grant_bytes(tier, nonce, dna))
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let args = [tier, &nonce.to_string(), &dna.to_string(), &sig];
        let r = self.raw_cmd("sys.unlock", &args)?;
        if r.ok() {
            Ok(r)
        } else {
            Err(Error::Device(r))
        }
    }

    /// Release the lease and drop the link. Takes `self`, so a closed device cannot be used again.
    /// Always `Ok`: the release is best effort. Dropping a `Device` instead closes the transport
    /// just the same but skips the release, so the board stays claimed until its lease expires.
    pub fn close(mut self) -> Result<(), Error> {
        // QUIESCE FIRST: acquisition state outlives the process, so a host that just exits hands
        // the next one a running engine and full pipes. Measured: one record per twenty sample
        // periods, sticky overrun CLEAR — valid frames, 95 % of the data gone, nothing reports it.
        let _ = self.quiesce();
        // BEST EFFORT, and its failure may not mask a close error (`kdi/client.py:186-190`: release
        // in the `try`, transport teardown in the `finally`). A device that stopped answering is
        // gone; reporting the stuck lease would turn every crashed link into two errors.
        if self.lease == Lease::Held {
            let _ = self.raw_cmd("sys.release", &[]);
        }
        // Drop is the rest of the close: the UDP socket and the driver handle both release in
        // their own Drop, so there is nothing more this can report that dropping does not do.
        drop(self);
        Ok(())
    }

    /// Flush both streams, disarm any other consumer, stop the engine — on the way IN if you may
    /// have inherited a dirty instrument, and [`Device::close`] does it on the way out. Otherwise
    /// the symptom is valid frames far too slowly, overrun clear. **Leaves it UNCONFIGURED (0.5).**
    pub fn quiesce(&mut self) -> Result<(), Error> {
        // CLEAR THE RUN BITS FIRST: they are host-written WireIn state, so the device-side reset
        // below cannot touch them, and a host that died mid-stream leaves it running for the next.
        // Measured: a samples-only host then gets one record per twenty periods, overrun clear.
        for s in [Stream::Samples, Stream::Digital] {
            let _ = self.write_field(crate::stream::run_reg(s), 0);
        }
        self.write_field("quiesce", 1)?;
        self.write_field("quiesce", 0)
    }

    /// Is the acquisition rate configured, so `rhd_matrix` may emit? False from power-up on a
    /// device that advertises [`Cap::RateControl`]. See [`Device::set_rate`].
    pub fn rate_ready(&mut self) -> Result<bool, Error> {
        Ok(self.read_reg("rate_ready")? & 1 != 0)
    }

    /// Configure the acquisition rate and VERIFY it took: POLLS `rate_ready` and re-applies, else
    /// `Io(TimedOut)`. `m`/`d` are the MMCM pair the device's rate table is keyed on; `0, 0` picks
    /// its default. Unconfigured it emits 1 frame per 20 periods, declaring full cadence (#131).
    pub fn set_rate(&mut self, m: u8, d: u8) -> Result<(), Error> {
        if !self.caps().has(Cap::RateControl) {
            // Older gateware has no gate and no readback: the rate is whatever it is, and saying so
            // is better than looping on a register that does not exist.
            return Err(io_err(
                io::ErrorKind::Unsupported,
                "this device does not advertise rate_control; its rate cannot be configured or verified",
            ));
        }
        let md = (u32::from(m) << 8) | u32::from(d);
        // Program at least once even if `rate_ready` already reads set: the caller asked for a
        // SPECIFIC rate, and a flag someone else's write left behind says nothing about which.
        for _ in 0..RATE_APPLY_TRIES {
            self.write_field("rate_md", md)?;
            self.write_field("rate_apply", 1)?;
            std::thread::sleep(Duration::from_millis(20));
            if self.rate_ready()? {
                return Ok(());
            }
        }
        Err(io_err(
            io::ErrorKind::TimedOut,
            format!(
                "rate_ready stayed clear after {RATE_APPLY_TRIES} attempts to program the rate"
            ),
        ))
    }

    fn read_reg(&mut self, name: &str) -> Result<u32, Error> {
        let r = reg(name)?;
        if r.kind != "wireout" {
            return Err(io_err(
                io::ErrorKind::InvalidInput,
                format!("{name} is a {} - not readable", r.kind),
            ));
        }
        self.link.reg_read(name, r)
    }

    /// EVERY WireIn field write is masked into this host's shadow of the WHOLE WORD (contract.yaml:
    /// 407-408). Both run bits share WireIn 0x11 and both burst bounds 0x13, so an unmasked write
    /// SILENTLY DISARMS THE OTHER STREAM. Write-only: the shadow starts at 0, as WireIns do.
    fn write_field(&mut self, name: &str, value: u32) -> Result<(), Error> {
        let r = reg(name)?;
        if r.kind == "wireout" {
            return Err(io_err(
                io::ErrorKind::InvalidInput,
                format!("{name} is a wireout - not writable"),
            ));
        }
        let word = self.wire_in.entry(r.addr).or_default();
        *word = apply_field(*word, r, value);
        let word = *word;
        self.link.reg_write(name, r, value, word)
    }
}

// ─────────────────────────────────────────────────────────────────────────── registers

/// One entry of the usb3 register map, resolved BY NAME. The numbers live only in the generated
/// `USB3_REG`; nothing in this crate writes an endpoint address.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct RegBind {
    kind: &'static str,
    addr: u8,
    lo: u8,
    width: u8,
}

impl RegBind {
    fn mask(self) -> u32 {
        // width 32 with lo 0 is the whole word; `1u32 << 32` would panic rather than mean `!0`.
        if self.width >= 32 {
            u32::MAX
        } else {
            ((1u32 << self.width) - 1) << self.lo
        }
    }
}

fn reg(name: &str) -> Result<RegBind, Error> {
    USB3_REG
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|&(_, kind, addr, lo, width)| RegBind {
            kind,
            addr,
            lo: lo.unwrap_or(0),
            width,
        })
        .ok_or_else(|| {
            io_err(
                io::ErrorKind::InvalidInput,
                format!("no register named {name} in this contract"),
            )
        })
}

fn apply_field(word: u32, r: RegBind, value: u32) -> u32 {
    let m = r.mask();
    (word & !m) | ((value << r.lo) & m)
}

/// `(run, burst, status, lanes)` for a stream, from the generated `STREAM_REGS`. This replaced an
/// `f"run_{stream}"` convention only the Python reference could know (the Python reference host).
/// `lanes` is `None` for a stream that declares no lane mask.
fn stream_regs(
    s: Stream,
) -> (
    &'static str,
    &'static str,
    &'static str,
    Option<&'static str>,
) {
    STREAM_REGS
        .iter()
        .find(|(n, ..)| *n == s.token())
        .map(|&(_, run, burst, status, lanes)| (run, burst, status, lanes))
        // Both tables are generated from the same contract block, so a miss is a generator bug and
        // not a runtime condition a caller could handle.
        .expect("STREAM_REGS and Stream are generated from the same contract")
}

// ─────────────────────────────────────────────────────────────────────────── link

/// The binding quarantine. Private on purpose — see the module docs.
enum Link {
    Udp(udp::Udp),
    #[cfg(feature = "usb3")]
    Usb3(usb3::Usb3),
    #[cfg(feature = "sim")]
    Sim(simlink::Sim),
}

impl Link {
    fn discover(&mut self) -> Result<Value, Error> {
        match self {
            Link::Udp(u) => u.discover(),
            #[cfg(feature = "usb3")]
            Link::Usb3(u) => Ok(u.identity()),
            #[cfg(feature = "sim")]
            Link::Sim(u) => Ok(u.identity()),
        }
    }

    // `r` and `word` are the ADDRESS-resolving bindings' half of the pair; the udp binding resolves
    // names on the device side, so they are genuinely unused without one of those features.
    #[cfg_attr(not(any(feature = "usb3", feature = "sim")), allow(unused_variables))]
    fn reg_read(&mut self, name: &str, r: RegBind) -> Result<u32, Error> {
        match self {
            // The UDP binding is a NAME map: the device resolves the field itself, so the host
            // must not shift what it gets back (`bindings.udp.note`, kdi/contract.yaml:196).
            Link::Udp(u) => u.reg_read(name),
            #[cfg(feature = "usb3")]
            Link::Usb3(u) => u.reg_read(r),
            #[cfg(feature = "sim")]
            Link::Sim(u) => u.reg_read(r),
        }
    }

    #[cfg_attr(not(any(feature = "usb3", feature = "sim")), allow(unused_variables))]
    fn reg_write(&mut self, name: &str, r: RegBind, field: u32, word: u32) -> Result<(), Error> {
        match self {
            Link::Udp(u) => u.reg_write(name, field),
            #[cfg(feature = "usb3")]
            Link::Usb3(u) => u.reg_write(r, word),
            #[cfg(feature = "sim")]
            Link::Sim(u) => u.reg_write(r, word),
        }
    }

    fn stream_read(&mut self, s: Stream, buf: &mut [u8]) -> Result<usize, Error> {
        match self {
            Link::Udp(u) => u.stream_read(s.token(), buf),
            #[cfg(feature = "usb3")]
            Link::Usb3(u) => u.stream_read(s, buf),
            #[cfg(feature = "sim")]
            Link::Sim(u) => u.stream_read(s, buf),
        }
    }

    /// `token` is the ENVELOPE key, and each binding encodes it — or cannot — on its own terms:
    /// the udp request is an object with room for it, the usb3 line form has none. See both.
    fn message(
        &mut self,
        id: &str,
        name: &str,
        args: &[&str],
        token: &str,
    ) -> Result<Reply, Error> {
        match self {
            Link::Udp(u) => u.message(id, name, args, token),
            #[cfg(feature = "usb3")]
            Link::Usb3(u) => u.message(id, name, args, token),
            #[cfg(feature = "sim")]
            Link::Sim(u) => u.message(id, name, args, token),
        }
    }

    fn has_message(&self) -> bool {
        match self {
            Link::Udp(_) => true,
            #[cfg(feature = "usb3")]
            Link::Usb3(_) => true,
            #[cfg(feature = "sim")]
            Link::Sim(_) => false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── values

/// The device's capability bitmap, read once at bind. The raw word is public so it can be logged
/// or compared verbatim; [`Caps::has`] and [`Caps::iter`] are how it is read.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Caps(pub u32);

/// Every capability bit, in bit order. HAND-WRITTEN: the generated `spec.rs` publishes no `ALL`
/// array and no `Cap::from_bit` (a reported generator gap). `caps_list_is_complete` below is an
/// exhaustive match, so a new contract capability stops this crate compiling until a human acts.
const ALL_CAPS: [Cap; 9] = [
    Cap::CleanFrame,
    Cap::CommandProtocol,
    Cap::Ddr3,
    Cap::Adio,
    Cap::Grounding,
    Cap::TtlIn,
    Cap::SlotHealth,
    Cap::FieldUpdate,
    Cap::RateControl,
];

impl Caps {
    /// Does this build have that capability? THE ONLY correct feature test — a version comparison
    /// is not one, because a minor is additive and says nothing about what a build implements.
    pub fn has(self, c: Cap) -> bool {
        self.0 >> c.bit() & 1 != 0
    }

    /// Every capability the device reported, in bit order. For logging what a board can do; a
    /// decision about ONE feature is [`Caps::has`].
    pub fn iter(self) -> impl Iterator<Item = Cap> {
        ALL_CAPS.into_iter().filter(move |c| self.has(*c))
    }
}

/// A device's answer. `#[must_use]` because `rc != 0` is returned, never raised: a dropped `Reply`
/// is a device error nobody looked at.
#[must_use]
#[derive(Clone, Debug)]
pub struct Reply {
    /// The request id this answers. Already checked against the request that was sent, so a late
    /// reply from a timed-out command can never arrive here as this command's answer.
    pub id: String,
    /// A platform errno, INFORMATIVE ONLY — the same "unknown command" is -38 on Linux and -88 on
    /// this target, which is exactly why `err` is the contract (kdi/contract.yaml:23-23).
    pub rc: i32,
    /// `None` on success — and also for a token this build does not know, which is legal from a
    /// device on a newer minor and must never be a parse failure. `ok()` still reports false,
    /// because it tests `rc`.
    pub err: Option<DeviceErr>,
    body: Value,
    raw: String,
}

impl Reply {
    /// Did the device accept the command? Tests `rc`, so it is false for a refusal whose `err`
    /// token this build does not recognise.
    pub fn ok(&self) -> bool {
        self.rc == 0
    }

    /// One key of the reply body, untyped. The typed [`Commands`] methods are what a caller
    /// normally wants; this is for a command on a newer minor that has no generated method yet.
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.body.get(k)
    }

    /// The reply object as it arrived, for a log or an archive.
    pub fn body(&self) -> &str {
        &self.raw
    }
}

fn reply_from(v: Value) -> Result<Reply, Error> {
    if !v.is_object() {
        return Err(io_err(
            io::ErrorKind::InvalidData,
            format!("reply is not a JSON object: {v}"),
        ));
    }
    let id = match v.get("id") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    Ok(Reply {
        id,
        // A reply with no `rc` is malformed, and -1 keeps `ok()` false rather than inventing
        // success out of a missing field.
        rc: v.get("rc").and_then(Value::as_i64).unwrap_or(-1) as i32,
        err: v
            .get("err")
            .and_then(Value::as_str)
            .and_then(DeviceErr::from_token),
        raw: v.to_string(),
        body: v,
    })
}

impl DeviceErr {
    /// Is retrying this command capable of a different answer? HAND-WRITTEN and exhaustive,
    /// mirroring `errors.*.retryable`. No wildcard arm, ever: a new token must stop this compiling,
    /// because the wrong default is dangerous — a retry loop on `not_present` drives absent pins.
    pub fn retryable(self) -> bool {
        match self {
            DeviceErr::NoDevice => true, // a Zephyr device is not ready yet
            DeviceErr::Busy => true,     // another host's lease may expire
            DeviceErr::NotReady => true, // boot/calibration still running
            DeviceErr::I2cNak => true,   // an EEPROM NAKs through its ~5 ms write cycle
            DeviceErr::BadArgs => false,
            DeviceErr::UnknownCmd => false,
            DeviceErr::TierLocked => false,
            DeviceErr::NotPresent => false,
            DeviceErr::NoIp => false,
            DeviceErr::Internal => false,
            DeviceErr::NotClaimed => false,
            DeviceErr::ConfirmRequired => false,
            DeviceErr::RoRegister => false,
            DeviceErr::NoSuchRegister => false,
            DeviceErr::ResponseTooLarge => false,
            DeviceErr::I2cTimeout => false, // a held bus does not free itself
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── errors

/// THERE IS NO DECODE VARIANT, deliberately: a rejected frame is a fact about link quality, not
/// something a caller can act on mid-recording (the Python `walk()` killed the run over one flipped
/// bit). [`StreamReader`] resyncs and counts it; every `Error` here is transport, host or device.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The device answered and REFUSED. Only the typed methods mint this — the generated
    /// [`Commands`] and [`Device::unlock_with`]: [`Device::raw_cmd`] returns `rc != 0` as data
    /// because its caller holds the `Reply`, but a typed method asked for a value and a refusal has
    /// none — else a struct of zeros from no keys.
    Device(Reply),
    /// THIS HOST refused, and nothing went to the wire — an argument outside the contract's
    /// charset, a deadline that expired, a transport that returned less than its framing declared.
    /// The token set is closed; a conforming library mints none of its own.
    Host(HostErr),
    /// The device is not one this host may drive. Always from the bind.
    Skew(Skew),
    /// The link, the filesystem or the operating system failed. Also carries this crate's
    /// argument-validation failures that are not part of the closed host set — a register name
    /// that is not in the contract, a reply that is not JSON, an ambiguous singular accessor.
    Io(io::Error),
    /// A device driver call failed, or a symbol was not in the library.
    #[cfg(feature = "usb3")]
    #[cfg_attr(docsrs, doc(cfg(feature = "usb3")))]
    Sdk(SdkErr),
}

/// The device is not one this host may talk to. Refused at BIND, never later: every one of these
/// is a reason the traffic that would follow cannot be trusted.
#[derive(Debug)]
#[non_exhaustive]
pub enum Skew {
    /// `contract_version` read 0 — an unmapped WireOut. NOT major 0.
    NotKdi,
    /// The device's contract MAJOR is not this host's. Majors are not compatible: flash the
    /// matching release rather than trying to talk across it.
    Major {
        /// The major the device announced.
        device: u16,
        /// [`KDI_MAJOR`], the major this host implements.
        host: u16,
    },
    /// `contract_ready` never set within [`ConnectOpts::ready_timeout`]. On a real board that
    /// window covers boot and DDR3 calibration; acquiring before it completes silently drops beats.
    NotReady,
    /// The device lacks capabilities [`ConnectOpts::need_caps`] asked for. Carries exactly the
    /// missing ones.
    MissingCaps(Vec<Cap>),
    /// `sys.claim` was refused — another host holds the board. A SKEW, not a device error: the
    /// second host's WireIn shadow starts at zero and knows nothing of the fields the holder owns.
    /// Carries the reply, so a log keeps the token the device actually sent.
    Busy(Reply),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // `err` before `rc`: the token IS the contract, the errno is a platform accident
            // (kdi/contract.yaml:23-23). An unknown token prints as `?` rather than being dropped
            // — it means a device on a newer minor, not a malformed reply.
            Error::Device(r) => write!(
                f,
                "device refused: {} (rc {})",
                r.err.map_or("?", DeviceErr::token),
                r.rc
            ),
            Error::Host(h) => write!(f, "{}", h.token()),
            Error::Skew(s) => write!(f, "{s}"),
            Error::Io(e) => write!(f, "{e}"),
            #[cfg(feature = "usb3")]
            Error::Sdk(e) => write!(f, "{e}"),
        }
    }
}

impl fmt::Display for Skew {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Skew::NotKdi => write!(
                f,
                "device does not speak KDI (contract_version reads 0) - flash a KDI bitstream"
            ),
            Skew::Major { device, host } => write!(
                f,
                "device contract major {device}, this host needs {host} - flash the matching release"
            ),
            Skew::NotReady => write!(
                f,
                "device never became contract_ready (boot/calibration window exceeded)"
            ),
            Skew::MissingCaps(c) => {
                let names: Vec<&str> = c.iter().map(|c| c.token()).collect();
                write!(f, "device lacks required capabilities: {}", names.join(", "))
            }
            Skew::Busy(r) => write!(
                f,
                "device busy, held by another host ({}) - one holder at a time",
                r.err.map_or("?", DeviceErr::token)
            ),
        }
    }
}

impl std::error::Error for Error {}

fn io_err(kind: io::ErrorKind, msg: impl Into<String>) -> Error {
    Error::Io(io::Error::new(kind, msg.into()))
}

/// A blocking-socket deadline is `WouldBlock` on Linux and `TimedOut` on Windows — both mean the
/// peer stopped answering, and the closed host set has ONE token for it (`host_timeout`,
/// contract.yaml:48-48). One definition: `udp.rs` and `simlink.rs` each carried their own copy.
pub(crate) fn io_or_timeout(e: io::Error) -> Error {
    match e.kind() {
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => Error::Host(HostErr::HostTimeout),
        _ => Error::Io(e),
    }
}

/// The compiled form of `ARG_CHARSET`. `charset_matches_spec` below fails if the contract ever
/// widens or narrows it, which is the only way this hand-expansion can drift.
fn safe_token(t: &str) -> bool {
    !t.is_empty()
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.' || b == b'-')
}

/// THE SIGNING BYTES of a `sys.unlock` grant: the compact, key-sorted JSON the device rebuilds
/// from the three positional args (`{"dna":<dna>,"nonce":<nonce>,"tier":"<tier>"}`), pinned byte
/// for byte by `kdi/vectors/unlock_vectors.json`. Sign them with Ed25519 outside this crate — a
/// station tool holds the key — and hand the 64-byte signature to [`Device::unlock_with`].
pub fn grant_bytes(tier: &str, nonce: u32, dna: u64) -> Vec<u8> {
    // `json!` keys come out sorted (serde_json's default map) — and dna, nonce, tier is ALSO the
    // insertion order, so a downstream `preserve_order` feature cannot change these bytes.
    serde_json::json!({"dna": dna, "nonce": nonce, "tier": tier})
        .to_string()
        .into_bytes()
}

// ─────────────────────────────────────────────────────────────────────────── checks
// The three things in this crate that are logic rather than plumbing, and need no device: the field
// masking (silent failure), the charset (command injection), and the capability list itself.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charset_matches_spec() {
        assert_eq!(ARG_CHARSET, "[A-Za-z0-9_.-]+");
        assert!(safe_token("adio.mode") && safe_token("0") && safe_token("a-b_c"));
        // The three that make this a security check rather than a tidiness one: CR appends a `kv`
        // command to the shared human console, LF the same, and 0x1e forges a response frame.
        assert!(!safe_token("0\rkv power up"));
        assert!(!safe_token("0\nkv"));
        assert!(!safe_token("\u{1e}003{}"));
        assert!(!safe_token("a b") && !safe_token(""));
    }

    #[test]
    fn field_writes_do_not_clear_their_neighbour() {
        // Resolved by NAME: this test would keep passing against literal addresses even if the
        // contract moved the words.
        let (run_s, run_d) = (reg("run_samples").unwrap(), reg("run_digital").unwrap());
        assert_eq!(run_s.addr, run_d.addr, "both run bits share one WireIn");
        let word = apply_field(0, run_d, 1);
        let word = apply_field(word, run_s, 1);
        assert_eq!(word, (1 << run_d.lo) | (1 << run_s.lo));
        // Stopping `samples` must leave `digital` acquiring. Unmasked, this write is the defect
        // `masked_field_write` exists for: it silently disarms the other stream.
        assert_eq!(apply_field(word, run_s, 0), 1 << run_d.lo);

        let (b_s, b_d) = (reg("burst_samples").unwrap(), reg("burst_digital").unwrap());
        assert_eq!(b_s.addr, b_d.addr, "both burst bounds share one WireIn");
        let word = apply_field(apply_field(0, b_d, 0xFFFF), b_s, 8);
        assert_eq!(word, (8 << b_s.lo) | 0xFFFF);
        // A value wider than the field must not bleed into the neighbour either.
        assert_eq!(apply_field(word, b_s, 0x1_0000), 0xFFFF);
    }

    #[test]
    fn whole_word_registers_mask_the_whole_word() {
        let lanes = reg("lanes_samples").unwrap();
        assert_eq!(lanes.width, 32);
        assert_eq!(apply_field(0xdead_beef, lanes, 0), 0);
        assert_eq!(apply_field(0, lanes, u32::MAX), u32::MAX);
    }

    #[test]
    fn caps_list_is_complete() {
        for c in ALL_CAPS {
            // Exhaustive on purpose: a capability added to the contract breaks this match, which
            // is the only thing that can catch a stale ALL_CAPS.
            match c {
                Cap::CleanFrame
                | Cap::CommandProtocol
                | Cap::Ddr3
                | Cap::Adio
                | Cap::Grounding
                | Cap::TtlIn
                | Cap::SlotHealth
                | Cap::FieldUpdate
                | Cap::RateControl => {}
            }
        }
        assert!(ALL_CAPS.windows(2).all(|w| w[0].bit() < w[1].bit()));
        let all = Caps(u32::MAX);
        assert_eq!(all.iter().count(), ALL_CAPS.len());
        assert_eq!(Caps(0).iter().count(), 0);
        let one = Caps(1 << Cap::Ddr3.bit());
        assert!(one.has(Cap::Ddr3) && !one.has(Cap::Adio));
        assert_eq!(one.iter().collect::<Vec<_>>(), vec![Cap::Ddr3]);
    }

    #[test]
    fn stream_registers_resolve_for_every_stream() {
        for s in [Stream::Samples, Stream::Digital] {
            let (run, burst, status, lanes) = stream_regs(s);
            assert_eq!(reg(run).unwrap().kind, "wirein");
            assert_eq!(reg(burst).unwrap().kind, "wirein");
            assert_eq!(reg(status).unwrap().kind, "wireout");
            // `lanes` is optional in the contract and only `samples` declares one. Asserting the
            // ABSENCE matters too: `Acquisition::lanes` is silently ignored for a stream with no
            // mask, and a bogus `lanes_digital` would write a register the device does not decode.
            assert_eq!(lanes.is_some(), s == Stream::Samples);
            if let Some(lanes) = lanes {
                assert_eq!(reg(lanes).unwrap().kind, "wirein");
            }
        }
    }

    #[test]
    fn retryable_matches_the_contract() {
        assert!(DeviceErr::Busy.retryable() && DeviceErr::NotReady.retryable());
        assert!(!DeviceErr::NotPresent.retryable() && !DeviceErr::BadArgs.retryable());
    }
}
