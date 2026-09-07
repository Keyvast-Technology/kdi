# KDI 0.7 — implementing a host

**Generated from `descriptor.json` by `kdi/gen.py`. Do not edit.** Every statement here comes
from the descriptor; if this file and the descriptor disagree, the descriptor is right and this
is a generator bug. The normative bundle is `descriptor.json`, `schema.json`, `manifest.json`
and `vectors/` — this document is a reading order over them, not a second source.

- **Contract version** `0.7` — major.minor. A host compares the MAJOR and nothing else.
- **Device** `keyvast-a75` (vendor `keyvast`, type `a75`, impl_version 1, board_id `0xa75`).

## 1. Bind, in this order

Every step is a register in the identity drawer below; the rules are that drawer's own `doc:`.

1. Find the device by identity (serial), not by transport.
2. Read `contract_version`. **A reading of 0 means NOT A KDI DEVICE** — an unmapped endpoint
   reads 0, so 0 is the absence of the register, not major 0. Refuse to bind.
3. Require `major` equality. A device MINOR higher than yours is fine; never do version
   arithmetic beyond that.
4. Poll `contract_ready` before touching the data path (give it up to 3000 ms).
5. Read `caps` and branch on capability bits — never on a version comparison.

## 2. Capabilities

| bit | capability | what it gates |
|---|---|---|
| 0 | `clean_frame` | this build emits the format-2 self-describing frame on the declared stream endpoints · commands: — |
| 1 | `command_protocol` | the typed request/response command channel is live (firmware-dependent: it is baked into the same .bit) · commands: `sys.hello`, `power.status`, `power.up`, `adio.mode`, `adio.adc`, `adio.fb`, `adio.dout`, `gnd.eeprom.read`, `sys.claim`, `sys.release`, `sys.challenge`, `sys.unlock`, `id.boards` |
| 2 | `ddr3` | the DDR3 pipe buffer is present and calibrated; without it a stream's depth is the on-chip FIFO only · commands: — |
| 3 | `adio` | ADIO analog/digital module I/O is present · commands: `adio.mode`, `adio.adc`, `adio.fb`, `adio.dout` |
| 4 | `grounding` | the grounding/module-ID board is present · commands: `gnd.eeprom.read` |
| 5 | `ttl_in` | TTL inputs are wired to the digital stream's lanes · commands: — |
| 6 | `slot_health` | per-slot presence and rail health are readable · commands: `power.status` |
| 7 | `field_update` | gateware images can be written over the wire and selected at boot (RESERVED — no command implements this yet) · commands: — |
| 8 | `rate_control` | the acquisition rate is host-configurable, and `rhd_matrix` emits NOTHING until it has been configured (#131). A device with this capability expects `rate_md` + `rate_apply` before acquisition and reports readiness on `rate_ready`; without it a host cannot know whether the cadence a frame declares is one the hardware was placed into. · commands: — |

## 3. The register drawers

The only raw registers in the contract. Everything else is a typed command or a stream.

### 3a. Identity (readable before the CPU boots)

| register | access | meaning |
|---|---|---|
| `contract_version` | ro | [31:16] major [15:0] minor. A READING OF 0 MEANS "NOT A KDI DEVICE" and a host MUST refuse to bind, regardless of its own major: an unmapped WireOut reads 0, so 0 is the absence of the register rather than major 0 — and while our own major IS 0 a plain major comparison cannot distinguish the two, so a non-KDI bitstream would otherwise bind successfully. A device MINOR higher than the host's is always fine; never do version arithmetic beyond the major equality test (docs/device_interface_plan.md §6a). |
| `caps` | ro | capability bitmap; bit assignments in `caps`, semantics in `cap_gates` |
| `gateware_sha` | ro | low 32 bits of the git sha (id_core) |
| `contract_ready` | ro | 1 once boot + DDR calibration complete. A host MUST poll this after connect and before any stream or command traffic, for at least `ready_timeout_ms`, and MUST refuse to bind if it never sets. This is the `init_calib` scar made explicit: acquiring before calibration silently drops beats (worst at one lane), and there is no other way to observe it. |

### 3b. Fast path (CPU-bypass, for streaming)

| register | access | meaning |
|---|---|---|
| `miso_delay` | rw | 4 bits per port: A=[3:0] B=[7:4] .. H=[31:28]. The MISO sampling tap. A WRONG VALUE DEGRADES SILENTLY -- amplitude falls per channel with no error anywhere. Sweep it against a known readback; the device publishes no optimum |
| `rate_md` | rw | [15:8] M, [7:0] D -- the MMCM feedback pair that selects the sample rate |
| `rate_apply` | wo | 0->1 programs the MMCM from rate_md. Poll rate_ready; re-apply if it stays 0 |
| `rate_ready` | ro | [0] the rate was configured by a host and rhd_matrix may emit. 0 = unconfigured, and the stream is silent by design |
| `quiesce` | rw | 1 then 0 = flush both streams, disarm consumers, stop the engine. Reverts the sample rate register and returns the device to UNCONFIGURED (see #131). |
| `run_samples` | rw | 1 = the rhd_matrix stream is acquiring (starts the SPI engine itself) |
| `run_digital` | rw | 1 = the adio_dig stream is acquiring |
| `lanes_samples` | rw | bit i = include acquisition lane i in rhd_matrix. RESETS TO 0 and a mask of 0 emits nothing, so write it before run_samples or the stream never produces a frame |
| `burst_samples` | rw | frames rhd_matrix emits per run; 0 = free-run. Choose N so N*frame_bytes is a multiple of 16 |
| `burst_digital` | rw | frames adio_dig emits per run; 0 = free-run. Choose N so N*frame_bytes is a multiple of 16 |
| `stream_status_samples` | ro | [15:0] words32 readable now (bytes = x4), [16] sticky overrun |
| `stream_status_digital` | ro | [15:0] words32 readable now (bytes = x4), [16] sticky overrun |
| `occupancy` | ro | 16-bit words resident anywhere in the pipe buffer, SATURATING at 0xffff (a true 0x10000 would make a 16-bit reader see EMPTY). A liveness/backpressure hint, NOT a frame count and NOT a correctness gate: frames are self-describing, so a host reads, decodes what arrived, and reads again if it wanted more. |
| `overflow` | ro | sticky: a frame was dropped |
| `console_tx_drop` | ro | sticky: the device's console/message TX buffer overflowed and DROPPED bytes, so any reply read while this is set may be TRUNCATED. Readable without the CPU, which is the point — a truncated reply is otherwise indistinguishable from a device that answered short. |

## 4. Streams

### 4.samples — `samples`

- direction `from_device`, frame format `2`, byte order `little`, word bits `16`
- worst-case frame **2408 bytes** — size reads from this, not from a frame you have already seen

**Wire layout.** Every offset below is normative.

| field | value |
|---|---|
| `header_at` | 0 |
| `header_bytes` | 32 |
| `descriptors_at` | 32 |
| `descriptor_stride` | desc_words |
| `lane_ids_at` | 0x20 + n_sections * desc_words * 2 |
| `lane_ids` | u16[n_lanes] per section, in DESCRIPTOR ORDER, ascending and unique within a section |
| `header_padding` | zero fill from the end of the lane-id array to hdr_words * 2 |
| `bodies_at` | hdr_words * 2 |
| `body_stride` | section_words |
| `element_order` | row_major |
| `element_at` | body_at + (row * n_lanes + lane) * (element_bits / 8) |
| `bit_packed` | element_bits == 1 packs each ROW into ceil(n_lanes/16) little-endian u16 words, LSB FIRST: lane index l of the row is bit (l & 15) of word (l >> 4). So 16 digital lines cost ONE word. |
| `trailer` | crc32 |
| `trailer_at` | frame_words * 2 - 4 |
| `trailer_bytes` | 4 |
| `trailer_residue` | 558161692 |

> ROW-MAJOR IS NORMATIVE AND IS NOT OBSERVABLE IN A SINGLE-ROW FRAME. A decoder that indexes (lane * rows + row) produces identical bytes for any section with rows == 1, so the published adio_dig vector alone cannot distinguish it — and on rhd_matrix (35 rows) the same decoder yields plausible neural data at the wrong channel index, which is the failure PR #15 shipped once already. The published vector set therefore includes a multi-row 16-bit section specifically to make this observable.

**Section descriptor.**

| field | type/value |
|---|---|
| `words` | 8 |

**Timebase.**

| field | type/value |
|---|---|
| `ticks_per_second` | 100000000 |
| `timestamp_bits` | 48 |
| `epoch` | run_start |

**Section kinds.**

- `adio_dig` (code `0x10`) — 1 row(s) x 1 bit(s), up to 16 lanes.

  > ADIO digital INPUT levels, one lane per physical line, bit-packed: 16 lines cost one word and every bit is named by its own lane id, so no host needs a slot->bit formula. LEVELS, not edges: each row is the pin level sampled once per row, so a pulse shorter than one row period can be missed entirely and an edge is located only to +-1 row. SAMPLE INSTANT: the level is latched at FRAME START, so it belongs to the frame's own timestamp instant (fixed). Derived from RTL and sim, NOT yet bench-measured. The residual error is the pin-to-latch synchroniser delay, which is a hardware measurement that has not been made — do not assume a tolerance. Note this is the SAMPLE instant, not an edge time: levels are still sampled once per row, so a pulse shorter than one row period can be missed and an edge is located only to +-1 row.
- `rhd_matrix` (code `0x20`) — 35 row(s) x 16 bit(s), up to 32 lanes.

  > One RHD acquisition lane per lane id. ROW ORDER IS ROTATED BY ONE and this is a property of the hardware, not a choice: the RHD SPI returns a command's result during the NEXT command, so row k carries the capture from command k-1 (RhdCore.scala:216-218, hardware-caught in PR #15). Concretely:
  >   row 0        the PREVIOUS timestep's aux2 (aux_adc) — note the lag
  >   rows 1..32   amplifier channels 0..31, ascending
  >   rows 33,34   this timestep's aux0 (temp) and aux1 (supply)
  > A host that assumes rows 0..31 are the amplifier reads channel n at row n and gets channel n-1, with row 0 pure garbage — plausible-looking neural data at the wrong index, which is exactly the failure PR #15 shipped once already. Amplifier codes are offset binary around 0x8000, NOT two's complement. The volts-per-code scale is a property of the chip profile and is deliberately NOT published here: a profile swap is a MAJOR bump a host must refuse to bind, never silently rescale.

**Invariants a conforming host may rely on, and must not violate.**

- **[both]** frame_words % 4 == 0 and hdr_words % 4 == 0
- **[host]** magic is a resync anchor ONLY, never a validity test; validity is CRC + declared length
- **[both]** CRC-32/ISO-HDLC (a.k.a. CRC-32, zlib/PKZIP): poly 0x04C11DB7, reflected form 0xEDB88320, init 0xFFFFFFFF, refin true, refout true, xorout 0xFFFFFFFF. Check value 0xCBF43926 over the nine bytes "123456789"
- **[both]** the trailer is a little-endian u32 at byte offset frame_words*2 - 4, computed over bytes [0, frame_words*2 - 4) — i.e. the whole frame except the trailer itself
- **[host]** crc32(whole frame INCLUDING its trailer) == 0x2144DF1C — the residue form, so a host needs no separate slice
- **[both]** section_words == words_per_lane * ceil(n_lanes * element_bits / 16)
- **[host]** take the descriptor stride from desc_words on the wire, never a compiled-in constant; reject desc_words < 8; ignore bytes past the fields you know
- **[host]** REJECT (never ignore) a frame with any reserved bit set: flags[15:3], dflags[0], dflags[7:3], descriptor pad, timestamp[63:48]
- **[both]** tick_num >= 1 and tick_den >= 1; a zero divides by zero in the loss oracle
- **[host]** for a bounded capture (burst_* != 0) the host MUST choose N such that (N * frame_bytes) % 16 == 0, because a USB3 pipe read is a multiple of 16 bytes and a KDI frame usually is not. Unaligned bursts leave a retrievable-only-later remainder
- **[both]** flags[0] marks the start of a contiguous segment of a stream. Its timestamp is the shared timebase value and need NOT be 0, and successive flagged frames MUST have strictly increasing timestamps. Not at-most-one-per-run_id: run_id is the DEVICE-WIDE epoch, so a stream that restarts while another holds the epoch open legitimately announces again under the same run_id
- **[both]** lane ids ascend and are unique within a section
- **[host]** two sections of the same kind in one frame are LEGAL; a singular accessor must raise, never return the first
- **[host]** skip an unknown kind by section_words; skip an unknown format by frame_words
- **[host]** frames lost between two timestamps = round(dt * tick_den / tick_num) - 1, WITHIN ONE STREAM only -- never across streams, which are not co-sampled
- **[both]** a run starts on the 0->1 EDGE of the stream's `run` register. The FALLING edge flushes that stream's pipe buffers in every clock domain, resets its packer phase, clears its sticky overrun and abandons the frame in flight. Therefore a host that wants a defined restart MUST write 0, then 1 — writing 1 to an already-set bit starts nothing and inherits the previous run's state, including a sticky overrun that belongs to it. Scoped to the stream whose bit moved: the other stream may be mid-recording
- **[host]** on the falling edge the host MUST also drop its own residue for that stream (any partial frame carried from a previous read), or the old run's bytes head the new run's stream and its whole frames arrive stamped with the old epoch
- **[host]** write `burst_<stream>` while that stream's run bit is 0. The bound is latched at frame admission, so a value written mid-run does not apply cleanly to the run already in progress
- **[host]** poll the stream's sticky overrun (stream_status_<stream> bit 16) AFTER a read, never before. It is sticky since the last flush, so a pre-read check reports loss from before the call — and it WILL be set for any host that does not drain continuously. Checked after, it means exactly `the frames this read returned may not be contiguous`, which is the only form a host can act on
- **[host]** a register bound to a FIELD (`0xNN.b` or `0xNN[hi:lo]`) MUST be written read-modify-masked, because registers share words: both run bits share one WireIn and both burst bounds share another. An unmasked write to one field clears its neighbour, which silently disarms the other stream
- **[both]** every frame's `contract_rev` equals this contract's MINOR. A host MUST accept a rev higher than its own and ignore it — a minor is additive by definition. A differing MAJOR cannot reach a bound host, because binding already refused it
- **[host]** `max_frame_bytes` in the descriptor is a derived READ-SIZING HINT and MUST NOT be used as a validity test. Two sections of one kind are legal, so a frame may legitimately exceed it and must still decode rather than be counted bad_length and resynced away
- **[host]** streams are INDEPENDENTLY SAMPLED and share only the timebase. A host MUST locate a stream's samples by that stream's own timestamps and MUST NOT pair samples across streams by index or by arrival order. adio_dig ships its first sample at the epoch origin; rhd_matrix ships its first FRAME at the acquisition engine's SECOND timestep boundary -- the first boundary is discarded, because the emitter admits a frame only once a timebase snapshot has retired and none has at the first boundary -- so the offset between the two streams' first samples is uniform in [1, 2) frame periods of the slower stream and is DIFFERENT ON EVERY RUN. A host MUST NOT assume a lower bound of 0: measured at 1 kS/s, adio_dig first stamp 2 ticks against rhd_matrix 104099 on a declared period of 101250, i.e. 1.028 periods on 5 of 5 runs. It does NOT drift: measured over four independent 2-minute runs the trend was +0.31, -0.34, -0.61 and +0.73 frame periods -- mean +0.02, sign alternating, i.e. read-phase noise around zero, which is what one shared counter predicts and two counters could not produce. Cross-stream alignment is exact integer subtraction of timestamps, and that is the whole reason the timebase is shared

**Reject tokens.** A decoder that refuses a frame must report one of these, and the published negative vectors carry one case per token.

`crc_err`, `bad_length`, `section_words`, `lane_ids`, `reserved_bits`, `tick_sane`, `timestamp_top16`, `first_of_run_dup`, `desc_words`, `hdr_words_fits`, `body_fits_frame`

**Host reject tokens** — raised by the host library, never on the wire.

`ambiguous_kind`, `body_shape`

**Counters** a decoder is expected to expose.

| counter | counts |
|---|---|
| `resync_bytes` | bytes skipped while scanning forward for the next magic |
| `unknown_kind` | sections skipped by section_words because the kind is not in this host's registry |
| `format_skipped` | frames skipped by frame_words because `format` is not one this host decodes |

**Frozen prefix** — these offsets never move, in any future contract version, so a
decoder can identify a frame before it knows the rest of the layout.

| field | offset | type | value |
|---|---|---|---|
| `magic` | 0 | u32 | 0x4644564b |
| `format` | 4 | u16 |  |
| `frame_words` | 16 | u32 |  |

**Lane-id blocks.** A lane id resolves to a physical input; the formula is normative.

| block | name | formula |
|---|---|---|
| `0x0000-0x0fff` | rhd | `slot*16 + unit` |
| `0x1000-0x1fff` | adio_digital | `0x1000 + slot*16 + channel` |
| `0x2000-0x2fff` | adio_analog | `0x2000 + slot*16 + channel` |

### 4.digital — `digital`

- direction `from_device`, frame format `2`, byte order `little`, word bits `16`
- worst-case frame **88 bytes** — size reads from this, not from a frame you have already seen

**Wire layout.** Every offset below is normative.

| field | value |
|---|---|
| `header_at` | 0 |
| `header_bytes` | 32 |
| `descriptors_at` | 32 |
| `descriptor_stride` | desc_words |
| `lane_ids_at` | 0x20 + n_sections * desc_words * 2 |
| `lane_ids` | u16[n_lanes] per section, in DESCRIPTOR ORDER, ascending and unique within a section |
| `header_padding` | zero fill from the end of the lane-id array to hdr_words * 2 |
| `bodies_at` | hdr_words * 2 |
| `body_stride` | section_words |
| `element_order` | row_major |
| `element_at` | body_at + (row * n_lanes + lane) * (element_bits / 8) |
| `bit_packed` | element_bits == 1 packs each ROW into ceil(n_lanes/16) little-endian u16 words, LSB FIRST: lane index l of the row is bit (l & 15) of word (l >> 4). So 16 digital lines cost ONE word. |
| `trailer` | crc32 |
| `trailer_at` | frame_words * 2 - 4 |
| `trailer_bytes` | 4 |
| `trailer_residue` | 558161692 |

> ROW-MAJOR IS NORMATIVE AND IS NOT OBSERVABLE IN A SINGLE-ROW FRAME. A decoder that indexes (lane * rows + row) produces identical bytes for any section with rows == 1, so the published adio_dig vector alone cannot distinguish it — and on rhd_matrix (35 rows) the same decoder yields plausible neural data at the wrong channel index, which is the failure PR #15 shipped once already. The published vector set therefore includes a multi-row 16-bit section specifically to make this observable.

**Section descriptor.**

| field | type/value |
|---|---|
| `words` | 8 |

**Timebase.**

| field | type/value |
|---|---|
| `ticks_per_second` | 100000000 |
| `timestamp_bits` | 48 |
| `epoch` | run_start |

**Section kinds.**

- `adio_dig` (code `0x10`) — 1 row(s) x 1 bit(s), up to 16 lanes.

  > ADIO digital INPUT levels, one bit-packed lane per physical line: 16 lines cost ONE word and every bit is named by its own lane id, so no host needs a slot-to-bit formula. The level is latched at FRAME START, so it belongs to its own frame's timestamp instant (fixed). LEVELS, not edges: a pulse shorter than one sample period can be missed and an edge is located only to +-1 sample.

**Invariants a conforming host may rely on, and must not violate.**

- **[both]** frame_words % 4 == 0 and hdr_words % 4 == 0
- **[host]** magic is a resync anchor ONLY, never a validity test; validity is CRC + declared length
- **[both]** CRC-32/ISO-HDLC (a.k.a. CRC-32, zlib/PKZIP): poly 0x04C11DB7, reflected form 0xEDB88320, init 0xFFFFFFFF, refin true, refout true, xorout 0xFFFFFFFF. Check value 0xCBF43926 over the nine bytes "123456789"
- **[both]** the trailer is a little-endian u32 at byte offset frame_words*2 - 4, computed over bytes [0, frame_words*2 - 4) — i.e. the whole frame except the trailer itself
- **[host]** crc32(whole frame INCLUDING its trailer) == 0x2144DF1C — the residue form, so a host needs no separate slice
- **[both]** section_words == words_per_lane * ceil(n_lanes * element_bits / 16)
- **[host]** take the descriptor stride from desc_words on the wire, never a compiled-in constant; reject desc_words < 8; ignore bytes past the fields you know
- **[host]** REJECT (never ignore) a frame with any reserved bit set: flags[15:3], dflags[0], dflags[7:3], descriptor pad, timestamp[63:48]
- **[both]** tick_num >= 1 and tick_den >= 1; a zero divides by zero in the loss oracle
- **[host]** for a bounded capture (burst_* != 0) the host MUST choose N such that (N * frame_bytes) % 16 == 0, because a USB3 pipe read is a multiple of 16 bytes and a KDI frame usually is not. Unaligned bursts leave a retrievable-only-later remainder
- **[both]** flags[0] marks the start of a contiguous segment of a stream. Its timestamp is the shared timebase value and need NOT be 0, and successive flagged frames MUST have strictly increasing timestamps. Not at-most-one-per-run_id: run_id is the DEVICE-WIDE epoch, so a stream that restarts while another holds the epoch open legitimately announces again under the same run_id
- **[both]** lane ids ascend and are unique within a section
- **[host]** two sections of the same kind in one frame are LEGAL; a singular accessor must raise, never return the first
- **[host]** skip an unknown kind by section_words; skip an unknown format by frame_words
- **[host]** frames lost between two timestamps = round(dt * tick_den / tick_num) - 1, WITHIN ONE STREAM only -- never across streams, which are not co-sampled
- **[both]** a run starts on the 0->1 EDGE of the stream's `run` register. The FALLING edge flushes that stream's pipe buffers in every clock domain, resets its packer phase, clears its sticky overrun and abandons the frame in flight. Therefore a host that wants a defined restart MUST write 0, then 1 — writing 1 to an already-set bit starts nothing and inherits the previous run's state, including a sticky overrun that belongs to it. Scoped to the stream whose bit moved: the other stream may be mid-recording
- **[host]** on the falling edge the host MUST also drop its own residue for that stream (any partial frame carried from a previous read), or the old run's bytes head the new run's stream and its whole frames arrive stamped with the old epoch
- **[host]** write `burst_<stream>` while that stream's run bit is 0. The bound is latched at frame admission, so a value written mid-run does not apply cleanly to the run already in progress
- **[host]** poll the stream's sticky overrun (stream_status_<stream> bit 16) AFTER a read, never before. It is sticky since the last flush, so a pre-read check reports loss from before the call — and it WILL be set for any host that does not drain continuously. Checked after, it means exactly `the frames this read returned may not be contiguous`, which is the only form a host can act on
- **[host]** a register bound to a FIELD (`0xNN.b` or `0xNN[hi:lo]`) MUST be written read-modify-masked, because registers share words: both run bits share one WireIn and both burst bounds share another. An unmasked write to one field clears its neighbour, which silently disarms the other stream
- **[both]** every frame's `contract_rev` equals this contract's MINOR. A host MUST accept a rev higher than its own and ignore it — a minor is additive by definition. A differing MAJOR cannot reach a bound host, because binding already refused it
- **[host]** `max_frame_bytes` in the descriptor is a derived READ-SIZING HINT and MUST NOT be used as a validity test. Two sections of one kind are legal, so a frame may legitimately exceed it and must still decode rather than be counted bad_length and resynced away
- **[host]** streams are INDEPENDENTLY SAMPLED and share only the timebase. A host MUST locate a stream's samples by that stream's own timestamps and MUST NOT pair samples across streams by index or by arrival order. adio_dig ships its first sample at the epoch origin; rhd_matrix ships its first FRAME at the acquisition engine's SECOND timestep boundary -- the first boundary is discarded, because the emitter admits a frame only once a timebase snapshot has retired and none has at the first boundary -- so the offset between the two streams' first samples is uniform in [1, 2) frame periods of the slower stream and is DIFFERENT ON EVERY RUN. A host MUST NOT assume a lower bound of 0: measured at 1 kS/s, adio_dig first stamp 2 ticks against rhd_matrix 104099 on a declared period of 101250, i.e. 1.028 periods on 5 of 5 runs. It does NOT drift: measured over four independent 2-minute runs the trend was +0.31, -0.34, -0.61 and +0.73 frame periods -- mean +0.02, sign alternating, i.e. read-phase noise around zero, which is what one shared counter predicts and two counters could not produce. Cross-stream alignment is exact integer subtraction of timestamps, and that is the whole reason the timebase is shared

**Reject tokens.** A decoder that refuses a frame must report one of these, and the published negative vectors carry one case per token.

`crc_err`, `bad_length`, `section_words`, `lane_ids`, `reserved_bits`, `tick_sane`, `timestamp_top16`, `first_of_run_dup`, `desc_words`, `hdr_words_fits`, `body_fits_frame`

**Host reject tokens** — raised by the host library, never on the wire.

`ambiguous_kind`, `body_shape`

**Counters** a decoder is expected to expose.

| counter | counts |
|---|---|
| `resync_bytes` | bytes skipped while scanning forward for the next magic |
| `unknown_kind` | sections skipped by section_words because the kind is not in this host's registry |
| `format_skipped` | frames skipped by frame_words because `format` is not one this host decodes |

**Frozen prefix** — these offsets never move, in any future contract version, so a
decoder can identify a frame before it knows the rest of the layout.

| field | offset | type | value |
|---|---|---|---|
| `magic` | 0 | u32 | 0x4644564b |
| `format` | 4 | u16 |  |
| `frame_words` | 16 | u32 |  |

**Lane-id blocks.** A lane id resolves to a physical input; the formula is normative.

| block | name | formula |
|---|---|---|
| `0x1000-0x1fff` | adio_digital | `0x1000 + slot*16 + channel` |

### 4.to_device — `to_device`

- direction `to_device`, frame format `2`, byte order `None`, word bits `None`

## 5. Commands

Typed request/response. A host calls these by name; it never writes a raw register outside the
two drawers above.

### `sys.claim`

- tier `public` · safety `attended` · scope `session`
- args: —
- returns: ok: bool
- errors: `busy`

  > Take the device lease. `token` is an opaque caller-chosen string in the envelope, not an arg. A second host is refused with `busy` and MUST NOT proceed — the board is a single-holder resource and a concurrent opener presents downstream as a gateware regression. `unknown_cmd` means this build has no lease; proceed unclaimed.

### `sys.release`

- tier `public` · safety `attended` · scope `session`
- args: —
- returns: ok: bool
- errors: —

  > Drop the lease held by `token`. Idempotent; safe to call on a session that never claimed.

### `sys.challenge`

- tier `public` · safety `ro` · scope `session`
- args: —
- returns: nonce: u32, dna: u64
- errors: `not_ready`

  > Begin an unlock: the device mints a fresh nonce and returns it with the FPGA DNA the grant must name. ONE outstanding challenge per device: a new challenge, a successful unlock or a reset retires the previous nonce. Refused with `not_ready` while the DNA has not been captured — a nonce is never bound to a meaningless value. `unknown_cmd` means this build has no unlock; proceed public.

### `sys.unlock`

- tier `public` · safety `attended` · scope `session`
- args: tier: enum, nonce: u32, dna: u64, sig: hex
- returns: ok: bool, tier: str
- errors: `tier_locked`, `bad_args`

  > Present a signed grant to raise the session's tier. THE GRANT IS THE THREE ARGUMENTS, and its signing bytes are the compact, key-sorted JSON object they rebuild: `{"dna":<dna>,"nonce":<nonce>,"tier":"<tier>"}` — no whitespace, integers in decimal, exactly that key order. `sig` is an Ed25519 signature (RFC 8032, 64 bytes, hex) over those bytes by a key whose public half the device holds. The device accepts iff the signature verifies, `nonce` equals its outstanding challenge, and `dna` equals its own DNA or is 0 (the wildcard). The nonce is SINGLE-USE: success or failure retires it. Refusal is `tier_locked` with an INFORMATIVE `why` — `no_key` (this build holds no public key), `no_challenge` (no outstanding nonce, or the DNA was never captured), `bad_nonce`, `bad_dna`, `bad_sig` — a host switches on `err`, never on `why`. Success returns the tier reached. THE TIER IS THE DEVICE'S, NOT A CONNECTION'S: this wire has no session boundary, so closing a link relocks nothing. It drops back to public on reset, and after 300 s in which the device saw no `kdi` command of any tier (`KEYVAST_UNLOCK_IDLE_SEC`, fixed per build); any `kdi` command restarts that clock. The wire shape is fixed now so the proof can be strengthened later without moving how a host speaks. On the usb3 binding the request line is ~190 chars (`nonce` up to 10 digits, `dna` up to 20, `sig` 128 hex), so the firmware pins `SHELL_CMD_BUFF_SIZE=256` (Zephyr's default; `SHELL_MINIMAL` would halve it and stays off). kdi/vectors/unlock_vectors.json pins the canonical bytes, a valid signature and three refusals. `worst_ms: 10000` carries 1.5x over a MEASURED **656,016,778 cycles = 6,560 ms at 100 MHz** for an accepted grant, end to end from the submitting newline to the reply, on the shipping fabric under Verilator (`make sim-unlock`). The same path with no verify, `sys.challenge`, is 48,292 cycles (0.48 ms), so the verify is essentially all of it. The cost is the CPU, not the protocol: `MulDivIterativePlugin(mulUnrollFactor = 1)` spends 33 cycles per multiply and TweetNaCl's two 256-step ladders alone are 9,216 field multiplies. A refused grant costs the same as an accepted one — a `bad_sig` refusal at the SAME nonce is 656,016,768, ten cycles apart, and three runs span 0.006 % — so this covers every outcome. It is a SIM figure: it excludes host transport, and the sim's power thread contends differently than a real board's.

### `sys.hello`

- tier `public` · safety `ro`
- args: —
- returns: proto: u8, cmdset: str, kdi: str, board_id: u32, fw: str, gw: str, tiers: str
- errors: —

  > Handshake. fw/gw are the firmware and gateware git shas (8 hex) — equal when both came from one build. The identity REGISTERS carry the same gateware sha pre-boot; the device DNA is not on this command (it is AXI-side, reachable via the human `kv id`, and on `sys.challenge`). `tiers` lists the tiers a session of THIS BUILD can reach, comma-joined in rank order: `public` alone means the build has no unlock and no factory command; `public,service,factory` means every tier is behind `sys.unlock`. A station tool refuses a build whose `tiers` lacks `factory` rather than failing midway.

### `id.boards`

- tier `public` · safety `ro`
- args: seg: enum
- returns: boards: [{addr: u8, serial: str}]
- errors: `bad_args`, `no_device`, `i2c_nak`, `i2c_timeout`

  > The Serial of every Board the device can see on one I2C segment: `main` is the main bus, `slotN` is module mux channel N. Board-ignorant by design — the device scans 7-bit addresses 0x50..0x57, reads 64 bytes at register 0 with a 16-bit register address, keeps the blocks that carry a valid identity record (the internal `board_record` layout), and returns the Serial string with the address it was found at. A NAK from an EEPROM address is "no Board there", never an error; a blank EEPROM (all 0xFF or all 0x00) is "no record" and is omitted; only a stuck bus is reported — and `i2c_nak` can only mean the mux itself (`addr` 0x77) refused the channel select. One segment per call so at most 8 entries, and the reply always fits `max_body_bytes`. On a `slot` segment the device holds the mux on that channel for the transaction and restores what the power sequencer expects. Every other record field is unreachable below the factory tier.

### `power.status`

- tier `public` · safety `ro`
- args: —
- returns: present: u8, reverify: bool
- errors: —

  > Read the power tree, writing nothing. present = module-present bitmask from the most recent sequence pass (a raw detect read is NOT equivalent — after a pass those bits are outputs driving the DCDC enables). reverify = the periodic re-verify thread is enabled.

### `power.up`

- tier `public` · safety `attended`
- args: —
- returns: ok: bool, present: u8
- errors: `no_device`

  > Run the rail sequence. Stim stays off. Level-set: safe to retry.

### `adio.mode`

- tier `public` · safety `attended`
- args: slot: u8, ch1: enum, ch2: enum
- returns: slot: u8, ch_mode: u16
- errors: `bad_args`, `not_present`, `no_ip`

  > Set a slot's two channel modes (I2C mux + CH_MODE, kept coherent).

### `adio.fb`

- tier `public` · safety `attended`
- args: slot: u8, on: u8
- returns: slot: u8, fb: u8
- errors: `bad_args`, `not_present`, `no_ip`

  > Cross-channel loopback tie. Independent of either channel's DIRECTION and of CH_MODE, and re-asserted with the expander, so setting a mode never silently drops a tie. The muxes make it a SELECT, not a strap: with the tie on, a channel routes to the other channel instead of to its jack.

### `adio.dout`

- tier `public` · safety `attended`
- args: slot: u8, mask: u8
- returns: slot: u8, dout: u8, din: u8
- errors: `bad_args`, `not_present`, `no_ip`

  > Drive the digital outputs (bit0 CH1, bit1 CH2) and echo the RAW pad readback, so one call is both a stimulus and its measurement. A non-zero mask is refused on an absent slot: driving a pin high there back-powers the module rail. Note the pin only moves if that channel's CH_MODE is out, and a driven channel reads 0 on its own TTL lane by design -- read the OTHER channel, through the tie.

### `adio.adc`

- tier `public` · safety `ro`
- args: slot: u8, ch: u8, n: u8
- returns: slot: u8, ch: u8, codes: u16[], valid: bool[]
- errors: `bad_args`, `no_ip`

  > n is range-checked, never silently clamped (the human `kv adio adc` clamps). valid[i] mirrors each sample's valid bit — a code with valid=false is meaningless.

## 6. Error registries

Switch on the token, never on prose. Both sets are CLOSED: a token outside them is a device or
host bug, not something to pattern-match.

### 6a. Device errors

| token | retryable | meaning |
|---|---|---|
| `bad_args` | no | argument count, type or range rejected before the handler ran |
| `unknown_cmd` | no | no such command in this cmdset |
| `tier_locked` | no | service/factory command refused; no unlock grant in this session |
| `not_present` | no | slot not in the last power-sequence present mask; refusing to drive its pins |
| `no_ip` | no | the slot's adio_core did not answer at its AXI page |
| `no_device` | yes | a Zephyr device backing this command is not ready |
| `i2c_nak` | yes | the addressed device did not ACK (absent, busy, or a write cycle in progress); `addr` names it |
| `i2c_timeout` | no | the bus did not complete the transfer (SCL/SDA held); `addr` names the target |
| `internal` | no | firmware bug — report it with the rc |
| `busy` | yes | another host holds the device lease; retry after it releases or its lease expires |
| `not_claimed` | no | a state-changing command was sent without holding the lease; claim first |
| `confirm_required` | no | a destructive command needs a matching `confirm` envelope echo |
| `not_ready` | yes | the device has not finished boot/calibration; poll contract_ready |
| `ro_register` | no | a write was addressed to a read-only register |
| `no_such_register` | no | no register or stream of that name in this binding |
| `response_too_large` | no | the reply did not fit the device's response buffer and was not sent |

### 6b. Host errors (raised by the host library, never on the wire)

| token | meaning |
|---|---|
| `host_timeout` | no reply / no data within the caller's deadline |
| `host_short_read` | the transport returned fewer bytes than the framing declared |
| `host_overflow` | this stream's sticky overrun was set: the frames just read may not be contiguous |
| `host_unsafe_arg` | an argument was refused by the host before it reached the wire (see arg_charset) |

## 7. Transport bindings

The contract is defined in symbolic NAMES. A binding maps them to one transport's physical
endpoints. **Resolve by name from the descriptor; never compile an address in.**

### `udp` (binding_version 0.2)

### `tcp` (binding_version 0.2)

### `usb3` (binding_version 0.2)

Endpoint grammar: `{'forms': [{'syntax': '<kind>:0xNN', 'means': 'the whole 32-bit register'}, {'syntax': '<kind>:0xNN.b', 'means': 'bit b (width 1); b counts from the LSB'}, {'syntax': '<kind>:0xNN[hi:lo]', 'means': 'bits hi..lo inclusive, value right-justified to lo; hi >= lo'}], 'kinds': ['wireout', 'wirein', 'triggerin'], 'default_width': 32, 'note': 'wireout is read-only, wirein is write-only, triggerin is a one-cycle pulse on `.b`'}`

**reg**

| name | endpoint |
|---|---|
| `contract_version` | `wireout:0x35` |
| `caps` | `wireout:0x36` |
| `gateware_sha` | `wireout:0x37` |
| `contract_ready` | `wireout:0x31.1` |
| `quiesce` | `wirein:0x00.0` |
| `run_samples` | `wirein:0x11.1` |
| `run_digital` | `wirein:0x11.0` |
| `lanes_samples` | `wirein:0x12` |
| `burst_digital` | `wirein:0x13[15:0]` |
| `burst_samples` | `wirein:0x13[31:16]` |
| `stream_status_samples` | `wireout:0x39` |
| `stream_status_digital` | `wireout:0x38` |
| `occupancy` | `wireout:0x20` |
| `overflow` | `wireout:0x31.0` |
| `console_tx_drop` | `wireout:0x31.2` |
| `miso_delay` | `wirein:0x04` |
| `rate_md` | `wirein:0x03` |
| `rate_apply` | `triggerin:0x40.0` |
| `rate_ready` | `wireout:0x3b.0` |

**stream**

| name | endpoint |
|---|---|
| `samples` | `okPipeOut:0xa3` |
| `digital` | `okPipeOut:0xa2` |

**message**

| name | endpoint |
|---|---|
| `status` | `wireout:0x30` |
| `tx_pipe` | `okPipeOut:0xa1` |
| `rx_data` | `wirein:0x0f` |
| `rx_count` | `wirein:0x10` |
| `rx_push` | `triggerin:0x43.0` |

**Every read must be a multiple of 16 bytes.**

**How to read a stream on this binding:**

> For each read: take `avail = stream_status_<stream>[15:0]` (words32), compute `bytes = avail * 4`, clamp to whatever the host wants, round DOWN to a multiple of `read_alignment`, and skip the read entirely if the result is 0. Only okBTPipeOut endpoints may be block-read; the kind is in the endpoint string above and MUST be honoured.

## 8. Proving an implementation

The golden vectors in `vectors/` are the oracle, and they are what a decoder we did not write
is certified against. They deliberately include cases that a plausible-but-wrong decoder fails:
multi-row and multi-lane frames (with `rows == 1` a row-major and a lane-major decoder emit
IDENTICAL bytes), forward-compatibility frames carrying a wider descriptor stride, element
widths and unregistered section kinds, and one negative case per reject token above.
