//! `.SPX` sector files (magic `SPD\x01` at +4): which objects populate a sector and where.
//! Layout from `decomp/notes/formats.md` "SPD / .SPX" (`Spx_ReadFile 0x4355f0`,
//! `Spx_ApplyPlacements 0x4351e0`, `Sped_ConstructEntity 0x434ac0`).

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
    /// Kind 3 only: index into `Spx::group_names` (-1 = none). The engine maps the name through
    /// SHIPTEXT.BIN to the id triggers use (`Spx_RemapPlacementNames 0x434f10`); see `FlightGroups`.
    pub group: i32,
    /// Render scale. Asteroids (kind 2) carry a 16.16 scale at +0x38
    /// (`Asteroid_SpawnAtLoad 0x407f30` copies it to the entity's scale); everything else is 1.
    pub scale: f32,
}

impl Placement {
    /// Kind-3 objects with flag 0x8 aren't spawned at sector load (`Ship_SpawnAtLoad 0x434e80`
    /// and its siblings); a trigger spawns them later. (0x1000 marks the kind-0 marker records,
    /// which are never built.)
    pub fn deferred(&self) -> bool {
        self.kind == 3 && self.flags & 0x8 != 0
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
    /// Group / contents / ship names (header +0x104..+0x10c; FG_*, CONT_*, ...).
    pub group_names: Vec<String>,
    /// Header +0x100: the quadrant id (`Quadrant_Init` → quadrant +0x30; startup kind 1 +0x2c).
    pub sector_id: i32,
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
        let kind = u16_(o)?;
        let fx = |k: usize| -> Result<f32> { Ok(i32_(o + k)? as f32 / 65536.0) };
        placements.push(Placement {
            kind,
            decl: u32_(o + 4)? as usize,
            pos: [fx(0x08)?, fx(0x0c)?, fx(0x10)?],
            orient: [i32_(o + 0x14)?, i32_(o + 0x18)?, i32_(o + 0x1c)?],
            id: i32_(o + 0x20)?,
            flags: u32_(o + 0x24)?,
            group: if kind == 3 && size >= 0x58 { i32_(o + 0x54)? } else { -1 },
            scale: if kind == 2 && size >= 0x3c { fx(0x38)? } else { 1.0 },
        });
        if size == 0 {
            break;
        }
        o += size;
    }

    let (ns, so) = (u32_(0x2c)? as usize, u32_(0x30)? as usize);
    let stages = if ns < 64 { (0..ns).map(|i| cstr(d, so + i * 0x2c, 0x10)).collect() } else { vec![] };

    // Header is 0x108, 0x11c or 0x1a4 bytes (`first_offset` at +0); the group-name table
    // descriptor needs at least 0x110.
    let group_names = if u32_(0)? >= 0x110 {
        let (gn, go) = (u32_(0x108)? as usize, u32_(0x10c)? as usize);
        if gn < 4096 { (0..gn).map(|i| cstr(d, go + i * 0x20, 0x20)).collect() } else { vec![] }
    } else {
        vec![]
    };

    let sector_id = if u32_(0)? >= 0x104 { i32_(0x100)? } else { 0 };
    Ok(Spx { quad: cstr(d, 0x34, 0x20), backdrop: cstr(d, 0x78, 0x14), decls, placements, stages, group_names, sector_id })
}

/// Group ids as the game numbers them: a group name's record index in SHIPTEXT.BIN (an RTXT
/// string table) minus the index of its `[FLIGHT_GROUPS]` section (`FUN_00435db0`,
/// `Spx_GroupNameToShiptextIdx 0x434ed0`). Format: `decomp/notes/formats.md` "RTXT".
#[derive(Debug, Default)]
pub struct FlightGroups {
    ids: HashMap<String, i32>,
}

impl FlightGroups {
    pub fn load(src: &AssetSource) -> Result<FlightGroups> {
        Self::parse(&src.read("SHIPTEXT.BIN")?)
    }

