//! `.des` object definitions (CBIN; `main.DESC_TYPE` picks the class). Layout from
//! `decomp/notes/formats.md` ".des", `ai.md` "Where skill comes from", `flight.md`.
//!
//! Values are kept in file units. Unit conversion (per-frame rates, 16.16, the global damage
//! scale) belongs to the simulation that uses them.

use crate::cbin::{self, Group, Value};
use crate::source::AssetSource;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// AI class bits (`g_AiClassNames`).
pub const CLASS_FIGHTER: u32 = 0x1;
pub const CLASS_BOMBER: u32 = 0x2;
pub const CLASS_CAPSHIP: u32 = 0x4;
pub const CLASS_STARBASE: u32 = 0x8;
pub const CLASS_TURRET: u32 = 0x10;

/// `CLASS_BOMBER+CLASS_FIGHTER` → bits. Unknown names are ignored.
pub fn class_bits(s: &str) -> u32 {
    s.split('+')
        .map(|c| match c.trim().to_ascii_uppercase().as_str() {
            "CLASS_FIGHTER" => CLASS_FIGHTER,
            "CLASS_BOMBER" => CLASS_BOMBER,
            "CLASS_CAPSHIP" => CLASS_CAPSHIP,
            "CLASS_STARBASE" => CLASS_STARBASE,
            "CLASS_TURRET" => CLASS_TURRET,
            _ => 0,
        })
        .fold(0, |a, b| a | b)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Skill {
    Rookie = 0,
    Veteran = 1,
    Ace = 2,
    Elite = 3,
}

impl Skill {
    pub fn parse(s: &str) -> Option<Skill> {
        Some(match s.trim().to_ascii_uppercase().as_str() {
            "ROOKIE" => Skill::Rookie,
            "VETERAN" => Skill::Veteran,
            "ACE" => Skill::Ace,
            "ELITE" => Skill::Elite,
            _ => return None,
        })
    }
    pub fn from_index(i: u8) -> Skill {
        [Skill::Rookie, Skill::Veteran, Skill::Ace, Skill::Elite][(i as usize).min(3)]
    }
}

/// Per-axis rotation block (`ShipDes_ParseRotation 0x463a90`). Max rate is degrees/s in the
/// file.
#[derive(Debug, Clone, Copy, Default)]
pub struct Axis {
    pub accel: f32,
    pub max_rate: f32,
    pub dampen: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Afterburner {
    pub supported: bool,
    pub accel: f32,
    pub top_speed_boost: f32,
    pub max_energy: f32,
    pub power_use: f32,
}

#[derive(Debug, Clone)]
pub struct AiBlock {
    pub object_class: u32,
    pub main_target_class: u32,
    pub target_class: u32,
    pub target_range: f32,
    pub target_360_range: f32,
    pub target_scan_angle: f32,
    pub skill: Skill,
}

/// One `weap_N` group: the AI's default loadout.
#[derive(Debug, Clone)]
pub struct AiWeapon {
    pub weapon: String,
    pub mount_slots: Vec<i32>,
}

/// The SHIP-class blocks the simulation needs.
#[derive(Debug, Clone, Default)]
pub struct ShipStats {
    /// yaw, pitch, roll.
    pub rotation: [Axis; 3],
    pub yaw_roll_ratio: f32,
    pub accel: f32,
    pub max_speed: f32,
    pub drift: f32,
    pub afterburner: Afterburner,
    pub side_thrusters: bool,
    pub shield_max: f32,
    pub hull: f32,
    pub laser_energy: f32,
    pub ai: Option<AiBlock>,
    pub ai_weapons: Vec<AiWeapon>,
    /// `connect` block part-slot indices.
    pub spawn_point: Option<i32>,
}

#[derive(Debug, Clone, Default)]
pub struct Des {
    pub file: String,
    /// Upper-cased `DESC_TYPE` (SHIP, CAPITOL_SHIP, STARBASE, ASTEROID, ...).
    pub desc_type: String,
    pub object_name: String,
    pub name_id: String,
    pub race: String,
    pub group_class: String,
    pub exterior_pak: Option<String>,
    /// The palette paired with `EXTERIOR_PAK`.
    pub exterior_pal: Option<String>,
    pub ocf_file: Option<String>,
    /// Only for `DESC_TYPE: SHIP`.
    pub ship: Option<ShipStats>,
    pub groups: Vec<Group>,
}

fn first_str(g: &Group, key: &str) -> Option<String> {
    g.get(key).and_then(|i| i.values.first()).and_then(|v| v.as_str()).map(str::to_string)
}

fn num(g: Option<&Group>, key: &str) -> f32 {
    g.and_then(|g| g.get(key)).and_then(|i| i.values.first()).and_then(Value::as_f32).unwrap_or(0.0)
}

impl Des {
    pub fn group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name.eq_ignore_ascii_case(name))
    }

    /// The `(pak, pal)` the server's class loader puts in the object model pool, if any
    /// (`ModelPool_Load` callers; `multiplayer.md` "Model pool entries"): `EXTERIOR_PAK` for every
    /// class except STARBASE and OCF capital ships; a pak named NULL is skipped.
    pub fn pool_model(&self) -> Option<(String, String)> {
        if self.desc_type == "STARBASE" || (self.desc_type == "CAPITOL_SHIP" && self.ocf_file.is_some()) {
            return None;
        }
        let pak = self.exterior_pak.clone()?;
        if pak.eq_ignore_ascii_case("NULL") {
            return None;
        }
        Some((pak, self.exterior_pal.clone().unwrap_or_default()))
    }

    pub fn is_ship(&self) -> bool {
        self.desc_type == "SHIP"
    }

    pub fn parse(file: &str, raw: &[u8]) -> Result<Des> {
        let groups = cbin::parse(raw)?;
        let mut d = Des { file: file.to_string(), groups, ..Default::default() };
        if let Some(main) = d.group("main").cloned() {
            d.desc_type = first_str(&main, "DESC_TYPE").unwrap_or_default().to_ascii_uppercase();
            d.object_name = first_str(&main, "OBJECT_NAME").unwrap_or_default();
            d.name_id = first_str(&main, "NAME_ID").unwrap_or_default();
            d.race = first_str(&main, "RACE").unwrap_or_default();
            d.group_class = first_str(&main, "GROUP_CLASS_ID").unwrap_or_default();
            d.exterior_pak = first_str(&main, "EXTERIOR_PAK");
            d.exterior_pal = main.get("EXTERIOR_PAK").and_then(|i| i.values.get(1)).and_then(|v| v.as_str()).map(str::to_string);
            d.ocf_file = first_str(&main, "OCF_FILE");
        }
        if d.is_ship() {
            d.ship = Some(d.parse_ship());
        }
        Ok(d)
    }

    fn parse_ship(&self) -> ShipStats {
        let rot = self.group("rotation");
        let axis = |p: &str| Axis {
            accel: num(rot, &format!("{p}_ACCEL")),
            max_rate: num(rot, &format!("MAX_{p}_RATE")),
            dampen: num(rot, &format!("{p}_DAMPEN")),
        };
        let speed = self.group("speed");
        let ab = self.group("afterburner");
        let ai = self.group("ai").map(|g| AiBlock {
            object_class: first_str(g, "OBJECT_CLASS").map(|s| class_bits(&s)).unwrap_or(0),
            main_target_class: first_str(g, "MAIN_TARGET_CLASS").map(|s| class_bits(&s)).unwrap_or(0),
            target_class: first_str(g, "TARGET_CLASS").map(|s| class_bits(&s)).unwrap_or(0),
            target_range: num(Some(g), "TARGET_RANGE"),
            target_360_range: num(Some(g), "TARGET_360_RANGE"),
            target_scan_angle: num(Some(g), "TARGET_SCAN_ANGLE"),
            // `ShipDes_ParseSkill` defaults to ROOKIE.
            skill: first_str(g, "SKILL").and_then(|s| Skill::parse(&s)).unwrap_or(Skill::Rookie),
        });
        let nweap = num(self.group("weapons"), "NUM_AI_WEAPONS") as usize;
        let ai_weapons = (1..=nweap.min(16))
            .filter_map(|i| {
                let g = self.group(&format!("weap_{i}"))?;
                let weapon = first_str(g, "AI_WEAPON")?;
                let n = num(Some(g), "NUM_MOUNTS") as usize;
                let mount_slots = (1..=n.min(16)).map(|m| num(Some(g), &format!("MOUNT_{m}_POINT_SLOT")) as i32).collect();
                Some(AiWeapon { weapon, mount_slots })
            })
            .collect();
        ShipStats {
            rotation: [axis("YAW"), axis("PITCH"), axis("ROLL")],
            yaw_roll_ratio: num(rot, "YAW_ROLL_RATIO"),
            accel: num(speed, "ACCELERATION"),
            max_speed: num(speed, "MAX_SPEED"),
            drift: num(speed, "DRIFT"),
            afterburner: Afterburner {
                supported: num(ab, "SUPPORTED") != 0.0,
                accel: num(ab, "ACCELERATION"),
                top_speed_boost: num(ab, "TOP_SPEED_BOOST"),
                max_energy: num(ab, "MAX_ENERGY"),
                power_use: num(ab, "POWER_USE"),
            },
            side_thrusters: num(self.group("side_thrusters"), "SUPPORTED") != 0.0,
            shield_max: num(self.group("shields"), "MAX_ENERGY"),
            hull: num(self.group("hull"), "DAMAGE"),
            laser_energy: num(self.group("energy"), "LASER_ENERGY"),
            ai,
            ai_weapons,
            spawn_point: self.group("connect").and_then(|g| g.get("SPAWN_POINT")).and_then(|i| i.values.first()).and_then(Value::as_i32),
        }
    }
}

