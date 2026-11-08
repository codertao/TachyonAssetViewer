//! Time- and camera-driven sector effects: lightning flashes and fog sheets. Both are ports of
//! space.exe's per-tick logic (`decomp/notes/formats.md` "Lightning" and "Fog").

use std::f32::consts::TAU;

/// The game's world update rate (`ModeLoop_FixedStep50Hz 0x44ead0`).
const TICK_HZ: f32 = 50.0;

/// `FUN_00468320`, run once per world tick in lightning sectors.
///
/// - Each tick has a 5-in-1000 chance to start a flash: 12–23 ticks long, with a random bolt
///   frame (0–15).
/// - During a flash, every tick sets the light level to a random 100–255.
/// - Every tick the level moves at most 6 towards `LIGHTNING_AMBIENT_LIGHT / 2`.
/// - Diffuse light is scaled by level / 256 (`FUN_004b4df0`). While the level is above the
///   ambient, `LIGHTNING_PAK` is drawn over the sky with its self-lit materials at
///   min(level, 255) (`FUN_00468440`, instance flag 0x80).
#[derive(Clone, Debug)]
pub struct Lightning {
    ambient: i32,
    /// 16.16.
    level: i32,
    counter: u32,
    frame: u32,
    tick: u64,
    rng: u32,
}

impl Lightning {
    pub fn new(lightning_ambient_light: i32) -> Self {
        let ambient = lightning_ambient_light >> 1;
        Lightning { ambient, level: ambient << 16, counter: 0, frame: 0, tick: 0, rng: 0x2545_F491 }
    }

    fn rand(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x >> 8
    }

    fn step(&mut self) {
        if self.rand() % 1000 < 5 {
            self.counter = self.rand() % 12 + 12;
            self.frame = self.rand() & 15;
        }
        if self.counter > 0 {
            self.counter -= 1;
            self.level = ((self.rand() % 156 + 100) << 16) as i32;
        }
        let d = (self.ambient << 16) - self.level;
        self.level += d.clamp(-0x60000, 0x60000);
    }

    /// Run the simulation up to `t` seconds (restarting if time went backwards).
    pub fn advance_to(&mut self, t: f32) {
        let target = (t.max(0.0) * TICK_HZ) as u64;
        if target < self.tick {
            *self = Lightning::new(self.ambient << 1);
        }
        // The level settles within a second; skip long gaps.
        if target - self.tick > 5000 {
            self.tick = target - 5000;
        }
        while self.tick < target {
            self.step();
            self.tick += 1;
        }
    }

    /// Multiplier on the diffuse light.
    pub fn diffuse_scale(&self) -> f32 {
        (self.level >> 8) as f32 / 65536.0
    }

    /// While flashing: (bolt animation counter, brightness 0..1).
    pub fn bolt(&self) -> Option<(usize, f32)> {
        let l = self.level >> 16;
        (l > self.ambient).then(|| (self.frame as usize, l.min(255) as f32 / 255.0))
    }
}

/// One fog sheet to draw this frame.
#[derive(Clone, Copy, Debug)]
pub struct SheetLayer {
    /// Fog-sheet depth units; the camera's forward motion moves sheets 0.4 per world unit, so
    /// the sheet sits about 2.5 × depth world units ahead.
    pub depth: f32,
    /// Texture offset (u, v).
    pub offset: [f32; 2],
    /// Tint multiplier on `FRONT_RGB`, 0..60/256.
    pub fade: f32,
}

/// `FUN_004b9250` (draw), `FUN_004b9a50` (camera translation) and `FUN_004b9b70` (camera
/// rotation): five screen-filling textured sheets, 50 units apart, that the camera flies
/// through. Each sheet's texture is screen-aligned and scaled with its depth; turning slides
/// them all together, strafing slides near ones more, and flying forward brings them closer
/// until the nearest fades out and is recycled at the back.
#[derive(Clone, Debug)]
pub struct FogSheets {
    /// Depth of the nearest sheet, kept in (1, 51].
    near: f32,
    scroll: [f32; 2],
    /// Per-sheet texture offsets (`0x6c9040`, 8 slots).
    slots: [[f32; 2]; 8],
    /// Accumulated strafe (`0x6c9b30`, `0x6c90a8`).
    strafe: [f32; 2],
    /// Accumulated roll, turns.
    roll: f32,
}

const SHEET_STEP: f32 = 50.0;
const SHEET_SHIFT: [f32; 2] = [0.28, 0.14];
/// World-unit deltas reach the fog code scaled by 65536 / 163840.
const FEED: f32 = 0.4;

fn wrap3(x: f32) -> f32 {
    if x > 3.0 {
        x - 3.0
    } else if x < -3.0 {
        x + 3.0
    } else {
        x
    }
}

