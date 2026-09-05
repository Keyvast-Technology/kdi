# Changelog

## v0.4.0 — two streams, a self-describing container, and registers a legacy host cannot clobber

**If you built against v0.3, three things moved and one of them was unfixable.**

**KDI's registers moved to `0x11`-`0x13`.** v0.3 placed them at `0x15`-`0x18`, every one of which
the incumbent acquisition host WRITES for its own purposes — `0x15` is its TTL-output word. A
legacy host sharing the instrument could therefore stop both KDI streams, and once the sample
stream also starts acquisition, the same write STARTS it. This was not a hypothetical: it was
found by checking the addresses against the hosts that actually speak to this board rather than
against the reference. `0x0f`-`0x13` is the only gap in that map.

Two registers share a word, so **every write must be masked**. A host that writes the whole word
stops the other stream.

**Streams are independent and separately addressed.** `adio_dig` and `rhd_matrix` emit on their own
endpoints with their own run bits, lane masks and burst bounds. Starting or stopping one leaves the
other's capture intact — asserted per-run, not assumed.

**One device-wide timebase.** Both streams stamp from a single counter, so aligning them is exact
integer subtraction. They are deliberately NOT co-sampled and the contract says so: applying one
stream's loss oracle across streams is forbidden, because their cadences are co-prime by design.

### The vector bundle is the part to re-read

v0.3 published ONE 64-byte `adio_dig` vector. That vector has `rows == 1`, where a row-major and a
lane-major decoder produce **identical bytes** — so a decoder that transposes `rhd_matrix`'s 35 rows
against its lanes passed 100% of the published oracle while emitting plausible neural data at the
wrong channel index. That is a failure this project shipped once.

The bundle is now six frames plus a streaming case and 20 negatives covering all 11 reject tokens.
`golden_wide` is the one to look at hardest: it uses descriptor strides and element widths no
current device emits, so a decoder that compiles in today's stride, today's element widths, or
faults on an unregistered kind is caught here rather than by a future device.

### Also

- `first_of_run` is an explicit flag, never inferred from `timestamp == 0`. v0.2's rule ("flags[0]
  implies timestamp 0") is retired: it is only satisfiable if every stream starts at the epoch
  origin, and streams start independently on a device-wide epoch.
- Pipe occupancy is in **32-bit words**; `frame_words` is in **16-bit words**. Both units appear in
  one protocol, so neither can be assumed — a host reading occupancy as 16-bit words reads half the
  resident data and then decodes a truncated frame.
- Every read must be a multiple of 16 bytes. A digital frame is 88 B, which is not, so size a
  bounded burst such that `frames x frame_bytes` is 16-byte aligned or the tail is unreadable until
  more data arrives.

## v0.1.1 — CORRECTION: rhd_matrix row order was off by one

**v0.1.0 published a wrong row order. If you built a decoder against it, fix this first.**

v0.1.0 said *"rows 0..31 are amplifier channels in ascending order, rows 32..34 are the chip's
three aux results"*. That is wrong in the most dangerous way available: a host implementing it
reads channel *n* at row *n* and gets **channel n-1**, with row 0 pure garbage — plausible-looking
neural data at the wrong index, with a valid CRC and correct lengths.

The RHD SPI returns a command's result during the **next** command, so row *k* carries the capture
from command *k-1*. The actual content of an `rhd_matrix` section is:

| row | content |
|---|---|
| 0 | the **previous** timestep's aux2 (aux_adc) — it lags |
| 1..32 | amplifier channels 0..31, ascending |
| 33, 34 | this timestep's aux0 (temp) and aux1 (supply) |

This is the hardware, not a choice, and it was already documented in the RTL: an earlier revision
shipped the un-rotated version once and a real RHD2132's ROM/ID answers missed their expected slots
at every MISO delay. Static-MISO simulations cannot see it.

No wire bytes change — only the published meaning of the rows. `format` stays 2 and the golden
vector is unaffected (it carries a digital section, not an rhd_matrix one).


## v0.1.0 — frame format 2: the self-describing container

**Wire-visible: the frame format changes completely. Format 1 frames no longer decode.**
Safe to do now because nothing has bound: no gateware emits format 2, and `clean_frame`
stays clear so a host that needs it refuses to bind.

`decode` now needs **no descriptor at all** — every length, lane identity, element width and
cadence is on the wire.

```
32 B header   magic | format=2 | flags | timestamp u48 | frame_words | layout |
              hdr_words | n_sections | run_id | contract_rev | desc_words
descriptors   n x 16 B, stride taken FROM THE WIRE so it can grow again
lane ids      sum(n_lanes) x u16 · bodies element-major (1/16/32/64-bit) · CRC-32 trailer
```

