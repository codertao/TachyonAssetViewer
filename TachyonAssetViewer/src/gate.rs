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
//! - **Particle paths** (`Anim_ParseParticlePath 0x4068c0`, `Anim_StepParticlePath 0x407760`,
//!   `Anim_EmitParticle 0x407610`, particle think `0x4074f0`): see `ParticlePath`. Both
//!   gate body anms have them (smoke.pak / vlt_part.pak).

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

/// One `<anim>_particle_path_NN` group. During playback, after `wait` ticks, each of the next
/// `life` ticks advances the path's values one step from `*_start` towards `*_end` and, if
/// `rand % 50 <= volume`, emits a particle:
/// - it starts at the source part's attach point and flies in a straight line to the
///   destination's over its lifetime (`particle_life`, ticks);
/// - each end point is jittered by up to ±spread/2 per world axis;
/// - its scale and intensity move linearly from the source's value to the destination's.
///
/// `src` / `dest` values are (intensity, scale, spread). The particle is the named PAK drawn as a
/// whole-instance billboard (instance flags 0x94).
#[derive(Clone, Debug)]
pub struct ParticlePath {
    wait: u32,
    life: u32,
    /// Index into `Anm::particle_paks`.
    model: usize,
    src_part: (usize, usize),
    dest_part: (usize, usize),
    src: [[f32; 3]; 2],
    dest: [[f32; 3]; 2],
    volume: [f32; 2],
    /// Ticks.
    particle_life: [f32; 2],
}

/// A live particle: world (engine-axis) position, instance scale and intensity (0..1).
#[derive(Clone, Copy, Debug)]
pub struct Spark {
    pub model: usize,
    pub pos: [f32; 3],
    pub scale: f32,
    pub intensity: f32,
}

#[derive(Clone, Debug)]
struct Anim {
    name: String,
    keys: Vec<Key>,
    paths: Vec<ParticlePath>,
}

