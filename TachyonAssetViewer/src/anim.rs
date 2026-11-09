//! State animation: spinning parts, blinking parts and scale curves, as space.exe runs them.
//!
//! - `Model_UpdateState 0x4b27d0`: each part animator sets its part's rotation to
//!   `base + rate × t` (engine angle units, 512 per turn) and may toggle the part's hidden bit
//!   (mode 1 random, 2 periodic, 4 copy another part).
//! - `State_EvalCurves 0x4b2640` / `Curve_Eval 0x4b25a0`: scale curves replace the object's scale.
//! - `Part_ComputeTransform 0x4b3f90`: a part's matrix is its own rotation, then each rotation
//!   parent's (skipping parents with flag 0x100), times the scale. Billboards (0x40) get the
//!   scale only.
//! - `FUN_004b3eb0`: child k of a part sits at `part.pos + part.matrix · mesh.pivots[k]`.

use crate::math::{self, Mat3};
use crate::pak::{self, Curve, Pak, PartAnim};

/// One part's pose in the PAK's own (engine-axis) model space.
#[derive(Clone, Copy, Debug)]
pub struct PartPose {
    /// Rotation · scale. For billboards, the scale only.
    pub m: Mat3,
    pub pos: [f32; 3],
    pub hidden: bool,
}

#[derive(Clone, Debug)]
struct RigPart {
    parent: Option<usize>,
    first_child: usize,
    n_children: usize,
    flags: u32,
    pivots: Vec<[f32; 3]>,
    attach: Vec<[f32; 3]>,
}

/// The part tree of one PAK LOD plus the animation of one state.
#[derive(Clone, Debug)]
pub struct Rig {
    parts: Vec<RigPart>,
    anims: Vec<PartAnim>,
    effect_flags: u32,
    curves: [Curve; 4],
}

/// The game re-rolls random blinks and flips parity curves once per update; the viewer uses a
/// fixed rate so the flicker looks similar at any refresh rate.
const UPDATE_HZ: f32 = 20.0;

/// Engine angle units (512 per turn) to degrees.
const UNIT_DEG: f32 = 360.0 / 512.0;

fn rand16(tick: u32, salt: u32) -> u32 {
    let mut x = tick.wrapping_mul(0x9E37_79B9) ^ salt.wrapping_mul(0x85EB_CA6B) ^ 0x27d4_eb2f;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x = x.wrapping_mul(0x297A_2D39);
    x ^= x >> 15;
    x & 0xFFFF
}

fn eval_curve(c: &Curve, t: f32, tick: u32, salt: u32) -> f32 {
    if c.mode & 1 != 0 {
        c.a + c.c * rand16(tick, salt) as f32 / 65536.0
    } else if c.mode & 2 != 0 {
        if tick & 1 == 0 { c.a } else { c.b }
    } else if c.mode & 4 != 0 {
        // 256-entry sine table indexed by (d × t) & 0xff.
        let idx = (c.d_raw as f64 * t as f64).rem_euclid(256.0);
        c.a + c.c * (idx * std::f64::consts::TAU / 256.0).sin() as f32
    } else {
        c.b
    }
}

