#!/usr/bin/env python3
"""Ask the registry about one version of one crate, before and after an upload.

    crates_io.py absent <crate> <version>   exit 1 if that version is already on crates.io
    crates_io.py newest <crate> <version>   exit 1 unless it is published AND the highest

`absent` runs before packaging, because a crates.io version is permanent -- never deletable, only
yankable -- so the second upload of a number is not a retry, it is a refusal you want early.

`newest` is the check this repository exists to have: a publish step can exit 0 having uploaded
nothing, and nobody finds out until a customer reports the version is missing. It polls, because
the API trails the upload by seconds; only a timeout is a failure.

A yanked version still occupies its number, so `absent` counts yanked ones and `newest` does not.
"""
from __future__ import annotations
import json
import sys
import time
import urllib.error
import urllib.request

# crates.io rejects a request with no descriptive User-Agent, with a 403 that reads like an auth
# failure. Identify the caller and where to complain about it.
UA = "keyvast-kdi-publish (+https://github.com/Keyvast-Technology/kdi)"
API = "https://crates.io/api/v1/crates/{}"
PATIENCE_S = 180
POLL_S = 10


def poll(crate: str) -> tuple[set[str], set[str], str | None]:
    """(every number ever published, the unyanked ones, the registry's newest)."""
    req = urllib.request.Request(API.format(crate), headers={"User-Agent": UA})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            doc = json.load(r)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return set(), set(), None       # the name has never been published at all
        raise
    all_nums = {v["num"] for v in doc["versions"]}
    live = {v["num"] for v in doc["versions"] if not v["yanked"]}
    return all_nums, live, doc["crate"].get("max_version")


def main() -> int:
    if len(sys.argv) != 4 or sys.argv[1] not in ("absent", "newest"):
        print(__doc__)
        return 2
    mode, crate, version = sys.argv[1:4]

    if mode == "absent":
        all_nums, _, newest = poll(crate)
        if version in all_nums:
            print(f"FAIL: {crate} {version} is already on crates.io. A version is permanent, so "
                  "this cannot be re-uploaded -- move the crate version on and re-tag.")
            return 1
        print(f"{crate} {version} is free ({len(all_nums)} published, newest {newest or 'none'})")
        return 0

    deadline = time.time() + PATIENCE_S
    while True:
        all_nums, live, newest = poll(crate)
        if version in live and newest == version:
            print(f"{crate} {version} is on crates.io and is now the newest version")
            return 0
        if time.time() >= deadline:
            saw = "yanked" if version in all_nums else "absent"
            print(f"FAIL: {PATIENCE_S}s after the upload, crates.io reports {crate} {version} as "
                  f"{saw} and newest={newest or 'none'}. The publish did not take -- do NOT assume "
                  "it did; check https://crates.io/crates/" + crate)
            return 1
        time.sleep(POLL_S)


if __name__ == "__main__":
    sys.exit(main())
