//! The USB3 device driver this crate ships, its provenance, and the staging that makes it loadable.
//! `--features bundled` compiles the target platform's driver into the artifact; the bytes are in
//! this source either way (Cargo packages it all), and `tests/provenance.rs` hashes them.

/// Where one vendored blob came from, and what it must still be. Provenance for a binary shipped
/// inside a public crate: "some file that was on a PC" is not good enough, and neither is a claim
/// stronger than what was actually recorded — see [`Vendored::note`].
#[derive(Debug)]
pub struct Vendored {
    /// Path within this crate, relative to its manifest directory.
    pub file: &'static str,
    /// Where the bytes came from.
    pub source: &'static str,
    /// Lowercase hex SHA-256 of the file, asserted by `tests/provenance.rs`.
    pub sha256: &'static str,
    /// Byte length, asserted by the same test.
    pub len: usize,
    /// What is NOT known about it. Read this before quoting the row above as a chain of custody.
    pub note: &'static str,
}

/// The one release every vendored driver was extracted from: a single archived source this
/// organisation can re-fetch, each row's SHA-256 saying which bytes came out of it. It does not
/// name the board's maker (white-labelling, `tests/vendor_neutral.rs`); coordinates are internal.
const RELEASE: &str = "host API release 6.0.0 (2026-07-21), archived in this organisation's \
                       private hardware-support repository";

/// Every binary blob in this crate's source, with its provenance. See [`Vendored`].
pub const VENDORED: &[Vendored] = &[
    // The four drivers all come from ONE release we control, and each sha256 is the file as
    // extracted from that release's tarball. The Windows row also records that the copy on the
    // bench is byte-identical, which is what lets a bench result stand for the vendored blob.
    Vendored {
        file: "vendor/driver-x86_64-windows.bin",
        source: RELEASE,
        sha256: "51fd8539027e23600bfc6c9427e6f0a74f31f19046732caf39e850e5a09ec8f6",
        len: 2_117_704,
        note: "verified byte-identical to the copy installed on the instrument bench, so the \
               hardware results recorded against that machine are results for THIS blob",
    },
    // The oldest-glibc Linux build ON PURPOSE: a glibc floor is a compatibility CEILING.
    // Measured: the newest needs GLIBC_2.38, excluding Ubuntu 22.04, Debian 12 and RHEL 9; this
    // one needs 2.34, same size. Re-measure a newer build: max of the blob's `GLIBC_*` tags.
    Vendored {
        file: "vendor/driver-x86_64-linux.bin",
        source: RELEASE,
        sha256: "4033682b24d877ffa00e4efb9448541a6edf13f347eba6ea286d460ff9ef25f6",
        len: 2_328_536,
        note: "needs glibc >= 2.34, libstdc++ >= 3.4.30 and libudev at run time; glibc-only, so a \
               musl system (Alpine) cannot load it. NOT exercised against a board — the bench \
               workstation is Windows",
    },
    // aarch64 Linux, for ARM acquisition hosts. Upstream builds it on Raspbian 12, but the
    // LIBRARY's floor is what binds: glibc 2.34, the same as x86_64 above, so it reaches every
    // ARM distribution x86_64 reaches, not just Debian 12 and newer.
    Vendored {
        file: "vendor/driver-aarch64-linux.bin",
        source: RELEASE,
        sha256: "ef2afb3a3660d107fe94b3d307614a6b8a2a3fa467a7761d7afdea483e03a3dd",
        len: 2_350_768,
        note: "needs glibc >= 2.34 and libudev at run time; glibc-only, so a musl system (Alpine) \
               cannot load it. NOT exercised against a board",
    },
    // The arm64 slice of macOS's ONE fat library: the target embeds only its arch. The Intel
    // slice is gone: every blob is a redistribution obligation and audit surface for EVERY
    // consumer. An Intel Mac still loads a driver from disk; only `bundled` is out.
    Vendored {
        file: "vendor/driver-aarch64-macos.bin",
        source: "the arm64 slice of the universal library in the release below",
        sha256: "a1a4a16ccb18d5a68066d31b5608a82419f9ba939d3b44c3d30e5248f49b6607",
        len: 2_088_080,
        note: "sliced out of the universal binary, not shipped separately upstream; NOT exercised \
               against a board",
    },
];

// ─────────────────────────────────────────────────────── the bytes, under `--features bundled`

#[cfg(feature = "bundled")]
pub(crate) use embed::staged_path;

