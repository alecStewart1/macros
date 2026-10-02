//! Durable, ordered buffer saves.
//!
//! Every buffer write goes through [`write_sequenced`]:
//!
//! - **Atomic**: the text goes to a temp file in the target's directory, is
//!   fsynced, takes over the original's permissions, and is renamed over the
//!   target — a crash or full disk mid-save leaves the old file intact rather
//!   than a truncated one.
//! - **Symlink-preserving**: a symlinked path is resolved first, so the link's
//!   target is rewritten and the link itself stays a link.
//! - **Ordered per file**: each save takes a [`SaveTicket`] (a global,
//!   monotonic generation) on the UI thread when it snapshots the text. Writes
//!   to one file are serialized, and a ticket older than one already written
//!   is dropped as [`SaveOutcome::Superseded`] — so a slow earlier async save
//!   can never land on top of a newer one.
//!
//! Where an atomic replace would change more than the file's bytes, the write
//! falls back to rewriting in place (as Emacs does): a hard-linked file (rename
//! would split the links), a file owned by another user (rename would steal
//! ownership), or a directory we can't create the temp file in.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// A claim on the next write of one file, taken when the text is snapshotted.
#[derive(Debug, Clone)]
pub struct SaveTicket {
    key: PathBuf,
    generation: u64,
}

/// What [`write_sequenced`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveOutcome {
    Written,
    /// A newer save of the same file already landed; this (older) text was
    /// deliberately not written.
    Superseded,
}

/// Per-file sequencing state: the newest ticket issued, and (behind its own
/// lock, held for the duration of a write) the newest generation written.
#[derive(Default)]
struct FileSeq {
    issued: u64,
    written: Arc<Mutex<u64>>,
}

static GENERATION: AtomicU64 = AtomicU64::new(0);

fn files() -> &'static Mutex<HashMap<PathBuf, FileSeq>> {
    static FILES: OnceLock<Mutex<HashMap<PathBuf, FileSeq>>> = OnceLock::new();
    FILES.get_or_init(Default::default)
}

/// The file a write to `path` must actually modify: `path` with any chain of
/// symlinks followed (a dangling link resolves to the file it would create),
/// made absolute so two spellings of one file share a sequence.
pub fn write_target(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    // Bounded, like the kernel's ELOOP limit, so a link cycle can't spin.
    for _ in 0..40 {
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => match fs::read_link(&current) {
                Ok(link) if link.is_absolute() => current = link,
                Ok(link) => {
                    current = current
                        .parent()
                        .map_or_else(|| link.clone(), |dir| dir.join(&link))
                }
                Err(_) => break,
            },
            _ => break,
        }
    }
    std::path::absolute(&current).unwrap_or(current)
}

/// Take the next save ticket for `path`. Call on the UI thread at the moment
/// the buffer text is captured, so ticket order is edit order.
pub fn ticket(path: &Path) -> SaveTicket {
    let key = write_target(path);
    let generation = GENERATION.fetch_add(1, Ordering::Relaxed) + 1;
    files()
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_default()
        .issued = generation;
    SaveTicket { key, generation }
}

/// Whether `ticket` is still the newest save requested for its file — i.e.
/// whether its failure should mark the buffer modified again (a newer save
/// that is in flight or done owns the buffer's saved state otherwise).
pub fn is_latest(ticket: &SaveTicket) -> bool {
    files()
        .lock()
        .unwrap()
        .get(&ticket.key)
        .is_none_or(|f| f.issued == ticket.generation)
}

/// Write `contents` to `path` atomically, in ticket order (see the module doc).
pub fn write_sequenced(
    ticket: &SaveTicket,
    path: &Path,
    contents: &[u8],
) -> io::Result<SaveOutcome> {
    let written = files()
        .lock()
        .unwrap()
        .entry(ticket.key.clone())
        .or_default()
        .written
        .clone();
    let mut last = written.lock().unwrap_or_else(|e| e.into_inner());
    if *last > ticket.generation {
        return Ok(SaveOutcome::Superseded);
    }
    atomic_write(path, contents)?;
    *last = ticket.generation;
    Ok(SaveOutcome::Written)
}

/// One-shot convenience for synchronous saves: take a ticket and write now.
pub fn write_now(path: &Path, contents: &[u8]) -> io::Result<()> {
    write_sequenced(&ticket(path), path, contents).map(|_| ())
}

