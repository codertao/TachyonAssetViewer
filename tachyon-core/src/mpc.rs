//! `*.mpc` multiplayer map configs: `[main] GAMETYPE, MAIN_SPX, MAX_PLAYERS, TITLE, IMAGE_NAME`.
//! The shipped `DM_*`/`BW_*` files are CBIN; a few leftovers (`basewar1.mpc`, `quadball1.mpc`)
//! are the uncompiled INI text, which this also reads.

use crate::cbin::{self, Value};
use crate::source::AssetSource;
use anyhow::{anyhow, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameType {
    Deathmatch,
    BaseWars,
    /// Anything else (QUADBALL is in one leftover file; the game has no rules for it).
    Other,
}

#[derive(Debug, Clone)]
pub struct MapConfig {
    /// File name without extension, as given on the command line (`DM_01`).
    pub name: String,
    pub file: String,
    pub game_type: GameType,
    pub game_type_name: String,
    pub main_spx: String,
    pub max_players: u32,
    pub title: String,
    pub image: String,
}

impl MapConfig {
    pub fn parse(file: &str, raw: &[u8]) -> Result<MapConfig> {
        let mut kv: Vec<(String, String)> = vec![];
        match cbin::parse(raw) {
            Ok(groups) => {
                for g in groups.iter().filter(|g| g.name.eq_ignore_ascii_case("main")) {
                    for it in &g.items {
                        let v = match it.values.first() {
                            Some(Value::Str(s)) => s.clone(),
                            Some(Value::Int(i)) => i.to_string(),
                            Some(Value::Float(f)) => f.to_string(),
                            None => String::new(),
                        };
                        kv.push((it.key.to_ascii_uppercase(), v));
                    }
                }
            }
            Err(_) => {
                let text = String::from_utf8_lossy(raw);
                let mut in_main = false;
                for line in text.lines().map(str::trim) {
                    if line.is_empty() || line.starts_with(';') {
                        continue;
                    }
                    if line.starts_with('[') {
                        in_main = line.eq_ignore_ascii_case("[main]");
                    } else if let (true, Some((k, v))) = (in_main, line.split_once(['=', ':'])) {
                        kv.push((k.trim().to_ascii_uppercase(), v.trim().to_string()));
                    }
                }
            }
        }
        let get = |k: &str| kv.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.clone());
        let game_type_name = get("GAMETYPE").ok_or_else(|| anyhow!("{file}: no GAMETYPE"))?.to_ascii_uppercase();
        let game_type = match game_type_name.as_str() {
            "DEATHMATCH" => GameType::Deathmatch,
            "BASEWARS" => GameType::BaseWars,
            _ => GameType::Other,
        };
        let name = file.rsplit_once('.').map(|(s, _)| s).unwrap_or(file).to_string();
        Ok(MapConfig {
            title: get("TITLE").unwrap_or_else(|| name.clone()),
            name,
            file: file.to_string(),
            game_type,
            game_type_name,
            main_spx: get("MAIN_SPX").ok_or_else(|| anyhow!("{file}: no MAIN_SPX"))?,
            max_players: get("MAX_PLAYERS").and_then(|s| s.parse().ok()).unwrap_or(32),
            image: get("IMAGE_NAME").unwrap_or_default(),
        })
    }
}

/// Every `.mpc` in the asset source, sorted by name. Files that fail to parse are returned as
/// errors alongside.
pub fn list(src: &AssetSource) -> (Vec<MapConfig>, Vec<String>) {
    let (mut ok, mut bad) = (vec![], vec![]);
    for f in src.list(&[".MPC"]) {
        match src.read(&f).and_then(|d| MapConfig::parse(&f, &d)) {
            Ok(m) => ok.push(m),
            Err(e) => bad.push(format!("{f}: {e}")),
        }
    }
    ok.sort_by_key(|m| m.name.to_ascii_uppercase());
    (ok, bad)
}

/// Find a map by name (`DM_01`, `dm_01.mpc`, ...), ignoring case.
pub fn find(src: &AssetSource, name: &str) -> Result<MapConfig> {
    let want = name.rsplit_once('.').filter(|(_, e)| e.eq_ignore_ascii_case("mpc")).map(|(s, _)| s).unwrap_or(name);
    let (maps, _) = list(src);
    maps.into_iter()
        .find(|m| m.name.eq_ignore_ascii_case(want))
        .ok_or_else(|| anyhow!("no map named {name} (try --list-maps)"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_mpc_parse() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let (maps, bad) = list(&src);
        assert!(bad.is_empty(), "{bad:?}");
        assert_eq!(maps.len(), 14);
        let dm1 = maps.iter().find(|m| m.name == "DM_01").unwrap();
        assert_eq!((dm1.game_type, dm1.main_spx.as_str(), dm1.max_players), (GameType::Deathmatch, "dm1a1_01.spx", 32));
        let bw = maps.iter().find(|m| m.name.eq_ignore_ascii_case("basewar1")).unwrap();
        assert_eq!((bw.game_type, bw.main_spx.as_str(), bw.max_players), (GameType::BaseWars, "bw01_01.spx", 64));
        for m in &maps {
            if m.game_type != GameType::Other && src.read(&m.main_spx).is_ok() {
                crate::spx::parse(&src.read(&m.main_spx).unwrap()).unwrap();
            }
        }
    }
}
