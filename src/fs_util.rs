use std::fs;
use std::path::Path;

use anyhow::Result;

/// Atomically writes to `path` by first preparing a temporary file, then renaming it.
///
/// # Single-writer assumption
///
/// The temporary file name is derived deterministically from `path` (same path with a `.tmp`
/// extension). Concurrent callers targeting the same `path` will race on the same temporary
/// file, causing data corruption or spurious errors. Callers must ensure that at most one
/// writer targets a given `path` at any time.
fn with_tmp_rename<F>(path: &Path, prepare: F) -> Result<()>
where
    F: FnOnce(&Path) -> std::io::Result<()>,
{
    let tmp = path.with_extension("tmp");
    prepare(&tmp)?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

pub fn atomic_write(path: &Path, content: &str) -> Result<()> {
    with_tmp_rename(path, |tmp| fs::write(tmp, content))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_creates_file_with_correct_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");

        atomic_write(&path, "hello world").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");
    }

    #[test]
    fn atomic_write_overwrites_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");

        fs::write(&path, "old content").unwrap();
        atomic_write(&path, "new content").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
    }

    #[test]
    fn atomic_write_leaves_no_tmp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.txt");

        atomic_write(&path, "content").unwrap();

        let tmp_path = path.with_extension("tmp");
        assert!(!tmp_path.exists());
    }
}
