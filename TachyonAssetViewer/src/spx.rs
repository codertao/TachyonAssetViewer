//! `.SPX` sector files (magic `SPD\x01` at +4): which objects populate a sector and where.
//! Layout from `decomp/notes/formats.md` "SPD / .SPX" (`Spx_ReadFile 0x4355f0`,
//! `Spx_ApplyPlacements 0x4351e0`, `Sped_ConstructEntity 0x434ac0`).

use crate::cbin::{self, Value};
use crate::source::AssetSource;
use anyhow::{ensure, Result};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct Placement {
    /// Editor category: 0 null, 1 gate, 2 asteroid/debris, 3 ship/station, 4 MP pickup.
    #[allow(dead_code)]
    pub kind: u16,
    /// Index into `Spx::decls`.
    pub decl: usize,
    /// World position, 16.16 fixed point (engine axes).
    pub pos: [f32; 3],
    /// Euler angles, 512 units per turn (same order as OCF `ANGLES`).
    pub orient: [i32; 3],
    pub id: i32,
    pub flags: u32,
    pub group: i32,
}

impl Placement {
    /// `Sped_ConstructEntity` skips defs with flag 0x1000 at sector load; triggers spawn them later.
    pub fn deferred(&self) -> bool {
        self.flags & 0x1000 != 0
    }
}

#[derive(Debug, Clone)]
pub struct Spx {
    pub quad: String,
    pub backdrop: String,
    /// SPED aliases (resolved through `desnames.cfg`).
    pub decls: Vec<String>,
    pub placements: Vec<Placement>,
    /// Conditional sub-sector stages (`bora05A.spx`, …).
    pub stages: Vec<String>,
}

fn cstr(d: &[u8], o: usize, max: usize) -> String {
    d.get(o..(o + max).min(d.len()))
        .unwrap_or(&[])
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as char)
        .collect()
}

pub fn parse(d: &[u8]) -> Result<Spx> {
    ensure!(d.len() >= 0x1a4 && &d[4..8] == b"SPD\x01", "not an SPD sector file");
    let u32_ = |o: usize| -> Result<u32> {
        Ok(u32::from_le_bytes(d.get(o..o + 4).ok_or_else(|| anyhow::anyhow!("read past end at {o:#x}"))?.try_into().unwrap()))
    };
    let i32_ = |o: usize| -> Result<i32> { Ok(u32_(o)? as i32) };
    let u16_ = |o: usize| -> Result<u16> {
        Ok(u16::from_le_bytes(d.get(o..o + 2).ok_or_else(|| anyhow::anyhow!("read past end at {o:#x}"))?.try_into().unwrap()))
    };

    let (rec, n, ofs) = (u32_(0x08)? as usize, u32_(0x0c)? as usize, u32_(0x10)? as usize);
    ensure!(n < 4096 && rec >= 0x20, "implausible SPED section");
    let decls = (0..n).map(|i| cstr(d, ofs + i * rec, 0x20)).collect();

    let (np, po) = (u32_(0x14)? as usize, u32_(0x18)? as usize);
    ensure!(np < 65536, "implausible placement count");
    let mut placements = Vec::with_capacity(np);
    let mut o = po;
    for _ in 0..np {
        let size = u16_(o + 2)? as usize;
        let fx = |k: usize| -> Result<f32> { Ok(i32_(o + k)? as f32 / 65536.0) };
        placements.push(Placement {
            kind: u16_(o)?,
            decl: u32_(o + 4)? as usize,
            pos: [fx(0x08)?, fx(0x0c)?, fx(0x10)?],
            orient: [i32_(o + 0x14)?, i32_(o + 0x18)?, i32_(o + 0x1c)?],
            id: i32_(o + 0x20)?,
            flags: u32_(o + 0x24)?,
            group: if size >= 0x58 { i32_(o + 0x54)? } else { -1 },
        });
        if size == 0 {
            break;
        }
        o += size;
    }

    let (ns, so) = (u32_(0x2c)? as usize, u32_(0x30)? as usize);
    let stages = if ns < 64 { (0..ns).map(|i| cstr(d, so + i * 0x2c, 0x10)).collect() } else { vec![] };

    Ok(Spx { quad: cstr(d, 0x34, 0x20), backdrop: cstr(d, 0x78, 0x14), decls, placements, stages })
}

/// The models a `.des` draws: `OCF_FILE` if present, else `EXTERIOR_PAK` (plus a hyper gate's
/// `CENTER_PAK`).
#[derive(Debug, Clone, Default)]
pub struct DesModel {
    #[allow(dead_code)]
    pub des_file: String,
    pub desc_type: String,
    pub models: Vec<String>,
}

/// SPED alias → `.des` (`desnames.cfg` `DES: alias, file`), with each `.des` read on demand.
pub struct DesResolver {
    names: HashMap<String, String>,
    cache: HashMap<String, Result<DesModel, String>>,
}

impl DesResolver {
    pub fn new(src: &AssetSource) -> Result<Self> {
        let groups = cbin::parse(&src.read("desnames.cfg")?)?;
        let mut names = HashMap::new();
        for g in &groups {
            for it in &g.items {
                if let (Some(Value::Str(a)), Some(Value::Str(f))) = (it.values.first(), it.values.get(1)) {
                    names.insert(a.to_uppercase(), f.clone());
                }
            }
        }
        Ok(Self { names, cache: HashMap::new() })
    }

