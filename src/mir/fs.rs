//! The file system queries the interpreter's `fs_method` shares in shape
//! with the runtime's `karac_mir_fs` (runtime/src/mir.rs), so both
//! backends give the same answers (docs/library/io.md § Files,
//! B-2026-10-14-54).

use std::io;

/// `fs.metadata(path)`: the size in bytes, whether it is a file, whether
/// it is a directory, and the modification time in milliseconds since the
/// Unix epoch. Follows symbolic links.
pub fn metadata(path: &str) -> io::Result<[i64; 4]> {
    let m = std::fs::metadata(path)?;
    let ms = match m.modified()?.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    };
    Ok([m.len() as i64, m.is_file() as i64, m.is_dir() as i64, ms])
}

/// `fs.read_dir(path)`: the entry names, sorted byte-wise; a name that is
/// not UTF-8 is an `InvalidData` error.
pub fn read_dir(path: &str) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a file name is not UTF-8"))?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}
