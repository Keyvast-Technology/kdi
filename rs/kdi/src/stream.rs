//! Acquisition: a stream as a sequence of RECORDS, not of bytes. Frames, CRCs, magic resync,
//! reject tokens, descriptor strides and partial-frame carry are plumbing a consumer of an
//! instrument library should never hold, so `kdi` holds them here — one buffer per stream, once.

use std::io;
use std::time::{Duration, Instant};

use crate::codec::{self, Walk};
use crate::{io_err, stream_regs, Device, Error, Kind, Stream, USB3_READ_ALIGNMENT};

/// One transport read. The floor is `Stream::max_frame_bytes` — a binding hands back whole frames
/// (udp) or a block rounded down to the pipe alignment (usb3), so a buffer smaller than one frame
/// can return 0 forever, which is a poll loop that never advances rather than an error.
const READ_BYTES: usize = 64 * 1024;

/// The longest partial frame worth carrying. Past this the carry is wreckage from a corrupted
/// length, not a frame straddling a read boundary — the largest frame this contract declares is
/// 2408 B, so nothing legal comes close. See `Decoder::advance`.
const MAX_CARRY: usize = READ_BYTES;

/// How long to wait before asking the transport again when it had nothing. Short enough that a
/// bounded capture is not paced by it, long enough that an idle stream does not spin a core.
const POLL: Duration = Duration::from_millis(2);

/// Per-read budget inside [`Device::live_lanes`]. Long enough that a configured device answers,
/// short enough that an empty one ends the probe rather than stalling a caller's bring-up.
const PROBE_WAIT: Duration = Duration::from_millis(100);

/// How to acquire. `lanes` is the rhd_matrix acquisition-lane mask (ignored where there is no lane
/// register). `burst` bounds the capture — the only mode where loss cannot happen INSIDE the data:
/// free-running, a host that cannot drain gets an unlocatable hole (contract.yaml:152-157).
#[derive(Clone, Debug)]
pub struct Acquisition {
    /// Bit i selects acquisition lane i. IGNORED by a stream that declares no lane register
    /// (`digital` is one), because the contract has no register there to write it to.
    pub lanes: u32,
    /// `Some(n)` is the requested bound; [`Device::start`] raises it to the smallest USB3-aligned
    /// burst covering `n`, and `None` free-runs. An unaligned bound strands a partial group nothing
    /// can retrieve: a bounded capture has no next read to carry it into ([`StreamReader`]).
    pub burst: Option<u16>,
}

impl Default for Acquisition {
    fn default() -> Self {
        // Every lane, free-running: a mask of 0 emits nothing (contract.yaml:148-151), so all is
        // the safe default — and also the most bandwidth. Measured, 32 lanes at 30 kS/s (≈68 MB/s):
        // 22 768 delivered, 125 364 lost in 5 s. Narrow `lanes`, or bound the capture with `burst`.
        Self {
            lanes: !0,
            burst: None,
        }
    }
}

