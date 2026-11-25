use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub fn cache_root() -> Result<PathBuf> {
    let base = dirs::cache_dir().context("couldn't find a cache directory (is HOME set?)")?;
    private_dir(&base.join("tawaki"))
}

pub fn data_root() -> Result<PathBuf> {
    let base = dirs::data_dir().context("couldn't find a data directory (is HOME set?)")?;
    private_dir(&base.join("tawaki"))
}

pub fn cache_sub(name: &str) -> Result<PathBuf> {
    private_dir(&cache_root()?.join(name))
}

pub fn private_dir(dir: &Path) -> Result<PathBuf> {
    if let Some(parent) = dir.parent() {
        if !parent.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("couldn't create {}", parent.display()))?;
        }
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("couldn't create {}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(dir)
        .with_context(|| format!("couldn't inspect {}", dir.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        bail!(
            "{} is not a real directory, refusing to use it",
            dir.display()
        );
    }
    if meta.uid() != crate::sys::euid() {
        bail!(
            "{} is owned by someone else, refusing to use it",
            dir.display()
        );
    }
    if meta.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("couldn't make {} private", dir.display()))?;
    }
    Ok(dir.to_path_buf())
}

fn owned_file(path: &Path) -> Option<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    (meta.file_type().is_file() && meta.uid() == crate::sys::euid() && meta.mode() & 0o022 == 0)
        .then_some(meta)
}

pub fn read_limited(path: &Path, max: u64) -> Option<Vec<u8>> {
    let meta = owned_file(path)?;
    if meta.len() > max {
        return None;
    }
    let f = std::fs::OpenOptions::new().read(true).open(path).ok()?;
    let mut buf = Vec::with_capacity(meta.len() as usize);
    f.take(max + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= max).then_some(buf)
}

pub fn age(path: &Path) -> Option<Duration> {
    owned_file(path)?.modified().ok()?.elapsed().ok()
}

pub fn fresh(path: &Path, ttl: Duration) -> bool {
    age(path).is_some_and(|a| a < ttl)
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("bad path")?;
    let name = path
        .file_name()
        .context("bad path")?
        .to_string_lossy()
        .into_owned();
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.{}.{nonce}.tmp", std::process::id()));
    let res = (|| -> Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("couldn't create {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path).with_context(|| format!("couldn't write {}", path.display()))
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

#[cfg(test)]
pub fn test_dir(tag: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let d = std::env::temp_dir().join(format!("tawaki-test-{tag}-{}-{nonce}", std::process::id()));
    std::fs::DirBuilder::new().mode(0o700).create(&d).unwrap();
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_is_created_0700() {
        let base = test_dir("pd");
        let d = private_dir(&base.join("a")).unwrap();
        let m = std::fs::metadata(&d).unwrap();
        assert_eq!(m.mode() & 0o777, 0o700);
    }

    #[test]
    fn private_dir_tightens_loose_permissions() {
        let base = test_dir("loose");
        let d = base.join("x");
        std::fs::create_dir(&d).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        private_dir(&d).unwrap();
        assert_eq!(std::fs::metadata(&d).unwrap().mode() & 0o777, 0o700);
    }

    #[test]
    fn private_dir_rejects_symlinks() {
        let base = test_dir("sym");
        let target = base.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = base.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(private_dir(&link).is_err());
        let file = base.join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(private_dir(&file).is_err());
    }

    #[test]
    fn atomic_write_is_private_and_readable() {
        let base = test_dir("aw");
        let p = base.join("f.json");
        write_atomic(&p, b"hello").unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
        assert_eq!(read_limited(&p, 100).unwrap(), b"hello");
        write_atomic(&p, b"bye").unwrap();
        assert_eq!(read_limited(&p, 100).unwrap(), b"bye");
        let leftovers = std::fs::read_dir(&base).unwrap().count();
        assert_eq!(leftovers, 1);
    }

    #[test]
    fn read_limited_enforces_size_and_rejects_links() {
        let base = test_dir("rl");
        let p = base.join("big");
        std::fs::write(&p, vec![b'a'; 64]).unwrap();
        assert!(read_limited(&p, 63).is_none());
        assert!(read_limited(&p, 64).is_some());
        let link = base.join("link");
        std::os::unix::fs::symlink(&p, &link).unwrap();
        assert!(read_limited(&link, 1000).is_none());
        assert!(age(&link).is_none());
    }

    #[test]
    fn read_limited_rejects_group_writable_files() {
        let base = test_dir("gw");
        let p = base.join("f");
        std::fs::write(&p, b"x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_limited(&p, 10).is_none());
    }

    #[test]
    fn fresh_respects_ttl() {
        let base = test_dir("ttl");
        let p = base.join("f");
        std::fs::write(&p, b"x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(fresh(&p, Duration::from_secs(60)));
        assert!(!fresh(&p, Duration::ZERO));
        assert!(!fresh(&base.join("missing"), Duration::from_secs(60)));
    }
}
