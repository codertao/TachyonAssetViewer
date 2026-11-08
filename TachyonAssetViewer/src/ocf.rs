//! OCF composite models (capital ships, stations): a CBIN listing PAKs and `part_NN` groups.
//!
//! Assembly follows space.exe (`Ocf_ParsePart 0x4094a0`, attach resolvers `0x4099d0` /
//! `0x409c40`, child update `0x408ef0`):
//! - part orientation = its own `ANGLES` (yaw, pitch, roll in degrees) relative to the root;
//!   it is NOT composed with the parent's;
//! - part offset = parent.offset + M(parent)·parentPak.attach[ATTACH[1]]
//!                               - M(self)·ownPak.attach[ATTACH[2]],
//!   with M = R(angles)·scale. The first part is the root at the origin.

use crate::cbin::{self, Group};
use crate::math::Mat3;
use crate::pak::Pak;
use anyhow::{anyhow, Result};
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct OcfPart {
    pub name: String,
    /// Resolved .PAK file name.
    pub pak_file: String,
    pub scale: f32,
    /// Degrees: yaw, pitch, roll.
    pub angles: [f32; 3],
    /// (parent part name, parent attach index, own attach index).
    pub attach: Option<(String, usize, usize)>,
}

#[derive(Debug, Clone)]
pub struct Ocf {
    pub name: String,
    pub parts: Vec<OcfPart>,
}

pub fn parse(raw: &[u8]) -> Result<Ocf> {
    let groups = cbin::parse(raw)?;
    let find = |n: &str| groups.iter().find(|g| g.name.eq_ignore_ascii_case(n));
    let name = find("header")
        .and_then(|g| g.get("NAME"))
        .and_then(|i| i.values.first())
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut aliases: HashMap<String, String> = HashMap::new();
    if let Some(g) = find("paks") {
        for it in g.get_all("PAK") {
            if let (Some(a), Some(f)) = (
                it.values.first().and_then(|v| v.as_str()),
                it.values.get(1).and_then(|v| v.as_str()),
            ) {
                aliases.insert(a.to_uppercase(), f.to_string());
            }
        }
    }
    let mut parts = Vec::new();
    for g in groups.iter().filter(|g| g.name.to_lowercase().starts_with("part_")) {
        parts.push(parse_part(g, &aliases)?);
    }
    Ok(Ocf { name, parts })
}

fn parse_part(g: &Group, aliases: &HashMap<String, String>) -> Result<OcfPart> {
    let sv = |k: &str, i: usize| g.get(k).and_then(|it| it.values.get(i)).and_then(|v| v.as_str());
    let fv = |k: &str, i: usize| g.get(k).and_then(|it| it.values.get(i)).and_then(|v| v.as_f32());
    let iv = |k: &str, i: usize| g.get(k).and_then(|it| it.values.get(i)).and_then(|v| v.as_i32());
    let alias = sv("PAK", 0).ok_or_else(|| anyhow!("{}: no PAK", g.name))?;
    let pak_file = aliases
        .get(&alias.to_uppercase())
        .cloned()
        .unwrap_or_else(|| format!("{alias}.PAK"));
    let attach = sv("ATTACH", 0).map(|p| {
        (
            p.to_string(),
            iv("ATTACH", 1).unwrap_or(0).max(0) as usize,
            iv("ATTACH", 2).unwrap_or(0).max(0) as usize,
        )
    });
    Ok(OcfPart {
        name: sv("NAME", 0).unwrap_or(&g.name).to_string(),
        pak_file,
        scale: fv("SCALE", 0).unwrap_or(1.0),
        angles: [
            fv("ANGLES", 0).unwrap_or(0.0),
            fv("ANGLES", 1).unwrap_or(0.0),
            fv("ANGLES", 2).unwrap_or(0.0),
        ],
        attach,
    })
}

/// World placement of one part.
#[derive(Debug, Clone)]
pub struct Placement {
    pub pak_file: String,
    /// Rotation·scale.
    pub basis: Mat3,
    pub scale: f32,
    pub offset: [f32; 3],
}

/// Resolve every part's transform. `paks` must contain each part's pak (upper-case key);
/// parts whose pak is missing are skipped, and so are their descendants' attach lookups
/// (they fall back to the parent's offset).
pub fn place(ocf: &Ocf, paks: &HashMap<String, Pak>) -> (Vec<Placement>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut by_name: HashMap<String, usize> = HashMap::new();
    let mut offsets: Vec<[f32; 3]> = Vec::with_capacity(ocf.parts.len());
    let mut bases: Vec<Mat3> = Vec::with_capacity(ocf.parts.len());
    let mut out = Vec::new();
    for (i, p) in ocf.parts.iter().enumerate() {
        let basis = Mat3::from_engine_euler_deg(p.angles).scaled(p.scale);
        let pak = paks.get(&p.pak_file.to_uppercase());
        let mut offset = [0.0f32; 3];
        if i > 0 {
            match &p.attach {
                Some((parent, pidx, oidx)) => match by_name.get(&parent.to_uppercase()) {
                    Some(&pi) => {
                        let pp = &ocf.parts[pi];
                        let pa = paks
                            .get(&pp.pak_file.to_uppercase())
                            .map(|k| k.attach_point(*pidx))
                            .unwrap_or([0.0; 3]);
                        let oa = pak.map(|k| k.attach_point(*oidx)).unwrap_or([0.0; 3]);
                        let a = bases[pi].mul_vec(pa);
                        let b = basis.mul_vec(oa);
                        for k in 0..3 {
                            offset[k] = offsets[pi][k] + a[k] - b[k];
                        }
                    }
                    None => warnings.push(format!("{}: unknown ATTACH parent {parent}", p.name)),
                },
                None => warnings.push(format!("{}: no ATTACH, placed at origin", p.name)),
            }
        }
        by_name.insert(p.name.to_uppercase(), i);
        offsets.push(offset);
        bases.push(basis);
        if pak.is_some() {
            out.push(Placement { pak_file: p.pak_file.clone(), basis, scale: p.scale, offset });
        } else {
            warnings.push(format!("{}: missing {}", p.name, p.pak_file));
        }
    }
    (out, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_shipped_ocf() {
        let Ok(rd) = std::fs::read_dir("../pff") else { return };
        let files: HashMap<String, std::path::PathBuf> = rd
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().to_uppercase(), e.path()))
            .collect();
        let mut n = 0;
        let mut problems = vec![];
        for (name, path) in &files {
            if !name.ends_with(".OCF") {
                continue;
            }
            let ocf = parse(&std::fs::read(path).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(!ocf.parts.is_empty(), "{name}: no parts");
            let names: Vec<String> = ocf.parts.iter().map(|p| p.name.to_uppercase()).collect();
            for (i, p) in ocf.parts.iter().enumerate() {
                if !files.contains_key(&p.pak_file.to_uppercase()) {
                    problems.push(format!("{name}/{}: pak {} not found", p.name, p.pak_file));
                }
                if let Some((parent, _, _)) = &p.attach {
                    if !names[..i].contains(&parent.to_uppercase()) {
                        problems.push(format!("{name}/{}: parent {parent} not defined earlier", p.name));
                    }
                }
            }
            n += 1;
        }
        assert_eq!(n, 91);
        assert!(problems.is_empty(), "{} problems: {:#?}", problems.len(), &problems[..problems.len().min(20)]);
    }
}
