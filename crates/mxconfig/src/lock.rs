//! Version locks: which schema versions hold a namespace's file open.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

use nix::fcntl::{FcntlArg, fcntl};
use nix::libc;

use crate::error::Error;

/// The byte held exclusively while the file is written or migrated.
const WRITE: i64 = 0;

/// A version held on a lock file. The kernel drops it when the holder exits.
#[derive(Debug)]
pub(crate) struct Held {
    _file: File,
}

/// What holding a version on a lock file came to.
#[derive(Debug)]
pub(crate) enum Hold {
    Held(Held),
    /// Another version holds the file. Nothing was taken.
    Taken(u32),
}

/// Hold `version` on the lock file at `path`, unless another version holds it.
pub(crate) fn hold(path: &Path, version: u32) -> io::Result<Hold> {
    let file = open(path)?;
    let at = byte(version);
    set(&file, libc::F_RDLCK, at, 1, false)?;
    // Taken first, then checked, so two openers never both miss each other.
    for (start, len) in [(byte(0), at - byte(0)), (at + 1, 0)] {
        if let Some(other) = holder(&file, start, len)? {
            let other = u32::try_from(other - byte(0)).unwrap_or(u32::MAX);
            return Ok(Hold::Taken(other));
        }
    }
    Ok(Hold::Held(Held { _file: file }))
}

/// Run `body` while holding the write byte of the lock file at `path`.
pub(crate) fn exclusive<R>(
    path: &Path,
    body: impl FnOnce() -> Result<R, Error>,
) -> Result<R, Error> {
    let file = open(path).map_err(|e| Error::io(path, e))?;
    // Writes are short, so wait for one in progress.
    set(&file, libc::F_WRLCK, WRITE, 1, true).map_err(|e| Error::io(path, e))?;
    // Closing the file releases the byte.
    body()
}

/// The byte that stands for `version`. Byte 0 is for writes.
fn byte(version: u32) -> i64 {
    i64::from(version) + 1
}

/// Open the lock file, creating it if missing.
fn open(path: &Path) -> io::Result<File> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

/// A lock of `kind` over `len` bytes from `start`. A `len` of 0 runs to the end.
fn region(kind: libc::c_int, start: i64, len: i64) -> libc::flock {
    libc::flock {
        l_type: i16::try_from(kind).expect("a lock type fits"),
        l_whence: i16::try_from(libc::SEEK_SET).expect("SEEK_SET fits"),
        l_start: start,
        l_len: len,
        l_pid: 0,
    }
}

/// Take a lock on `file`, waiting for others if `wait`.
fn set(file: &File, kind: libc::c_int, start: i64, len: i64, wait: bool) -> io::Result<()> {
    let lock = region(kind, start, len);
    let arg = if wait {
        FcntlArg::F_OFD_SETLKW(&lock)
    } else {
        FcntlArg::F_OFD_SETLK(&lock)
    };
    fcntl(file, arg)?;
    Ok(())
}

/// The first byte another holder has locked in the region, if any.
fn holder(file: &File, start: i64, len: i64) -> io::Result<Option<i64>> {
    let mut lock = region(libc::F_WRLCK, start, len);
    fcntl(file, FcntlArg::F_OFD_GETLK(&mut lock))?;
    let free = i32::from(lock.l_type) == libc::F_UNLCK;
    Ok((!free).then_some(lock.l_start))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The version `result` holds, failing the test otherwise.
    fn must_hold(result: io::Result<Hold>) -> Held {
        match result.unwrap() {
            Hold::Held(version) => version,
            Hold::Taken(other) => panic!("taken by v{other}"),
        }
    }

    #[test]
    fn one_version_holds_and_others_are_turned_away() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml.lock");
        let first = must_hold(hold(&path, 2));
        let again = must_hold(hold(&path, 2));
        assert!(matches!(hold(&path, 1).unwrap(), Hold::Taken(2)));
        assert!(matches!(hold(&path, 3).unwrap(), Hold::Taken(2)));
        drop((first, again));
        let _older = must_hold(hold(&path, 1));
    }

    #[test]
    fn a_held_version_does_not_block_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml.lock");
        let _held = must_hold(hold(&path, 1));
        let wrote = exclusive(&path, || Ok(7)).unwrap();
        assert_eq!(wrote, 7);
    }
}