impl Device {
    /// Start a stream and return its reader. The contract's `run_restart` (yaml:399-404): stop, arm
    /// the burst WHILE STOPPED, then start. The FALLING edge flushes the pipes, resets the packer
    /// phase and clears the sticky overrun; re-asserting inherits the previous run (rc=-75).
    pub fn start(&mut self, s: Stream, a: &Acquisition) -> Result<StreamReader<'_>, Error> {
        // #131: an unconfigured device emits NOTHING on rhd_matrix, by design — before this
        // contract it emitted one frame per twenty periods declaring full cadence, undetectably.
        // Without this check a host that forgot `set_rate` gets silence and no reason.
        if s == Stream::Samples && self.caps().has(crate::Cap::RateControl) && !self.rate_ready()? {
            return Err(io_err(
                io::ErrorKind::InvalidInput,
                "the acquisition rate is not configured, so rhd_matrix emits nothing:                  call Device::set_rate first (contract 0.5, rate_control)",
            ));
        }
        let (run, burst, _, lanes) = stream_regs(s);
        let bound = match a.burst {
            Some(want) => codec::aligned_burst_at_least(
                s.max_frame_bytes() as u32,
                u32::from(want),
                USB3_READ_ALIGNMENT as u32,
            )
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| {
                io_err(
                    io::ErrorKind::InvalidInput,
                    format!("burst {want} cannot be represented after alignment"),
                )
            })?,
            None => 0,
        };
        self.write_field(run, 0)?;
        self.write_field(burst, u32::from(bound))?;
        if let Some(lanes_reg) = lanes {
            self.write_field(lanes_reg, a.lanes)?;
        }
        // The reference host waits here (`kdi/client.py:170`) and it costs nothing: two WireIn
        // updates are microseconds apart on USB3, and the gateware's flush crosses clock domains.
        std::thread::sleep(Duration::from_millis(10));
        self.write_field(run, 1)?;
        Ok(StreamReader {
            dev: self,
            stream: s,
            buf: Vec::new(),
            pos: 0,
            dec: Decoder::default(),
        })
    }

    /// Which lanes carry a chip: nothing published says, so one bounded burst measures it. **A lane
    /// with no chip reads a constant** — `0xffff` idle, `0` unfitted, 100% over 1.47M samples.
    /// Narrowing took a host from 86.5% loss to zero. Quiet ≠ absent, so it returns the mask only.
    pub fn live_lanes(&mut self, s: Stream, records: u16) -> Result<u32, Error> {
        let mut live = 0u32;
        let mut rd = self.start(
            s,
            &Acquisition {
                lanes: !0,
                burst: Some(records.max(1)),
            },
        )?;
        // Bounded above by `records` reads that yield nothing, so a silent device ends the probe
        // instead of hanging it.
        let mut idle = 0;
        while idle < i32::from(records.max(1)) {
            match rd.next(PROBE_WAIT)? {
                None => idle += 1,
                Some(rec) => {
                    for b in rec.blocks() {
                        if b.kind() != Kind::RhdMatrix {
                            continue;
                        }
                        let rows = b.rows();
                        let nlanes = b.lanes().len().min(32) as u16;
                        for l in 0..nlanes {
                            for r in 0..rows {
                                match b.value(r, l) {
                                    Some(v) if v != 0xffff && v != 0 => {
                                        live |= 1u32 << l;
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
            }
        }
        rd.stop()?;
        Ok(live)
    }

    /// Start several streams at once and read them through ONE borrow. They are **not co-sampled**:
    /// each record is tagged with its stream and carries that stream's own timestamp, so join them
    /// by timestamp (one timebase, exact integer arithmetic), never by arrival order.
    pub fn start_multi(
        &mut self,
        want: &[(Stream, Acquisition)],
    ) -> Result<MultiReader<'_>, Error> {
        if want.is_empty() {
            return Err(io_err(
                io::ErrorKind::InvalidInput,
                "start_multi needs at least one stream",
            ));
        }
        // A stream named twice would give two decoders the same bytes, and each would report the
        // other's frames as loss.
        for (i, (s, _)) in want.iter().enumerate() {
            if want[..i].iter().any(|(o, _)| o == s) {
                return Err(io_err(
                    io::ErrorKind::InvalidInput,
                    format!("stream {s:?} requested twice"),
                ));
            }
        }

        for (s, a) in want {
            let (run, burst, _, lanes) = stream_regs(*s);
            let bound = match a.burst {
                Some(w) => codec::aligned_burst_at_least(
                    s.max_frame_bytes() as u32,
                    u32::from(w),
                    USB3_READ_ALIGNMENT as u32,
                )
                .and_then(|n| u16::try_from(n).ok())
                .ok_or_else(|| {
                    io_err(
                        io::ErrorKind::InvalidInput,
                        format!("burst {w} cannot be represented after alignment"),
                    )
                })?,
                None => 0,
            };
            self.write_field(run, 0)?;
            self.write_field(burst, u32::from(bound))?;
            if let Some(lanes_reg) = lanes {
                self.write_field(lanes_reg, a.lanes)?;
            }
        }
        // One wait for all of them, after every falling edge and before any rising one: the flush
        // crosses clock domains, and per-stream waits would only stagger the starts further.
        std::thread::sleep(Duration::from_millis(10));
        for (s, _) in want {
            self.write_field(stream_regs(*s).0, 1)?;
        }

        Ok(MultiReader {
            dev: self,
            lanes: want
                .iter()
                .map(|(s, _)| Lane {
                    stream: *s,
                    buf: Vec::new(),
                    pos: 0,
                    dec: Decoder::default(),
                })
                .collect(),
        })
    }
}

/// The `run` register of a stream, so `Device::quiesce` can clear it without duplicating the
/// register table. Crate-private: which register carries a run bit is not a host's business.
pub(crate) fn run_reg(s: Stream) -> &'static str {
    stream_regs(s).0
}

/// One stream's decode state inside a [`MultiReader`]: the same buffer, carry and decoder a
/// [`StreamReader`] owns, minus the device borrow that made holding two impossible.
struct Lane {
    stream: Stream,
    buf: Vec<u8>,
    pos: usize,
    dec: Decoder,
}

/// Several live streams sharing one device borrow, from [`Device::start_multi`]. Reading is
/// round-robin and fair: each call advances every stream's decoder before refilling any, so a fast
/// stream cannot starve a slow one of attention.
pub struct MultiReader<'d> {
    dev: &'d mut Device,
    lanes: Vec<Lane>,
}

impl MultiReader<'_> {
    /// The next record from any stream, tagged with which, or `None` if none arrived within
    /// `timeout`. As with [`StreamReader::next`] the record borrows the reader: consume it before
    /// the next call — including the OTHER stream's, since one buffer's refill invalidates it.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<(Stream, Record<'_>)>, Error> {
        let deadline = Instant::now() + timeout;
        // Indices, not borrows: the loop both refills buffers and must end holding a borrow of one,
        // which does not typecheck any other way. Same trade StreamReader::next makes.
        let dev = &mut *self.dev;
        let found = pump_multi(&mut self.lanes, deadline, |lane| {
            let stream = lane.stream;
            refill_into(&mut lane.buf, &mut lane.pos, |dst| {
                dev.link.stream_read(stream, dst)
            })
        })?;
        let Some((i, d)) = found else {
            return Ok(None);
        };
        let lane = &self.lanes[i];
        let frame = codec::Frame::parse(&lane.buf[d.start..d.end])
            .expect("advance() validated these exact bytes and nothing has moved them since");
        Ok(Some((
            lane.stream,
            Record {
                frame,
                lost: d.lost,
            },
        )))
    }

    /// One stream's sticky overrun and readable depth. Read AFTER the data, for the reason
    /// [`StreamReader::health`] gives.
    pub fn health(&mut self, s: Stream) -> Result<Health, Error> {
        let v = self.dev.read_reg(stream_regs(s).2)?;
        Ok(Health {
            readable_bytes: (v & 0xFFFF) as usize * 4,
            overrun: v & (1 << 16) != 0,
        })
    }

    /// One stream's recovery counters, or `None` if this reader was not started with it.
    pub fn stats(&self, s: Stream) -> Option<Stats> {
        self.lanes
            .iter()
            .find(|l| l.stream == s)
            .map(|l| l.dec.stats)
    }

    /// Stop every stream this reader started and release the device.
    ///
    /// Dropping instead leaves them ALL acquiring, for the same reason a single reader does.
    pub fn stop(self) -> Result<(), Error> {
        let mut first = Ok(());
        // Every stream gets its stop attempted even if an earlier one failed: leaving a stream
        // running because a different one errored is the worst of both outcomes.
        for lane in &self.lanes {
            let r = self.dev.write_field(stream_regs(lane.stream).0, 0);
            if first.is_ok() {
                first = r;
            }
        }
        first
    }
}

/// A live stream. OWNS the byte buffer, the partial-frame carry and the decode cursor — which is
/// the whole reason the caller no longer can get them wrong.
pub struct StreamReader<'d> {
    dev: &'d mut Device,
    stream: Stream,
    /// Undecoded bytes: the carry from the previous call followed by whatever the last read
    /// appended. Compacted on refill, so a long recording does not retain what it has returned.
    buf: Vec<u8>,
    /// Decode cursor into `buf`. Everything before it has been returned or deliberately discarded.
    pos: usize,
    dec: Decoder,
}

