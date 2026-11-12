//! NovaLogic PFF archive (see TachyonFileFormats/ksy/pff.ksy).

use anyhow::{bail, Result};

#[derive(Debug, Clone)]
pub struct PffEntry {
    pub name: String,
    pub offset: usize,
    pub size: usize,
}

fn u32_at(d: &[u8], o: usize) -> Result<u32> {
    match d.get(o..o + 4) {
        Some(b) => Ok(u32::from_le_bytes(b.try_into().unwrap())),
        None => bail!("PFF truncated at {o:#x}"),
    }
}

/// Parse the entry table. Deleted (tombstoned) entries are skipped.
pub fn parse_entries(d: &[u8]) -> Result<Vec<PffEntry>> {
    if d.len() < 20 || &d[4..7] != b"PFF" {
        bail!("not a PFF archive");
    }
    let count = u32_at(d, 8)? as usize;
    let seg = u32_at(d, 12)? as usize;
    let list = u32_at(d, 16)? as usize;
    if seg < 32 {
        bail!("PFF entry size {seg} too small");
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = list + i * seg;
        let deleted = u32_at(d, e)?;
        let offset = u32_at(d, e + 4)? as usize;
        let size = u32_at(d, e + 8)? as usize;
        let raw = &d[e + 16..e + 32];
        let name: String = raw
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect();
        if deleted != 0 || name.is_empty() {
            continue;
        }
        if offset + size > d.len() {
            bail!("PFF entry {name} runs past end of archive");
        }
        out.push(PffEntry { name, offset, size });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_matches_extracted_files() {
        let (Ok(arc), Ok(loose)) = (std::fs::read("../../GoG/Tachyon.pff"), std::fs::read("../../pff/CUTLESS.PAK")) else {
            return;
        };
        let entries = parse_entries(&arc).unwrap();
        let paks = entries.iter().filter(|e| e.name.to_uppercase().ends_with(".PAK")).count();
        assert!(paks >= 1159, "only {paks} PAKs in archive");
        let e = entries.iter().find(|e| e.name.eq_ignore_ascii_case("CUTLESS.PAK")).unwrap();
        assert_eq!(&arc[e.offset..e.offset + e.size], &loose[..]);
    }
}
