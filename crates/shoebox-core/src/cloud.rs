//! Files whose contents are in the cloud only: placeholders that Google
//! Drive (streaming), iCloud Drive (Optimise Mac Storage), Dropbox and
//! OneDrive (online-only) leave on disk. macOS marks them `SF_DATALESS`;
//! reading one downloads it first, so the scan reads none of them: they're
//! listed from their name, size and sidecars until they're downloaded
//! (opened, or Download from Cloud). Linux has no such flag.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Files tests treat as cloud-only (a real placeholder needs a cloud drive).
static PRETEND: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Is this file in the cloud only? `meta` from `symlink_metadata` (which
/// never downloads anything).
pub fn is_cloud_only(path: &Path, meta: &std::fs::Metadata) -> bool {
    dataless(meta)
        || PRETEND
            .lock()
            .ok()
            .is_some_and(|s| s.as_ref().is_some_and(|s| s.contains(path)))
}

#[cfg(target_os = "macos")]
fn dataless(meta: &std::fs::Metadata) -> bool {
    use std::os::macos::fs::MetadataExt;
    const SF_DATALESS: u32 = 0x4000_0000;
    meta.st_flags() & SF_DATALESS != 0
}

#[cfg(not(target_os = "macos"))]
fn dataless(_meta: &std::fs::Metadata) -> bool {
    false
}

/// [`is_cloud_only`] by path.
pub fn path_is_cloud_only(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| is_cloud_only(path, &m))
}

/// Download a cloud-only file by reading it through (the cloud drive
/// fetches it); nothing is written. Does nothing to a file already here.
pub fn download(path: &Path) -> std::io::Result<()> {
    download_within(path, DOWNLOAD_PATIENCE)
}

/// How long [`download`] keeps at it: a large raw over a slow line.
pub const DOWNLOAD_PATIENCE: std::time::Duration = std::time::Duration::from_secs(600);

/// A read the cloud drive gave up on for now, while it goes on fetching:
/// Google Drive answers a read it can't serve in time with `ETIMEDOUT`.
fn still_fetching(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::ETIMEDOUT)
        || matches!(
            e.kind(),
            std::io::ErrorKind::TimedOut
                | std::io::ErrorKind::Interrupted
                | std::io::ErrorKind::WouldBlock
        )
}

/// [`download`], trying again (from where it got to) while the drive says
/// it's still fetching, for up to `patience`.
pub fn download_within(path: &Path, patience: std::time::Duration) -> std::io::Result<()> {
    if !path_is_cloud_only(path) {
        return Ok(());
    }
    let deadline = std::time::Instant::now() + patience;
    let mut done: u64 = 0;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let r = (|| -> std::io::Result<()> {
            use std::io::{Seek, SeekFrom};
            let mut f = std::fs::File::open(path)?;
            f.seek(SeekFrom::Start(done))?;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    return Ok(());
                }
                done += n as u64;
            }
        })();
        match r {
            Ok(()) => break,
            Err(e) if still_fetching(&e) && std::time::Instant::now() < deadline => {
                tracing::info!("{}: still downloading ({e}); waiting", path.display());
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            Err(e) => return Err(e),
        }
    }
    if let Ok(mut s) = PRETEND.lock()
        && let Some(s) = s.as_mut()
    {
        s.remove(path);
    }
    Ok(())
}

/// Tests: treat `path` as cloud-only (until [`download`]ed).
#[doc(hidden)]
pub fn pretend_cloud_only(path: &Path) {
    if let Ok(mut s) = PRETEND.lock() {
        s.get_or_insert_with(HashSet::new)
            .insert(path.to_path_buf());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_timeout_is_waited_out_other_errors_are_not() {
        assert!(still_fetching(&std::io::Error::from_raw_os_error(
            libc::ETIMEDOUT
        )));
        assert!(!still_fetching(&std::io::Error::from_raw_os_error(
            libc::ENOENT
        )));
        assert!(!still_fetching(&std::io::Error::from_raw_os_error(
            libc::EACCES
        )));
    }

    #[test]
    fn pretend_and_download() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("a.jpg");
        std::fs::write(&p, b"x").unwrap();
        assert!(!path_is_cloud_only(&p));
        pretend_cloud_only(&p);
        assert!(path_is_cloud_only(&p));
        download(&p).unwrap();
        assert!(!path_is_cloud_only(&p));
        assert_eq!(std::fs::read(&p).unwrap(), b"x", "nothing written");
    }
}
