//! Durable reservations committed before allocation; failures never advance memory.
use super::{Lock, managed, managed_file};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Watermark {
    schema_version: u32,
    reserved_through: u64,
}
pub struct Sequences<'a> {
    _lock: &'a mut Lock,
    dir: PathBuf,
    next: u64,
    through: u64,
}
impl<'a> Sequences<'a> {
    /// The exclusive borrow enforces both lock lifetime and allocator uniqueness.
    ///
    /// ```compile_fail
    /// use herdr_tokens::runtime::{Lock, Sequences};
    /// fn competing(lock: &mut Lock, dir: std::path::PathBuf) {
    ///     let mut first = Sequences::open(dir.clone(), lock).unwrap();
    ///     let _second = Sequences::open(dir, lock).unwrap();
    ///     first.allocate().unwrap();
    /// }
    /// ```
    /// ```compile_fail
    /// use herdr_tokens::runtime::{Lock, Sequences};
    /// fn release(mut lock: Lock, dir: std::path::PathBuf) {
    ///     let mut sequences = Sequences::open(dir, &mut lock).unwrap();
    ///     drop(lock);
    ///     sequences.allocate().unwrap();
    /// }
    /// ```
    pub fn open(dir: PathBuf, lock: &'a mut Lock) -> Result<Self> {
        let path = dir.join("sequence.toml");
        managed(&path, false)?;
        let through = match OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(path)
        {
            Ok(f) => {
                managed_file(&f)?;
                let mut text = String::new();
                f.take(4097).read_to_string(&mut text)?;
                ensure!(text.len() <= 4096, "sequence state oversized");
                let w: Watermark = toml::from_str(&text)
                    .context("corrupt sequence state; do not reset while Herdr retains history")?;
                ensure!(w.schema_version == 1, "unsupported sequence schema");
                w.reserved_through
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(e.into()),
        };
        let mut result = Self {
            _lock: lock,
            dir,
            next: through,
            through,
        };
        result.reserve()?;
        Ok(result)
    }
    fn reserve(&mut self) -> Result<()> {
        self.reserve_with(persist)
    }
    fn reserve_with(&mut self, write: impl FnOnce(&Path, u64) -> Result<()>) -> Result<()> {
        let end = self
            .through
            .checked_add(1024)
            .context("sequence range exhausted")?;
        write(&self.dir, end)?;
        self.next = self.through + 1;
        self.through = end;
        Ok(())
    }
    pub fn allocate(&mut self) -> Result<u64> {
        if self.next > self.through {
            self.reserve()?;
        }
        let n = self.next;
        self.next = n.checked_add(1).context("sequence range exhausted")?;
        Ok(n)
    }
}

fn persist(dir: &Path, end: u64) -> Result<()> {
    let mut temp = tempfile::NamedTempFile::new_in(dir)?;
    write!(temp, "schema_version = 1\nreserved_through = {end}\n")?;
    temp.as_file().sync_all()?;
    temp.persist(dir.join("sequence.toml"))?;
    File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_reservation_never_advances_and_restart_skips_committed_range() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let endpoint =
            crate::runtime::Endpoint::new(&dir.path().join("api"), Some(&dir.path().join("r")))
                .unwrap();
        let mut lock = endpoint.acquire().unwrap().unwrap();
        let mut seq = Sequences::open(dir.path().into(), &mut lock).unwrap();
        for expected in 1..=1024 {
            assert_eq!(seq.allocate().unwrap(), expected);
        }
        let before = std::fs::read(dir.path().join("sequence.toml")).unwrap();
        assert!(
            seq.reserve_with(|_, _| anyhow::bail!("injected persistence failure"))
                .is_err()
        );
        assert_eq!(
            std::fs::read(dir.path().join("sequence.toml")).unwrap(),
            before
        );
        assert_eq!(seq.allocate().unwrap(), 1025);
        drop(seq);
        let mut seq = Sequences::open(dir.path().into(), &mut lock).unwrap();
        assert_eq!(seq.allocate().unwrap(), 2049);
    }
}
