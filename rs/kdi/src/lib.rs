//! KDI host library — identity-first discovery, the bind handshake, acquisition as a stream of
//! decoded records, and the typed command channel.
//!
//! Everything physical is quarantined in a PRIVATE `Link` enum. There is deliberately no public
//! `trait Transport`: a trait with one useful implementation is an abstraction nobody asked for,
//! and it would freeze a shape before a second binding exists to argue with it. The quarantine —
//! "no endpoint number appears outside the binding module" — is the property that actually
//! matters, and an enum gives it identically (`kdi/rs/README.md`, P6 is explicit future work).
//!
//! Three rules run through the whole file, each one a defect this project already shipped:
//!
//! * **A device error is DATA.** [`Device::raw_cmd`] returns `Ok(Reply)` for a well-framed reply with
//!   `rc != 0`; only a host/transport failure is `Err`. The Python reference raises
//!   `CommandError` (the Python reference host), so a caller cannot distinguish "the device said
//!   not_present" from "the link died". The generated [`Commands`] methods are the one exception
//!   and say why at [`Error::Device`]: they were asked for a value the refusal does not contain.
//! * **Enumeration errors are RETURNED.** [`find`] hands back everything it could not enumerate,
//!   because "driver present but broken" must be distinguishable from "no board"
//!   (the Python reference host catches bare `Exception` and loses that).
//! * **The wire is not the API.** A transport read is bytes, and a frame straddles two of them
//!   routinely — but that is [`StreamReader`]'s problem, not a caller's. Frame alignment, CRC,
//!   resync, reject tokens and partial-frame carry live inside this crate; [`Device::start`] hands
//!   back [`Record`]s. The decoder is the hidden [`codec`] module and no type of its appears in
//!   this crate's public API — it is an implementation detail, not a second product, and a host
//!   that had to name one of its types would be back to owning the plumbing.

// A `pub` item with no doc comment is a support ticket. This is deny rather than warn because a
// warning in a crate that already builds clean is a warning nobody sees.
#![deny(missing_docs)]
// `doc(cfg(..))` labels a feature-gated item in the rendered docs instead of letting it vanish
// from a default-feature build. It is a NIGHTLY feature, so both the `feature` gate and every use
// of it are behind `cfg(docsrs)` — set only by the `rustdoc-args` in Cargo.toml, never by a normal
// build, which therefore compiles on stable exactly as before.
#![cfg_attr(docsrs, feature(doc_cfg))]

mod commands;
mod spec;
mod stream;

pub use commands::*;
pub use spec::*;
pub(crate) use spec::{STREAM_REGS, USB3_REG};
#[cfg(feature = "usb3")]
pub(crate) use spec::{USB3_MSG, USB3_STREAM};
pub use stream::*;

/// The decoder, quarantined.
///
/// `#[doc(hidden)]`, so it is absent from the docs and from every reasonable reading of this
/// crate's API — but NOT `pub(crate)`, deliberately: the decoder's own vector, robustness and
/// differential harnesses are integration tests and an example, which compile against the crate
/// from outside and therefore cannot reach a `pub(crate)` module. Making it private would mean
/// either moving those into `src/` as unit tests, losing `KDI_FUZZ_SOAK`'s separate `--test`
/// target, or a blanket `#![allow(dead_code)]` over the 56 items nothing else in this crate calls.
///
/// Nothing here is supported. `tests/layering.rs` still asserts no codec type reaches this crate's
/// real public API, which is the property that ever mattered.
#[doc(hidden)]
pub mod codec;

/// The USB3 device driver this crate ships, and where it came from. The provenance table is
/// compiled in whether or not `--features bundled` is on, because Cargo packages a crate's whole
/// source and the bytes travel with every copy of it either way. There is no gateware image here:
/// [`Device::open_usb3_configured`] takes the bitstream the caller already has.
pub mod bundled;

mod udp;
#[cfg(feature = "usb3")]
mod usb3;
// The Verilator harness, `--features sim`. NOT a KDI transport binding — a raw-poke wire to the
// elaborated fabric, and the only thing that has ever checked this crate's register addresses
// against RTL rather than against the descriptor they came from. See its module docs for what it
// does not test (the USB3 driver FFI, the stream, the command channel).
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

