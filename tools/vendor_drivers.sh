#!/usr/bin/env bash
# Fetch the USB3 drivers `--features bundled` compiles in, and verify them against the provenance
# table in `src/bundled.rs` (file, source, sha256, length), which is the anchor rather than this
# script. The bytes must be in the PUBLISHED crate -- a consumer has no credential -- not in git.
set -euo pipefail

# CONFIGURED, NOT HARDCODED: this script is mirrored into the public repo, where naming the
# private sibling that holds the drivers tells a customer where private code lives.
REPO="${VENDOR_DRIVER_REPO:-${KV_DRIVER_REPO:-}}"
[ -n "$REPO" ] || { echo "set VENDOR_DRIVER_REPO to the owner/name holding the driver release" >&2; exit 2; }
TAG=frontpanel-6.0.0
# Destination is an ARGUMENT with the in-repo default, because this script is mirrored into the
# public repo where the layout differs -- a path computed from $0 is right in exactly one of
# the two places it now runs.
DEST="${1:-$(cd "$(dirname "$0")/.." && pwd)/kdi/rs/kdi/vendor}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# asset : path inside it : vendored name : optional macho arch to slice
SPECS=(
  "frontpanel-api-windows-x64.tar.gz|windows-x64/lib/x64/okFrontPanel.dll|driver-x86_64-windows.bin|"
  "frontpanel-api-rockylinux9-x64.tar.gz|rockylinux9-x64/libokFrontPanel.so.6.0.0|driver-x86_64-linux.bin|"
  "frontpanel-api-raspbian12-aarch64.tar.gz|raspbian12-aarch64/libokFrontPanel.so.6.0.0|driver-aarch64-linux.bin|"
  "frontpanel-api-macos-arm64.tar.gz|macos-arm64/libokFrontPanel.6.0.0.dylib|driver-aarch64-macos.bin|arm64"
)

mkdir -p "$DEST"
for spec in "${SPECS[@]}"; do
  IFS='|' read -r asset inner out arch <<<"$spec"
  [ -f "$WORK/$asset" ] || gh release download "$TAG" -R "$REPO" -p "$asset" -D "$WORK"
  [ -d "$WORK/x-$asset" ] || { mkdir -p "$WORK/x-$asset"; tar -xzf "$WORK/$asset" -C "$WORK/x-$asset" 2>/dev/null; }
  src="$WORK/x-$asset/$inner"
  if [ -n "$arch" ]; then
    # macOS ships ONE universal library; slice it so no build carries a library it cannot call.
    python3 - "$src" "$arch" "$DEST/$out" <<'PY'
import struct, sys, pathlib
fat = pathlib.Path(sys.argv[1]).read_bytes()
want = {"x86_64": 0x1000007, "arm64": 0x100000C}[sys.argv[2]]
assert struct.unpack(">I", fat[:4])[0] == 0xCAFEBABE, "not a 32-bit fat mach-o (FAT64 unsupported here)"
n = struct.unpack(">I", fat[4:8])[0]
for i in range(n):
    cpu, _sub, off, size, _al = struct.unpack(">iiIII", fat[8 + i * 20 : 28 + i * 20])
    if (cpu & 0xFFFFFFFF) == want:
        pathlib.Path(sys.argv[3]).write_bytes(fat[off : off + size]); sys.exit(0)
sys.exit(f"no {sys.argv[2]} slice in {sys.argv[1]}")
PY
  else
    cp "$src" "$DEST/$out"
  fi
done

# THE VERIFICATION, against the table in the source. A fetch that does not match is a fetch that
# must not be packaged.
python3 - "$DEST" <<'PY'
import hashlib, pathlib, re, sys
dest = pathlib.Path(sys.argv[1])
table = (dest.parent / "src/bundled.rs").read_text(encoding="utf-8")
rows = re.findall(r'file:\s*"vendor/([^"]+)".*?sha256:\s*"([0-9a-f]{64})".*?len:\s*([\d_]+)', table, re.S)
assert rows, "no provenance rows found in src/bundled.rs"
bad = []
for name, sha, ln in rows:
    p = dest / name
    if not p.is_file():
        bad.append(f"{name}: missing"); continue
    b = p.read_bytes()
    got = hashlib.sha256(b).hexdigest()
    if got != sha or len(b) != int(ln.replace("_", "")):
        bad.append(f"{name}: sha {got[:12]}… len {len(b)} != recorded {sha[:12]}… / {ln}")
if bad:
    sys.exit("vendored drivers do not match src/bundled.rs:\n  " + "\n  ".join(bad))
print(f"  {len(rows)} drivers fetched and verified against src/bundled.rs")
PY
