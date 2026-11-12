//! Where assets come from: an extracted directory or the Tachyon.pff archive itself.

use crate::pff;
use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

enum Backing {
    Dir,
    Pff(memmap2::Mmap),
}

pub struct AssetSource {
    pub path: PathBuf,
    backing: Backing,
    /// Upper-cased name -> (display name, location). For directories the location is the
    /// real file path; for archives it's (offset, size).
    index: HashMap<String, (String, Loc)>,
}

#[derive(Clone)]
enum Loc {
    File(PathBuf),
    Range(usize, usize),
}

impl AssetSource {
    pub fn open(path: &Path) -> Result<Self> {
        let mut index = HashMap::new();
        if path.is_dir() {
            for ent in std::fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
                let ent = ent?;
                if !ent.file_type()?.is_file() {
                    continue;
                }
                let name = ent.file_name().to_string_lossy().into_owned();
                index.insert(name.to_uppercase(), (name, Loc::File(ent.path())));
            }
            Ok(Self { path: path.to_path_buf(), backing: Backing::Dir, index })
        } else {
            let f = File::open(path).with_context(|| format!("opening {}", path.display()))?;
            // SAFETY: read-only map of a file we don't modify while the viewer runs.
            let map = unsafe { memmap2::Mmap::map(&f)? };
            for e in pff::parse_entries(&map)? {
                index.insert(e.name.to_uppercase(), (e.name, Loc::Range(e.offset, e.size)));
            }
            Ok(Self { path: path.to_path_buf(), backing: Backing::Pff(map), index })
        }
    }

    pub fn is_archive(&self) -> bool {
        matches!(self.backing, Backing::Pff(_))
    }

    /// Display names of all entries with one of the given (upper-case) extensions, sorted.
    pub fn list(&self, exts: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = self
            .index
            .iter()
            .filter(|(k, _)| exts.iter().any(|e| k.ends_with(e)))
            .map(|(_, (n, _))| n.clone())
            .collect();
        v.sort_by_key(|n| n.to_uppercase());
        v
    }

    /// Read a file by name, case-insensitively.
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let (_, loc) = self
            .index
            .get(&name.to_uppercase())
            .ok_or_else(|| anyhow!("{name} not found"))?;
        match (loc, &self.backing) {
            (Loc::File(p), _) => std::fs::read(p).with_context(|| format!("reading {}", p.display())),
            (Loc::Range(o, s), Backing::Pff(map)) => Ok(map[*o..*o + *s].to_vec()),
            _ => unreachable!(),
        }
    }
}