/// Discover devices by IDENTITY across every binding this build has, and report what failed.
///
/// The second half of the tuple is the point: a broken enumerator (an SDK that loads but does not
/// answer, a discovery directory that cannot be read) is NOT the same observation as an empty
/// bench, and a host that collapses the two tells a user to check their cable when the fault is on
/// their own machine.
///
/// Not errors, and therefore not returned: an announced device that fails its probe (it is gone —
/// that IS "no board"), and an announcement for a transport this build cannot open.
pub fn find(f: &Filter) -> (Vec<DeviceInfo>, Vec<Error>) {
    let mut found = Vec::new();
    let mut errs = Vec::new();

    // The software transport's enumeration analog: one JSON file per announcing device
    // (`kdi/transport.py:67-99`).
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
    // stays "found" until something unlinks its file (`kdi/transport.py:87-93`).
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
            // that each need a patch (`ready_timeout_ms`, kdi/contract.yaml:123-127).
            ready_timeout: Duration::from_millis(READY_TIMEOUT_MS),
        }
    }
}

/// What happened when this host asked for the device LEASE at bind.
///
/// `Unsupported` IS NOT A FAILURE. `sys.claim` is a `scope: session` command, and the contract's
/// own rule for those is capability discovery BY TRYING: "a build that does not implement it
/// answers `unknown_cmd`, and A HOST MUST TREAT `unknown_cmd` ON A SESSION COMMAND AS 'this build
/// has no such facility' AND PROCEED" (kdi/contract.yaml:512-519). Today's firmware implements no
/// session command and the software reference device implements all four — so a real board
/// reporting `Unsupported` and the model reporting `Held` are both correct, and a host that
/// treated the first as an error could never bind a board.
///
/// There is no `NotHeld`: `busy` is refused at bind and never becomes a state to observe here.
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

/// An opaque, caller-unique lease tag: `host-<pid>-<hex>`, in the spirit of the Python reference host.
///
/// NOT A SECRET, so deliberately not a crypto RNG and deliberately not a new dependency: the
/// device compares it for EQUALITY against the current holder (the Python reference host), so the only
/// property it needs is not colliding with another host's. The pid separates processes on this
/// machine; the clock's nanoseconds mixed with the address of a stack local separate two processes
/// that started in the same nanosecond on different machines.
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