impl Default for FogSheets {
    fn default() -> Self {
        FogSheets { near: 2.0, scroll: [0.0; 2], slots: [[0.0; 2]; 8], strafe: [0.0; 2], roll: 0.0 }
    }
}

impl FogSheets {
    /// Camera motion along its own right, up and forward axes, in world units.
    pub fn translate(&mut self, right: f32, up: f32, forward: f32) {
        self.strafe[0] += right * FEED * 16.0;
        self.strafe[1] -= up * FEED * 16.0;
        self.near -= forward * FEED;
        let mut guard = 0;
        while self.near > 51.0 && guard < 64 {
            self.near -= SHEET_STEP;
            self.scroll = [0, 1].map(|i| self.scroll[i] - SHEET_SHIFT[i]);
            self.slots.copy_within(0..7, 1);
            guard += 1;
        }
        while self.near <= 1.0 && guard < 64 {
            self.near += SHEET_STEP;
            self.scroll = [0, 1].map(|i| self.scroll[i] + SHEET_SHIFT[i]);
            self.slots.copy_within(1..8, 0);
            guard += 1;
        }
        if !(1.0..=51.0).contains(&self.near) {
            self.near = (self.near - 1.0).rem_euclid(SHEET_STEP) + 1.0;
        }
    }

    /// Camera rotation since the last frame, in turns (yaw right, pitch up, roll).
    pub fn rotate(&mut self, yaw: f32, pitch: f32, roll: f32) {
        // The game's angle unit here is a quarter turn.
        let (mut du, mut dv) = (yaw * 4.0 * 8.0, -pitch * 4.0 * 8.0);
        let (s, c) = (roll * TAU).sin_cos();
        (du, dv) = (c * du - s * dv, s * du + c * dv);
        self.strafe = [c * self.strafe[0] - s * self.strafe[1], s * self.strafe[0] + c * self.strafe[1]];
        self.roll -= roll;
        let (rs, rc) = self.rot();
        let shift = [(rc * du - rs * dv) / 384.0, (rs * du + rc * dv) / 384.0];
        for (k, slot) in self.slots.iter_mut().enumerate() {
            let d = self.near + SHEET_STEP * k as f32;
            slot[0] = wrap3(slot[0] + shift[0] * d * 0.01);
            slot[1] = wrap3(slot[1] + shift[1] * d * 0.01);
        }
        self.scroll = self.scroll.map(wrap3);
    }

    /// (sin, cos) of the accumulated roll.
    pub fn rot(&self) -> (f32, f32) {
        (self.roll * TAU).sin_cos()
    }

    /// The sheets to draw, nearest first.
    pub fn layers(&self) -> Vec<SheetLayer> {
        let (s, c) = self.rot();
        let cam = [(c * self.strafe[0] - s * self.strafe[1]) / 384.0, (s * self.strafe[0] + c * self.strafe[1]) / 384.0];
        (0..5)
            .filter_map(|e| {
                let d = self.near + SHEET_STEP * e as f32;
                if d >= 200.0 {
                    return None;
                }
                let off = [0, 1].map(|i| e as f32 * SHEET_SHIFT[i] + self.scroll[i] + self.slots[e][i] + cam[i]);
                Some(SheetLayer { depth: d, offset: off, fade: sheet_fade(d) / 256.0 })
            })
            .collect()
    }
}

/// Sheet brightness (0..60) by depth: fades in from depth 1 to 41, full to 80, half by 160,
/// out at 200.
fn sheet_fade(d: f32) -> f32 {
    let f = if d >= 160.0 {
        (200.0 - d) * 0.75
    } else if d >= 80.0 {
        30.0 + (160.0 - d) * 0.375
    } else if d < 41.0 {
        60.0 - (41.0 - d) * 1.5
    } else {
        60.0
    };
    f.clamp(0.0, 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lightning_flashes_and_settles() {
        let mut l = Lightning::new(100);
        let (mut flashes, mut was) = (0, false);
        for i in 0..50 * 120 {
            l.advance_to(i as f32 / 50.0);
            let on = l.bolt().is_some();
            flashes += (on && !was) as usize;
            was = on;
        }
        // 0.5% per tick: about 30 flashes in two minutes.
        assert!((10..60).contains(&flashes), "{flashes}");
        let mut q = Lightning::new(100);
        q.advance_to(0.0);
        assert!((q.diffuse_scale() - 50.0 / 256.0).abs() < 1e-3);
    }

    #[test]
    fn sheets_recycle_as_camera_advances() {
        let mut f = FogSheets::default();
        assert_eq!(f.layers().len(), 4);
        f.translate(0.0, 0.0, 100.0);
        let l = f.layers();
        assert!(l[0].depth > 1.0 && l[0].depth <= 51.0);
        assert!((sheet_fade(80.0) - 60.0).abs() < 1e-3 && sheet_fade(200.0) == 0.0 && sheet_fade(1.0) == 0.0);
    }
}
