//! The file system queries the interpreter's `fs_method` shares in shape
//! with the runtime's `karac_mir_fs` (runtime/src/mir.rs), so both
//! backends give the same answers (docs/library/io.md § Files,
//! B-2026-10-14-54, B-2026-10-15-59).

use std::io;

/// `fs.metadata(path)` (`follow`) or `fs.symlink_metadata(path)`: the size
/// in bytes, whether it is a file, a directory or a symbolic link, the
/// modification time in milliseconds since the Unix epoch, and the mode's
/// permission bits.
pub fn metadata(path: &str, follow: bool) -> io::Result<[i64; 6]> {
    let m = if follow {
        std::fs::metadata(path)?
    } else {
        std::fs::symlink_metadata(path)?
    };
    let ms = match m.modified()?.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    };
    Ok([
        m.len() as i64,
        m.is_file() as i64,
        m.is_dir() as i64,
        ms,
        m.file_type().is_symlink() as i64,
        mode(&m),
    ])
}

/// The permission bits of a mode (`0o7777`): the Unix mode, or on other
/// systems one made from the read-only flag (`0o666` / `0o444` for a file,
/// `0o777` / `0o555` for a directory).
fn mode(m: &std::fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        i64::from(m.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let all = if m.is_dir() { 0o777 } else { 0o666 };
        if m.permissions().readonly() {
            all & !0o222
        } else {
            all
        }
    }
}

/// `fs.set_permissions(path, mode)`: a mode outside `0..=0o7777` is
/// `InvalidInput`; on systems without Unix modes only the owner's write
/// bit counts, as the read-only flag. Follows symbolic links.
pub fn set_permissions(path: &str, mode: i64) -> io::Result<()> {
    if !(0..=0o7777).contains(&mode) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a mode is 0 to 0o7777",
        ));
    }
    #[cfg(unix)]
    let p = {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(mode as u32)
    };
    #[cfg(not(unix))]
    let p = {
        let mut p = std::fs::metadata(path)?.permissions();
        p.set_readonly(mode & 0o200 == 0);
        p
    };
    std::fs::set_permissions(path, p)
}

/// `fs.set_modified(path, ms)`: sets the modification time to `ms`
/// milliseconds since the Unix epoch (negative before it) and leaves the
/// access time. Follows symbolic links; the file is opened to set it, so
/// it must be readable (a directory, listable).
pub fn set_modified(path: &str, ms: i64) -> io::Result<()> {
    let d = std::time::Duration::from_millis(ms.unsigned_abs());
    let t = if ms >= 0 {
        std::time::UNIX_EPOCH.checked_add(d)
    } else {
        std::time::UNIX_EPOCH.checked_sub(d)
    }
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the time is out of range"))?;
    open_for_times(path)?.set_modified(t)
}

#[cfg(windows)]
fn open_for_times(path: &str) -> io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_WRITE_ATTRIBUTES, and FILE_FLAG_BACKUP_SEMANTICS so a directory
    // opens too.
    std::fs::OpenOptions::new()
        .access_mode(0x100)
        .custom_flags(0x0200_0000)
        .open(path)
}

#[cfg(not(windows))]
fn open_for_times(path: &str) -> io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// `fs.symlink(target, link)`: makes `link` a symbolic link to `target`,
/// which is stored as given (a relative target is relative to the link's
/// directory) and need not exist. On Windows the link is a directory link
/// when the target is a directory; elsewhere without links it is
/// `Unsupported`.
pub fn symlink(target: &str, link: &str) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
    }
    #[cfg(windows)]
    {
        let at = std::path::Path::new(link)
            .parent()
            .unwrap_or(std::path::Path::new(""))
            .join(target);
        if at.is_dir() {
            std::os::windows::fs::symlink_dir(target, link)
        } else {
            std::os::windows::fs::symlink_file(target, link)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "symbolic links are not supported here",
        ))
    }
}

/// `fs.read_link(path)`: the target a symbolic link stores; a target that
/// is not UTF-8 is an `InvalidData` error.
pub fn read_link(path: &str) -> io::Result<String> {
    std::fs::read_link(path)?
        .into_os_string()
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a link target is not UTF-8"))
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
