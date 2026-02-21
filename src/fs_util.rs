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
