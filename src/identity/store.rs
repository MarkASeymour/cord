use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use super::IdentityError;

pub fn resolve_config_dir(
    override_dir: Option<PathBuf>,
) -> Result<PathBuf, IdentityError> {
    if let Some(dir) = override_dir {
        return Ok(dir);
    }
    let dirs = ProjectDirs::from("", "", "cord").ok_or(IdentityError::NoConfigDir)?;
    Ok(dirs.config_dir().to_path_buf())
}

pub fn ensure_dir(dir: &Path) -> Result<(), IdentityError> {
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(dir)?.permissions();
        if perms.mode() & 0o777 != 0o700 {
            perms.set_mode(0o700);
            fs::set_permissions(dir, perms)?;
        }
    }
    Ok(())
}

pub fn write_atomic_0600(path: &Path, bytes: &[u8]) -> Result<(), IdentityError> {
    // temp file then rename. a crash mid write leaves the original intact.
    let dir = path
        .parent()
        .ok_or_else(|| IdentityError::Corrupt("path has no parent".into()))?;
    let tmp = path.with_extension(format!("tmp.{:08x}", rand::random::<u32>()));
    {
        let mut f = open_create_exclusive(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(unix)]
fn open_create_exclusive(path: &Path) -> Result<File, IdentityError> {
    use std::os::unix::fs::OpenOptionsExt;
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?)
}

#[cfg(not(unix))]
fn open_create_exclusive(path: &Path) -> Result<File, IdentityError> {
    Ok(OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn atomic_write_creates_0600_file() {
        let dir = std::env::temp_dir().join(format!("cord-test-{:x}", rand::random::<u64>()));
        ensure_dir(&dir).unwrap();
        let path = dir.join("secret");
        write_atomic_0600(&path, b"hello").unwrap();

        let mut buf = Vec::new();
        File::open(&path).unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"hello");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "expected 0600, got {:o}", mode);
        }

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn override_dir_passes_through() {
        let custom = std::env::temp_dir().join("cord-override");
        let resolved = resolve_config_dir(Some(custom.clone())).unwrap();
        assert_eq!(resolved, custom);
    }
}