impl Rig {
    pub fn new(pak: &Pak, lod: usize, state: usize) -> Rig {
        let parts = pak
            .lods
            .get(lod)
            .map(|l| {
                l.parts
                    .iter()
                    .map(|p| RigPart {
                        parent: p.parent,
                        first_child: p.first_child,
                        n_children: p.n_children,
                        flags: p.flags,
                        pivots: pak.meshes.get(&p.mesh_ofs).map(|m| m.pivots.clone()).unwrap_or_default(),
                        attach: pak.meshes.get(&p.mesh_ofs).map(|m| m.attach.clone()).unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let (anims, effect_flags, curves) = match pak.states.get(state) {
            Some(s) => (s.part_anims.clone(), s.effect_flags, s.curves),
            None => (vec![], 0, [Curve::default(); 4]),
        };
        Rig { parts, anims, effect_flags, curves }
    }

    /// Whether anything moves, blinks or scales over time.
    pub fn is_animated(&self) -> bool {
        self.effect_flags & 0xFE != 0
            || self.anims.iter().any(|a| a.part < self.parts.len() && (a.mode & 7 != 0 || a.rate.iter().any(|&r| r != 0.0)))
    }

    pub fn spinning_parts(&self) -> usize {
        self.anims.iter().filter(|a| a.part < self.parts.len() && a.rate.iter().any(|&r| r != 0.0)).count()
    }

    pub fn blinking_parts(&self) -> usize {
        self.anims.iter().filter(|a| a.part < self.parts.len() && a.mode & 7 != 0).count()
    }

    /// Object scale from the state's curves (1.0 on axes no curve drives).
    fn scale(&self, t: f32, tick: u32) -> [f32; 3] {
        let f = self.effect_flags;
        let mut s = [1.0f32; 3];
        let c = |i: usize| eval_curve(&self.curves[i], t, tick, 0x5CA1E + i as u32);
        if f & 2 != 0 {
            let v = c(1);
            return [v; 3];
        }
        if f & 4 != 0 {
            s[0] = c(1);
        }
        if f & 8 != 0 {
            s[1] = c(2);
        }
        if f & 0x10 != 0 {
            s[2] = c(3);
        }
        if f & 0x20 != 0 {
            let v = c(1);
            s[0] = v;
            s[1] = v;
        } else if f & 0x40 != 0 {
            let v = c(2);
            s[1] = v;
            s[2] = v;
        } else if f & 0x80 != 0 {
            let v = c(3);
            s[2] = v;
            s[0] = v;
        }
        s
    }

    /// A part's attach point in model space under `poses` (`FUN_00407560`): the transformed
    /// point, or the model origin if the part has no such point.
    pub fn attach_point(&self, poses: &[PartPose], part: usize, idx: usize) -> [f32; 3] {
        match (self.parts.get(part), poses.get(part)) {
            (Some(rp), Some(pp)) => match rp.attach.get(idx) {
                Some(&a) => math::add(pp.pos, pp.m.mul_vec(a)),
                None => [0.0; 3],
            },
            _ => [0.0; 3],
        }
    }

    pub fn pose(&self, t: f32) -> Vec<PartPose> {
        self.pose_with(t, None, 1.0)
    }

    /// `pose`, with part rotations (degrees) from an `.anm` replacing the state's
    /// (`Anim_CopyPartRotations` writes the same fields) and an extra uniform instance scale.
    pub fn pose_with(&self, t: f32, rot_deg: Option<&[[f32; 3]]>, extra_scale: f32) -> Vec<PartPose> {
        let n = self.parts.len();
        let tick = (t * UPDATE_HZ) as u32;
        let mut rot = vec![[0.0f32; 3]; n];
        let mut hidden: Vec<bool> = self.parts.iter().map(|p| p.flags & pak::PART_HIDDEN != 0).collect();
        for (k, a) in self.anims.iter().enumerate() {
            if a.part >= n {
                continue;
            }
            rot[a.part] = [0, 1, 2].map(|i| a.base[i] + a.rate[i] * t);
            if a.mode & 1 != 0 {
                hidden[a.part] = a.threshold < rand16(tick, k as u32);
            } else if a.mode & 2 != 0 {
                let phase = (a.other as i64 + (a.period_raw as f64 * t as f64) as i64) & 0xFFFF;
                hidden[a.part] = (a.threshold as i64) < phase;
            } else if a.mode & 4 != 0 && (a.other as usize) < n {
                hidden[a.part] = hidden[a.other as usize];
            }
        }
        if let Some(r) = rot_deg {
            for (dst, src) in rot.iter_mut().zip(r) {
                *dst = src.map(|a| a / UNIT_DEG);
            }
        }
        let s = self.scale(t, tick).map(|v| v * extra_scale);
        let smat = Mat3([[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]]);
        let own: Vec<Mat3> = rot.iter().map(|r| Mat3::from_engine_euler_deg(r.map(|a| a * UNIT_DEG))).collect();
        let billboard = |i: usize| self.parts[i].flags & pak::PART_BILLBOARD != 0;
        let mut m = Vec::with_capacity(n);
        for i in 0..n {
            if billboard(i) {
                m.push(smat);
                continue;
            }
            let mut r = own[i];
            let mut q = self.parts[i].parent;
            let mut guard = 0;
            while let Some(j) = q {
                if j >= n || guard > n {
                    break;
                }
                if self.parts[j].flags & 0x100 == 0 {
                    r = own[j].mul(&r);
                }
                q = self.parts[j].parent;
                guard += 1;
            }
            m.push(r.mul(&smat));
        }
        // Positions: parts in index order write their children's positions (later writers win,
        // which is how explicit sub-trees override the root's blanket children).
        let mut pos = vec![[0.0f32; 3]; n];
        for i in 0..n {
            if billboard(i) {
                continue;
            }
            let p = &self.parts[i];
            for k in 0..p.n_children {
                let c = p.first_child + k;
                if c >= n {
                    break;
                }
                let pivot = p.pivots.get(k).copied().unwrap_or([0.0; 3]);
                pos[c] = math::add(pos[i], m[i].mul_vec(pivot));
            }
        }
        (0..n).map(|i| PartPose { m: m[i], pos: pos[i], hidden: hidden[i] }).collect()
    }
}
