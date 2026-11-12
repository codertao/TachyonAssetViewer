//! The server's world: the startup name tables (model pool and descriptor registry, filled in
//! load order) and the entities in the map's quadrant.

use std::collections::HashMap;
use std::rc::Rc;
use tachyon_core::des::Des;
use tachyon_core::net::proto::RecordOut;

/// 16.16 fixed point.
pub const FX: i32 = 1 << 16;
/// Angles: 512 units per turn in 16.16.
pub const ANGLE_MASK: i32 = 0x1ff_ffff;

/// `g_ObjModelPool` and `g_DescRegistry` as the server fills them: first free slot, 256 each.
#[derive(Default)]
pub struct Tables {
    pub models: Vec<Option<(String, String)>>,
    pub descs: Vec<Option<String>>,
    model_idx: HashMap<String, u8>,
    desc_idx: HashMap<String, (u16, Option<u8>)>,
}

impl Tables {
    pub fn new() -> Self {
        Tables { models: vec![None; 256], descs: vec![None; 256], ..Default::default() }
    }

    /// Register a descriptor (and its pool model) like `Desc_CreateByType`; returns
    /// `(desc slot, model slot)`. Re-registering returns the existing slots.
    pub fn add_des(&mut self, des: &Des) -> Option<(u16, Option<u8>)> {
        let key = des.file.to_ascii_uppercase();
        if let Some(&r) = self.desc_idx.get(&key) {
            return Some(r);
        }
        let slot = self.descs.iter().position(Option::is_none)? as u16;
        self.descs[slot as usize] = Some(des.file.clone());
        let model = des.pool_model().and_then(|(pak, pal)| self.add_model(&pak, &pal));
        self.desc_idx.insert(key, (slot, model));
        Some((slot, model))
    }

    /// `ModelPool_Load`: find by pak name or take the first free slot.
    pub fn add_model(&mut self, pak: &str, pal: &str) -> Option<u8> {
        let key = pak.to_ascii_uppercase();
        if let Some(&s) = self.model_idx.get(&key) {
            return Some(s);
        }
        let s = self.models.iter().position(Option::is_none)? as u8;
        self.models[s as usize] = Some((pak.to_string(), pal.to_string()));
        self.model_idx.insert(key, s);
        Some(s)
    }

    pub fn model_entries(&self) -> Vec<(u16, String, String)> {
        self.models.iter().enumerate().filter_map(|(i, m)| m.as_ref().map(|(a, b)| (i as u16, a.clone(), b.clone()))).collect()
    }

    pub fn desc_entries(&self) -> Vec<(u16, String)> {
        self.descs.iter().enumerate().filter_map(|(i, d)| d.as_ref().map(|n| (i as u16, n.clone()))).collect()
    }
}

/// The power block (`PowerBlock`; `shields.md`). Values in the game's units.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)] // laser/burner are used by power ops and regen (M2)
pub struct Power {
    pub laser: i32,
    pub shield_front: i32,
    pub shield_rear: i32,
    pub shield_rate: i32,
    pub burner: i32,
    pub hull: i32,
    pub shield_max: i32,
    pub hull_max: i32,
}

