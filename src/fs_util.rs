use std::fs;
use std::path::Path;

use anyhow::Result;

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

pub fn atomic_copy(src: &Path, dst: &Path) -> Result<()> {
    with_tmp_rename(dst, |tmp| fs::copy(src, tmp).map(|_| ()))
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

    #[test]
    fn atomic_copy_copies_file_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.txt");
        let dst = dir.path().join("dst.txt");

        fs::write(&src, "copy me").unwrap();
        atomic_copy(&src, &dst).unwrap();

        assert_eq!(fs::read_to_string(&dst).unwrap(), "copy me");
    }

    #[test]
    fn atomic_copy_fails_for_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("nonexistent.txt");
        let dst = dir.path().join("dst.txt");

        let result = atomic_copy(&src, &dst);
        assert!(result.is_err());
    }
}
