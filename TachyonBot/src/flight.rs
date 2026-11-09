//! Circle autopilot. Engine axes: y up, z forward; yaw `a` turns +z toward +x, so the nose
//! points along (sin a, 0, cos a). Angles are 512 units per turn in 16.16.

use crate::proto::{ShipState, UNIT};
use std::f64::consts::TAU;

pub struct Circle {
    centre: [f64; 3],
    radius: f64,
    /// Radians per 50 Hz tick.
    step: f64,
    theta: f64,
    pos: [f64; 3],
    active: bool,
}

impl Circle {
    pub fn new(radius: f64, period_s: f64) -> Circle {
        Circle { centre: [0.0; 3], radius, step: TAU / (period_s * 50.0), theta: 0.0, pos: [0.0; 3], active: false }
    }

    /// Start a new circle that passes through `pos` (world units), heading +z there.
    pub fn start_at(&mut self, pos: [f64; 3]) {
        self.centre = [pos[0] - self.radius, pos[1], pos[2]];
        self.theta = 0.0;
        self.pos = pos;
        self.active = true;
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn position(&self) -> [f64; 3] {
        self.pos
    }

    pub fn radius(&self) -> f64 {
        self.radius
    }

    pub fn speed(&self) -> f64 {
        self.radius * self.step * 50.0
    }

    /// Advance one tick and return the state to report.
    pub fn tick(&mut self) -> ShipState {
        if !self.active {
            return ShipState::default();
        }
        self.theta = (self.theta + self.step) % TAU;
        let (s, c) = self.theta.sin_cos();
        let new = [self.centre[0] + self.radius * c, self.centre[1], self.centre[2] + self.radius * s];
        let vel = [0, 1, 2].map(|k| new[k] - self.pos[k]);
        self.pos = new;
        // Moving along (−sin θ, 0, cos θ) means yaw = −θ.
        let yaw = (-self.theta).rem_euclid(TAU) / TAU * 512.0;
        ShipState {
            pos: new.map(|v| (v * UNIT) as i32),
            vel: vel.map(|v| (v * UNIT) as i32),
            angles: [(yaw * UNIT) as i32, 0, 0],
            valid: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heading_follows_the_circle() {
        let mut c = Circle::new(1000.0, 10.0);
        c.start_at([0.0, 0.0, 0.0]);
        let s = c.tick();
        // First step moves +z with yaw ≈ 0.
        assert!(s.vel[2] > 0 && s.vel[0].abs() < s.vel[2] / 10);
        for _ in 0..124 {
            c.tick();
        }
        // A quarter turn later (125 ticks of 500) it's heading −x: yaw ≈ 384 units (270°).
        let s = c.tick();
        assert!(s.vel[0] < 0);
        let yaw = s.angles[0] as f64 / UNIT;
        assert!((yaw - 384.0).abs() < 2.0, "{yaw}");
        assert!((c.speed() - 628.3).abs() < 1.0);
    }
}
