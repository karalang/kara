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

/// The `std::fs::OpenOptions` for `OpenOptions.open`'s bits (read 1, write
/// 2, append 4, truncate 8, create 16, create_new 32), or `InvalidInput`
/// for a combination that opens nothing: no access mode, `truncate` /
/// `create` / `create_new` without `write` or `append`, or `create_new`
/// with `truncate` (docs/library/io.md § Files, B-2026-10-15-28). The
/// runtime's `karac_mir_file` checks the same.
pub fn open_options(flags: i64) -> io::Result<std::fs::OpenOptions> {
    let bit = |b: i64| flags & b != 0;
    let (read, write, append) = (bit(1), bit(2), bit(4));
    let (truncate, create, create_new) = (bit(8), bit(16), bit(32));
    let writes = write || append;
    if !(read || writes)
        || ((truncate || create || create_new) && !writes)
        || (create_new && truncate)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "these options open nothing",
        ));
    }
    let mut o = std::fs::OpenOptions::new();
    o.read(read)
        .write(write)
        .append(append)
        .truncate(truncate)
        .create(create)
        .create_new(create_new);
    Ok(o)
}