* **`magic` is a resync anchor only, never a validity test.** Validity is CRC + declared
  length + timestamp continuity. Host-side the CRC is one call: `zlib.crc32(frame) ==
  0x2144DF1C`. A separate header CRC is impossible — the emitter produces the header long
  before the body exists — so the substitute is that the next magic must appear at the
  declared stride.
* **A new module type is a new `kind`**, skipped by `section_words`. Additive: no decoder
  change, no version bump. An unknown *format* is skippable too, by `frame_words`; `magic`,
  `format` and `frame_words` are frozen at their offsets for every future format.
* **Cadence is an exact rational** (`tick_num`/`tick_den`; 30 kHz = 10000/3). Rate is never
  normative in this contract — the timebase is. A hardware rate change needs no contract
  change. An integer tick count could not express 30 kHz and would drift 1.44 s over a
  4-hour recording.
* **One shared 48-bit timebase at 10 ns**, sampled per frame, epoch per run paired with
  `run_id`. Aligning two streams is exact integer subtraction — no sync channel, no
  per-stream linear fit. The frame-to-frame delta **jitters** (3333/3334 at 30 kHz) and there
  is deliberately no constant-delta invariant, because at 30 kS/s one is not implementable.
* **Digital input is bit-packed**: 16 lines cost one word, every bit named by its own lane
  id, so no host needs a slot-to-bit formula.
* **`to_device` is reserved** — declared, not implemented. The container is
  direction-agnostic; reserving the direction before anything binds is nearly free.
* **7 negative vectors** now ship with the normative reason token each must be rejected
  with, so a decoder can be proven to reject what it must, not merely accept what it should.

Known limitation, published deliberately: digital-in levels are sampled at the *end* of the
frame that carries them, so they trail their own timestamp by up to one frame period and the
offset varies with the enabled lane count. Derived from RTL, not bench-measured. A future
emitter latches at frame start and this note tightens.

`kdi:` stays `0.1`: it is the command-set version and no command changed.


All notable, **wire-visible** changes to the Keyvast Device Interface. One entry
per contract version.

## v0.0.2 — 2026-07-25

**The typed command protocol is live on hardware.** The device now answers framed
request/response commands over the control channel (capability `command_protocol`),
verified on real hardware: 10/10 cases including every negative path.

- Commands published: `sys.hello`, `power.status`, `power.up`, `adio.mode`, `adio.adc`.
  All five are implemented in the shipping firmware and hardware-verified.
- `power.status` returns `{present, reverify}` (was `{rail, present}`): `present` is the
  module mask from the last power-sequence pass, which is the only meaningful source —
  a raw detect read stops reflecting presence once those bits drive the DCDC enables.
- `adio.adc` gained `valid[]` alongside `codes[]`: a code whose valid bit is clear is
  meaningless, so the host is told rather than left to guess.
- Arguments are **range-checked, never clamped** — out-of-range or malformed args are
  refused with `bad_args` (the human console clamps; a machine caller gets told).
- Removed (not implemented, so not published — publishing a command the device answers
  `unknown_cmd` to is a contract lie): `adio.dacsweep`. Also withheld from the internal
  set: `dbg.peek` (an unguarded raw read can DECERR-halt the control CPU; it needs a
  mapped-page allowlist first) and `eeprom.write` (identity-rewriting; needs the factory
  unlock + confirm-echo).
- Device capabilities now report `command_protocol`; `caps` reads `0x7e`.

Pre-1.0 note: `kdi` stays `0.1` — the register/frame surface is unchanged. Before 1.0 the
command set may still change; after 1.0 these would be major/minor bumps under §13 of the
design document.

## v0.0.1 — 2026-07-24

- First published KDI descriptor (contract `kdi 0.1`), software reference,
  pre-bitstream.
- Command-first contract: 6 public commands (`sys.hello`, `power.status`,
  `power.up`, `adio.mode`, `adio.adc`, `adio.dacsweep`); private service/factory
  commands withheld by construction.
- Two register drawers: identity/caps/heartbeat/`contract_ready`, and a
  CPU-bypass fast-path (`run`/`occupancy`/`overflow`/`frame_counter`/`data_ready`).
- One stream (`samples`), self-describing header, descriptor-driven frame size.
- Capabilities: clean_frame, command_protocol, ddr3, adio, grounding, ttl_in,
  slot_health, field_update.
- Conformance: 11/11 checks pass against the software device (see `manifest.json`).
