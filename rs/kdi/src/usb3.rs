//! The USB3 binding (`bindings.usb3`, kdi/contract.yaml:153-218): an optional C driver loaded at
//! run time. **THE ONLY PLACE IN THE CRATE THAT MAY NAME THE VENDOR**, and only in the two
//! `VENDOR-NAMES` carve-outs; `tests/vendor_neutral.rs` enforces that, and symbols resolve by name.

use std::ffi::{c_char, c_int, c_long, c_uchar, c_ulong, c_void, CStr, CString};
use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use libloading::Library;
use serde_json::{json, Value};

use crate::{
    io_err, reg, reply_from, stream_regs, Addr, DeviceInfo, Error, HostErr, RegBind, Reply, Stream,
    TransportKind, RESP_LEN_DIGITS, RESP_SENTINEL, USB3_MSG, USB3_READ_ALIGNMENT, USB3_STREAM,
};

/// Which driver call failed, or which symbol was not there. A symbol miss carries the C name it
/// looked for, because that is the only string that identifies it; every other message here names
/// the operation, not the vendor's entry point.
#[derive(Debug)]
pub struct SdkErr(pub String);

impl fmt::Display for SdkErr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "device driver: {}", self.0)
    }
}

fn sdk(msg: impl Into<String>) -> Error {
    Error::Sdk(SdkErr(msg.into()))
}

/// The environment override for the driver's location — a path list in the platform's `PATH`
/// syntax, searched before the embedded copy and named in the load failure. It replaced
/// `KDI_FP_DIR`, which is NOT read any more; the Python reference still uses that name.
const DRIVER_DIR_ENV: &str = "KDI_DRIVER_DIR";

type Handle = *mut c_void;

/// The driver's "no error" code. Every negative status is a failure; a positive one has never been
/// observed.
const OK_NO_ERROR: c_int = 0;

/// The entry points, **measured from the shipped driver**: names from its export table, arities
/// from disassembly, call order from Python — never tidy one toward the published C API.
/// `c_long` for pipe lengths is the Windows ABI's 32 bits; 64 misreads a short read.
struct Fp {
    /// `(realm, out) -> handle`. The realm is `strlen`'d with NO null check, so `""` is passed —
    /// the C++ default. The second argument IS null-checked, so `null_mut()` is safe there.
    devices_construct: unsafe extern "C" fn(*const c_char, *mut c_void) -> Handle,
    devices_destruct: unsafe extern "C" fn(Handle),
    devices_get_count: unsafe extern "C" fn(Handle) -> c_int,
    /// Copies at most 10 characters and NUL-terminates at offset 10, so the buffer needs 11 bytes.
    devices_get_serial: unsafe extern "C" fn(Handle, c_int, *mut c_char),
    /// Returns an OWNED front-panel handle, released out of a `shared_ptr` by the wrapper —
    /// the `destruct` entry point below is its deallocator. NULL is returned when nothing opened.
    devices_open: unsafe extern "C" fn(Handle, *const c_char) -> Handle,
    destruct: unsafe extern "C" fn(Handle),
    close: unsafe extern "C" fn(Handle),
    /// `(handle, image, length) -> status`. Takes the FRONT-PANEL handle and is called BEFORE
    /// `get_data_port` — the port is a view of a configured device (`tools/rhd_term.py:168-171`).
    /// OPTIONAL and resolved lazily: only configuring needs it, so a driver lacking it still opens.
    configure_from_memory: Option<unsafe extern "C" fn(Handle, *const c_uchar, c_ulong) -> c_int>,
    /// **The port comes back through an OUT-PARAMETER, not in the return register** — reading it
    /// as a return value yields whatever the inner call happened to leave behind. The port is
    /// borrowed from the front panel and must not be freed: the DLL exports no destructor for it.
    get_data_port: unsafe extern "C" fn(Handle, *mut Handle) -> c_int,
    // Everything below takes the DATA PORT handle, never the front-panel one.
    update_wire_outs: unsafe extern "C" fn(Handle) -> c_int,
    get_wire_out_value: unsafe extern "C" fn(Handle, c_int) -> u32,
    set_wire_in_value: unsafe extern "C" fn(Handle, c_int, u32, u32) -> c_int,
    update_wire_ins: unsafe extern "C" fn(Handle) -> c_int,
    activate_trigger_in: unsafe extern "C" fn(Handle, c_int, c_int) -> c_int,
    read_from_pipe_out: unsafe extern "C" fn(Handle, c_int, c_long, *mut u8) -> c_long,
    read_from_block_pipe_out: unsafe extern "C" fn(Handle, c_int, c_int, c_long, *mut u8) -> c_long,
    // LAST FIELD ON PURPOSE: fields drop in declaration order, so the library outlives every
    // pointer taken out of it.
    _lib: Library,
}