/// The decode half of a reader: a pure function of the bytes handed it, plus the running state the
/// loss oracle needs. Split from the transport half ONLY so a test can drive it — a healthy device
/// produces neither a corrupt frame nor a split one, so a conformance run says nothing about them.
#[derive(Default)]
struct Decoder {
    stats: Stats,
    frame_bytes: Option<u32>,
    /// `(run_id, timestamp)` of the previous record. Not a `codec::Header`: nothing outside this
    /// crate may learn that a header exists.
    prev: Option<(u16, u64)>,
}

impl StreamReader<'_> {
    /// The next record, or `None` if none arrived within `timeout`. Borrows `self`, so a record is
    /// consumed before the buffer refills — the zero-copy guarantee is the compiler's. A record is
    /// never returned twice and a whole frame never lost: the two defects the Python residue had.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Record<'_>>, Error> {
        let deadline = Instant::now() + timeout;
        // Fields borrowed disjointly so the read closure may hold `dev` while `buf` is out, the
        // same arrangement `refill_into` needs and for the same reason.
        let dev = &mut *self.dev;
        let stream = self.stream;
        let found = pump(
            &mut self.dec,
            &mut self.buf,
            &mut self.pos,
            deadline,
            |dst| dev.link.stream_read(stream, dst),
        )?;
        let Some(d) = found else {
            return Ok(None);
        };
        // A second parse of bytes `advance` already validated. It buys the borrow: a `Frame` holds
        // `buf`, and a loop that refills `buf` and returns a borrow of it does not typecheck. Cost:
        // one extra CRC pass, table-driven, ~2 KB at the largest declared geometry.
        let frame = codec::Frame::parse(&self.buf[d.start..d.end])
            .expect("advance() validated these exact bytes and nothing has moved them since");
        Ok(Some(Record {
            frame,
            lost: d.lost,
        }))
    }

    /// Backpressure and the sticky overrun. Read AFTER data, as the contract requires
    /// (`overrun_after_read`, contract.yaml:108-108): it is sticky since the last flush, so a check
    /// after the read means exactly "the records this read returned may not be contiguous".
    pub fn health(&mut self) -> Result<Health, Error> {
        let v = self.dev.read_reg(stream_regs(self.stream).2)?;
        Ok(Health {
            // [15:0] is words32 — the pipe is a 32-bit StreamFifoCC, and `frame_words` is words16,
            // so the two units sit side by side in this protocol (kdi/contract.yaml:176-183).
            readable_bytes: (v & 0xFFFF) as usize * 4,
            overrun: (v >> 16) & 1 != 0,
        })
    }

    /// What this reader had to recover from, cumulative since [`Device::start`]. Where a decode
    /// rejection ends up — nothing about a bad frame reaches the caller as an `Err`.
    pub fn stats(&self) -> Stats {
        self.dec.stats
    }

    /// Bytes in one frame of this stream, once one has been seen. `None` before that, and it cannot
    /// be otherwise: format 2 carries its geometry on the wire, so nothing off the device predicts
    /// this.
    pub fn frame_bytes(&self) -> Option<u32> {
        self.dec.frame_bytes
    }

    /// The largest burst `<= want` a bounded capture may legally use, or the smallest legal one;
    /// `None` until a frame has been seen. A USB3 read is a multiple of 16 B and a frame usually is
    /// not (adio_dig is 88 B), so an unaligned burst strands a group. Every binding (yaml:389-390).
    pub fn aligned_burst(&self, want: u16) -> Option<u16> {
        let n = codec::aligned_burst(
            self.dec.frame_bytes?,
            u32::from(want),
            USB3_READ_ALIGNMENT as u32,
        )?;
        u16::try_from(n).ok()
    }

    /// Clear this stream's run bit and give the [`Device`] back. Takes `self`, so the borrow ends
    /// here. Dropping instead leaves the device ACQUIRING, deliberately: a drop cannot report a
    /// failed write, and a silent half-stop is worse than acquiring ([`Device::start`] stops it).
    pub fn stop(self) -> Result<(), Error> {
        self.dev.write_field(stream_regs(self.stream).0, 0)
    }
}

/// Compact away what has been decoded, then append one transport read. `drain(..pos)`, NEVER
/// `clear()`: from `pos` on is the CARRY, and dropping it charges the DEVICE a `bad_frames` for
/// bytes the HOST threw away. Mutating it to `clear` passed the whole suite — UDP has no partials.
fn refill_into(
    buf: &mut Vec<u8>,
    pos: &mut usize,
    read: impl FnOnce(&mut [u8]) -> Result<usize, Error>,
) -> Result<usize, Error> {
    if *pos > 0 {
        buf.drain(..*pos);
        *pos = 0;
    }
    let at = buf.len();
    buf.resize(at + READ_BYTES, 0);
    let got = match read(&mut buf[at..]) {
        Ok(n) => n,
        // Truncate before propagating: an error must not leave the buffer holding READ_BYTES of
        // zeros that the next `advance` would walk as wreckage.
        Err(e) => {
            buf.truncate(at);
            return Err(e);
        }
    };
    buf.truncate(at + got);
    Ok(got)
}

