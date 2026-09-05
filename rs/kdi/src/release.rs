//! The release manifest a bitstream is published with, and the judgement a host makes against an
//! image BEFORE a byte reaches the board: these bytes are the image this manifest describes, and
//! its contract major is one this host can bind to (ADR-0001: compatibility is major-equality).

use std::io;

use serde_json::Value;

use crate::{io_err, Error, Skew, KDI_MAJOR};

/// What a publication attaches beside a bitstream: enough for a program to decide, offline,
/// whether to flash it. Built by `tools/gateware_manifest.py` from the contract at the release
/// tag's own commit — the version is derived, never typed.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct ReleaseManifest {
    /// The published image's file name, which carries the contract version as `-kdiX.Y`.
    pub file: String,
    /// Where that file is served from.
    pub url: String,
    /// Its length in bytes.
    pub size: u64,
    /// Its SHA-256, lower-case hex. [`ReleaseManifest::judge`] checks the image against this and
    /// refuses on a mismatch: a manifest describes ONE image, and a name is not an identity.
    pub sha256: String,
    /// `(major, minor)` of the contract that image implements.
    pub contract: (u16, u16),
    /// The identity the board will report once flashed — `Device::gateware_sha` in hex, the first
    /// 8 of the commit the image was built from. Compare it after flashing.
    pub gateware_sha: String,
}

impl ReleaseManifest {
    /// Parse a published manifest. [`Error::Io`] with `InvalidData` when it is not JSON, is
    /// missing a field, or carries a contract that is not `major.minor` — a manifest this host
    /// cannot fully read is refused rather than half-believed.
    pub fn parse(json: &str) -> Result<ReleaseManifest, Error> {
        let v: Value = serde_json::from_str(json).map_err(|e| bad(&format!("not JSON: {e}")))?;
        let contract = text(&v, "contract")?;
        let (major, minor) = contract
            .split_once('.')
            .ok_or_else(|| bad(&format!("contract \"{contract}\" is not major.minor")))?;
        let num = |s: &str| {
            s.parse::<u16>()
                .map_err(|_| bad(&format!("contract \"{contract}\" is not major.minor")))
        };
        Ok(ReleaseManifest {
            file: text(&v, "file")?,
            url: text(&v, "url")?,
            size: v["size"]
                .as_u64()
                .ok_or_else(|| bad("\"size\" is missing or not a number"))?,
            sha256: text(&v, "sha256")?,
            contract: (num(major)?, num(minor)?),
            gateware_sha: text(&v, "gateware_sha")?,
        })
    }

    /// Judge `image` against this manifest, returning the contract version it implements.
    ///
    /// The BYTES are checked first: what the manifest says about the contract is a claim about one
    /// image, so believing it before identifying the image is backwards. Then the major, by
    /// equality ([`Skew::Major`]) — the same rule the bind handshake applies to a live board.
    pub fn judge(&self, image: &[u8]) -> Result<(u16, u16), Error> {
        let got = sha256_hex(image);
        if got != self.sha256.to_ascii_lowercase() {
            return Err(bad(&format!(
                "these bytes are not the image this manifest describes ({}): sha256 {got}, \
                 manifest says {} - re-download it",
                self.file, self.sha256
            )));
        }
        if self.contract.0 != KDI_MAJOR {
            return Err(Error::Skew(Skew::Major {
                device: self.contract.0,
                host: KDI_MAJOR,
            }));
        }
        Ok(self.contract)
    }
}

impl crate::Device {
    /// Judge `image` against `manifest`, then load it and bind — the composed call, so a host
    /// cannot flash first and discover the skew afterwards. Everything
    /// [`Device::open_usb3_configured`](crate::Device::open_usb3_configured) says still applies:
    /// volatile, and STATE-CHANGING on a shared board.
    #[cfg(feature = "usb3")]
    #[cfg_attr(docsrs, doc(cfg(feature = "usb3")))]
    pub fn open_usb3_from_manifest(
        serial: &str,
        image: &[u8],
        manifest: &ReleaseManifest,
    ) -> Result<crate::Device, Error> {
        manifest.judge(image)?;
        crate::Device::open_usb3_configured(serial, image)
    }
}

fn bad(msg: &str) -> Error {
    io_err(
        io::ErrorKind::InvalidData,
        format!("release manifest: {msg}"),
    )
}

fn text(v: &Value, key: &str) -> Result<String, Error> {
    v[key]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| bad(&format!("\"{key}\" is missing or not a string")))
}

// FIPS 180-4, ~50 lines, because the alternative is a dependency in a crate that is published to
// customers — a decision, not a convenience. Its vectors are in tests/release_manifest.rs.
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut blocks = data.chunks_exact(64);
    for b in &mut blocks {
        compress(&mut h, b);
    }
    // The tail padded in place: 0x80, zeros, then the length in BITS. Two blocks when the
    // remainder leaves no room for the 9 trailing bytes — the 56..64 case.
    let rest = blocks.remainder();
    let mut tail = [0u8; 128];
    tail[..rest.len()].copy_from_slice(rest);
    tail[rest.len()] = 0x80;
    let n = if rest.len() + 9 <= 64 { 64 } else { 128 };
    tail[n - 8..n].copy_from_slice(&(data.len() as u64 * 8).to_be_bytes());
    for b in tail[..n].chunks_exact(64) {
        compress(&mut h, b);
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

fn compress(h: &mut [u32; 8], block: &[u8]) {
    let mut w = [0u32; 64];
    for (i, c) in block.chunks_exact(4).enumerate() {
        w[i] = u32::from_be_bytes(c.try_into().expect("4 bytes"));
    }
    for i in 16..64 {
        let (a, b) = (w[i - 15], w[i - 2]);
        let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
        let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let mut v = *h;
    for (k, wi) in K.iter().zip(w.iter()) {
        let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
        let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
        let t1 = v[7]
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(*k)
            .wrapping_add(*wi);
        let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
        let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        v = [
            t1.wrapping_add(s0.wrapping_add(maj)),
            v[0],
            v[1],
            v[2],
            v[3].wrapping_add(t1),
            v[4],
            v[5],
            v[6],
        ];
    }
    for (a, b) in h.iter_mut().zip(v) {
        *a = a.wrapping_add(b);
    }
}
