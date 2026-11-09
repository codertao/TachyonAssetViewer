//! Hyper gates: `.anm` part animation and the gate's open / closed poses
//! (`decomp/notes/animation.md`, "Gates").
//!
//! A gate `.des` names three models at the same position and orientation, each with its own
//! `.anm`:
//! - `EXTERIOR_PAK` + `ANIMFILE`: the body. Its three pylon bases are the same mesh stacked at
//!   rest; the anm's first keyframe fans them out by ±120°.
//! - `CENTER_PAK` + `CENTER_ANIMFILE`: the shimmering portal (billboard).
//! - `FLARE_PAK` + `FLARE_ANIMFILE`: a flickering flare (billboard).
//!
//! The centre and flare keyframes drive instance scale and self-lit intensity rather than parts.
//!
//! - **Static pose** (`Anim_ApplyPose 0x407b50`, called every frame from the gate hooks
//!   `0x425ba0` / `0x425c10` / `0x425c70`): clear all part rotations, then apply the named
//!   animation's **first** keyframe. A closed gate uses `GATE_OPEN` / `GATEWAY_OPEN`, an open one
//!   `GATE_CLOSE` / `GATEWAY_CLOSE`: each animation starts from the pose it leaves.
//! - **Playback** (`Anim_Play 0x407af0`, `Anim_StepFrame 0x4071d0`, `Anim_ComputeDeltas
//!   0x406470`): each keyframe moves linearly from the previous pose to its `PART_DEST` (parts
//!   it doesn't name keep their value) over `max(TIME, 1)` ticks, then holds for `PAUSE` ticks.
//!   SCALE and INTENSITY interpolate the same way. 50 ticks per second.
//! - **Flare flicker** (`FUN_00425d10`): while lit, every tick sets intensity to 128..255 and adds
//!   ±0.125 to the scale.

use crate::cbin::{self, Value};
use anyhow::Result;

pub const TICK_HZ: f32 = 50.0;

#[derive(Clone, Debug)]
struct Key {
    /// Ticks (`ROUND(secs × 50)`).
    time: u32,
    pause: u32,
    /// (part index, rotation in degrees: the three part angles, OCF `ANGLES` order).
    parts: Vec<(usize, [f32; 3])>,
    scale: f32,
    /// 0..255; `None` when the keyframe has no INTENSITY (the engine stores -1).
    intensity: Option<f32>,
}

#[derive(Clone, Debug)]
struct Anim {
    name: String,
    keys: Vec<Key>,
}

/// One `.anm` file.
#[derive(Clone, Debug)]
pub struct Anm {
    n_parts: usize,
    anims: Vec<Anim>,
}

/// A pose: part rotations (degrees) plus the instance scale and intensity.
#[derive(Clone, Debug, PartialEq)]
pub struct Pose {
    pub rot: Vec<[f32; 3]>,
    pub scale: f32,
    pub intensity: Option<f32>,
}

fn lerp(a: f32, b: f32, f: f32) -> f32 {
    a + (b - a) * f
}