/// Decode-refill loop, transport as a closure so it can be driven with no device. THE RULE: a
/// refill that produced bytes has earned a decode attempt, whatever the clock says — checking the
/// deadline first made `next(ZERO)` report "nothing" holding 64 KB: 1 record per call, not 256.
fn pump(
    dec: &mut Decoder,
    buf: &mut Vec<u8>,
    pos: &mut usize,
    deadline: Instant,
    mut read: impl FnMut(&mut [u8]) -> Result<usize, Error>,
) -> Result<Option<Decoded>, Error> {
    loop {
        let (next, hit) = dec.advance(buf, *pos);
        *pos = next;
        if hit.is_some() {
            return Ok(hit);
        }
        let got = refill_into(buf, pos, &mut read)?;
        if got > 0 {
            let (next, hit) = dec.advance(buf, *pos);
            *pos = next;
            if hit.is_some() {
                return Ok(hit);
            }
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        if got == 0 {
            std::thread::sleep(POLL);
        }
    }
}

/// [`pump`] for several lanes, round-robin, refill as a closure so it needs no device. This loop
/// CANNOT share `pump` — it advances every lane before refilling any — so the ordering rule lives
/// twice, and once it was fixed only in `StreamReader`: `MultiReader::next(0)` saw a full refill.
fn pump_multi(
    lanes: &mut [Lane],
    deadline: Instant,
    mut refill: impl FnMut(&mut Lane) -> Result<usize, Error>,
) -> Result<Option<(usize, Decoded)>, Error> {
    // Every lane's decoder is advanced before any lane is refilled, so a fast stream cannot starve
    // a slow one of attention.
    fn scan(lanes: &mut [Lane]) -> Option<(usize, Decoded)> {
        for (i, lane) in lanes.iter_mut().enumerate() {
            let (pos, d) = lane.dec.advance(&lane.buf, lane.pos);
            lane.pos = pos;
            if let Some(d) = d {
                return Some((i, d));
            }
        }
        None
    }
    loop {
        if let Some(h) = scan(lanes) {
            return Ok(Some(h));
        }
        let mut got = 0usize;
        for lane in lanes.iter_mut() {
            got += refill(lane)?;
        }
        if got > 0 {
            // A refill that produced bytes has earned a decode attempt, whatever the clock says.
            if let Some(h) = scan(lanes) {
                return Ok(Some(h));
            }
        }
        if Instant::now() >= deadline {
            return Ok(None);
        }
        if got == 0 {
            std::thread::sleep(POLL);
        }
    }
}

/// One decode step's result: where the frame sits in the buffer it was decoded from, and how many
/// records the device dropped before it.
#[derive(Copy, Clone)]
struct Decoded {
    start: usize,
    end: usize,
    lost: u64,
}

impl Decoder {
    /// Decode the next whole frame in `buf` at or after `from`: the new cursor (before it is
    /// returned or deliberately discarded, after it is the carry) and the frame's bounds if any.
    /// A rejected frame is stepped over and counted here, never raised — a caller reads `Stats`.
    fn advance(&mut self, buf: &[u8], from: usize) -> (usize, Option<Decoded>) {
        let mut walk = Walk::new(&buf[from..]);
        let mut hit = None;
        let mut bad = 0u64;
        for f in walk.by_ref() {
            match f {
                Ok(frame) => {
                    let h = frame.header();
                    hit = Some((
                        h.timestamp,
                        h.run_id,
                        h.flags.first_of_run(),
                        h.frame_words,
                        frame.bytes().len(),
                        // The cadence of the FIRST section. Sections of one frame share its single
                        // timestamp, so the oracle needs one; a frame whose sections disagreed
                        // would need a per-section timeline the contract does not define.
                        frame.section_at(0).map(|s| s.cadence()),
                    ));
                    break;
                }
                Err(_) => bad += 1,
            }
        }
        // Bytes the walk consumed, whether as frames or as wreckage it resynced past. Taken from
        // the tail rather than by pointer arithmetic: the walk is the authority on what it ate.
        let pos = buf.len() - walk.tail().len();
        let c = walk.counters();

        self.stats.bad_frames += bad;
        self.stats.resync_bytes += c.resync_bytes as u64;
        self.stats.skipped_unknown_format += c.format_skipped as u64;
        self.stats.skipped_unknown_kind += c.unknown_kind as u64;

        let Some((ts, run_id, first, frame_words, len, cadence)) = hit else {
            // A CARRY THIS LONG IS WRECKAGE, NOT A STRADDLING FRAME. `frame_words` is checked only
            // for `% 4` and a floor, so a corrupt length can declare 8 GB and park this cursor for
            // life, counters at zero: 202 frames in, 1 out. False magic does it 1 resync in 4.
            if buf.len() - pos > MAX_CARRY {
                self.stats.bad_frames += 1;
                self.stats.resync_bytes += 1;
                return (pos + 1, None);
            }
            return (pos, None);
        };
        self.frame_bytes.get_or_insert(frame_words * 2);
        let lost = self.lost_before(ts, run_id, first, cadence);
        self.prev = Some((run_id, ts));
        self.stats.records += 1;
        self.stats.lost_records += lost;
        (
            pos,
            Some(Decoded {
                start: pos - len,
                end: pos,
                lost,
            }),
        )
    }

    /// The published `loss_oracle` (kdi/contract.yaml:185), applied within ONE stream — the only
    /// scope it is defined for. Streams are not co-sampled and share only the timebase, so the same
    /// arithmetic across two of them measures nothing.
    fn lost_before(&self, ts: u64, run_id: u16, first: bool, cadence: Option<(u32, u16)>) -> u64 {
        // 0 for the first record of a run and for the first after a new epoch: no previous stamp in
        // this segment to measure a gap against, and `first_of_run` says exactly that. A gap
        // across a restart would report the whole idle period as lost data.
        if first {
            return 0;
        }
        let (Some((prev_run, prev_ts)), Some((num, den))) = (self.prev, cadence) else {
            return 0;
        };
        if prev_run != run_id {
            return 0;
        }
        codec::lost_frames(ts.saturating_sub(prev_ts), num, den)
            // Err is unreachable — `tick_sane` is enforced at parse — and a negative answer means
            // the stamps did not advance a whole period, which is not loss.
            .unwrap_or(0)
            .max(0) as u64
    }
}

/// One frame, decoded. No CRC, no magic, no reject token, no descriptor stride.
pub struct Record<'a> {
    frame: codec::Frame<'a>,
    lost: u64,
}

impl<'a> Record<'a> {
    /// Shared-timebase ticks. ONE free-running counter sampled per frame by every section, so
    /// aligning two streams is exact integer subtraction (kdi/contract.yaml:97-97).
    pub fn timestamp(&self) -> u64 {
        self.frame.header().timestamp
    }

    /// The DEVICE-WIDE acquisition epoch, not this stream's. Two streams can hold one open.
    pub fn run_id(&self) -> u16 {
        self.frame.header().run_id
    }