/// Copy a symbol's address out of the library. Sound only because `Fp` keeps the `Library` alive
/// for exactly as long as the pointers, and because a C function pointer is `Copy`.
unsafe fn sym<T: Copy>(lib: &Library, name: &str) -> Result<T, Error> {
    let s: libloading::Symbol<T> = lib
        .get(CString::new(name).unwrap().as_bytes_with_nul())
        .map_err(|_| sdk(name))?;
    Ok(*s)
}

impl Fp {
    /// Load the driver, in this order — the order is the API, not an implementation detail:
    /// `driver_dir`, `$KDI_DRIVER_DIR` (a `PATH`-style list), the compiled-in copy (`bundled`,
    /// staged under temp), the OS path. Embedded before OS: a wrong second driver hard-aborts.
    fn load(driver_dir: Option<&Path>) -> Result<Fp, Error> {
        // ── VENDOR-NAMES-BEGIN ── the driver's file name on each platform. It is the vendor's
        // file, so this is the vendor's spelling; nothing else in the crate says it. See the
        // module docs — this is a name a loader must match, not something to be hidden.
        let names: &[&str] = if cfg!(target_os = "windows") {
            &["okFrontPanel.dll"]
        } else if cfg!(target_os = "macos") {
            &["libokFrontPanel.dylib"]
        } else {
            &["libokFrontPanel.so"]
        };
        // ── VENDOR-NAMES-END ──
        // The name we stage under, so copying that file into `$KDI_DRIVER_DIR` works. Outside the
        // carve-out: these are ours.
        let staged: &[&str] = if cfg!(target_os = "windows") {
            &["kdi_driver.dll"]
        } else if cfg!(target_os = "macos") {
            &["libkdi_driver.dylib"]
        } else {
            &["libkdi_driver.so"]
        };
        let mut dirs: Vec<PathBuf> = driver_dir.into_iter().map(PathBuf::from).collect();
        if let Some(p) = std::env::var_os(DRIVER_DIR_ENV) {
            dirs.extend(std::env::split_paths(&p));
        }
        let mut tried = Vec::new();
        let lib = 'found: {
            for d in &dirs {
                for n in names.iter().chain(staged.iter()) {
                    let path = d.join(n);
                    match unsafe { Library::new(&path) } {
                        Ok(l) => break 'found l,
                        // THE DIRECTORY, NOT THE FILE — nor the loader's error. This string
                        // reaches a customer; the file name is the vendor's, and so is
                        // `libloading`'s `Display`. Where we looked is the useful half.
                        Err(_) => tried.push(d.display().to_string()),
                    }
                }
            }
            // The staging failure is RETURNED, not pushed onto `tried`: a temp dir that cannot be
            // written is a permission fault with a fix, and letting it fall through would report it
            // as NotFound — which `enumerate` reads as "no board on this machine" and swallows.
            #[cfg(feature = "bundled")]
            {
                let path = crate::bundled::staged_path().map_err(|e| {
                    io_err(
                        e.kind(),
                        format!(
                            "the bundled device driver could not be unpacked: {e}. Set \
                             ${DRIVER_DIR_ENV} to a directory holding the driver to bypass it."
                        ),
                    )
                })?;
                match unsafe { Library::new(&path) } {
                    Ok(l) => break 'found l,
                    // Reached when the temp dir is mounted `noexec`: the bytes are there and the
                    // mapping is refused. `$KDI_DRIVER_DIR` is the way out. The directory, not
                    // the staged file name and not the loader's error (it names the file).
                    Err(_) => tried.push(format!(
                        "{} (embedded copy)",
                        path.parent().unwrap_or(path.as_path()).display()
                    )),
                }
            }
            for n in names {
                match unsafe { Library::new(n) } {
                    Ok(l) => break 'found l,
                    Err(_) => tried.push("the OS search path".into()),
                }
            }
            // NotFound, not `Sdk`: "this machine has no driver installed" is a fact about the
            // machine, and `enumerate` must tell it apart from "the driver is here and broken"
            // WITHOUT matching on message text. That is why `find` returns its errors.
            return Err(io_err(
                std::io::ErrorKind::NotFound,
                format!(
                    "no device driver loaded (set ${DRIVER_DIR_ENV} to the directory holding it); \
                     tried {}",
                    tried.join("; ")
                ),
            ));
        };
        // ── VENDOR-NAMES-BEGIN ── the driver's exported symbol names, ALL resolved here (a miss at
        // load names itself; one at first use surfaces mid-recording). `dlsym` needs plain literals
        // that `strings` shows, so do not obfuscate or split them — it would cost only that name.
        unsafe {
            Ok(Fp {
                devices_construct: sym(&lib, "okFrontPanelDevices_Construct")?,
                devices_destruct: sym(&lib, "okFrontPanelDevices_Destruct")?,
                devices_get_count: sym(&lib, "okFrontPanelDevices_GetCount")?,
                devices_get_serial: sym(&lib, "okFrontPanelDevices_GetSerial")?,
                devices_open: sym(&lib, "okFrontPanelDevices_Open")?,
                destruct: sym(&lib, "okFrontPanel_Destruct")?,
                close: sym(&lib, "okFrontPanel_Close")?,
                configure_from_memory: sym(&lib, "okFrontPanel_ConfigureFPGAFromMemory").ok(),
                get_data_port: sym(&lib, "okFrontPanel_GetFPGADataPortClassic")?,
                update_wire_outs: sym(&lib, "okFPGADataPortClassic_UpdateWireOuts")?,
                get_wire_out_value: sym(&lib, "okFPGADataPortClassic_GetWireOutValue")?,
                set_wire_in_value: sym(&lib, "okFPGADataPortClassic_SetWireInValue")?,
                update_wire_ins: sym(&lib, "okFPGADataPortClassic_UpdateWireIns")?,
                activate_trigger_in: sym(&lib, "okFPGADataPortClassic_ActivateTriggerIn")?,
                read_from_pipe_out: sym(&lib, "okFPGADataPortClassic_ReadFromPipeOut")?,
                read_from_block_pipe_out: sym(&lib, "okFPGADataPortClassic_ReadFromBlockPipeOut")?,
                _lib: lib,
            })
        }
        // ── VENDOR-NAMES-END ──
    }
}