#[cfg(feature = "bundled")]
mod embed {
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static PART_SEQ: AtomicU64 = AtomicU64::new(0);

    /// The driver for the target platform.
    ///
    /// `include_bytes!`, so it is in the artifact's data, not read from disk at run time.
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    pub(crate) const DRIVER: &[u8] = include_bytes!("../vendor/driver-x86_64-windows.bin");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub(crate) const DRIVER: &[u8] = include_bytes!("../vendor/driver-x86_64-linux.bin");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    pub(crate) const DRIVER: &[u8] = include_bytes!("../vendor/driver-aarch64-linux.bin");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    pub(crate) const DRIVER: &[u8] = include_bytes!("../vendor/driver-aarch64-macos.bin");

    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    const DRIVER_FILE: &str = "vendor/driver-x86_64-windows.bin";
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    const DRIVER_FILE: &str = "vendor/driver-x86_64-linux.bin";
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    const DRIVER_FILE: &str = "vendor/driver-aarch64-linux.bin";
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    const DRIVER_FILE: &str = "vendor/driver-aarch64-macos.bin";

    // A target with no vendored driver fails HERE, at build time, naming what is missing — rather
    // than quietly behaving like a non-bundled build on that platform only. To vendor another:
    // extract it from the release in `VENDORED`, add the file, a `cfg` arm above and a row.
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    compile_error!(
        "`bundled` has no vendored device driver for this target. Vendored: Windows x86_64, \
         Linux x86_64 and aarch64, and macOS aarch64. Build without the `bundled` feature and \
         supply the driver in a directory ($KDI_DRIVER_DIR), or vendor the library for this \
         target - see src/bundled.rs."
    );

    /// The staged file's name. NEUTRAL ON PURPOSE: this is what lands in a customer's temp
    /// directory, and it is the one name in the whole scheme that we get to choose.
    #[cfg(target_os = "windows")]
    const STAGED_NAME: &str = "kdi_driver.dll";
    #[cfg(target_os = "macos")]
    const STAGED_NAME: &str = "libkdi_driver.dylib";
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    const STAGED_NAME: &str = "libkdi_driver.so";

    /// Write `DRIVER` under the system temp dir if it is not already there, and hand back the path.
    /// The directory is named for the driver's SHA-256, so a STALE COPY cannot be loaded, and the
    /// file is renamed into place; [`trusted_dir`] runs BEFORE the already-staged shortcut.
    pub(crate) fn staged_path() -> io::Result<PathBuf> {
        let sha16 = &super::VENDORED
            .iter()
            .find(|v| v.file == DRIVER_FILE)
            .expect("VENDORED has a row for this platform's driver")
            .sha256[..16];
        let dir = std::env::temp_dir().join(format!("kdi-driver-{sha16}"));
        let path = dir.join(STAGED_NAME);
        create_dir_private(&dir)?;
        trusted_dir(&dir)?;
        if is_staged(&path) {
            return Ok(path);
        }
        // Pid plus a counter: two threads in one process share a pid, and both writes must finish
        // before either is renamed.
        let n = PART_SEQ.fetch_add(1, Ordering::Relaxed);
        let part = dir.join(format!("{STAGED_NAME}.{}.{n}.part", std::process::id()));
        fs::write(&part, DRIVER)?;
        #[cfg(unix)]
        fs::set_permissions(
            &part,
            <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
        )?;
        // A rename onto an existing file fails on Windows, and "it already exists" is precisely the
        // race being handled — so the rename failing is only an error if what is there is not the
        // library we wanted.
        if let Err(e) = fs::rename(&part, &path) {
            let _ = fs::remove_file(&part);
            if !is_staged(&path) {
                return Err(io::Error::new(
                    e.kind(),
                    format!(
                        "could not stage the device driver at {}: {e}",
                        path.display()
                    ),
                ));
            }
        }
        Ok(path)
    }

    /// Create the staging directory, 0700 on unix so that nobody else can put a file in it.
    /// Already-exists is success — reuse across processes is the point of the hashed name — and
    /// [`trusted_dir`] is what decides whether an existing one may be used.
    fn create_dir_private(dir: &std::path::Path) -> io::Result<()> {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        <fs::DirBuilder as std::os::unix::fs::DirBuilderExt>::mode(&mut b, 0o700);
        b.create(dir)
    }

