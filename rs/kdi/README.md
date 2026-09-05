# kdi

**The Keyvast Device Interface host library.** Find an instrument by identity, bind it, acquire
from it as a stream of decoded records, and drive its command channel with typed methods generated
from the contract.

Frames, CRCs, magic-word resync, reject tokens, descriptor strides and partial-frame carry are all
real and all necessary — and every one of them is plumbing a consumer of an instrument library
should never have had to hold. This crate holds them, once. The decoder is a **hidden**
dependency: no type of its appears anywhere in this crate's public API.

## What it needs

An instrument, and `--features usb3`.

`kdi::find` discovers devices by identity across every binding this build has. Besides USB3 it can
bind a *software device model* over UDP, which is how the reference implementation runs its tests
with no hardware — but that model is part of the reference implementation and is **not distributed
with this crate**, so the UDP path is not something you can exercise from a `cargo add kdi` alone.
It is compiled in because it costs nothing and because a second binding is what keeps the transport
boundary honest.

A real instrument needs `--features usb3`, which loads the USB3 **device driver** at run time. Where
it is loaded from, in order: the `driver_dir` argument to `Device::open_usb3`, then
`$KDI_DRIVER_DIR` (a path list), then the copy compiled in by `--features bundled`, then the
operating system's search path.

`bundled` is **off by default and puts your platform's USB3 driver into the artifact** (~2.1 MB).
What you build then runs on a machine with no driver installed (the driver is unpacked to a
private directory under the system temp dir on first use). Configuring a board takes a bitstream
you already have — see Binding versus configuring below.

| Target | Vendored driver |
|---|---|
| `x86_64-pc-windows-*` | yes — **the one exercised against real hardware** |
| `x86_64-unknown-linux-gnu` | yes — glibc >= 2.34, so RHEL/Rocky 9, Ubuntu 22.04, Debian 12 and newer. **Not musl** |
| `x86_64-apple-darwin`, `aarch64-apple-darwin` | yes |
| anything else | **compile error** naming what is missing |

An un-vendored target fails the build rather than quietly behaving like a build without the feature,
because a feature that means something different per platform only shows the difference on a
customer's machine. Only the Windows driver has been run against a board; the other three are
verified to export the entry points this crate resolves, and no further.

Every driver sits in this crate's source and is downloaded either way — Cargo packages a whole
crate directory — so the feature buys artifact size, not download size, and one architecture's
library never ends up inside another's binary. `kdi::bundled::VENDORED` says where each came from,
with its SHA-256 and length, and a test re-checks every one of them against the directory on every
platform.

## Binding versus configuring

`Device::open_usb3` **binds what is already running and never flashes.** A host that configures the
FPGA before reading its identity has learned nothing about the device it found, so this is the call
an artifact gate wants, and it is safe against a shared instrument.

`Device::open_usb3_configured` (`--features usb3`) **loads the bitstream you pass, then binds.**
Configuration is volatile — it does not touch flash and a power cycle undoes it — but it is
state-changing for whoever else is using that board. The driver's status is checked and a failure
is an error naming the operation and the status, never a quiet success onto a board still running
whatever was resident. Readiness afterwards is polled on the contract's `contract_ready` register
rather than slept through. The library does not compare the identity against a compiled-in
constant; print `Device::gateware_sha()` / `Device::kdi()` and compare them yourself.

```text
cargo run --features usb3 --example usb3_configure -- <bit> [serial]
```

## Ten lines that work

```rust
use std::time::Duration;
use kdi::{Acquisition, ConnectOpts, Device, Filter, Kind, Stream};

let (found, _errs) = kdi::find(&Filter::default());
let mut dev = Device::open(&found[0], &ConnectOpts::default())?;   // version, ready, caps, lease
let mut rx = dev.start(Stream::Samples, &Acquisition::default())?; // stop → arm → start
while let Some(rec) = rx.next(Duration::from_secs(1))? {
    let b = rec.block(Kind::RhdMatrix)?.unwrap();
    println!("{} {:?} {}", rec.timestamp(), b.amplifier(7, 0), rec.lost_before());
}
println!("{:?}", rx.stats());
rx.stop()?;
```

Runnable versions, with the failure cases spelled out:

```text
cargo run --example stream     # acquisition: records, timestamps, loss
cargo run --example control    # bind, caps, typed commands, and a refusal
```

## Three rules the API is built around

- **A device error is DATA.** `Device::raw_cmd` returns `Ok(Reply)` for a well-framed reply with
  `rc != 0`; only a host or transport failure is `Err`. A caller can always tell "the device said
  `not_present`" from "the link died". The generated `Commands` methods are the one exception, and
  say why: they were asked for a value that a refusal does not contain.
- **A bad frame is COUNTED, never raised.** `StreamReader` resyncs past a rejected frame and records
  it in `Stats::bad_frames`. `Error` has no decode variant at all — a recording must not die on one
  flipped bit, and the missing frame still shows up in the next record's `lost_before()`.
- **Enumeration errors are RETURNED.** `find` hands back everything it could not enumerate, because
  "driver present but broken" must be distinguishable from "no board on the bench".

## What it deliberately does not do

No volts, no °C, no sample rate in Hz. **The timebase is normative, the rate is not** — 30 kS/s is
`10000/3` ticks per sample, and an integer 3333 drifts 1.44 s over a four-hour recording, so
`Block::cadence()` hands back the exact rational. Scales are a chip-profile property the contract
withholds on purpose: a profile swap is a major bump a host must refuse to bind, never silently
rescale.

There is no public `trait Transport`, no C ABI and no TCP. Each is deliberate: a trait with one
useful implementation freezes a shape before a second binding exists to argue with it, and the
property that actually matters — no endpoint number outside the binding module — a private enum
gives identically.

## Status

**The API is not frozen.** This is 0.x and the surface may change; `Cargo.toml`'s version tracks the
KDI contract version, and the crate asserts at compile time that the two agree, so a release can
never claim a contract it does not implement.

What is verified, and how: the data plane is exercised on real hardware across sample rates and lane
masks, bounded and free-running — roughly 6,600 probe-runs with zero failures, which bounds the
failure rate under 0.045% at 95% confidence. The command channel, the bind handshake and the flash
path are likewise exercised on silicon including their failure modes.

**One thing is not hardware-verified and you should know which:** the `rhd_matrix` row order.
Checking it on a bench needs a *driven* input; with floating inputs the amplifier rows and the slow
aux rows are both noise and the comparison decides nothing. The authority is a golden chip-model
simulation that drives known values, and the published order is that model's.

## Licence

Apache-2.0 — see `LICENSE`.

The bundled device drivers under `vendor/` are **not** covered by it: they are third-party binaries
redistributed with permission, and they statically embed Lua 5.3.1 (MIT). Both are attributed in
`NOTICE`, which you receive with this crate. `src/bundled.rs` records each blob's origin, length and
SHA-256, and is compiled in whether or not the `bundled` feature is on, so the provenance travels
with every copy.

## The contract

This crate implements a published contract you can read without it:
<https://github.com/Keyvast-Technology/kdi> — `spec/` holds the descriptor, JSON Schema, golden
vectors and a
manifest. A second implementation is built against that, not against this crate.