impl Fp {
    /// The devices manager. Short-lived ON PURPOSE: the Python reference drops the manager the
    /// moment `Open` returns and the board stays open — holding it for the session would keep
    /// an enumeration object alive for nothing.
    unsafe fn devices(&self) -> Result<Handle, Error> {
        // `c""`, not `b"\0"`: the byte string dated from a false MSRV 1.75 floor, before C-string
        // literals (1.77). Clippy's `manual_c_str_literals` asks for this form.
        let h = unsafe { (self.devices_construct)(c"".as_ptr(), std::ptr::null_mut()) };
        if h.is_null() {
            return Err(sdk("the device manager could not be constructed"));
        }
        Ok(h)
    }

    fn close_device(&self, hnd: Handle) {
        unsafe {
            (self.close)(hnd);
            (self.destruct)(hnd);
        }
    }
}

pub(crate) struct Usb3 {
    fp: Fp,
    hnd: Handle,
    /// The classic-data-port view of `hnd` — every wire, trigger and pipe goes through this,
    /// never through `hnd`. Borrowed from the front panel, so `Drop` does not free it.
    port: Handle,
    serial: String,
    /// Console bytes read but not yet framed. The vUART is shared with the human console, so a
    /// drain routinely returns a banner or the tail of someone's `kv` output.
    console: Vec<u8>,
}