    /// The start of a contiguous segment of THIS stream. Its timestamp need not be 0, and there may
    /// be several per `run_id`.
    pub fn first_of_run(&self) -> bool {
        self.frame.header().flags.first_of_run()
    }

    /// Records the DEVICE dropped between the previous record and this one, from this stream's own
    /// declared cadence. 0 for the first record of a run.
    pub fn lost_before(&self) -> u64 {
        self.lost
    }

    /// Every block this build understands. A kind it does not is SKIPPED, not an error — that is
    /// what makes a KDI minor additive — and each skip is counted in [`Stats`]
    /// so the silence is observable.
    pub fn blocks(&self) -> impl Iterator<Item = Block<'a>> + 'a {
        let f = self.frame;
        (0..f.header().n_sections).filter_map(move |i| {
            let sec = f.section_at(i)?;
            Kind::from_code(sec.desc().kind).map(|kind| Block { sec, kind })
        })
    }

    /// The one block of this kind. `Ok(None)` = absent; `Err` = the frame carries more than one,
    /// which is LEGAL (`dup_kinds_ok`, kdi/contract.yaml:108), so a singular accessor must refuse
    /// rather than return half the data.
    pub fn block(&self, kind: Kind) -> Result<Option<Block<'a>>, Error> {
        match self.frame.section(kind.code()) {
            Ok(sec) => Ok(sec.map(|sec| Block { sec, kind })),
            // `ambiguous_kind` is in `host_reject_tokens`, not `reject_tokens`: the FRAME is valid
            // and this is host-API misuse, so it must never be counted against the device
            // (kdi/contract.yaml:149-153).
            Err(a) => Err(io_err(
                io::ErrorKind::InvalidInput,
                format!(
                    "{}: {} sections of kind {} in one frame - use blocks()",
                    a.token(),
                    a.count,
                    kind.token()
                ),
            )),
        }
    }
}

/// One typed section of a record.
#[derive(Copy, Clone)]
pub struct Block<'a> {
    sec: codec::Section<'a>,
    kind: Kind,
}

impl<'a> Block<'a> {
    /// What this block is. Always a kind this build knows — one it does not never becomes a
    /// `Block`.
    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The lane ids of this block, ascending. Pass these to [`Block::amplifier`] / [`Block::value`]
    /// / [`Block::level`] — they are labels, not packed indices.
    pub fn lanes(&self) -> impl ExactSizeIterator<Item = u16> + 'a {
        self.sec.lane_ids()
    }

    /// Rows in this block — samples per lane in THIS record. Fixed per kind today (35 for
    /// `rhd_matrix`, 1 for `adio_dig`) but read from the wire, never assumed.
    pub fn rows(&self) -> u16 {
        self.sec.desc().rows
    }

    /// Ticks per sample as the exact rational the wire states. RATE IS NEVER NORMATIVE IN THIS
    /// CONTRACT, THE TIMEBASE IS — 30 kS/s is 10000/3, and an integer 3333 drifts 1.44 s over a
    /// four-hour recording — so there is deliberately no `rate_hz()`.
    pub fn cadence(&self) -> (u32, u16) {
        self.sec.cadence()
    }

    /// Raw element at (row, physical lane id). All element widths normalise to u64. `None` if
    /// `lane` is not in [`Block::lanes`].
    pub fn value(&self, row: u16, lane: u16) -> Option<u64> {
        self.sec.element(row, self.lane_index(lane)?)
    }

    /// One whole row, lane by lane in [`Block::lanes`] order. `None` past the last row. NO
    /// rotation is applied — this is the raw row, which for `rhd_matrix` is not the channel index;
    /// use [`Block::amplifier`].
    pub fn row(&self, row: u16) -> Option<impl ExactSizeIterator<Item = u64> + 'a> {
        self.sec.row(row)
    }

    /// Packed index of a physical lane id, or `None` if this block does not carry it.
    fn lane_index(&self, id: u16) -> Option<u16> {
        self.sec.lane_ids().position(|x| x == id).map(|i| i as u16)
    }

    /// Amplifier channel `channel` of physical lane `lane`, rhd_matrix only. ROWS ARE ROTATED BY
    /// ONE: SPI returns each result during the NEXT command, so row 0 is the previous aux2, rows
    /// 1..32 are channels 0..31, 33/34 aux0/aux1 (`RhdCore.scala:136`, PR #15). Offset binary.
    pub fn amplifier(&self, channel: u8, lane: u16) -> Option<u16> {
        if self.kind != Kind::RhdMatrix || channel > 31 {
            return None;
        }
        self.sec
            .element(u16::from(channel) + 1, self.lane_index(lane)?)
            .map(|v| v as u16)
    }

    /// One of the three auxiliary results of physical lane `lane`. rhd_matrix only.
    pub fn aux(&self, which: Aux, lane: u16) -> Option<u16> {
        if self.kind != Kind::RhdMatrix {
            return None;
        }
        let row = match which {
            // Row 0 lags a whole timestep — the rotation above, at its seam.
            Aux::AuxAdc => 0,
            Aux::Temp => 33,
            Aux::Supply => 34,
        };
        self.sec
            .element(row, self.lane_index(lane)?)
            .map(|v| v as u16)
    }

    /// A digital lane's level, addressed by physical lane id. `None` for a block whose elements
    /// are not single bits — a 16-bit sample compared against zero is not a level, it is a wrong
    /// answer that always looks true.
    pub fn level(&self, row: u16, lane: u16) -> Option<bool> {
        if self.sec.desc().element_bits != 1 {
            return None;
        }
        self.sec
            .element(row, self.lane_index(lane)?)
            .map(|v| v != 0)
    }
}

/// Which auxiliary result of an RHD timestep. Their rows are not adjacent and `AuxAdc` is a
/// timestep behind the other two; naming them is what keeps a caller out of that arithmetic.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum Aux {
    /// The chip's on-die temperature sensor result (aux0), from THIS timestep.
    Temp,
    /// The supply-voltage sensor result (aux1), from THIS timestep.
    Supply,
    /// The auxiliary ADC input (aux2) — a WHOLE TIMESTEP BEHIND the other two, because it lands
    /// on the row the SPI rotation moved to the front of the frame.
    AuxAdc,
}