/// One `.anm` file.
#[derive(Clone, Debug)]
pub struct Anm {
    n_parts: usize,
    anims: Vec<Anim>,
    /// `particle_paks` `PARTICLE: NAME, pak, pal`: the pak file of each particle type.
    pub particle_paks: Vec<String>,
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
        let mut particles: Vec<(String, String)> = vec![];
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
                    ("particle_paks", "PARTICLE") => {
                        if let (Some(Value::Str(n)), Some(Value::Str(f))) = (it.values.first(), it.values.get(1)) {
                            particles.push((n.to_uppercase(), f.clone()));
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
            let mut paths = vec![];
            for k in 0.. {
                let gname = format!("{}_particle_path_{k:02}", name.to_lowercase());
                let Some(g) = groups.iter().find(|g| g.name.to_lowercase() == gname) else { break };
                let vals = |key: &str| g.items.iter().find(|i| i.key.eq_ignore_ascii_case(key)).map(|i| &i.values[..]).unwrap_or(&[]);
                let f = |key: &str, i: usize| vals(key).get(i).and_then(|v| v.as_f32()).unwrap_or(0.0);
                let v3 = |key: &str| [f(key, 0), f(key, 1), f(key, 2)];
                let part = |key: &str| {
                    let p = match vals(key).first() {
                        Some(Value::Str(n)) => part_index(n).unwrap_or(0),
                        _ => 0,
                    };
                    (p, f(key, 1).max(0.0) as usize)
                };
                let model = match vals("PARTICLES").first() {
                    Some(Value::Str(n)) => particles.iter().position(|(p, _)| p == &n.to_uppercase()),
                    _ => None,
                };
                let (Some(model), life) = (model, secs(vals("LIFETIME").first())) else { continue };
                if life == 0 {
                    continue;
                }
                paths.push(ParticlePath {
                    wait: secs(vals("WAIT").first()),
                    life,
                    model,
                    src_part: part("SRC_PART"),
                    dest_part: part("DEST_PART"),
                    src: [v3("SRC_START"), v3("SRC_END")],
                    dest: [v3("DEST_START"), v3("DEST_END")],
                    // Integers in the file (`Cbin_GetParam` type 1).
                    volume: [f("VOLUME_START", 0).trunc(), f("VOLUME_END", 0).trunc()],
                    particle_life: [f("PARTICLE_LIFE_START", 0) * TICK_HZ, f("PARTICLE_LIFE_END", 0) * TICK_HZ],
                });
            }
            anims.push(Anim { name, keys, paths });
        }
        let n_parts = defines.iter().map(|(_, i)| i + 1).max().unwrap_or(0);
        Ok(Anm { n_parts, anims, particle_paks: particles.into_iter().map(|(_, f)| f).collect() })
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

    pub fn has_particles(&self) -> bool {
        self.anims.iter().any(|a| !a.paths.is_empty())
    }

    /// The particles alive `ticks` into a timed playback of `name`. `attach(tick, part, point)`
    /// gives the world position of a part's attach point at an earlier tick (the pose then).
    pub fn particles(&self, name: &str, ticks: f32, salt: u32, mut attach: impl FnMut(u32, usize, usize) -> [f32; 3]) -> Vec<Spark> {
        let mut out = vec![];
        let Some(a) = self.anim(name) else { return out };
        for (pi, p) in a.paths.iter().enumerate() {
            let max_n = p.particle_life[0].max(p.particle_life[1]).ceil() + 1.0;
            // Emission ticks e = wait + j, j = 1..=life; only recent ones can still be alive.
            let first = ((ticks - p.wait as f32 - max_n).floor().max(1.0)) as u32;
            let last = ((ticks - p.wait as f32).floor().min(p.life as f32)).max(0.0) as u32;
            for j in first..=last {
                let e = p.wait + j;
                let age = ticks - e as f32;
                let f = j as f32 / p.life as f32;
                let at = |v: &[[f32; 3]; 2], i: usize| lerp(v[0][i], v[1][i], f);
                let n = lerp(p.particle_life[0], p.particle_life[1], f).floor();
                if n < 1.0 || age < 0.0 || age >= n {
                    continue;
                }
                let r = |k: u32| hash3(salt, (pi as u32) << 16 | j, k);
                if (r(0) % 50) as f32 > lerp(p.volume[0], p.volume[1], f).trunc() {
                    continue;
                }
                // FUN_00407560: attach point + (rand % spread − spread / 2) on each axis.
                let jitter = |base: [f32; 3], spread: f32, k: u32| {
                    let s = (spread * 65536.0).max(1.0) as u32;
                    [0, 1, 2].map(|i| base[i] + ((r(k + i as u32) % s) as f32 - (s / 2) as f32) / 65536.0)
                };
                let src = jitter(attach(e, p.src_part.0, p.src_part.1), at(&p.src, 2), 1);
                let dst = jitter(attach(e, p.dest_part.0, p.dest_part.1), at(&p.dest, 2), 4);
                let g = age / n;
                // The instance intensity is stored 16.16 but clamped to 0..255 when drawn
                // (`FUN_004b4c10`), so anything above 1/256 is full brightness.
                let raw = lerp(at(&p.src, 0), at(&p.dest, 0), g) * 65536.0;
                out.push(Spark {
                    model: p.model,
                    pos: [0, 1, 2].map(|i| lerp(src[i], dst[i], g)),
                    scale: lerp(at(&p.src, 1), at(&p.dest, 1), g),
                    intensity: raw.clamp(0.0, 255.0) / 255.0,
                });
            }
        }
        out
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
            // TIME 0 snaps on its first step (`Anim_StepFrame` adds the step and reaches the
            // keyframe in the same tick), so no blend is ever drawn.
            if t < mov && k.time > 0 {
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

fn hash3(a: u32, b: u32, c: u32) -> u32 {
    let mut x = a.wrapping_mul(0x9E37_79B9) ^ b.wrapping_mul(0x85EB_CA6B) ^ c.wrapping_mul(0xC2B2_AE35) ^ 0x27d4_eb2f;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    x
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
    pub fn names(self) -> (&'static str, &'static str) {
        match self {
            Role::Body => ("GATE_OPEN", "GATE_CLOSE"),
            Role::Center | Role::Flare => ("GATEWAY_OPEN", "GATEWAY_CLOSE"),
        }
    }
}

/// Seconds a cycling gate rests open and closed.
const CYCLE_HOLD: f32 = 3.0;

/// In cycle mode: ticks since the open and close playbacks started (negative before the close).
pub fn cycle_ticks(t: f32, cycle: (f32, f32)) -> (f32, f32) {
    let hold = CYCLE_HOLD * TICK_HZ;
    let tick = (t * TICK_HZ).rem_euclid(cycle.0 + cycle.1 + 2.0 * hold);
    (tick, tick - cycle.0 - hold)
}

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
        // Zero-TIME first keyframes snap: no blend from the cleared pose at the start.
        for name in ["GATE_OPEN", "GATE_CLOSE"] {
            for t in [0.0, 0.5, 0.99] {
                assert_eq!(a.play(name, t).rot, a.first_pose(name).rot, "{name} at {t}");
            }
        }
        // And GATE_CLOSE ends where GATE_OPEN starts.
        assert_eq!(a.play("GATE_CLOSE", a.duration("GATE_CLOSE") + 1.0).rot, closed.rot);

        // Particles: 20 paths in GATE_OPEN, blue / violet streams from the tips to the centre.
        assert_eq!(a.particle_paks.len(), 2);
        let tip = |_: u32, part: usize, _: usize| if part == 0 { [0.0; 3] } else { [30.0, 0.0, 0.0] };
        let mid = a.particles("GATE_OPEN", 150.0, 7, tip);
        assert!(mid.len() > 100 && mid.len() < 2000, "{}", mid.len());
        assert!(mid.iter().all(|p| p.pos[0] > -2.0 && p.pos[0] < 32.0 && p.intensity > 0.99));
        assert!(a.particles("GATE_OPEN", 0.5, 7, tip).is_empty());
        // The centre burst after the 6 s wait sprays out to ±15.
        let burst = a.particles("GATE_OPEN", 330.0, 7, |_, _, _| [0.0; 3]);
        assert!(burst.iter().any(|p| p.pos[0].abs() > 3.0));
        assert!(!a.has_particles() || a.particles("GATE_OPEN", 1e5, 7, tip).is_empty());
        assert!(Anm::parse(&src.read("Megagate.anm").unwrap()).unwrap().has_particles());

        let w = Anm::parse(&src.read("gate_way.anm").unwrap()).unwrap();
        assert_eq!((w.first_pose("GATEWAY_CLOSE").scale, w.first_pose("GATEWAY_CLOSE").intensity), (1.0, Some(255.0)));
        assert_eq!(w.first_pose("GATEWAY_OPEN").intensity, Some(0.0));
        let m = Anm::parse(&src.read("Megagate.anm").unwrap()).unwrap();
        assert_eq!(m.n_parts, 34);
        assert_eq!(m.first_pose("GATE_CLOSE").rot, m.play("GATE_OPEN", 1e6).rot);
    }
}