impl Usb3 {
    /// Open a board, optionally CONFIGURING it from `image` first — see
    /// [`crate::Device::open_usb3_configured`] for what that means and does not mean.
    pub(crate) fn open(
        serial: &str,
        driver_dir: Option<&Path>,
        image: Option<&[u8]>,
    ) -> Result<Usb3, Error> {
        // Validate before loading the driver or opening the board. An empty configure could be a
        // no-op that leaves the resident image running, and a too-large image must not leak the
        // handle we would otherwise have opened before discovering it cannot cross the ABI.
        let image = match image {
            Some([]) => {
                return Err(io_err(
                    std::io::ErrorKind::InvalidInput,
                    "empty gateware image",
                ))
            }
            Some(img) => Some((
                img,
                img.len().try_into().map_err(|_| {
                    io_err(
                        std::io::ErrorKind::InvalidInput,
                        "gateware image is too large to send",
                    )
                })?,
            )),
            None => None,
        };
        let c = CString::new(serial)
            .map_err(|_| io_err(std::io::ErrorKind::InvalidInput, "serial contains a NUL"))?;
        let fp = Fp::load(driver_dir)?;
        // An empty serial reaches the driver as an empty string, which it reads as "the first
        // device" — the same thing an empty serial means in the Python reference.
        let devs = unsafe { fp.devices()? };
        let hnd = unsafe { (fp.devices_open)(devs, c.as_ptr()) };
        unsafe { (fp.devices_destruct)(devs) };
        if hnd.is_null() {
            return Err(sdk(format!("opening device {serial:?} returned no handle")));
        }
        // THE RETURN CODE IS CHECKED. The scar: every Python caller guarded on a `NoError`
        // attribute of the DEVICE CLASS, where it does not exist, so the `and` short-circuited
        // and A FAILED FLASH TESTED GREEN (`tools/vuart_term.py:60-68`).
        if let Some((img, len)) = image {
            // Resolved lazily (see the field): a driver without this entry point can still do
            // everything except configure, so the refusal names the missing capability rather than
            // failing at load and taking `open_usb3` down with it.
            let Some(configure) = fp.configure_from_memory else {
                fp.close_device(hnd);
                return Err(sdk(
                    "this device driver cannot configure a device from memory; \
                     bind an already-configured board with open_usb3 instead",
                ));
            };
            let rc = unsafe { configure(hnd, img.as_ptr().cast::<c_uchar>(), len) };
            if rc != OK_NO_ERROR {
                fp.close_device(hnd);
                return Err(sdk(format!(
                    "configuring the device with a {} byte gateware image failed (rc = {rc}); \
                     the board is still running whatever was resident",
                    img.len()
                )));
            }
        }
        // GATED ON THE OUT-PARAMETER, not on `rc`: the out-param is what was measured to carry the
        // port, and a null one would turn every later wire read into a plausible-looking zero.
        // `rc` is reported because it is the only thing that says WHY.
        let mut port: Handle = std::ptr::null_mut();
        let rc = unsafe { (fp.get_data_port)(hnd, &mut port) };
        if port.is_null() {
            fp.close_device(hnd);
            return Err(sdk(format!(
                "the device exposes no classic data port (rc = {rc})"
            )));
        }
        Ok(Usb3 {
            fp,
            hnd,
            port,
            serial: serial.to_string(),
            console: Vec::new(),
        })
    }

