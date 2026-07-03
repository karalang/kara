//! Extraction of fetched registry tarballs into a source directory.
//!
//! Slice 2 of the registry fetch epic (phase-5-diagnostics.md resolver
//! follow-up (b)). This decouples "get the bytes" ([`crate::registry_proxy`]
//! `fetch_registry_package`) from "put them on disk so the compiler can
//! read the package". Slice 3 wires the two together in the resolver.
//!
//! The proxy serves each package version as a gzip-compressed tarball
//! (`application/gzip`, `Karac-Content-Hash: blake3:<hex>`). Here we
//! decompress (`flate2`) and unpack (`tar`) into a destination directory.
//! Unpacking uses the `tar` crate's default protection: entries whose
//! resolved path would escape the destination (`../…`, absolute paths,
//! escaping symlinks) are refused rather than written outside — the
//! zip-slip guard a package manager must have.

use std::path::Path;

/// Failure extracting a fetched tarball.
#[derive(Debug)]
pub struct ExtractError {
    message: String,
}

impl ExtractError {
    fn new(message: impl Into<String>) -> Self {
        ExtractError {
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        "E_REGISTRY_EXTRACT_FAILED"
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ExtractError {}

/// Extract a gzip-compressed tarball (`gz_bytes`) into `dest`, creating
/// `dest` if needed. The archive's own directory structure is mirrored
/// under `dest` (e.g. an entry `mylib/src/lib.kara` lands at
/// `dest/mylib/src/lib.kara`).
///
/// Path-traversal safety: unpacking refuses any entry resolving outside
/// `dest`, so a malicious archive cannot write elsewhere on disk.
pub fn extract_tarball(gz_bytes: &[u8], dest: &Path) -> Result<(), ExtractError> {
    std::fs::create_dir_all(dest)
        .map_err(|e| ExtractError::new(format!("could not create {}: {e}", dest.display())))?;

    let decoder = flate2::read::GzDecoder::new(gz_bytes);
    let mut archive = tar::Archive::new(decoder);
    // Don't restore ownership/permissions from the archive — a mirror's
    // uid/gid/mode bits are meaningless on the fetching machine, and
    // honoring them risks writing unreadable or setuid files.
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    archive
        .unpack(dest)
        .map_err(|e| ExtractError::new(format!("could not extract tarball: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    fn temp_dir() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("kara-extract-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a gzip-compressed tarball in memory from `(path, contents)`
    /// entries, using the same `tar` + `flate2` crates the extractor reads.
    fn make_targz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        for (path, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        let gz = builder.into_inner().unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_nested_files_with_content() {
        let targz = make_targz(&[
            ("mylib/kara.toml", b"[package]\nname = \"mylib\"\n"),
            ("mylib/src/lib.kara", b"pub fn hi() -> i64 { 42 }\n"),
        ]);
        let dest = temp_dir();
        extract_tarball(&targz, &dest).expect("extract");

        assert_eq!(
            std::fs::read_to_string(dest.join("mylib/kara.toml")).unwrap(),
            "[package]\nname = \"mylib\"\n"
        );
        assert_eq!(
            std::fs::read_to_string(dest.join("mylib/src/lib.kara")).unwrap(),
            "pub fn hi() -> i64 { 42 }\n"
        );
    }

    #[test]
    fn creates_missing_destination() {
        let targz = make_targz(&[("a.txt", b"x")]);
        let dest = temp_dir().join("does/not/exist/yet");
        extract_tarball(&targz, &dest).expect("extract into fresh dir");
        assert_eq!(std::fs::read_to_string(dest.join("a.txt")).unwrap(), "x");
    }

    #[test]
    fn garbage_bytes_are_an_error_not_a_panic() {
        let dest = temp_dir();
        let err = extract_tarball(b"this is not a gzip stream at all", &dest).unwrap_err();
        assert_eq!(err.code(), "E_REGISTRY_EXTRACT_FAILED");
    }

    #[test]
    fn traversal_entry_does_not_escape_destination() {
        // Hand-craft a tar entry named "../escape.txt" (bypassing the
        // builder's path validation) to prove the unpack guard refuses it.
        let mut header = tar::Header::new_gnu();
        let body = b"pwned";
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        // set_path rejects "..", so write the name straight into the header.
        let name = b"../escape.txt";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();

        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            builder.append(&header, &body[..]).unwrap();
            builder.finish().unwrap();
        }
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&tar_bytes).unwrap();
        let targz = gz.finish().unwrap();

        let parent = temp_dir();
        let dest = parent.join("pkg");
        // Extraction succeeds (the guard skips the bad entry) but nothing is
        // written outside `dest`.
        let _ = extract_tarball(&targz, &dest);
        assert!(
            !parent.join("escape.txt").exists(),
            "traversal entry escaped the destination directory"
        );
    }
}