    /// Why a unix staging directory must not be used. Split out so the 0755-other-user case can
    /// be asserted without being able to chown.
    #[cfg(any(unix, test))]
    fn trust_reason(is_dir: bool, mode: u32, uid: u32, euid: u32) -> Option<&'static str> {
        if !is_dir {
            return Some("not a directory");
        }
        if mode & 0o022 != 0 {
            return Some("writable by others");
        }
        if uid != euid {
            return Some("owned by another user");
        }
        None
    }

    /// Refuse a staging directory another local user could have put a file into: group/other-
    /// writable, or a 0755 one THEY own — permissions alone are not decisive, the owner must be us.
    /// `symlink_metadata`, not `metadata`: a planted symlink would have the real dir checked.
    #[cfg(unix)]
    fn trusted_dir(dir: &std::path::Path) -> io::Result<()> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let md = fs::symlink_metadata(dir)?;
        let why = trust_reason(
            md.file_type().is_dir(),
            md.permissions().mode(),
            md.uid(),
            euid(),
        );
        if let Some(why) = why {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing to load the device driver from {}: it is {why} and so another user \
                     on this machine could choose what is loaded. Remove it, or point \
                     $KDI_DRIVER_DIR at a directory you control.",
                    dir.display(),
                ),
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn euid() -> u32 {
        extern "C" {
            fn geteuid() -> u32;
        }
        // SAFETY: geteuid is a POSIX libc call with no preconditions.
        unsafe { geteuid() }
    }

    /// Windows' temp directory is per-user, so there is nothing to check.
    #[cfg(not(unix))]
    fn trusted_dir(_dir: &std::path::Path) -> io::Result<()> {
        Ok(())
    }

    /// Length, not contents: the file is 2 MB and this runs on the way to every device open. The
    /// name is already a hash of the bytes and [`trusted_dir`] has required a 0700 directory we
    /// own, so length is the second half of a check whose first half is the directory.
    fn is_staged(path: &std::path::Path) -> bool {
        fs::metadata(path).is_ok_and(|m| m.len() == DRIVER.len() as u64)
    }

    /// The staging is a file-system dance with a race in it, and this is the check that it works:
    /// stage twice, get the same path, find the whole library both times. The second call is what
    /// exercises "already staged" — the branch that would otherwise only be hit in production.
    #[cfg(test)]
    mod tests {
        #[test]
        fn staging_is_idempotent_and_complete() {
            let a = super::staged_path().expect("stage the driver");
            let b = super::staged_path().expect("stage it again");
            assert_eq!(a, b);
            assert_eq!(
                std::fs::read(&b).expect("read it back").len(),
                super::DRIVER.len()
            );
            // The name a customer meets in their temp directory is ours, not anyone else's.
            let name = b.file_name().unwrap().to_string_lossy().to_lowercase();
            assert!(name.contains("kdi"), "staged as {name}");
            let dir = a.parent().unwrap().file_name().unwrap().to_string_lossy();
            assert!(
                dir.starts_with("kdi-driver-") && dir.len() == "kdi-driver-".len() + 16,
                "content-addressed dir, got {dir}"
            );
        }

        #[test]
        fn trust_reason_catches_mode_and_owner() {
            assert_eq!(
                super::trust_reason(false, 0o700, 1, 1),
                Some("not a directory")
            );
            assert_eq!(
                super::trust_reason(true, 0o777, 1, 1),
                Some("writable by others")
            );
            assert_eq!(
                super::trust_reason(true, 0o755, 2, 1),
                Some("owned by another user")
            );
            assert_eq!(super::trust_reason(true, 0o755, 1, 1), None);
            assert_eq!(super::trust_reason(true, 0o700, 1, 1), None);
        }

        /// The attack the unix hardening exists for: another local user gets there first and leaves
        /// a directory anyone can write to, so loading out of it loads their library. Asserted on a
        /// directory built here — the real path is a fixed hash and must not race itself.
        #[cfg(unix)]
        #[test]
        fn a_world_writable_staging_directory_is_refused() {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("kdi-trust-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("make the directory");

            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .expect("lock it down");
            super::trusted_dir(&dir).expect("a private directory is fine");

            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
                .expect("open it up");
            let err = super::trusted_dir(&dir).expect_err("world-writable must be refused");
            assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
            // The message has to tell the user what to do about it, not just that it failed.
            assert!(err.to_string().contains("KDI_DRIVER_DIR"), "{err}");

            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