    /// Tier-A identity: what is knowable without the gateware answering. Deliberately NOT read
    /// from WireOut 0x3e, which carries the legacy RHX BOARD_ID — a different thing from KDI's
    /// `board_id` (the Python reference host).
    pub(crate) fn identity(&self) -> Value {
        json!({"serial": self.serial, "transport": "usb3"})
    }

    /// The update is CHECKED. The wire-out read reports no error of its own — it returns 0 for an
    /// address it cannot serve — so an unchecked failed transfer here would surface as a perfectly
    /// plausible zero register rather than as an error.
    pub(crate) fn reg_read(&mut self, r: RegBind) -> Result<u32, Error> {
        let rc = unsafe { (self.fp.update_wire_outs)(self.port) };
        if rc != OK_NO_ERROR {
            return Err(sdk(format!("register-block read failed (rc = {rc})")));
        }
        let v = unsafe { (self.fp.get_wire_out_value)(self.port, c_int::from(r.addr)) };
        Ok((v & r.mask()) >> r.lo)
    }

    /// `word` is the CALLER's shadow of the whole WireIn, already masked
    /// (`Device::write_field`). The driver's mask is wide because the HOST owns the
    /// read-modify-write: a second process with a different idea of it is what it exposes.
    pub(crate) fn reg_write(&mut self, r: RegBind, word: u32) -> Result<(), Error> {
        match r.kind {
            "wirein" => {
                let rc = unsafe {
                    (self.fp.set_wire_in_value)(self.port, c_int::from(r.addr), word, u32::MAX)
                };
                if rc != OK_NO_ERROR {
                    return Err(sdk(format!(
                        "register 0x{:02x} write failed (rc = {rc})",
                        r.addr
                    )));
                }
                // Checked: the wire-in write only stages a shadow word, so an unchecked failure
                // here is a write that silently never reached the board.
                let rc = unsafe { (self.fp.update_wire_ins)(self.port) };
                if rc != OK_NO_ERROR {
                    return Err(sdk(format!("register-block write failed (rc = {rc})")));
                }
                Ok(())
            }
            // A triggerin is a one-cycle pulse on `.b`; there is no value to write
            // (`endpoint_grammar`, kdi/contract.yaml:153-153).
            "triggerin" => self.trigger(r.addr, r.lo),
            k => Err(io_err(
                std::io::ErrorKind::InvalidInput,
                format!("{k} endpoints are not writable"),
            )),
        }
    }

    fn trigger(&mut self, addr: u8, bit: u8) -> Result<(), Error> {
        let rc = unsafe {
            (self.fp.activate_trigger_in)(self.port, c_int::from(addr), c_int::from(bit))
        };
        if rc != OK_NO_ERROR {
            return Err(sdk(format!(
                "trigger 0x{addr:02x}.{bit} failed (rc = {rc})"
            )));
        }
        Ok(())
    }

    /// `bindings.usb3.stream_read_rule` (kdi/contract.yaml:170-179). A plain `okPipeOut` is NOT
    /// FPGA-paced — past resident data it returns zero fill, which the decoder walks as a bogus
    /// header — so size it from the occupancy word: `words32 * 4`, clamped, rounded DOWN.
    pub(crate) fn stream_read(&mut self, s: Stream, buf: &mut [u8]) -> Result<usize, Error> {
        let (kind, addr) = stream_ep(s)?;
        let avail = self.reg_read(reg(stream_regs(s).2)?)? & 0xFFFF;
        let mut n = (avail as usize * 4).min(buf.len());
        n -= n % USB3_READ_ALIGNMENT;
        if n == 0 {
            return Ok(0);
        }
        // HONOUR THE PIPE KIND THE BINDING DECLARES. Only an okBTPipeOut has
        // ep_ready/ep_blockstrobe; block-reading a plain pipe cannot work against this gateware
        // (the reference host does the same).
        let rc = if kind.eq_ignore_ascii_case("okbtpipeout") {
            unsafe {
                (self.fp.read_from_block_pipe_out)(
                    self.port,
                    c_int::from(addr),
                    USB3_READ_ALIGNMENT as c_int,
                    n as c_long,
                    buf.as_mut_ptr(),
                )
            }
        } else {
            unsafe {
                (self.fp.read_from_pipe_out)(
                    self.port,
                    c_int::from(addr),
                    n as c_long,
                    buf.as_mut_ptr(),
                )
            }
        };
        if rc < 0 {
            return Err(sdk(format!("stream pipe 0x{addr:02x} read {n} = {rc}")));
        }
        let got = rc as usize;
        if got < n {
            // The framing said `n` and the transport gave less: the closed host set has a token
            // for exactly this, and it must not be papered over as a short but valid read.
            return Err(Error::Host(HostErr::HostShortRead));
        }
        Ok(got.min(buf.len()))
    }