/// Replace `path`'s contents atomically (temp file + fsync + rename), writing
/// through symlinks and keeping the original's permissions.
pub fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let target = write_target(path);
    let existing = fs::metadata(&target).ok();
    if let Some(meta) = &existing {
        // The rename would silently replace a file we aren't allowed to write
        // (the directory's permissions are what govern a rename). Refuse the
        // same way a plain write would.
        if !writable(&target) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not writable", target.display()),
            ));
        }
        if must_write_in_place(meta) {
            return write_in_place(&target, contents);
        }
    }
    let dir = match target.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let (mut file, tmp) = match create_temp(dir, &name.to_string_lossy()) {
        Ok(created) => created,
        // Can't create a sibling (unwritable directory) but the file itself
        // may still be writable — degrade to a plain in-place write.
        Err(err) if err.kind() == io::ErrorKind::PermissionDenied && existing.is_some() => {
            return write_in_place(&target, contents);
        }
        Err(err) => return Err(err),
    };
    let result = (|| {
        file.write_all(contents)?;
        if let Some(meta) = &existing {
            file.set_permissions(meta.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &target)
    })();
    if let Err(err) = result {
        let _ = fs::remove_file(&tmp);
        return Err(err);
    }
    // Make the rename itself durable (best effort; not all filesystems allow
    // fsync on a directory handle).
    if let Ok(dir) = File::open(dir) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Create a fresh, exclusively-owned temp file next to the target.
fn create_temp(dir: &Path, name: &str) -> io::Result<(File, PathBuf)> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    loop {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{name}.macros-save-{}-{n}~", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(file) => return Ok((file, tmp)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }
}

/// Rewrite in place (truncate + write + fsync): keeps the inode, so hard links
/// and ownership survive, at the cost of atomicity.
fn write_in_place(target: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .create(true)
        .open(target)?;
    file.write_all(contents)?;
    file.sync_all()
}

#[cfg(unix)]
fn must_write_in_place(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    meta.nlink() > 1 || meta.uid() != euid
}

#[cfg(not(unix))]
fn must_write_in_place(_meta: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn writable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string for the call's duration.
    unsafe { libc::access(c_path.as_ptr(), libc::W_OK) == 0 }
}

#[cfg(not(unix))]
fn writable(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| !m.permissions().readonly())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "macros-save-test-{tag}-{}-{}",
            std::process::id(),
            GENERATION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn no_temp_files_left(dir: &Path) {
        let leftovers: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("macros-save-"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }

    #[test]
    fn atomic_write_creates_replaces_and_keeps_permissions() {
        let dir = scratch_dir("perm");
        let file = dir.join("a.txt");
        atomic_write(&file, b"one").unwrap(); // new file
        assert_eq!(fs::read_to_string(&file).unwrap(), "one");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o750)).unwrap();
            atomic_write(&file, b"two").unwrap();
            let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o750);
        }
        assert_eq!(fs::read_to_string(&file).unwrap(), "two");
        no_temp_files_left(&dir);
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_written_through_not_replaced() {
        let dir = scratch_dir("link");
        let real = dir.join("real.txt");
        fs::write(&real, "old").unwrap();
        let link = dir.join("link.txt");
        std::os::unix::fs::symlink("real.txt", &link).unwrap(); // relative link
        atomic_write(&link, b"new").unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&real).unwrap(), "new");
        // A dangling link creates its target.
        let dangling = dir.join("dangling.txt");
        std::os::unix::fs::symlink("made.txt", &dangling).unwrap();
        atomic_write(&dangling, b"hi").unwrap();
        assert_eq!(fs::read_to_string(dir.join("made.txt")).unwrap(), "hi");
        assert!(
            fs::symlink_metadata(&dangling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_stay_linked() {
        let dir = scratch_dir("hard");
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        fs::write(&a, "old").unwrap();
        fs::hard_link(&a, &b).unwrap();
        atomic_write(&a, b"new").unwrap();
        assert_eq!(fs::read_to_string(&b).unwrap(), "new");
        fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn read_only_files_are_not_silently_replaced() {
        use std::os::unix::fs::PermissionsExt;
        // Root bypasses file mode bits (access(W_OK) succeeds on a 0444 file),
        // so the write legitimately goes through — e.g. CI running as root.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let dir = scratch_dir("ro");
        let file = dir.join("ro.txt");
        fs::write(&file, "keep").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o444)).unwrap();
        let err = atomic_write(&file, b"clobber").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep");
        no_temp_files_left(&dir);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_older_save_never_lands_after_a_newer_one() {
        let dir = scratch_dir("seq");
        let file = dir.join("f.txt");
        let old = ticket(&file);
        let new = ticket(&file);
        assert!(!is_latest(&old));
        assert!(is_latest(&new));
        // The newer write lands first (the older one was slow to start)...
        assert_eq!(
            write_sequenced(&new, &file, b"new").unwrap(),
            SaveOutcome::Written
        );
        // ...so the older one is dropped instead of clobbering it.
        assert_eq!(
            write_sequenced(&old, &file, b"old").unwrap(),
            SaveOutcome::Superseded
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        // In-order writes both land.
        let a = ticket(&file);
        let b = ticket(&file);
        write_sequenced(&a, &file, b"a").unwrap();
        assert_eq!(
            write_sequenced(&b, &file, b"b").unwrap(),
            SaveOutcome::Written
        );
        assert_eq!(fs::read_to_string(&file).unwrap(), "b");
        fs::remove_dir_all(&dir).ok();
    }
}