/// SPED alias → `.des` file (`desnames.cfg` `DES: alias, file`; `Spx_ResolveDesName 0x435120`),
/// with each `.des` parsed once on demand.
pub struct DesRegistry {
    names: HashMap<String, String>,
    cache: HashMap<String, std::result::Result<std::rc::Rc<Des>, String>>,
}

impl DesRegistry {
    pub fn new(src: &AssetSource) -> Result<Self> {
        let groups = cbin::parse(&src.read("desnames.cfg")?).context("desnames.cfg")?;
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

    /// The `.des` file an alias names in desnames.cfg, if any.
    pub fn file_for(&self, alias: &str) -> Option<String> {
        self.names.get(&alias.to_uppercase()).cloned()
    }

    /// As the engine resolves it: unknown aliases (the player-ship names DART, CLAW, ... that
    /// some SPX files use) fall back to `DEFAULT` (`Spx_ResolveDesName 0x435120`).
    pub fn file_or_default(&self, alias: &str) -> Option<String> {
        self.file_for(alias).or_else(|| self.file_for("DEFAULT"))
    }

    pub fn aliases(&self) -> impl Iterator<Item = (&str, &str)> {
        self.names.iter().map(|(a, f)| (a.as_str(), f.as_str()))
    }

    pub fn resolve(&mut self, src: &AssetSource, alias: &str) -> std::result::Result<std::rc::Rc<Des>, String> {
        let key = alias.to_uppercase();
        if let Some(r) = self.cache.get(&key) {
            return r.clone();
        }
        let r = match self.file_or_default(&key) {
            Some(f) => src
                .read(&f)
                .and_then(|d| Des::parse(&f, &d))
                .map(std::rc::Rc::new)
                .map_err(|e| format!("{key}: {f}: {e}")),
            None => Err(format!("{key}: not in desnames.cfg")),
        };
        self.cache.insert(key, r.clone());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutlass() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let d = Des::parse("CUTLASS.DES", &src.read("CUTLASS.DES").unwrap()).unwrap();
        let s = d.ship.unwrap();
        assert_eq!(d.desc_type, "SHIP");
        assert_eq!(s.max_speed, 510.0);
        assert_eq!(s.rotation[1].max_rate, 121.0);
        assert_eq!(s.shield_max, 23500.0);
        assert_eq!(s.hull, 34500.0);
        let ai = s.ai.unwrap();
        assert_eq!(ai.skill, Skill::Veteran);
        assert_eq!(ai.target_class, CLASS_FIGHTER | CLASS_BOMBER);
        assert_eq!(s.ai_weapons[0].weapon, "BORA_HEAVY_LASER");
        assert_eq!(s.ai_weapons[0].mount_slots, [4, 5]);
        assert!(crate::weapons::by_name(&s.ai_weapons[0].weapon).is_some());
    }

    /// Every shipped `.des` parses; every ship's AI weapons name a known weapon.
    #[test]
    fn all_des_parse() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let mut n = 0;
        let mut unknown = vec![];
        for f in src.list(&[".DES"]) {
            let d = Des::parse(&f, &src.read(&f).unwrap()).unwrap_or_else(|e| panic!("{f}: {e}"));
            n += 1;
            for w in d.ship.iter().flat_map(|s| &s.ai_weapons) {
                if crate::weapons::by_name(&w.weapon).is_none() {
                    unknown.push(format!("{f}: {}", w.weapon));
                }
            }
        }
        assert!(n > 200, "{n} .des");
        assert!(unknown.is_empty(), "{unknown:?}");
    }
}