    /// The message channel (`bindings.usb3.message`): a `kdi ` line in, a sentinel-framed JSON
    /// reply out. THE LEASE TOKEN IS DELIBERATELY NOT SENT — this line form is positional-only
    /// (`envelope_on_the_wire`, kdi/contract.yaml:202-202), so `sys.claim` answers `unknown_cmd`.
    pub(crate) fn message(
        &mut self,
        id: &str,
        name: &str,
        args: &[(&str, crate::Arg<'_>)],
        _token: &str,
    ) -> Result<Reply, Error> {
        let line = request_line(id, name, args);

        // Drain first: the console emits banners and prior output on this same wire, and a stale
        // frame left in the buffer would be scanned as this command's answer (`response.rules`).
        self.drain()?;
        self.console.clear();
        self.send(&line, Duration::from_secs(2))?;

        // The command's own `worst_ms`, doubled for margin, never below the 4 s floor. A flat wait
        // made every slower command unreachable and the contract's published field decorative.
        let worst = crate::spec::WORST_MS
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(0, |(_, ms)| *ms);
        let deadline = Instant::now() + Duration::from_millis(4_000.max(2 * worst));
        let mut scanned = 0usize;
        loop {
            let got = self.drain()?;
            if got == 0 {
                if Instant::now() >= deadline {
                    return Err(Error::Host(HostErr::HostTimeout));
                }
                std::thread::sleep(Duration::from_millis(2));
                continue;
            }
            // Scan PAST non-matching ids rather than returning the first frame: a late reply from
            // a previously timed-out command must never be returned as this command's answer.
            while let Some((v, end)) = next_frame(&self.console, scanned) {
                scanned = end;
                if let Ok(r) = reply_from(v) {
                    if r.id == id {
                        return Ok(r);
                    }
                }
            }
        }
    }

    /// Push ASCII into the console RX FIFO, 4 bytes per word, gated on `status[31:16]`
    /// (`rx_framing`). The FIFO has no back-pressure of its own.
    fn send(&mut self, s: &str, timeout: Duration) -> Result<(), Error> {
        let rx_data = msg_ep("rx_data")?;
        let rx_count = msg_ep("rx_count")?;
        let rx_push = msg_ep("rx_push")?;
        let deadline = Instant::now() + timeout;
        for chunk in s.as_bytes().chunks(4) {
            loop {
                if self.status()? >> 16 >= chunk.len() as u32 {
                    break;
                }
                if Instant::now() >= deadline {
                    return Err(Error::Host(HostErr::HostTimeout));
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            let word = chunk
                .iter()
                .enumerate()
                .fold(0u32, |w, (i, b)| w | u32::from(*b) << (8 * i));
            self.reg_write(rx_data, word)?;
            self.reg_write(rx_count, chunk.len() as u32)?;
            self.trigger(rx_push.addr, rx_push.lo)?;
        }
        Ok(())
    }

    fn status(&mut self) -> Result<u32, Error> {
        self.reg_read(msg_ep("status")?)
    }

    /// One batch of console TX, from self-framing 32-bit words: byte 0 counts 0..3, bytes 1..3
    /// are that many payload bytes (`tx_framing`); returns how many were appended. Rounds the
    /// request UP to 16 B, unlike `stream_read`, so `got < want` is legal: no `HostShortRead`.
    fn drain(&mut self) -> Result<usize, Error> {
        let words = self.status()? & 0xFFFF;
        if words == 0 {
            return Ok(0);
        }
        let tx = msg_ep("tx_pipe")?;
        let want =
            (words.min(256) as usize * 4).div_ceil(USB3_READ_ALIGNMENT) * USB3_READ_ALIGNMENT;
        let mut raw = vec![0u8; want];
        let rc = unsafe {
            (self.fp.read_from_pipe_out)(
                self.port,
                c_int::from(tx.addr),
                want as c_long,
                raw.as_mut_ptr(),
            )
        };
        if rc < 0 {
            return Err(sdk(format!("console pipe 0x{:02x} read = {rc}", tx.addr)));
        }
        if rc % 4 != 0 {
            return Err(sdk(format!(
                "console pipe 0x{:02x} read = {rc} (not a whole number of words)",
                tx.addr
            )));
        }
        let mut n = 0;
        for w in raw[..(rc as usize).min(raw.len())].chunks_exact(4) {
            let cnt = (w[0] as usize).min(3);
            self.console.extend_from_slice(&w[1..1 + cnt]);
            n += cnt;
        }
        Ok(n)
    }
}

impl Drop for Usb3 {
    fn drop(&mut self) {
        self.fp.close_device(self.hnd);
    }
}

/// `0x1e`, then EXACTLY 3 lowercase ASCII hex digits of body length, then that many bytes of UTF-8
/// JSON (`response.framing`). Returns the parsed body and the offset just past it. An unparseable
/// body is ABSENT and the scan continues: a truncated frame is ordinary on this shared wire.
fn next_frame(cap: &[u8], from: usize) -> Option<(Value, usize)> {
    let mut i = from.min(cap.len());
    while let Some(rel) = cap[i..].iter().position(|b| *b == RESP_SENTINEL) {
        let at = i + rel;
        let body_at = at + 1 + RESP_LEN_DIGITS;
        if body_at > cap.len() {
            return None; // the length digits have not arrived yet
        }
        let len = std::str::from_utf8(&cap[at + 1..body_at])
            .ok()
            .and_then(|s| usize::from_str_radix(s, 16).ok());
        if let Some(len) = len {
            if body_at + len > cap.len() {
                return None; // the body has not arrived yet; keep the sentinel for the next pass
            }
            if let Ok(v) = serde_json::from_slice::<Value>(&cap[body_at..body_at + len]) {
                return Some((v, body_at + len));
            }
        }
        // Not a frame after all (a 0x1e in ordinary console output, or an unparseable body):
        // step past this sentinel and keep scanning.
        i = at + 1;
    }
    None
}

fn stream_ep(s: Stream) -> Result<(&'static str, u8), Error> {
    USB3_STREAM
        .iter()
        .find(|(n, ..)| *n == s.token())
        .map(|&(_, kind, addr)| (kind, addr))
        .ok_or_else(|| {
            io_err(
                std::io::ErrorKind::InvalidInput,
                format!("the usb3 binding maps no stream {}", s.token()),
            )
        })
}

/// A message-channel endpoint, resolved by ROLE. They were host-side constants for three
/// revisions, the one part a host could not resolve by name, and this project has moved the
/// vUART status word once, 0x26 -> 0x30 (kdi/contract.yaml:180-196).
fn msg_ep(role: &str) -> Result<RegBind, Error> {
    USB3_MSG
        .iter()
        .find(|(r, ..)| *r == role)
        .map(|&(_, kind, addr, bit)| RegBind {
            kind,
            addr,
            lo: bit.unwrap_or(0),
            width: if bit.is_some() { 1 } else { 32 },
        })
        .ok_or_else(|| {
            io_err(
                std::io::ErrorKind::InvalidInput,
                format!("the usb3 message channel declares no {role} endpoint"),
            )
        })
}

/// Attached boards, by serial. Transport-native and cheap: no gateware, no `ConfigureFPGA` — the
/// analog of reading a USB descriptor. A driver that is present but broken must be DISTINGUISHABLE
/// from an empty bench, so every failure here is returned to `find` rather than swallowed.
pub(crate) fn enumerate() -> Result<Vec<DeviceInfo>, Error> {
    let fp = match Fp::load(None) {
        Ok(fp) => fp,
        // No driver installed is not a fault: it is a machine with no USB3 binding, which is
        // the ordinary case everywhere except the bench. A driver that IS there and fails to give
        // up a symbol is `Error::Sdk` and travels back to the caller.
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let hnd = unsafe { fp.devices()? };
    let n = unsafe { (fp.devices_get_count)(hnd) };
    let mut out = Vec::new();
    for i in 0..n.max(0) {
        // The driver copies at most 10 characters and NUL-terminates at offset 10, so 11 bytes
        // is the real floor; 32 is slack against a future model with a longer serial.
        let mut raw = [0 as c_char; 32];
        unsafe { (fp.devices_get_serial)(hnd, i, raw.as_mut_ptr()) };
        let serial = unsafe { CStr::from_ptr(raw.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        out.push(DeviceInfo {
            serial: serial.clone(),
            // Empty, not guessed: `device.vendor`/`device.compatible` are in contract.yaml but not
            // in the generated spec, and a literal here would be a second source of truth. See
            // `Device::open_usb3`.
            vendor: String::new(),
            compatible: String::new(),
            board_id: None,
            kdi: None,
            transport: TransportKind::Usb3,
            addr: Addr::Serial(serial),
        });
    }
    unsafe { (fp.devices_destruct)(hnd) };
    Ok(out)
}

/// `request.line`: `"kdi <id> <name> <arg>*\r"`, values positional IN THE ORDER GIVEN. The names
/// the udp envelope keys by are dropped — this form has nowhere to put them, which is why a
/// positional-only host never noticed the udp side could not name them at all (#179).
fn request_line(id: &str, name: &str, args: &[(&str, crate::Arg<'_>)]) -> String {
    let mut line = String::from(crate::REQ_TAG);
    line.push_str(id);
    line.push(' ');
    line.push_str(name);
    for (_, a) in args {
        line.push(' ');
        let _ = write!(line, "{a}");
    }
    // `terminator: "\r"`. The charset check that makes this safe already ran in `Device::cmd`.
    line.push('\r');
    line
}

#[cfg(test)]
mod tests {
    use crate::Arg::{Int, Text};

    /// The vUART half of #179: `raw_cmd_named`'s pairs must reach the line as BARE TOKENS in the
    /// order given, with the names gone and `Int`/`Text` indistinguishable — the same call the
    /// udp binding keys an object from. Nothing else can see this without a board.
    #[test]
    fn the_line_carries_the_values_positionally_and_drops_the_names() {
        let args = [
            ("bus", Text("module")),
            ("ch", Int(0)),
            ("addr", Int(0x52)),
            ("data", Text("02")),
        ];
        assert_eq!(
            super::request_line("7", "x.y", &args),
            "kdi 7 x.y module 0 82 02\r"
        );
        // A transposition is a DIFFERENT line, which is the whole reason the order is the caller's
        // to get right: the device sees tokens, and nothing on this wire can name them back.
        let swapped = [args[1], args[0], args[2], args[3]];
        assert_ne!(
            super::request_line("7", "x.y", &swapped),
            super::request_line("7", "x.y", &args)
        );
        assert_eq!(
            super::request_line("1", "sys.hello", &[]),
            "kdi 1 sys.hello\r"
        );
    }

    #[test]
    fn an_empty_image_is_rejected_before_the_driver_is_loaded() {
        let Err(crate::Error::Io(e)) = super::Usb3::open("", None, Some(&[])) else {
            panic!("an empty image reached the device driver");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("empty"));
    }
}