impl Power {
    /// Initial values for a ship (`Ship_LoadDes` scales shields and hull by
    /// `g_ShipDesStatScale` = 153/256).
    pub fn for_des(des: &Des) -> Power {
        let Some(s) = &des.ship else { return Power::default() };
        let shield = (s.shield_max as i32 * 153) >> 8;
        let hull = (s.hull as i32 * 153) >> 8;
        Power {
            laser: s.laser_energy as i32,
            shield_front: shield,
            shield_rear: shield,
            shield_rate: 0,
            burner: s.afterburner.max_energy as i32,
            hull,
            shield_max: shield,
            hull_max: hull,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// A connected player's ship (conn slot).
    Player(u16),
    /// Placed from the SPX (M2).
    #[allow(dead_code)]
    Static,
}

pub struct Entity {
    pub id: u32,
    pub owner: Owner,
    pub des: Rc<Des>,
    pub desc_slot: u16,
    pub model: u8,
    /// Shown (smoothed) state, 16.16.
    pub pos: [i32; 3],
    pub vel: [i32; 3],
    pub angles: [i32; 3],
    /// Network target for players (`TachSrv_PlayerThink` eases toward it).
    pub net_pos: [i32; 3],
    pub net_angles: [i32; 3],
    /// DCB +0xc0 (0x01|0x04 for players; 0x40/0x80 front/rear shield up).
    pub state_flags: u8,
    pub throttle: u8,
    pub team: u8,
    /// DCB +0xc3: bit 0 = not network-visible (dead); `& 3 == 3` = static object.
    pub net_flags: u8,
    /// 16.16 render scale.
    pub scale: i32,
    pub power: Power,
    pub target: u32,
}

impl Entity {
    pub fn record(&self, extra_flags: u8, ext: bool) -> RecordOut {
        RecordOut {
            id: self.id,
            model: self.model,
            flags: self.state_flags | extra_flags,
            throttle: self.throttle,
            team: self.team,
            pos: self.pos,
            vel: self.vel,
            angles: self.angles,
            ext: ext.then(|| ((self.scale >> 8) as u16, self.desc_slot as i16)),
        }
    }

    /// `TachSrv_PlayerThink`: dead-reckon the target, ease the shown position 1/16 and the angles
    /// 1/8 per tick (the ×0x80 wraps the difference so it turns the short way).
    pub fn smooth_step(&mut self) {
        for k in 0..3 {
            self.net_pos[k] = self.net_pos[k].wrapping_add(self.vel[k]);
            self.pos[k] = self.pos[k].wrapping_add(self.net_pos[k].wrapping_sub(self.pos[k]) >> 4);
            let d = self.net_angles[k].wrapping_sub(self.angles[k]).wrapping_mul(0x80) >> 10;
            self.angles[k] = self.angles[k].wrapping_add(d) & ANGLE_MASK;
        }
        let mut f = self.state_flags & !0xc0;
        let one_pct = self.power.shield_max / 100;
        if self.power.shield_front > one_pct {
            f |= 0x40;
        }
        if self.power.shield_rear > one_pct {
            f |= 0x80;
        }
        self.state_flags = f;
    }
}

/// Distance in 16.16 units (the game's `distance_between`), as i64 to avoid overflow.
pub fn distance(a: [i32; 3], b: [i32; 3]) -> i64 {
    let d: [f64; 3] = std::array::from_fn(|k| a[k] as f64 - b[k] as f64);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() as i64
}

pub struct World {
    pub tables: Tables,
    /// In quadrant-list order (new entities go to the front, as `Quad_LinkDynamic` does).
    pub order: Vec<u32>,
    pub entities: HashMap<u32, Entity>,
    next_id: u32,
}

impl World {
    pub fn new() -> Self {
        World { tables: Tables::new(), order: vec![], entities: HashMap::new(), next_id: 0x100 }
    }

    /// `DcbPool_Alloc`: odd ids from a counter stepping by 2.
    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id | 1;
        self.next_id = self.next_id.wrapping_add(2);
        id
    }

    pub fn spawn(&mut self, e: Entity) -> u32 {
        let id = e.id;
        self.order.insert(0, id);
        self.entities.insert(id, e);
        id
    }

    pub fn remove(&mut self, id: u32) -> Option<Entity> {
        self.order.retain(|&x| x != id);
        self.entities.remove(&id)
    }

    pub fn get(&self, id: u32) -> Option<&Entity> {
        self.entities.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Entity> {
        self.entities.get_mut(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoothing_converges_and_wraps() {
        let des = Rc::new(Des::default());
        let mut e = Entity {
            id: 1, owner: Owner::Static, des, desc_slot: 0, model: 0, pos: [0; 3], vel: [0; 3],
            angles: [ANGLE_MASK - 10 * FX, 0, 0], net_pos: [100 * FX, 0, 0], net_angles: [10 * FX, 0, 0],
            state_flags: 5, throttle: 0, team: 0, net_flags: 0, scale: FX, power: Power::default(), target: 0,
        };
        for _ in 0..200 {
            e.smooth_step();
        }
        assert!((e.pos[0] - 100 * FX).abs() < FX);
        // Turned the short way across 0 (from -10 to +10), not the long way round.
        assert!((e.angles[0] - 10 * FX).abs() < FX, "{}", e.angles[0] as f64 / FX as f64);
    }
}
