//! Atomic save.
//!
//! 1. Resolve a symlinked target to its real path.
//! 2. Write a temporary file in the target's directory, readable only by the owner.
//! 3. fsync it and copy the target's permission bits.
//! 4. Rename it over the target.
//! 5. On Linux, fsync the directory so the rename is durable.
//!
//! Any failure removes the temporary file and leaves the target untouched.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

static TEMP_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Writes `contents` to `target` atomically and returns the path actually written (the
/// symlink target when `target` is a symlink).
pub fn save_atomic(target: &Path, contents: &[u8]) -> io::Result<PathBuf> {
    save_atomic_with(target, contents, |file, bytes| file.write_all(bytes))
}

/// `save_atomic` with the write step supplied by the caller, so tests can inject failures.
fn save_atomic_with(
    target: &Path,
    contents: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<PathBuf> {
    let target = resolve_symlink(target)?;
    let dir = match target.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let name = target
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
    let temp = dir.join(format!(
        ".{}.mdt-save-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let original = fs::metadata(&target).ok().map(|m| m.permissions());
        let mut options = File::options();
        options.write(true).create_new(true);
        // Only the owner can read the temporary file until it has the target's permissions,
        // so a private file's new content is never briefly readable by others. A new file
        // gets the default mode.
        #[cfg(unix)]
        if original.is_some() {
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        }
        let mut file = options.open(&temp)?;
        write(&mut file, contents)?;
        file.sync_all()?;
        if let Some(permissions) = original {
            fs::set_permissions(&temp, permissions)?;
        }
        drop(file);
        fs::rename(&temp, &target)?;
        #[cfg(target_os = "linux")]
        File::open(&dir)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map(|()| target)
}

/// Follows symlinks at the final path component. A missing target is returned as given,
/// so saving to a new path creates it.
fn resolve_symlink(path: &Path) -> io::Result<PathBuf> {
    let mut path = path.to_path_buf();
    for _ in 0..40 {
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = fs::read_link(&path)?;
                path = match path.parent() {
                    Some(parent) if link.is_relative() => parent.join(link),
                    _ => link,
                };
            }
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(path),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".mdt-save-"))
            .collect()
    }

    #[test]
    fn unedited_crlf_file_is_byte_identical_after_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        let original = b"# Title  \r\n\r\nline\r\nmixed\n\tend\r\n".to_vec();
        fs::write(&path, &original).unwrap();
        let bytes = fs::read(&path).unwrap();
        let decoded = crate::decode::decode(&bytes).unwrap();
        save_atomic(
            &path,
            &crate::decode::encode([decoded.text.as_bytes()], decoded.bom),
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(leftovers(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        save_atomic(&path, b"new").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn temp_file_is_private_until_permissions_are_copied() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "secret").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut mode_while_writing = 0;
        save_atomic_with(&path, b"new secret", |file, bytes| {
            mode_while_writing = file.metadata()?.permissions().mode() & 0o777;
            file.write_all(bytes)
        })
        .unwrap();
        assert_eq!(mode_while_writing, 0o600, "others never see the content");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn injected_write_failure_leaves_original_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "original").unwrap();
        let err = save_atomic_with(&path, b"new content", |file, bytes| {
            file.write_all(&bytes[..3])?;
            Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"))
        })
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::StorageFull);
        assert_eq!(fs::read(&path).unwrap(), b"original");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn saving_through_symlink_writes_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.md");
        let link = dir.path().join("link.md");
        fs::write(&real, "old").unwrap();
        std::os::unix::fs::symlink("real.md", &link).unwrap();
        let written = save_atomic(&link, b"new").unwrap();
        assert_eq!(written, real);
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
    }

    #[test]
    fn saving_to_a_new_path_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.md");
        save_atomic(&path, b"hello").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello");
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_leaves_original_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "original").unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = save_atomic(&path, b"new");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), b"original");
    }
}
