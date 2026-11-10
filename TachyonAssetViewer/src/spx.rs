//! `.SPX` sector files (magic `SPD\x01` at +4): which objects populate a sector and where.
//! Layout from `decomp/notes/formats.md` "SPD / .SPX" (`Spx_ReadFile 0x4355f0`,
//! `Spx_ApplyPlacements 0x4351e0`, `Sped_ConstructEntity 0x434ac0`).

use crate::cbin::{self, Value};
use crate::gate::Role;
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

    Ok(Spx { quad: cstr(d, 0x34, 0x20), backdrop: cstr(d, 0x78, 0x14), decls, placements, stages, group_names })
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

/// The models a `.des` draws: `OCF_FILE` if present, else `EXTERIOR_PAK` (plus a hyper gate's
/// `CENTER_PAK` and `FLARE_PAK`, all at the gate's position and orientation: `FUN_004260c0`,
/// `FUN_00425fe0`).
#[derive(Debug, Clone, Default)]
pub struct DesModel {
    #[allow(dead_code)]
    pub des_file: String,
    pub desc_type: String,
    pub models: Vec<String>,
    /// Per model: its `.anm` and gate role (hyper gates only; see `gate`).
    pub anims: Vec<Option<(String, Role)>>,
    /// A hyper gate's ambient loop (`[sound] INNER_RING_SOUND`).
    pub ring: Option<RingSound>,
}

/// `[sound] INNER_RING_SOUND: sample`, `INNER_FALLOFF: a, b`, `INNER_PITCH: p`: a looping 3D
/// sound attached to the gate when it's created (`FUN_00426700` → `FUN_00466160` →
/// `FUN_00466000`). The des keys are read through the table at `0x535ec8`, which has no
/// `OUTER_*` keys, so the megagate's `OUTER_RING_SOUND` is ignored.
#[derive(Debug, Clone)]
pub struct RingSound {
    pub sample: String,
    /// World units: inaudible beyond `max` (`a << 13` as 16.16); full volume within `min`
    /// (`b << 13`, or `max / 25` if `b` is 0).
    pub max: f32,
    pub min: f32,
    /// 65536 = 1.
    pub pitch: f32,
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
                return Ok(DesModel { des_file: String::new(), desc_type: String::new(), models: vec![format!("{key}.PAK")], anims: vec![None], ring: None })
            }
            None => return Err(format!("{key}: not in desnames.cfg")),
        };
        load_des(src, key, &des_file)
    }
}

/// Read one `.des` (`key` labels errors).
pub fn load_des(src: &AssetSource, key: &str, des_file: &str) -> Result<DesModel, String> {
    {
        let groups = src
            .read(&des_file)
            .and_then(|d| cbin::parse(&d))
            .map_err(|e| format!("{key}: {des_file}: {e}"))?;
        let mut m = DesModel { des_file: des_file.to_string(), ..Default::default() };
        let mut ocf = None;
        let mut exterior = None;
        let mut center = None;
        let mut flare = None;
        let (mut anim, mut center_anim, mut flare_anim) = (None, None, None);
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
                    "FLARE_PAK" => flare = Some(first),
                    "ANIMFILE" => anim = Some(first),
                    "CENTER_ANIMFILE" => center_anim = Some(first),
                    "FLARE_ANIMFILE" => flare_anim = Some(first),
                    _ => {}
                }
            }
        }
        // Only hyper gates use the centre / flare models; the Enforcer's ANIMFILE is a pose
        // driven by turning (not drawn here).
        let gate = m.desc_type == "HYPER_GATE";
        let role = |a: Option<String>, r: Role| a.filter(|_| gate).map(|a| (a, r));
        if let Some(sg) = groups.iter().find(|g| g.name.eq_ignore_ascii_case("sound")).filter(|_| gate) {
            let item = |k: &str| sg.items.iter().find(|i| i.key.eq_ignore_ascii_case(k));
            let num = |k: &str, i: usize| item(k).and_then(|it| it.values.get(i)).and_then(|v| v.as_f32()).unwrap_or(0.0);
            if let Some(Value::Str(sample)) = item("INNER_RING_SOUND").and_then(|i| i.values.first()) {
                let max = num("INNER_FALLOFF", 0) / 8.0;
                let b = num("INNER_FALLOFF", 1);
                let pitch = num("INNER_PITCH", 0) / 65536.0;
                m.ring = Some(RingSound {
                    sample: sample.clone(),
                    max,
                    min: if b != 0.0 { b / 8.0 } else { max / 25.0 },
                    pitch: if pitch > 0.0 { pitch } else { 1.0 },
                });
            }
        }
        if let Some(o) = ocf {
            m.models.push(o);
            m.anims.push(None);
        } else if let Some(e) = exterior {
            m.models.push(e);
            m.anims.push(role(anim, Role::Body));
            if let Some(c) = center {
                m.models.push(c);
                m.anims.push(role(center_anim, Role::Center));
            }
            if let Some(f) = flare.filter(|_| gate) {
                m.models.push(f);
                m.anims.push(role(flare_anim, Role::Flare));
            }
        }
        Ok(m)
    }
}

/// The hyper-gate `.des` that uses `pak_file` as one of its models, if any.
pub fn gate_des_for(src: &AssetSource, pak_file: &str) -> Option<DesModel> {
    src.list(&[".DES"]).into_iter().find_map(|f| {
        let m = load_des(src, &f, &f).ok()?;
        (m.anims.iter().any(|a| a.is_some()) && m.models.iter().any(|x| x.eq_ignore_ascii_case(pak_file))).then_some(m)
    })
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

    /// Group ids match the trigger numbering, asteroid scales and the deferred flag decode.
    #[test]
    fn neut04c_groups_scale_deferred() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../pff")) else { return };
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