    pub fn parse(d: &[u8]) -> Result<FlightGroups> {
        let u32_ = |o: usize| -> Result<usize> {
            Ok(u32::from_le_bytes(d.get(o..o + 4).ok_or_else(|| anyhow::anyhow!("RTXT: read past end at {o:#x}"))?.try_into().unwrap()) as usize)
        };
        // NUL-terminated string at o, and the offset just past it.
        let strz = |o: usize| -> Result<(String, usize)> {
            let n = d.get(o..).and_then(|t| t.iter().position(|&c| c == 0)).ok_or_else(|| anyhow::anyhow!("RTXT: bad string at {o:#x}"))?;
            Ok((d[o..o + n].iter().map(|&c| c as char).collect(), o + n + 1))
        };
        ensure!(d.len() >= 16 && &d[0..4] == b"RTXT", "not an RTXT file");
        let gto = u32_(4)?;
        let ngroups = u32_(gto)?;
        ensure!(ngroups < 4096, "implausible RTXT group count");
        let headers: Vec<(usize, usize)> = (0..ngroups).map(|g| Ok((u32_(gto + 4 + 8 * g)?, u32_(gto + 8 + 8 * g)?))).collect::<Result<_>>()?;
        // Group names follow the headers; each group's keys start at gto + 4 + keys_offset.
        let mut p = gto + 4 + 8 * ngroups;
        let mut names = Vec::with_capacity(ngroups);
        for _ in 0..ngroups {
            let (n, next) = strz(p)?;
            names.push(n);
            p = next;
        }
        let (mut index, mut base, mut all) = (0i32, None, HashMap::new());
        for (name, (keys_off, count)) in names.iter().zip(&headers) {
            if name == "FLIGHT_GROUPS" {
                base = Some(index);
            }
            let mut k = gto + 4 + keys_off;
            for _ in 0..*count {
                let (key, next) = strz(k)?;
                all.entry(key).or_insert(index);
                k = next;
                index += 1;
            }
        }
        let base = base.ok_or_else(|| anyhow::anyhow!("SHIPTEXT.BIN has no [FLIGHT_GROUPS]"))?;
        Ok(FlightGroups { ids: all.into_iter().map(|(k, v)| (k, v - base)).collect() })
    }

    /// "FG_NERGAL (227)" for a placement's group index, or the raw index if it can't be resolved.
    pub fn label(&self, spx: &Spx, group: i32) -> String {
        match usize::try_from(group).ok().and_then(|g| spx.group_names.get(g)) {
            Some(n) => match self.ids.get(n) {
                Some(id) => format!("{n} ({id})"),
                None => n.clone(),
            },
            None => format!("#{group}"),
        }
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
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let s = parse(&src.read("BORA05.SPX").unwrap()).unwrap();
        assert_eq!(s.backdrop, "BORA5.BDF");
        assert_eq!(s.decls.len(), 4);
        assert_eq!(s.placements.len(), 5);
        assert_eq!(s.placements[4].id, 255);
        assert_eq!(s.stages, ["bora05A.spx", "bora05B.spx"]);
        assert_eq!(find_parent(&src, "bora05A.spx").as_deref().map(str::to_uppercase).as_deref(), Some("BORA05.SPX"));
    }

    /// Group ids match the trigger numbering, asteroid scales and the deferred flag decode.
    #[test]
    fn neut04c_groups_scale_deferred() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let s = parse(&src.read("NEUT04C.SPX").unwrap()).unwrap();
        let g = FlightGroups::load(&src).unwrap();
        let labels: Vec<String> = s.placements.iter().filter(|p| p.group >= 0).map(|p| g.label(&s, p.group)).collect();
        // NEUT04C's triggers address these as groups 227, 251 and 222.
        for want in ["FG_NERGAL (227)", "FG_WYRM (251)", "FG_CRONUS (222)"] {
            assert!(labels.iter().any(|l| l == want), "{want} not in {labels:?}");
        }
        assert!(s.placements.iter().all(|p| p.kind != 2 || p.group == -1));
        assert!(s.placements.iter().all(|p| (p.kind == 2) || p.scale == 1.0));

        let mut scales = vec![];
        let mut deferred = 0;
        for n in src.list(&[".SPX"]) {
            let s = parse(&src.read(&n).unwrap()).unwrap();
            scales.extend(s.placements.iter().filter(|p| p.kind == 2).map(|p| p.scale));
            deferred += s.placements.iter().filter(|p| p.deferred()).count();
        }
        assert_eq!(scales.len(), 10074);
        let (lo, hi) = scales.iter().fold((f32::MAX, f32::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        assert_eq!((lo, hi), (1.0, 10.0));
        assert!(deferred > 0);
    }

    /// Every shipped SPX parses, and every placement's alias resolves to a `.des` file.
    #[test]
    fn all_spx_parse() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let reg = crate::des::DesRegistry::new(&src).unwrap();
        let (mut files, mut objs, mut unresolved) = (0, 0, HashMap::<String, usize>::new());
        for n in src.list(&[".SPX"]) {
            let s = parse(&src.read(&n).unwrap()).unwrap_or_else(|e| panic!("{n}: {e}"));
            files += 1;
            for p in &s.placements {
                objs += 1;
                let alias = s.decls.get(p.decl).cloned().unwrap_or_default();
                if reg.file_for(&alias).is_none() {
                    *unresolved.entry(alias).or_default() += 1;
                }
            }
        }
        eprintln!("{files} SPX, {objs} objects, unresolved aliases: {unresolved:?}");
        assert!(files > 250);
    }
}