impl Anm {
    pub fn parse(d: &[u8]) -> Result<Anm> {
        let groups = cbin::parse(d)?;
        let mut defines: Vec<(String, usize)> = vec![];
        let mut names = vec![];
        for g in &groups {
            let gname = g.name.to_lowercase();
            for it in &g.items {
                let k = it.key.to_uppercase();
                match (gname.as_str(), k.as_str()) {
                    ("defines", "PART_DEFINE") => {
                        if let (Some(Value::Str(n)), Some(i)) = (it.values.first(), it.values.get(1).and_then(|v| v.as_i32())) {
                            defines.push((n.to_uppercase(), i.max(0) as usize));
                        }
                    }
                    ("anim_names", "ANIM_NAME") => {
                        if let Some(Value::Str(n)) = it.values.first() {
                            names.push(n.clone());
                        }
                    }
                    _ => {}
                }
            }
        }
        let part_index = |n: &str| defines.iter().find(|(d, _)| d == &n.to_uppercase()).map(|(_, i)| *i);
        let secs = |v: Option<&Value>| ((v.and_then(|v| v.as_f32()).unwrap_or(0.0) * TICK_HZ).round()).max(0.0) as u32;
        let mut anims = vec![];
        for name in names {
            let mut keys = vec![];
            for k in 0.. {
                let gname = format!("{}_frame_{k:02}", name.to_lowercase());
                let Some(g) = groups.iter().find(|g| g.name.to_lowercase() == gname) else { break };
                let get = |key: &str| g.items.iter().find(|i| i.key.eq_ignore_ascii_case(key)).and_then(|i| i.values.first());
                let parts = g
                    .items
                    .iter()
                    .filter(|i| i.key.eq_ignore_ascii_case("PART_DEST"))
                    .filter_map(|i| {
                        let Some(Value::Str(p)) = i.values.first() else { return None };
                        let a = [1, 2, 3].map(|j| i.values.get(j).and_then(|v| v.as_f32()).unwrap_or(0.0));
                        Some((part_index(p)?, a))
                    })
                    .collect();
                keys.push(Key {
                    time: secs(get("TIME")),
                    pause: secs(get("PAUSE")),
                    parts,
                    scale: get("SCALE").and_then(|v| v.as_f32()).unwrap_or(0.0),
                    intensity: get("INTENSITY").and_then(|v| v.as_f32()),
                });
            }
            anims.push(Anim { name, keys });
        }
        let n_parts = defines.iter().map(|(_, i)| i + 1).max().unwrap_or(0);
        Ok(Anm { n_parts, anims })
    }

    fn anim(&self, name: &str) -> Option<&Anim> {
        self.anims.iter().find(|a| a.name.eq_ignore_ascii_case(name))
    }

    fn zero(&self) -> Pose {
        Pose { rot: vec![[0.0; 3]; self.n_parts], scale: 0.0, intensity: None }
    }

    fn apply(&self, pose: &mut Pose, k: &Key) {
        for &(p, a) in &k.parts {
            if p < pose.rot.len() {
                pose.rot[p] = a;
            }
        }
        pose.scale = k.scale;
        if k.intensity.is_some() {
            pose.intensity = k.intensity;
        }
    }

    /// `Anim_ApplyPose`: the first keyframe over a cleared pose.
    pub fn first_pose(&self, name: &str) -> Pose {
        let mut pose = self.zero();
        if let Some(k) = self.anim(name).and_then(|a| a.keys.first()) {
            self.apply(&mut pose, k);
        }
        pose
    }

    /// Length of a timed playback, ticks.
    pub fn duration(&self, name: &str) -> f32 {
        self.anim(name).map_or(0.0, |a| a.keys.iter().map(|k| (k.time.max(1) + k.pause) as f32).sum())
    }

    /// The pose `ticks` into a timed playback (held at the end).
    pub fn play(&self, name: &str, ticks: f32) -> Pose {
        let mut pose = self.zero();
        let Some(a) = self.anim(name) else { return pose };
        let mut t = ticks.max(0.0);
        for k in &a.keys {
            let mov = k.time.max(1) as f32;
            if t < mov {
                let f = t / mov;
                let mut dest = pose.clone();
                self.apply(&mut dest, k);
                for (r, d) in pose.rot.iter_mut().zip(&dest.rot) {
                    *r = [0, 1, 2].map(|i| lerp(r[i], d[i], f));
                }
                pose.scale = lerp(pose.scale, dest.scale, f);
                pose.intensity = match (pose.intensity, dest.intensity) {
                    (Some(p), Some(d)) => Some(lerp(p, d, f)),
                    (_, d) => d,
                };
                return pose;
            }
            self.apply(&mut pose, k);
            t -= mov;
            if t < k.pause as f32 {
                return pose;
            }
            t -= k.pause as f32;
        }
        pose
    }
}

/// What the viewer shows gates doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GateMode {
    Open,
    Closed,
    /// Open, hold, close, hold, repeat.
    Cycle,
}

/// Which of a gate's three models an instance is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Body,
    Center,
    Flare,
}