    pub fn resolve(&mut self, src: &AssetSource, alias: &str) -> Result<DesModel, String> {
        let key = alias.to_uppercase();
        if let Some(r) = self.cache.get(&key) {
            return r.clone();
        }
        let r = self.load(src, &key);
        self.cache.insert(key, r.clone());
        r
    }

    fn load(&self, src: &AssetSource, key: &str) -> Result<DesModel, String> {
        let des_file = match self.names.get(key) {
            Some(f) => f.clone(),
            // Player-ship aliases (DART, CLAW, …) aren't in desnames.cfg: fall back to
            // `<alias>.des`, then a PAK of the same name.
            None if src.read(&format!("{key}.des")).is_ok() => format!("{key}.des"),
            None if src.read(&format!("{key}.PAK")).is_ok() => {
                return Ok(DesModel { des_file: String::new(), desc_type: String::new(), models: vec![format!("{key}.PAK")] })
            }
            None => return Err(format!("{key}: not in desnames.cfg")),
        };
        let groups = src
            .read(&des_file)
            .and_then(|d| cbin::parse(&d))
            .map_err(|e| format!("{key}: {des_file}: {e}"))?;
        let mut m = DesModel { des_file: des_file.clone(), ..Default::default() };
        let mut ocf = None;
        let mut exterior = None;
        let mut center = None;
        if let Some(main) = groups.iter().find(|g| g.name.eq_ignore_ascii_case("main")) {
            for it in &main.items {
                let first = match it.values.first() {
                    Some(Value::Str(s)) => s.clone(),
                    _ => continue,
                };
                match it.key.to_uppercase().as_str() {
                    "DESC_TYPE" => m.desc_type = first.to_uppercase(),
                    "OCF_FILE" => ocf = Some(first),
                    "EXTERIOR_PAK" => exterior = Some(first),
                    "CENTER_PAK" => center = Some(first),
                    _ => {}
                }
            }
        }
        if let Some(o) = ocf {
            m.models.push(o);
        } else if let Some(e) = exterior {
            m.models.push(e);
            m.models.extend(center);
        }
        Ok(m)
    }
}

/// The main sector a sub-sector stage belongs to: `bora05A.spx` → `BORA05.SPX`, confirmed by its
/// stage table; otherwise any SPX whose stage table names it.
pub fn find_parent(src: &AssetSource, name: &str) -> Option<String> {
    let lists = |p: &str| -> bool {
        src.read(p)
            .ok()
            .and_then(|d| parse(&d).ok())
            .is_some_and(|s| s.stages.iter().any(|st| st.eq_ignore_ascii_case(name)))
    };
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    if stem.len() > 1 && stem.chars().last().is_some_and(|c| c.is_ascii_alphabetic()) {
        let cand = format!("{}.SPX", &stem[..stem.len() - 1]);
        if lists(&cand) {
            return src.list(&[".SPX"]).into_iter().find(|n| n.eq_ignore_ascii_case(&cand));
        }
    }
    src.list(&[".SPX"]).into_iter().find(|p| !p.eq_ignore_ascii_case(name) && lists(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bora05_layout() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../pff")) else { return };
        let s = parse(&src.read("BORA05.SPX").unwrap()).unwrap();
        assert_eq!(s.backdrop, "BORA5.BDF");
        assert_eq!(s.decls.len(), 4);
        assert_eq!(s.placements.len(), 5);
        assert_eq!(s.placements[4].id, 255);
        assert_eq!(s.stages, ["bora05A.spx", "bora05B.spx"]);
        assert_eq!(find_parent(&src, "bora05A.spx").as_deref().map(str::to_uppercase).as_deref(), Some("BORA05.SPX"));
    }

    /// Every shipped SPX parses; report how many objects resolve to a model.
    #[test]
    fn all_spx_resolve() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../pff")) else { return };
        let mut des = DesResolver::new(&src).unwrap();
        let (mut files, mut objs, mut ok) = (0, 0, 0);
        let mut missing: HashMap<String, usize> = HashMap::new();
        for n in src.list(&[".SPX"]) {
            let s = parse(&src.read(&n).unwrap()).unwrap_or_else(|e| panic!("{n}: {e}"));
            files += 1;
            for p in &s.placements {
                objs += 1;
                let alias = s.decls.get(p.decl).cloned().unwrap_or_default();
                match des.resolve(&src, &alias) {
                    Ok(m) if !m.models.is_empty() && m.models.iter().all(|f| src.read(f).is_ok()) => ok += 1,
                    Ok(m) => *missing.entry(format!("{alias} ({} {}) {:?}", m.des_file, m.desc_type, m.models)).or_default() += 1,
                    Err(e) => *missing.entry(e).or_default() += 1,
                }
            }
        }
        let mut v: Vec<_> = missing.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        eprintln!("{files} SPX, {objs} objects, {ok} with models");
        for (k, c) in v.iter().take(40) {
            eprintln!("  {c:5} {k}");
        }
        assert!(files > 250);
    }
}
