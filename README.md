# Keyvast Device Interface (KDI)

The **contract** for the Keyvast acquisition instrument, and a **Rust host library** that
implements it.

| | |
|---|---|
| [`spec/`](spec/) | the contract — a machine-readable descriptor, its JSON Schema, golden vectors and a manifest. Everything a host needs, in any language, over any transport. |
| [`rs/`](rs/) | the Rust SDK, published as [`kdi`](https://crates.io/crates/kdi). Source only; the device drivers it bundles are fetched at build time, not tracked here. |

## Releases

Three artifacts on three schedules, so a tag says what actually moved:

| tag | what it is | changes when |
|---|---|---|
| `spec-v0.4.0` | the contract bundle — descriptor, schema, vectors, manifest | the **wire format** moves. Rarely, and deliberately |
| `gateware-v0.9.0` | a bitstream to flash, with the `gateware_sha` the board will report | a build is worth shipping to instruments |
| `v0.5.0` → [`kdi` on crates.io](https://crates.io/crates/kdi) | the Rust host library | the SDK changes. The tag is what publishes it |

One contract version outlives many gateware builds, which is why they are not cut together: a
contract release means the wire format changed, and saying that when it has not is worse than
saying nothing. Each gateware release names the contract version it implements.

**`spec/` does not depend on `rs/`, and that is the point.** The contract is what a second
implementation is built against — not this crate, and not any behaviour only this crate has. A
difference between the two is a bug in the contract, not a feature of Rust. `rs/` is here so the
published crate has a home, an issue tracker and readable history; it is a *consumer* of `spec/`
like any other host would be.

`rs/` is a **one-way mirror** of the source that builds the published crate, and this repository is
where that crate is **published from** — so what is on crates.io is by construction what you can see
here, at the commit that published it. Pull requests against `rs/` cannot be merged as-is; open them
anyway if you have found something, and say so — the fix lands upstream and appears here on the next
release.

The publish workflow is [`.github/workflows/publish.yml`](.github/workflows/publish.yml), readable
for the same reason: you should be able to see how the thing you depend on is built.

> **Status: v0.4.0 — format 2 is emitted by shipping gateware and verified on hardware.**
> The device emits format-2 frames on two independent streams and advertises the `clean_frame`
> capability, so a host that requires it will bind. The data plane is hardware-verified: ~6,600
> probe-runs across sample rates and lane masks, bounded and free-running, with zero failures —
> which bounds the failure rate under 0.045% at 95% confidence. The control plane, the command
> channel and the flash path are likewise exercised on silicon, including their failure modes.
>
> **One thing is NOT hardware-verified, and you should know which:** the `rhd_matrix` ROW ORDER.
> Checking it on a bench needs a *driven* input — with floating inputs both the amplifier rows and
> the slow aux rows are noise and the comparison decides nothing. The authority for row order is a
> golden chip-model simulation that drives known values through a modelled headstage. The order
> published here is that model's, and it is the one the gateware is built against.
>
> This is generated output; the source lives in a separate repo.

## What's here

| File | Purpose |
|------|---------|
| `spec/descriptor.json` | **the contract** — device identity, capabilities, the two register drawers, the sample-stream layout, and the public command set. |
| `spec/schema.json` | JSON Schema (draft 2020-12) — validate a descriptor without our code. |
| `spec/manifest.json` | sha256 of every artifact + contract/build identity + the conformance result. The only place hashes live. |
| `spec/vectors/*.bin` + `spec/vectors/golden_frame.json` | **six** frames and their expected decodes, a streaming case, **and 20 negative frames each with the exact reason token it must be rejected with** — so a decoder is testable with no hardware, in both directions. The set is deliberately wider than one case: at `rows == 1` a row-major and a lane-major decoder emit IDENTICAL bytes, so a single-frame oracle certifies a decoder that transposes `rhd_matrix`'s 35 rows against its lanes and emits plausible data at the wrong channel. `golden_wide` exists for the same reason in the other direction: it uses descriptor strides and element widths no current device emits, so a decoder that compiles those in is caught before a future device meets it. |
| `CHANGELOG.md` | one entry per contract version; wire-visible changes only. |

## How to consume it

A host binds to the device by **identity** (serial/board_id), reads the descriptor,
and speaks the contract via three primitives — `reg_read/write(name)`,
`stream(name)`, `message(request) → response` — resolving every name through the
descriptor's `bindings` block. It never hardcodes a physical endpoint.

1. Read `spec/descriptor.json` (validate against `spec/schema.json`).
2. Handshake: check `contract_version` major, poll `contract_ready`, require the
   capability bits you need.
3. Send typed commands from `commands`; read `streams.samples` and decode with
   `bytes_per_frame` (never a hand-rolled formula).

The command set is **command-first**; only two tiny register "drawers" (pre-boot
identity/caps + a CPU-bypass fast-path) are raw registers. Private
(service/factory) commands are **not** in this descriptor by construction.

## Versioning

`kdi` (major.minor) is the contract; a host checks the **major** and refuses on
mismatch, then branches on **capability bits**, never on a version range. See
`CHANGELOG.md`.