/// A stream's backpressure reading. From [`StreamReader::health`], and only meaningful when read
/// after the data it describes.
#[derive(Copy, Clone, Debug)]
pub struct Health {
    /// Bytes resident in the device's pipe for this stream right now. A liveness and backpressure
    /// hint, NOT a frame count: frames are self-describing, so a host reads what is there and
    /// reads again if it wanted more.
    pub readable_bytes: usize,
    /// The device dropped a frame at some point since this run started. STICKY, so it says "the
    /// records this read returned may not be contiguous", never "the last read overflowed".
    pub overrun: bool,
}

/// What the reader had to recover from, cumulative over the life of the stream. `bad_frames` is
/// where a decode rejection ends up, and the whole reason [`Error`] has no decode variant: a
/// rejected frame is a fact about the link's quality, not a reason to abandon a recording.
#[derive(Copy, Clone, Default, Debug)]
pub struct Stats {
    /// Records handed back by [`StreamReader::next`].
    pub records: u64,
    /// Frames the decoder REJECTED and stepped over. Non-zero means the link is damaging data;
    /// the recording continued regardless, which is the point.
    pub bad_frames: u64,
    /// Bytes skipped while scanning forward for the next frame anchor. Rises with `bad_frames`
    /// for the two rejections whose own declared length cannot be trusted.
    pub resync_bytes: u64,
    /// Whole frames stepped over because their `format` is not one this build decodes. Legal, and
    /// how a future format stays skippable — but a rising count means the device is emitting
    /// something this build understands none of.
    pub skipped_unknown_format: u64,
    /// Sections skipped because their `kind` is not in this build's registry. Legal — it is what
    /// makes a KDI minor additive — and counted so the silence is observable.
    pub skipped_unknown_kind: u64,
    /// Records the DEVICE dropped, summed from every [`Record::lost_before`].
    pub lost_records: u64,
}

// ─────────────────────────────────────────────────────────────────────── checks
// The two behaviours a conformance run cannot exercise, because a healthy device produces neither:
// a corrupt frame, and a frame split across two reads — silent: loss looks like a quiet device.

#[cfg(test)]
mod tests {
    use super::*;
    // Prefixed, not glob-imported: `codec::Kind` and `crate::Kind` are two generated enums with one
    // name, and the whole point of this module is that only the second reaches a caller.
    use crate::codec as c;

    /// The smallest legal format-2 frame: one 16-bit rhd_matrix section, one lane, one row.
    /// Hand-built rather than replayed from `kdi/vectors/`, because these tests need a SEQUENCE of
    /// frames with chosen timestamps and one deliberately corrupted — which no published vector is.
    fn frame(ts: u64, run_id: u16, first: bool, sample: u16) -> Vec<u8> {
        let mut f = vec![0u8; 64]; // frame_words 32: the alignment rule forces a multiple of 4
        let put16 = |f: &mut Vec<u8>, at: usize, v: u16| {
            f[at..at + 2].copy_from_slice(&v.to_le_bytes());
        };
        let put32 = |f: &mut Vec<u8>, at: usize, v: u32| {
            f[at..at + 4].copy_from_slice(&v.to_le_bytes());
        };
        put32(&mut f, c::OFF_MAGIC, c::MAGIC);
        put16(&mut f, c::OFF_FORMAT, c::FORMAT);
        put16(
            &mut f,
            c::OFF_FLAGS,
            if first { c::FLAG_FIRST_OF_RUN } else { 0 },
        );
        f[c::OFF_TIMESTAMP..c::OFF_TIMESTAMP + 8].copy_from_slice(&ts.to_le_bytes());
        put32(&mut f, c::OFF_FRAME_WORDS, 32);
        put16(&mut f, c::OFF_LAYOUT, 1);
        // 28 words = 56 B: the descriptor block ends at 0x30 and its one lane id at 0x32, and
        // hdr_words must be a multiple of 4 and cover both.
        put16(&mut f, c::OFF_HDR_WORDS, 28);
        put16(&mut f, c::OFF_N_SECTIONS, 1);
        put16(&mut f, c::OFF_RUN_ID, run_id);
        put16(&mut f, c::OFF_CONTRACT_REV, c::CONTRACT_REV);
        put16(&mut f, c::OFF_DESC_WORDS, c::DESC_WORDS_MIN);
        let d = c::DESCRIPTORS_AT;
        f[d + c::DOFF_KIND] = Kind::RhdMatrix.code();
        put16(&mut f, d + c::DOFF_N_LANES, 1);
        put16(&mut f, d + c::DOFF_WORDS_PER_LANE, 1);
        put16(&mut f, d + c::DOFF_SECTION_WORDS, 1);
        // 100 ticks per sample, so a gap is countable by inspection.
        put32(&mut f, d + c::DOFF_TICK_NUM, 100);
        put16(&mut f, d + c::DOFF_TICK_DEN, 1);
        put16(
            &mut f,
            c::DESCRIPTORS_AT + c::DESC_WORDS_MIN as usize * 2,
            7,
        ); // one lane, id 7
        put16(&mut f, 56, sample);
        let crc = c::crc32(&f[..60]);
        put32(&mut f, 60, crc);
        f
    }

    fn drain(dec: &mut Decoder, buf: &[u8]) -> (usize, Vec<(u16, u64)>) {
        let mut pos = 0;
        let mut out = Vec::new();
        loop {
            let (next, hit) = dec.advance(buf, pos);
            pos = next;
            match hit {
                // (first sample of the frame, lost_before) — enough to tell WHICH frame came back.
                Some(d) => {
                    let f = c::Frame::parse(&buf[d.start..d.end]).expect("advance accepted it");
                    let s = f.section_at(0).unwrap().element(0, 0).unwrap() as u16;
                    out.push((s, d.lost));
                }
                None => return (pos, out),
            }
        }
    }

