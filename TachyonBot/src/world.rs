//! What the bot knows about the world, built from snapshots and events. Prints a line for each
//! spawn, disappearance and significant change.

use crate::proto::{race, Record, REC_FAR, REC_PAGED};
use std::collections::{BTreeMap, HashMap};
use std::time::Instant;

/// A jump this far (world units) from the dead-reckoned position is reported.
const JUMP: f64 = 5.0;
/// A turn of this many u8 angle steps (32 = 45°) between snapshots is reported.
const TURN: i32 = 2;
/// Snapshots a near record survives without being refreshed (the game uses 1 then expires
/// after the next miss); far records last 1000 / send interval ≈ 62.
const LIFE_NEAR: u32 = 2;
const LIFE_FAR: u32 = 62;

pub struct Entity {
    pub rec: Record,
    seen: Instant,
    life: u32,
}

#[derive(Default)]
pub struct Tables {
    pub models: HashMap<u16, String>,
    pub descs: HashMap<i16, String>,
}

pub struct World {
    pub entities: BTreeMap<u32, Entity>,
    pub tables: Tables,
    pub me: Option<u32>,
    /// `--track`: log every record for this entity.
    pub track: Option<u32>,
    track_last: Option<(Instant, [f64; 3])>,
    pub roster: BTreeMap<u16, String>,
    start: Instant,
}

impl World {
    pub fn new() -> World {
        World { entities: BTreeMap::new(), tables: Tables::default(), me: None, track: None, track_last: None, roster: BTreeMap::new(), start: Instant::now() }
    }

    pub fn log(&self, msg: impl AsRef<str>) {
        let t = self.start.elapsed().as_secs_f64();
        println!("[{t:8.2}] {}", msg.as_ref());
    }

    fn describe(&self, r: &Record) -> String {
        let model = self.tables.models.get(&(r.model as u16)).cloned().unwrap_or_else(|| format!("#{}", r.model));
        let mut s = format!("model={model}");
        if let Some(d) = r.desc {
            s += &format!(" desc={}", self.tables.descs.get(&d).cloned().unwrap_or_else(|| format!("#{d}")));
        }
        s += &format!(" team={} flags={:#04x}", race(r.team as u32), r.flags);
        s
    }

    /// Apply one snapshot's records; `from` is our position, for distances.
    pub fn apply_snapshot(&mut self, records: &[Record], from: [f64; 3]) {
        let now = Instant::now();
        let mut fresh = vec![];
        for r in records {
            if Some(r.id) == self.me {
                continue;
            }
            fresh.push(r.id);
            if Some(r.id) == self.track {
                let measured = self.track_last.map(|(t, p)| {
                    let dt = now.duration_since(t).as_secs_f64();
                    dist(p, r.pos) / dt.max(1e-3)
                });
                self.track_last = Some((now, r.pos));
                let v = (r.vel[0] * r.vel[0] + r.vel[1] * r.vel[1] + r.vel[2] * r.vel[2]).sqrt();
                self.log(format!(
                    "TRACK {:#010x} pos {} vel/tick {:.2} (= {:.0} u/s at 50 Hz) measured {} throttle {} yaw {}",
                    r.id,
                    fmt(r.pos),
                    v,
                    v * 50.0,
                    measured.map_or("-".into(), |m| format!("{m:.0} u/s")),
                    r.throttle,
                    r.angles[0]
                ));
            }
            self.apply_record(r.clone(), now, from, "SPAWN ");
        }
        // Age everything not in this snapshot; paged (static) records never expire this way.
        let mut gone = vec![];
        for (id, e) in &mut self.entities {
            if fresh.contains(id) || e.rec.flags & REC_PAGED != 0 {
                continue;
            }
            e.life = e.life.saturating_sub(1);
            if e.life == 0 {
                gone.push(*id);
            }
        }
        for id in gone {
            self.remove(id, "out of view");
        }
    }

    pub fn apply_record(&mut self, r: Record, now: Instant, from: [f64; 3], verb: &str) {
        let life = if r.flags & REC_FAR != 0 { LIFE_FAR } else { LIFE_NEAR };
        let range = dist(r.pos, from);
        let Some(e) = self.entities.get_mut(&r.id) else {
            self.log(format!("{verb}{:#010x} {} at {} dist={range:.0}", r.id, self.describe(&r), fmt(r.pos)));
            self.entities.insert(r.id, Entity { rec: r, seen: now, life });
            return;
        };
        let ticks = now.duration_since(e.seen).as_secs_f64() * 50.0;
        let predicted = [0, 1, 2].map(|k| e.rec.pos[k] + e.rec.vel[k] * ticks);
        let mut changes = vec![];
        let old = &e.rec;
        if old.model != r.model {
            changes.push(format!("model {}→{}", old.model, r.model));
        }
        if old.team != r.team {
            changes.push(format!("team {}→{}", race(old.team as u32), race(r.team as u32)));
        }
        if (old.throttle == 255) != (r.throttle == 255) {
            changes.push(format!("afterburner {}", if r.throttle == 255 { "on" } else { "off" }));
        }
        if (old.flags ^ r.flags) & !REC_FAR != 0 {
            changes.push(format!("flags {:#04x}→{:#04x}", old.flags, r.flags));
        }
        if old.desc != r.desc && r.desc.is_some() {
            changes.push(format!("desc {:?}→{:?}", old.desc, r.desc));
        }
        let off = dist(predicted, r.pos);
        if off > JUMP {
            changes.push(format!("jumped {off:.0} from predicted, now {}", fmt(r.pos)));
        }
        let turn = (0..3).map(|k| (old.angles[k] as i8).wrapping_sub(r.angles[k] as i8).unsigned_abs() as i32).max().unwrap();
        if turn >= TURN {
            changes.push(format!("turned {:.0}°", turn as f64 * 360.0 / 256.0));
        }
        let speed_was = len(old.vel) * 50.0;
        let speed = len(r.vel) * 50.0;
        if (speed - speed_was).abs() > 100.0 && (speed - speed_was).abs() > 0.5 * speed_was.max(speed) {
            changes.push(format!("speed {speed_was:.0}→{speed:.0} u/s"));
        }
        e.rec = r;
        e.seen = now;
        e.life = life;
        if !changes.is_empty() {
            let id = e.rec.id;
            self.log(format!("CHANGE {id:#010x} {} (dist={range:.0})", changes.join(", ")));
        }
    }

    pub fn remove(&mut self, id: u32, why: &str) {
        if let Some(e) = self.entities.remove(&id) {
            self.log(format!("GONE   {id:#010x} {} ({why})", self.describe(&e.rec)));
        }
    }

    pub fn set_team(&mut self, id: u32, team: u8) {
        if let Some(e) = self.entities.get_mut(&id) {
            let was = e.rec.team;
            e.rec.team = team;
            self.log(format!("CHANGE {id:#010x} allegiance {}→{}", race(was as u32), race(team as u32)));
        }
    }

    pub fn summary(&self) -> String {
        let far = self.entities.values().filter(|e| e.rec.flags & REC_FAR != 0).count();
        let paged = self.entities.values().filter(|e| e.rec.flags & REC_PAGED != 0).count();
        format!("{} entities known ({far} far, {paged} static), {} players", self.entities.len(), self.roster.len())
    }
}

pub fn dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    len([a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}
fn len(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}
pub fn fmt(p: [f64; 3]) -> String {
    format!("({:.0}, {:.0}, {:.0})", p[0], p[1], p[2])
}
