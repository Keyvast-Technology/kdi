# AGENTS.md

**This is a distribution repository, not a development one.** Most of what you see was generated
elsewhere and copied here in one direction. Read this before you edit anything; the failure mode is
a commit that vanishes without a conflict, which looks exactly like a commit that was never made.

`CONTRIBUTING.md` says the same thing to humans, shorter. This file is the one with the mechanics.

## Never hand-commit to a mirrored path

| Path | Written by | Your commit there |
|---|---|---|
| `rs/` | the release sync, which does `rm -rf rs/` before it copies | **deleted**, silently, at the next release |
| `spec/` | the contract publication, file by file | overwritten at the next contract version |
| `tools/check_publishable.py`, `tools/vendor_drivers.sh` | the same sync | overwritten |
| `README.md`, `CHANGELOG.md`, `CONTRIBUTING.md`, `AGENTS.md`, `LICENSE`, `NOTICE`, `.github/` | people, here | normal — this is where you work |

A fix to `rs/` or `spec/` lands in the private build repo and appears here on the following release.
There is no route from here back to it: a mirror anyone can push to becomes a second source of
truth, and then nobody knows which copy the device was built against.

**Nothing committed here may name the board vendor, a site, a specific instrument's serial number,
or the private build repo.** `tools/check_publishable.py` holds the exact patterns and enforces them
on the crate tarball. Prose files are not in the tarball, so on those the rule is enforced by you.

## How a version is published

One act: a maintainer pushes an annotated `vX.Y.Z` tag. That fires
[`.github/workflows/publish.yml`](.github/workflows/publish.yml), which uploads the `kdi` crate to
crates.io. Nothing else uploads — not a merge to `main`, not a manual dispatch.

```sh
git tag -a v0.5.0 -m "kdi 0.5.0"   # the version must already be the one in rs/Cargo.toml
git push origin v0.5.0
```

Rehearse first. `gh workflow run publish.yml --ref v0.5.0` (or `--ref main`) runs **every** check
below, packages, gates the tarball and does a `--dry-run` publish. It cannot upload: the upload
steps are gated on the event being a tag push, so there is no flag to get wrong.

A crates.io version is permanent — never deletable, only yankable, and its files are served
forever. That is why the tag is deliberate, why it validates rather than rewrites, and why the last
step asks the registry whether the upload actually happened instead of believing a zero exit.

## What the tag validates

It **never rewrites**. Every disagreement below is a refusal, and the fix is the tag or the source,
never the workflow.

1. The tag is `vX.Y.Z` with three bare numeric parts. `spec-v*` and `gateware-v*` do not publish a
   crate and are refused by name if one reaches this workflow.
2. `X.Y.Z` equals the crate's declared version in `rs/Cargo.toml`.
3. `X.Y` equals `KDI_VERSION`, the generated contract version in `rs/kdi/src/codec/spec.rs`. The
   crate version tracks the **contract**, not this repository's history; only the patch is free.
   `rs/kdi/src/codec/mod.rs` already asserts this at compile time — the workflow check is the
   readable early failure for the same disagreement.
4. crates.io has never served `X.Y.Z`, checked **before** packaging. A yanked version still occupies
   its number.
5. The packaged tarball passes `tools/check_publishable.py` — no test tree, licence obligations
   shipped, no identifying strings, README consistent with the manifest.
6. `git status --porcelain` is empty for the **whole repository**, checked immediately before
   packaging. A crate published from a dirty tree cannot be reproduced from the repository link it
   ships, and that link is the only provenance a customer gets. Cargo's own dirty check is not that
   check: it looks only at `rs/kdi`, so it passes over an uncommitted `README.md`, and it refuses
   over the four `include`d, gitignored driver binaries — which is why `--allow-dirty` is still on
   the `cargo package` line and why the step above it is the real gate. Those four are hash-verified
   against `src/bundled.rs`, which ships in the tarball, so a consumer can check the bytes they got.
7. **After** the upload, crates.io serves `X.Y.Z` and reports it as the newest version. A publish
   believed to have happened that had not is a failure this project has already paid for, and a
   zero exit is not evidence against it.

## The three tag families

| Tag | Artifact | Cut by | Moves when |
|---|---|---|---|
| `vX.Y.Z` | the `kdi` crate on crates.io | a maintainer, by hand | the SDK changes. `X.Y` is the contract version; `Z` is the SDK's own patch |
| `spec-vX.Y.0` | the contract bundle in `spec/` — descriptor, schema, vectors, manifest | the contract publication | the **wire format** moves. Rarely, and deliberately |
| `gateware-vX.Y.Z` | a bitstream to flash, with the `gateware_sha` the board reports | the gateware publication | a build is worth shipping to instruments |

They are separate because one contract version outlives many gateware builds, and saying the wire
format changed when it has not is worse than saying nothing.

**Creating a `vX.Y.Z` tag is restricted to maintainers** by a repository ruleset ("Crate release
tags (v\*) are maintainer-only", target `tag`, pattern `refs/tags/v*`, rule `creation`); org owners
and repository admins bypass it, nobody else. It does not match `spec-v*` or `gateware-v*`, which
are created by automation.
