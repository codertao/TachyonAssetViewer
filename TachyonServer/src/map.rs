//! A loaded map: its `.mpc`, the main SPX, and the `.des` (plus OCF for stations and capital
//! ships) for every placement.

use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::rc::Rc;
use tachyon_core::des::{Des, DesRegistry};
use tachyon_core::mpc::{GameType, MapConfig};
use tachyon_core::ocf::{self, Ocf};
use tachyon_core::race::Races;
use tachyon_core::source::AssetSource;
use tachyon_core::spx::{self, Spx};

pub struct Map {
    pub config: MapConfig,
    pub spx: Spx,
    /// Per `spx.decls` entry: its `.des`, or the resolve error.
    pub decls: Vec<std::result::Result<Rc<Des>, String>>,
    /// OCF by file name, for the `.des` files that name one.
    pub ocfs: BTreeMap<String, Rc<Ocf>>,
    #[allow(dead_code)] // used from M2 (team hostility, /ai add)
    pub races: Races,
    pub registry: DesRegistry,
    pub src: AssetSource,
    /// Problems that don't stop the server (unresolved aliases, missing OCFs).
    pub warnings: Vec<String>,
}

impl Map {
    pub fn load(src: AssetSource, config: MapConfig) -> Result<Map> {
        let src_owned = src;
        let src = &src_owned;
        if config.game_type == GameType::Other {
            anyhow::bail!("{}: game type {} isn't supported (only DEATHMATCH and BASEWARS)", config.name, config.game_type_name);
        }
        let spx = spx::parse(&src.read(&config.main_spx).with_context(|| format!("{}: MAIN_SPX", config.name))?)
            .with_context(|| config.main_spx.clone())?;
        let mut registry = DesRegistry::new(src)?;
        let races = Races::load(src).context("race.cfg")?;
        let mut warnings = vec![];
        let mut ocfs = BTreeMap::new();
        let decls: Vec<_> = spx
            .decls
            .iter()
            .map(|alias| {
                if registry.file_for(alias).is_none() {
                    warnings.push(format!("alias {alias:?} isn't in desnames.cfg; using DEFAULT as the game does"));
                }
                let r = registry.resolve(src, alias);
                if let Ok(d) = &r
                    && let Some(f) = &d.ocf_file
                    && !ocfs.contains_key(&f.to_uppercase())
                {
                    match src.read(f).and_then(|raw| ocf::parse(&raw)) {
                        Ok(o) => {
                            ocfs.insert(f.to_uppercase(), Rc::new(o));
                        }
                        Err(e) => warnings.push(format!("{}: {f}: {e}", d.file)),
                    }
                }
                r
            })
            .collect();
        for (i, r) in decls.iter().enumerate() {
            if let Err(e) = r {
                warnings.push(format!("decl {i}: {e}"));
            }
        }
        Ok(Map { config, spx, decls, ocfs, races, registry, warnings, src: src_owned })
    }

    pub fn des_for(&self, decl: usize) -> Option<&Rc<Des>> {
        self.decls.get(decl).and_then(|r| r.as_ref().ok())
    }

    pub fn ocf_for(&self, des: &Des) -> Option<&Rc<Ocf>> {
        des.ocf_file.as_ref().and_then(|f| self.ocfs.get(&f.to_uppercase()))
    }

    /// One-paragraph summary for the console.
    pub fn summary(&self) -> String {
        let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
        let mut turrets = 0;
        let mut deferred = 0;
        for p in &self.spx.placements {
            if p.deferred() {
                deferred += 1;
                continue;
            }
            let Some(d) = self.des_for(p.decl) else { continue };
            *by_type.entry(d.desc_type.clone()).or_default() += 1;
            if let Some(o) = self.ocf_for(d) {
                turrets += o.parts.iter().filter(|x| x.is_turret()).count();
            }
        }
        let types: Vec<String> = by_type.iter().map(|(t, n)| format!("{n} {}", t.to_lowercase())).collect();
        format!(
            "{} ({}, {}, max {} players): quadrant {:?}, {} placements [{}], {} turrets, {} deferred",
            self.config.name,
            self.config.game_type_name,
            self.config.main_spx,
            self.config.max_players,
            self.spx.quad,
            self.spx.placements.len(),
            types.join(", "),
            turrets,
            deferred,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every DM/BW map loads with all its placements resolved.
    #[test]
    fn all_maps_load() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let (maps, _) = tachyon_core::mpc::list(&src);
        let mut loaded = 0;
        for m in maps.into_iter().filter(|m| m.game_type != GameType::Other) {
            let name = m.name.clone();
            let src2 = AssetSource::open(std::path::Path::new("../../pff")).unwrap();
            let map = Map::load(src2, m).unwrap_or_else(|e| panic!("{name}: {e:#}"));
            assert!(map.decls.iter().all(|d| d.is_ok()), "{name}: {:?}", map.warnings);
            eprintln!("{}", map.summary());
            loaded += 1;
        }
        assert!(loaded >= 11, "{loaded}");
    }
}