    /// A corrupt frame must be COUNTED and stepped over, never returned or fatal, and the frames
    /// after it must still arrive. A bad CRC resyncs; a bad `frame_words` does not — without the
    /// bound the reader wedges for life, counters at zero. Drives `advance` in `next()`'s ORDER.
    #[test]
    fn a_corrupt_length_cannot_wedge_the_reader() {
        let mut wire = frame(1000, 1, true, 0x1111);
        let mut bad = frame(1100, 1, false, 0x2222);
        // Legal-looking: a multiple of 4, above the floor, just enormous. Addressed by
        // `c::OFF_FRAME_WORDS`, not a literal: the first cut wrote 8 (OFF_TIMESTAMP), corrupting
        // the stamp instead of the length and passed with the bound disabled.
        let at = c::OFF_FRAME_WORDS;
        let words = u32::from_le_bytes(bad[at..at + 4].try_into().unwrap()) | (1 << 28);
        bad[at..at + 4].copy_from_slice(&words.to_le_bytes());
        wire.extend_from_slice(&bad);
        // Enough to pass READ_BYTES: the bound is what a LIVE stream trips, and it only trips once
        // the host has actually buffered that much. A short wire wedges without ever reaching the
        // threshold — which is the honest reason this test feeds 80 KB rather than a few frames.
        let after = (READ_BYTES / 64) + 200;
        for i in 0..after {
            wire.extend_from_slice(&frame(1200 + i as u64 * 100, 1, false, 0x3000 + i as u16));
        }

        let mut dec = Decoder::default();
        let (mut buf, mut pos, mut fed, mut records) = (Vec::new(), 0usize, 0usize, 0usize);
        for _ in 0..(after * 3) {
            let (p, hit) = dec.advance(&buf, pos);
            pos = p;
            if hit.is_some() {
                records += 1;
                continue;
            }
            if fed >= wire.len() {
                break;
            }
            let take = (wire.len() - fed).min(512);
            let at = buf.len();
            buf.extend_from_slice(&wire[fed..fed + take]);
            fed += take;
            let _ = at;
            // Compact exactly as refill_into does, so `pos` parking is observable.
            if pos > 0 {
                buf.drain(..pos);
                pos = 0;
            }
        }
        // Without the bound this is 1. The 40 frames behind the wreckage must all arrive.
        assert!(
            records >= 100,
            "only {records} records past a corrupt length - the reader wedged"
        );
        assert!(
            buf.len() <= READ_BYTES,
            "buffer grew to {} bytes: the carry is unbounded",
            buf.len()
        );
    }

    #[test]
    fn a_corrupt_frame_is_counted_and_the_stream_survives_it() {
        let mut blob = frame(1000, 1, true, 0x1111);
        let mut bad = frame(1100, 1, false, 0x2222);
        bad[57] ^= 0xFF; // one flipped bit in the body: crc_err, which is resyncable
        blob.extend_from_slice(&bad);
        blob.extend_from_slice(&frame(1400, 1, false, 0x3333));

        let mut dec = Decoder::default();
        let (pos, got) = drain(&mut dec, &blob);
        assert_eq!(pos, blob.len(), "nothing is left over");
        // The corrupt frame does not appear, and the good one after it does.
        assert_eq!(
            got.iter().map(|g| g.0).collect::<Vec<_>>(),
            [0x1111, 0x3333]
        );
        assert_eq!(dec.stats.bad_frames, 1);
        assert!(dec.stats.resync_bytes > 0, "crc_err resyncs, never steps");
        // 1400 - 1000 is four periods of 100, so three records are missing between them: the two
        // the device never sent AND the one this host threw away. The timestamp gap cannot tell
        // them apart, and pretending otherwise is what made the Python oracle double-count.
        assert_eq!(got[1].1, 3);
        assert_eq!(dec.stats.lost_records, 3);
        assert_eq!(dec.stats.records, 2);
    }

    /// A frame straddles two transport reads routinely: the first half must be CARRIED, not eaten
    /// and not re-returned. The DECODE half only — that `advance` leaves the cursor in front of the
    /// partial frame; whether the buffer survives a refill is asserted separately below.
    #[test]
    fn a_frame_split_across_two_reads_is_carried_not_lost() {
        let a = frame(1000, 1, true, 0x1111);
        let b = frame(1100, 1, false, 0x2222);
        let whole: Vec<u8> = a.iter().chain(&b).copied().collect();
        let cut = a.len() + 30; // mid-header of the second frame

        let mut dec = Decoder::default();
        let (pos, got) = drain(&mut dec, &whole[..cut]);
        assert_eq!(got.iter().map(|g| g.0).collect::<Vec<_>>(), [0x1111]);
        assert_eq!(pos, a.len(), "the partial frame stays in the buffer");
        assert_eq!(dec.stats.bad_frames, 0, "a partial frame is not a bad one");

        // What the reader does next: compact, append, decode from 0 again.
        let rest = &whole[pos..];
        let (pos, got) = drain(&mut dec, rest);
        assert_eq!(got, [(0x2222, 0)], "contiguous: no loss across the split");
        assert_eq!(pos, rest.len());
        assert_eq!(dec.stats.records, 2, "and neither frame came back twice");
        assert_eq!(dec.frame_bytes, Some(64));
    }

    /// The TRANSPORT half of the carry rule, reached by no other test here: a refill must compact
    /// away what was returned and nothing else. Found by mutation — `drain(..pos)` → `clear()`, the
    /// Python residue defect exactly, passed the workspace, `--features conform` and clippy.
    #[test]
    fn refill_compacts_only_what_was_decoded() {
        let a = frame(1000, 1, true, 0x1111);
        let b = frame(1100, 1, false, 0x2222);
        let whole: Vec<u8> = a.iter().chain(&b).copied().collect();
        let cut = a.len() + 30; // the second frame arrives headless

        let (mut buf, mut pos) = (Vec::new(), 0usize);
        let mut dec = Decoder::default();

        // Read 1: one whole frame, then the head of the next.
        let n = refill_into(&mut buf, &mut pos, |dst| {
            dst[..cut].copy_from_slice(&whole[..cut]);
            Ok(cut)
        })
        .expect("read 1");
        assert_eq!(n, cut);
        let (next, hit) = dec.advance(&buf, pos);
        pos = next;
        assert!(hit.is_some() && pos == a.len());

        // Read 2: the rest. The 30 carried bytes must still be in front of it.
        let rest = &whole[cut..];
        refill_into(&mut buf, &mut pos, |dst| {
            dst[..rest.len()].copy_from_slice(rest);
            Ok(rest.len())
        })
        .expect("read 2");
        assert_eq!(
            buf.len(),
            b.len(),
            "the returned frame is compacted away, the carry is not"
        );
        let (_, hit) = dec.advance(&buf, pos);
        let d = hit.expect("the split frame completes across the two reads");
        let f = c::Frame::parse(&buf[d.start..d.end]).expect("advance accepted it");
        assert_eq!(
            f.section_at(0).unwrap().element(0, 0).unwrap() as u16,
            0x2222
        );
        // The counter that would have lied: a dropped carry resyncs, and charges the device.
        assert_eq!(dec.stats.bad_frames, 0);
        assert_eq!(dec.stats.records, 2);
    }

