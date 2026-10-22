//! Thin FFI wrappers. The only place `unsafe` is allowed.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

/// Rename `src` to `dst`, failing with `AlreadyExists` if `dst` exists.
///
/// The no-overwrite check and the rename are one atomic step in the kernel
/// (`renamex_np(RENAME_EXCL)` on macOS, `renameat2(RENAME_NOREPLACE)` on
/// Linux): there is no window in which another process can create `dst` and
/// have it replaced. Platforms without such a call get an error, never a
/// plain (overwriting) rename.
pub fn rename_excl(src: &Path, dst: &Path) -> io::Result<()> {
    let (s, d) = (cstr(src)?, cstr(dst)?);
    #[cfg(target_os = "macos")]
    // SAFETY: both pointers are valid NUL-terminated strings that outlive the call.
    let rc = unsafe { libc::renamex_np(s.as_ptr(), d.as_ptr(), libc::RENAME_EXCL) };
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: as above; AT_FDCWD makes both paths relative to the cwd (they're absolute).
    let rc = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            s.as_ptr(),
            libc::AT_FDCWD,
            d.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    let rc: libc::c_int = {
        let _ = (&s, &d);
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no atomic no-overwrite rename on this platform",
        ));
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Bytes free for an unprivileged writer on the volume holding `path`.
pub fn free_bytes(path: &Path) -> io::Result<u64> {
    let p = cstr(path)?;
    // SAFETY: an all-zero statvfs is a valid value for the out parameter.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `p` is a valid NUL-terminated string and `st` a valid out pointer.
    let rc = unsafe { libc::statvfs(p.as_ptr(), &mut st) };
    if rc == 0 {
        Ok(u64::from(st.f_bavail).saturating_mul(st.f_frsize))
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_space_is_known() {
        let d = tempfile::tempdir().unwrap();
        assert!(free_bytes(d.path()).unwrap() > 0);
    }

    #[test]
    fn refuses_to_overwrite() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&a, "a").unwrap();
        std::fs::write(&b, "b").unwrap();
        let err = rename_excl(&a, &b).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&b).unwrap(), b"b");
        assert!(a.exists());
    }

    #[test]
    fn renames_when_free() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        std::fs::write(&a, "a").unwrap();
        rename_excl(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"a");
    }
}