impl Role {
    fn names(self) -> (&'static str, &'static str) {
        match self {
            Role::Body => ("GATE_OPEN", "GATE_CLOSE"),
            Role::Center | Role::Flare => ("GATEWAY_OPEN", "GATEWAY_CLOSE"),
        }
    }
}

/// Seconds a cycling gate rests open and closed.
const CYCLE_HOLD: f32 = 3.0;

/// The pose of one gate model at `t` seconds. `cycle` gives the open and close durations (ticks)
/// shared by all three models of the gate type, so they stay in step.
pub fn gate_pose(anm: &Anm, role: Role, mode: GateMode, t: f32, cycle: (f32, f32)) -> Pose {
    let (open, close) = role.names();
    match mode {
        GateMode::Open => anm.first_pose(close),
        GateMode::Closed => anm.first_pose(open),
        GateMode::Cycle => {
            let hold = CYCLE_HOLD * TICK_HZ;
            let (d_open, d_close) = cycle;
            let tick = (t * TICK_HZ).rem_euclid(d_open + d_close + 2.0 * hold);
            if tick < d_open {
                anm.play(open, tick)
            } else if tick < d_open + hold {
                anm.first_pose(close)
            } else if tick < d_open + hold + d_close {
                anm.play(close, tick - d_open - hold)
            } else {
                anm.first_pose(open)
            }
        }
    }
}

/// `FUN_00425d10`: while the flare is lit, each tick re-rolls its intensity (128..255) and
/// jitters its scale by ±0.125.
pub fn flare_flicker(pose: &mut Pose, tick: u32, salt: u32) {
    if pose.intensity.is_some_and(|i| i >= 1.0) {
        let mut x = tick.wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B) ^ 0x6A09_E667;
        x ^= x >> 15;
        x = x.wrapping_mul(0x2C1B_3C6D);
        x ^= x >> 12;
        pose.intensity = Some(128.0 + (x & 0x7f) as f32);
        pose.scale += ((x >> 8) & 0x3fff) as f32 / 65536.0 - 0.125;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::AssetSource;

    #[test]
    fn stargate_poses() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../pff")) else { return };
        let a = Anm::parse(&src.read("stargate.anm").unwrap()).unwrap();
        assert_eq!(a.n_parts, 22);
        // Closed: only the pylon bases fan out.
        let closed = a.first_pose("GATE_OPEN");
        assert_eq!((closed.rot[2], closed.rot[3], closed.rot[4]), ([0.0, 0.0, 120.0], [0.0, 0.0, -120.0], [0.0; 3]));
        // Open: the end of GATE_OPEN is the start of GATE_CLOSE.
        let open = a.first_pose("GATE_CLOSE");
        let end = a.play("GATE_OPEN", a.duration("GATE_OPEN") + 1.0);
        assert_eq!(open.rot, end.rot);
        assert_eq!(open.rot[11], [-38.5, 0.0, 0.0]);
        // Halfway through the 3.5 s hinge swing (frame 1 starts after the 6 s pause).
        let mid = a.play("GATE_OPEN", 1.0 + 300.0 + 87.5);
        assert!((mid.rot[4][0] + 30.0).abs() < 1e-3, "{:?}", mid.rot[4]);
        // And GATE_CLOSE ends where GATE_OPEN starts.
        assert_eq!(a.play("GATE_CLOSE", a.duration("GATE_CLOSE") + 1.0).rot, closed.rot);

        let w = Anm::parse(&src.read("gate_way.anm").unwrap()).unwrap();
        assert_eq!((w.first_pose("GATEWAY_CLOSE").scale, w.first_pose("GATEWAY_CLOSE").intensity), (1.0, Some(255.0)));
        assert_eq!(w.first_pose("GATEWAY_OPEN").intensity, Some(0.0));
        let m = Anm::parse(&src.read("Megagate.anm").unwrap()).unwrap();
        assert_eq!(m.n_parts, 34);
        assert_eq!(m.first_pose("GATE_CLOSE").rot, m.play("GATE_OPEN", 1e6).rot);
    }
}