    /// A zero timeout must still decode what the refill it performed just returned — the shape a
    /// batching host uses ("everything resident, do not wait"). The old ordering answered `None`
    /// holding both frames: one record per call. Mutating `pump` fails this at the first assert.
    #[test]
    fn a_zero_timeout_still_decodes_what_the_refill_returned() {
        let a = frame(2000, 1, true, 0xAAAA);
        let b = frame(2100, 1, false, 0xBBBB);
        let whole: Vec<u8> = a.iter().chain(&b).copied().collect();

        let (mut buf, mut pos) = (Vec::new(), 0usize);
        let mut dec = Decoder::default();
        let mut served = false;

        // Already expired: the caller is asking for whatever can be produced without waiting.
        let deadline = Instant::now();
        let mut read = |dst: &mut [u8]| {
            if served {
                return Ok(0);
            }
            served = true;
            dst[..whole.len()].copy_from_slice(&whole);
            Ok(whole.len())
        };

        let first = pump(&mut dec, &mut buf, &mut pos, deadline, &mut read)
            .expect("read 1")
            .expect("the refill returned two whole frames; one of them is due now");
        let f = c::Frame::parse(&buf[first.start..first.end]).expect("pump validated it");
        assert_eq!(
            f.section_at(0).unwrap().element(0, 0).unwrap() as u16,
            0xAAAA
        );

        // The second is already in `buf`, so it needs no transport at all.
        let second = pump(&mut dec, &mut buf, &mut pos, deadline, &mut read)
            .expect("read 2")
            .expect("the second frame was resident before the call");
        let f = c::Frame::parse(&buf[second.start..second.end]).expect("pump validated it");
        assert_eq!(
            f.section_at(0).unwrap().element(0, 0).unwrap() as u16,
            0xBBBB
        );

        // Genuinely empty now: an expired deadline must not spin.
        assert!(pump(&mut dec, &mut buf, &mut pos, deadline, &mut read)
            .expect("read 3")
            .is_none());
    }

    /// The SAME rule, on the multi-stream loop, which cannot share `pump`. Drives `pump_multi`
    /// directly — a real `MultiReader` needs a `Device` and a socket. An earlier cut called `pump`,
    /// which would have passed with `pump_multi` still checking its deadline first.
    #[test]
    fn the_multi_stream_loop_also_decodes_what_the_refill_returned() {
        let payloads = [0xC0DEu16, 0xF00D];
        let mut lanes: Vec<Lane> = [Stream::Samples, Stream::Digital]
            .iter()
            .map(|&stream| Lane {
                stream,
                buf: Vec::new(),
                pos: 0,
                dec: Decoder::default(),
            })
            .collect();
        let frames: Vec<Vec<u8>> = payloads
            .iter()
            .enumerate()
            .map(|(i, &p)| frame(3000 + i as u64, 1, true, p))
            .collect();

        let mut served = [false, false];
        let deadline = Instant::now(); // already expired: "do not wait", not "do not look"
        let hit = pump_multi(&mut lanes, deadline, |lane| {
            let i = usize::from(lane.stream != Stream::Samples);
            if served[i] {
                return Ok(0);
            }
            served[i] = true;
            let f = &frames[i];
            lane.buf.extend_from_slice(f);
            Ok(f.len())
        })
        .expect("read")
        .expect("a refill that produced whole frames must not be reported as no-data");

        let (i, d) = hit;
        let f = c::Frame::parse(&lanes[i].buf[d.start..d.end]).expect("validated");
        assert_eq!(
            f.section_at(0).unwrap().element(0, 0).unwrap() as u16,
            payloads[i]
        );
    }

    /// `first_of_run` is the one case where a timestamp gap is NOT loss: the stream was stopped.
    /// Measuring across it reports the whole idle period as missing data.
    #[test]
    fn a_restart_is_not_a_gap() {
        let mut dec = Decoder::default();
        let blob: Vec<u8> = frame(1000, 1, true, 0x1111)
            .into_iter()
            .chain(frame(9_000_000, 2, true, 0x2222))
            .collect();
        let (_, got) = drain(&mut dec, &blob);
        assert_eq!(got, [(0x1111, 0), (0x2222, 0)]);
        assert_eq!(dec.stats.lost_records, 0);
    }

    /// Aligning against `max_frame_bytes` must still be a multiple of 16 against every known
    /// *actual* size. That is the under-align failure: a bound sized for the worst-case frame
    /// leaving a partial group of a smaller one.
    #[test]
    fn aligned_from_max_frame_bytes_covers_known_actual_sizes() {
        for actual in [88u32, 136, 2408] {
            for s in [Stream::Digital, Stream::Samples] {
                let max = s.max_frame_bytes() as u32;
                for want in [1u32, 6, 20] {
                    let n = codec::aligned_burst_at_least(max, want, USB3_READ_ALIGNMENT as u32)
                        .expect("alignment is 16");
                    assert!(n >= want, "n={n} did not cover want={want}");
                    assert_eq!(
                        (n * actual) % 16,
                        0,
                        "n={n} from max={max} want={want} vs actual={actual}"
                    );
                }
            }
        }
    }
}