/// A bound device. Holds the link, the identity it was opened as, the capability word read at
/// bind, the lease token minted for this session, and a shadow of every WireIn word this host has
/// written (`Device::write_field`, private — the shadow is why an unmasked field write cannot
/// silently disarm a neighbouring stream).
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
    /// wait for `contract_ready`, read the capability word, and take the device lease.
    ///
    /// Errors are [`Error::Skew`] when the device is not one this host may drive (see [`Skew`] for
    /// each reason, [`Skew::Busy`] included — another host holds the board), and [`Error::Io`] when
    /// this build has no binding for the device's transport.
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

    /// Open a board over the USB3 device driver, by serial — empty for the first one found.
    ///
    /// **It BINDS WHAT IS RUNNING and never flashes**, which is the whole reason it is safe to
    /// point at a shared instrument: a host that configures the FPGA before reading its identity
    /// has learned nothing about the device it found (the project's engineering notes, "Verify the artifact that is
    /// actually on the bench"). [`Device::open_usb3_configured`] is the one that loads an image
    /// the caller supplies.
    ///
    /// `driver_dir` is where to look for the driver, and it WINS over everything else: then
    /// `$KDI_DRIVER_DIR` (a path list), then the copy compiled in under the `bundled` feature,
    /// then the operating system's own search path. Pass `None` unless you are pointing a
    /// build at a driver it was not made with.
    ///
    /// NOT EXERCISED BY CI — there is no driver on a CI machine and the board is remote
    ///. Every entry point resolves by NAME at load time, so a wrong
    /// guess is `Error::Sdk("<symbol>")` on the bench, not a link failure here. Verified by hand
    /// against a real instrument, matching the reference host's handshake.
    #[cfg(feature = "usb3")]
    #[cfg_attr(docsrs, doc(cfg(feature = "usb3")))]
    pub fn open_usb3(serial: &str, driver_dir: Option<&Path>) -> Result<Device, Error> {
        Device::usb3(serial, driver_dir, None)
    }

    /// Load `image` into the FPGA, then bind it.
    ///
    /// `image` is **the** release bitstream — `make all` / the GitHub release asset — not a second
    /// copy this crate vendors. Configuration is **VOLATILE**: it does not touch flash and is lost
    /// on a power cycle. It also happens to whatever board answers to `serial`, so this is a
    /// state-changing call on a shared instrument: whatever was running is gone until someone
    /// loads it again.
    ///
    /// **This is NOT what [`Device::open_usb3`] does, and the distinction is load-bearing.**
    /// `open_usb3` binds what is already running and never flashes, because a host that configures
    /// first has learned nothing about the device it found — an artifact gate that reconfigures
    /// cannot tell you what was on the bench (the project's engineering notes, "Verify the artifact that is actually on
    /// the bench"). Use this one to PUT a known image on a board; use `open_usb3` to find out what
    /// a board is running.
    ///
    /// The configure's status is checked and a failure is an `Error::Sdk` naming the operation and
    /// the status — never a silent success onto a board still running the resident bitstream. The
    /// bind that follows then waits on the contract's own `contract_ready` register rather than
    /// sleeping a fixed interval (`kdi/contract.yaml:115-121`), so calibration is observed rather
    /// than assumed.
    /// An empty or ABI-sized-too-large image is rejected before the driver is loaded or the board
    /// is opened.
    ///
    /// This call does not compare [`Device::gateware_sha`] against a compiled-in constant: without
    /// a known image the library cannot know the WireOut sha. Callers who care compare
    /// [`Device::gateware_sha`] / [`Device::kdi`] themselves against the artifact they just sent.
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
            // not in the generated `spec.rs`, so there is nothing to resolve them from and a
            // literal here would be a second source of truth. Reported as a generator gap; the
            // consequence is that a `Filter::compatible` drops usb3 boards until it is closed.
            vendor: String::new(),
            compatible: String::new(),
            board_id: None,
            kdi: None,
            transport: TransportKind::Usb3,
            addr: Addr::Serial(serial.to_string()),
        };
        Device::bind(link, info, &ConnectOpts::default())
    }

    /// THE BIND SEQUENCE, in the one order that is correct (the Python reference host, published in
    /// `identity_registers`): not-KDI, then major, then ready, then caps, then the LEASE.
    ///
    /// `contract_version == 0` MUST be refused BEFORE the major comparison. An unmapped WireOut
    /// reads 0, so 0 is the ABSENCE of the register rather than major 0 — and while our own major
    /// is 0 the comparison cannot tell them apart, so a non-KDI bitstream would otherwise bind
    /// successfully and every subsequent read would be a plausible-looking zero.
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
        // a minor is additive by definition (kdi/contract.yaml:111-112).
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
        // the request carries the holder's token (`kdi/device.py:306-307`).
        d.lease = if d.caps.has(Cap::CommandProtocol) {
            d.claim()?
        } else {
            Lease::Unsupported
        };
        Ok(d)
    }

    /// Take the device lease, if this build has one (the Python reference host).
    ///
    /// Three answers, and only one of them refuses:
    ///
    /// * `rc == 0` — held.
    /// * `unknown_cmd` — this build implements no session command, which is not an error; see
    ///   [`Lease`].
    /// * anything else, `busy` first among them — REFUSE THE BIND. The board is a single-holder
    ///   resource, and a second host that proceeded anyway would write the same WireIn words from
    ///   its own zero-initialised shadow ([`Device::write_field`]): the damage presents downstream
    ///   as a gateware regression, not as two hosts (kdi/contract.yaml:531-534).
    ///
    /// A binding with no message channel at all (`Link::has_message` is false — the Verilator
    /// harness, which runs no firmware) reports [`Lease::Unsupported`] without sending. That is
    /// the `supports("message")` gate in the reference (the Python reference host). A transport
    /// failure on a binding that DOES have a message channel is a bind failure: a dead vUART is
    /// not "this build has no lease".
    fn claim(&mut self) -> Result<Lease, Error> {
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
    /// silently drops beats, worst at one lane, and there is no other way to observe it
    /// (kdi/contract.yaml:115-121; `tools/rhd_term.py:123-136` is the blind 2 s sleep this
    /// replaces).
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

    /// Send a command by NAME, arguments POSITIONAL in the order the contract declares them
    /// (`request.arg_order: declared`, kdi/contract.yaml:770).
    ///
    /// The escape hatch, and it is load-bearing rather than a convenience: a device on a NEWER
    /// MINOR legitimately has commands this build's generated methods do not know, and the
    /// contract's rule is that a higher minor binds normally. Prefer the typed methods in
    /// [`Commands`] — they check every declared range before a byte is sent.
    ///
    /// Every token — id, name and each argument — must match `ARG_CHARSET` or this returns
    /// `Err(Host(HostUnsafeArg))` WITHOUT SENDING A BYTE. This wire is shared with an ungated
    /// human shell: a value containing CR appends an arbitrary `kv` command (which can write
    /// EEPROMs) and one containing the response sentinel forges a reply
    /// (`arg_charset_rule`, kdi/contract.yaml:772-778).
    ///
    /// A well-framed reply with `rc != 0` is `Ok(Reply)` — a device error is DATA.
    pub fn raw_cmd(&mut self, name: &str, args: &[&str]) -> Result<Reply, Error> {
        self.next_id = self.next_id.wrapping_add(1);
        let id = format!("{:x}", self.next_id);
        if !safe_token(name) || !args.iter().all(|a| safe_token(a)) {
            return Err(Error::Host(HostErr::HostUnsafeArg));
        }
        debug_assert!(safe_token(&id));
        // The lease token rides on EVERY request, claim included, exactly as the reference does
        // (`kdi/client.py:169`). It is an envelope key and never an argument, so it is not subject
        // to the charset gate above — but `mint_token` emits `[A-Za-z0-9-]` anyway.
        self.link.message(&id, name, args, &self.token)
    }

    /// Release the lease and drop the link. Takes `self`, so a closed device cannot be used again.
    ///
    /// Always `Ok` today: the release is BEST EFFORT and its failure is deliberately not reported.
    /// A device that stopped answering is already gone, and holding a lease open on it is not
    /// something this host can fix — reporting it would turn every crashed link into two errors.
    /// Dropping a `Device` without calling this closes the transport just the same; what it skips
    /// is the release, so the board stays claimed until its lease expires.
    pub fn close(mut self) -> Result<(), Error> {
        // QUIESCE FIRST. The instrument is single-owner and its acquisition state outlives the
        // process that set it, so a host that just exits hands the next one a running engine and
        // full pipes. Measured: a host opening after that received one record per twenty sample
        // periods with the sticky overrun CLEAR -- valid frames, monotonic stamps, 95 % of the data
        // gone, and nothing in the published surface able to see it. Leaving it running is not a
        // neutral act, so closing cleans up whether or not the caller remembered to.
        //
        // Best effort, like the release below: a device that has stopped answering is already gone.
        let _ = self.quiesce();
        // BEST EFFORT, and its failure may not mask a close error (`kdi/client.py:335-340` puts
        // the release in the `try` and the transport teardown in the `finally`). A device that
        // stopped answering is already gone; holding the lease open on it is not a thing this
        // host can fix, and reporting it would turn every crashed link into two errors.
        if self.lease == Lease::Held {
            let _ = self.raw_cmd("sys.release", &[]);
        }
        // Drop is the rest of the close: the UDP socket and the driver handle both release in
        // their own Drop, so there is nothing more this can report that dropping does not do.
        drop(self);
        Ok(())
    }

    /// Return acquisition to a defined state: flush both streams, disarm any other consumer, stop
    /// the engine.
    ///
    /// Call it on the way IN if you may have inherited a dirty instrument, and know that
    /// [`Device::close`] already calls it on the way out. Without it a host has no way to escape
    /// what a previous application left behind, and no way to detect that it has not: the symptom
    /// is well-formed frames arriving far too slowly, with the sticky overrun clear.
    ///
    /// **It returns the device to UNCONFIGURED** (contract 0.5): the rate register reverts to the
    /// default and `rate_ready` clears with it, so `rhd_matrix` emits nothing until
    /// [`Device::set_rate`] is called again. The pulse is masked, so it does not disturb the
    /// register's other fields.
    pub fn quiesce(&mut self) -> Result<(), Error> {
        self.write_field("quiesce", 1)?;
        self.write_field("quiesce", 0)
    }

    /// Is the acquisition rate configured, so `rhd_matrix` may emit?
    ///
    /// False from power-up on a device that advertises [`Cap::RateControl`]. See [`set_rate`].
    ///
    /// [`set_rate`]: Device::set_rate
    pub fn rate_ready(&mut self) -> Result<bool, Error> {
        Ok(self.read_reg("rate_ready")? & 1 != 0)
    }

    /// Configure the acquisition rate, and VERIFY it took.
    ///
    /// `m`/`d` are the MMCM feedback pair the device's rate table is keyed on. Passing `0, 0`
    /// selects the device default, which is a legitimate choice: what matters is not WHICH rate is
    /// chosen but that one was applied. Until it is, the device emits one frame per twenty sample
    /// periods while its header declares the full cadence (#131) — measured, and unrelated to
    /// transport, which loses nothing.
    ///
    /// This POLLS `rate_ready` and re-applies rather than assuming the write took.
    ///
    /// Returns `Io(TimedOut)` if the device never reports ready.
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

    /// EVERY WireIn field write is masked into this host's shadow of the WHOLE WORD, then the word
    /// is written (`masked_field_write`, kdi/contract.yaml:407-408). Registers share words: both
    /// run bits are on WireIn 0x11 and both burst bounds on 0x13, because KDI owns only three
    /// WireIns. An unmasked write to one field clears its neighbour, which SILENTLY DISARMS THE
    /// OTHER STREAM.
    ///
    /// The shadow starts at zero, which is the post-configure state of every WireIn. WireIns are
    /// write-only, so a host attaching to an already-running board cannot read one back and must
    /// re-establish every field it intends to own.
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
            // must not shift what it gets back (`bindings.udp.note`, kdi/contract.yaml:663).
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

/// Every capability bit, in bit order.
///
/// HAND-WRITTEN, because the generated `spec.rs` publishes no `ALL` array and no `Cap::from_bit`
/// (reported as a generator gap). `caps_list_is_complete` below is an exhaustive match, so adding
/// a capability to the contract stops this crate compiling until a human extends the list.
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
    /// this target, which is exactly why `err` is the contract (kdi/contract.yaml:59-62).
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
    /// Is retrying this command capable of a different answer?
    ///
    /// HAND-WRITTEN and exhaustive, mirroring `errors.*.retryable` in the contract. No wildcard
    /// arm, ever: a token added to `contract.yaml` must stop this compiling until a human decides,
    /// because the wrong default is the dangerous one — a retry loop on `not_present` drives pins
    /// on a slot that is not there.
    pub fn retryable(self) -> bool {
        match self {
            DeviceErr::NoDevice => true, // a Zephyr device is not ready yet
            DeviceErr::Busy => true,     // another host's lease may expire
            DeviceErr::NotReady => true, // boot/calibration still running
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
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────── errors

/// THERE IS NO DECODE VARIANT, deliberately. A rejected frame is not an error a caller can act on
/// in the middle of a recording — it is a fact about the link's quality, and killing hours of
/// acquisition over one flipped bit is the behaviour the Python reference's `walk()` had
/// (the Python reference host). [`StreamReader`] resyncs past it and counts it in [`Stats::bad_frames`];
/// every `Error` below means the transport, the host or the device failed, never the bytes.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The device answered and REFUSED. Only the generated [`Commands`] methods mint this:
    /// [`Device::raw_cmd`] still returns `rc != 0` as data, because its caller holds the `Reply` and
    /// is obliged to read it — but a typed method asked for a value, and a refusal contains none,
    /// so the alternative is a struct of zeros parsed out of absent keys.
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
    /// `sys.claim` was refused — another host holds the board. A SKEW, not a device error,
    /// because it is a reason the traffic that would follow cannot be trusted: the second host's
    /// WireIn shadow starts at zero and knows nothing of the fields the holder owns. Carries the
    /// reply so a log keeps the token the device actually sent.
    Busy(Reply),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // `err` before `rc`: the token IS the contract, the errno is a platform accident
            // (kdi/contract.yaml:59-62). An unknown token prints as `?` rather than being dropped
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
/// peer stopped answering, and the closed host set has ONE token for that (`host_timeout`,
/// `contract.yaml:90-94`), never a minted one.
///
/// One definition, because it was two: `udp.rs` and `simlink.rs` each owned the rule and each
/// carried its own copy of the platform note. Two transports agreeing by coincidence is how they
/// stop agreeing — a third would have made it three.
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

// ─────────────────────────────────────────────────────────────────────────── checks
//
// The three things in this crate that are logic rather than plumbing, and none of them needs a
// device: the field masking (whose failure is silent), the charset (whose failure is command
// injection), and the completeness of the capability list.

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
            // ABSENCE matters as much as the presence: `Acquisition::lanes` is silently ignored for
            // a stream with no mask, and a table that grew a bogus `lanes_digital` would turn that
            // into a write to a register the device does not decode.
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
