//! Tachyon's game layer: the join blob, PLAYERDATA, startup records, snapshot records.
//! Layouts are from `decomp/notes/multiplayer.md`.

use crate::net::netsock::{cstr, fixed_str};

/// One world unit in 16.16 fixed point.
pub const UNIT: f64 = 65536.0;

pub fn u16_at(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
pub fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}
pub fn i32_at(b: &[u8], o: usize) -> i32 {
    u32_at(b, o) as i32
}
pub fn vec3_at(b: &[u8], o: usize) -> [f64; 3] {
    [0, 4, 8].map(|k| i32_at(b, o + k) as f64 / UNIT)
}
pub fn str_at(b: &[u8], o: usize) -> String {
    b.get(o..).map(cstr).unwrap_or_default()
}

/// Custom 16-bit float (`f16_Decode 0x48e8c0`): bits 4-15 mantissa, bits 1-3 exponent,
/// bit 0 sign (one's complement).
pub fn f16_decode(x: u16) -> i32 {
    let v = ((x as u32 & 0xfff0) << ((x as u32 & 0xf) | 1)) as i32;
    if x & 1 != 0 { !v } else { v }
}

/// `f16_Encode 0x48e870`.
pub fn f16_encode(v: i32) -> u16 {
    let neg = v < 0;
    let u = (v ^ (v >> 31)) as u32;
    let top = if u == 0 { 31 } else { 31 - u.leading_zeros() };
    let e = top.saturating_sub(15);
    let m = u >> (e | 1);
    (m as u16 & 0xfff0) | (e as u16 & 0xe) | neg as u16
}

pub struct JoinBlob<'a> {
    pub ship: &'a str,
    pub team: u32,
    pub portrait: &'a str,
    pub game_id: u32,
    /// Weapon type for mount 1 (stored as type + 1; 0 = empty).
    pub weapon: u8,
}

/// The 0xc4-byte create-player data (`Profile_LoadoutInit 0x44bd40`).
pub fn join_blob(j: &JoinBlob) -> Vec<u8> {
    let mut b = vec![0u8; 0xc4];
    let mut put = |o: usize, v: &[u8]| b[o..o + v.len()].copy_from_slice(v);
    put(0x00, &fixed_str(j.ship, 32)); // GALSPAN-side ship for team games
    put(0x20, &fixed_str(j.ship, 32)); // BORA-side ship
    put(0x40, &j.team.to_le_bytes());
    put(0x44, &fixed_str(j.portrait, 16));
    put(0x58, &(j.weapon as u16 + 1).to_le_bytes());
    put(0x78, &j.game_id.to_le_bytes());
    put(0x7c, &0x1002u32.to_le_bytes());
    put(0xa0, &j.team.to_le_bytes());
    put(0xa4, &fixed_str(j.ship, 32)); // deathmatch ship
    b
}

/// Our ship as reported in PLAYERDATA.
#[derive(Clone, Copy, Default)]
pub struct ShipState {
    /// 16.16 world position.
    pub pos: [i32; 3],
    /// Position change per 50 Hz tick, 16.16.
    pub vel: [i32; 3],
    /// Euler angles (yaw, pitch, roll), 512 units per turn, 16.16.
    pub angles: [i32; 3],
    pub valid: bool,
}

/// The 0x4c-byte PLAYERDATA body (`Tach_WritePlayerData 0x417eb0`).
pub fn player_data(s: &ShipState, frame: u32) -> Vec<u8> {
    let mut b = vec![0u8; 0x4c];
    let mut put = |o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    if s.valid {
        for k in 0..3 {
            put(k * 4, s.pos[k] as u32);
            put(0x3c + k * 4, s.vel[k] as u32);
        }
    }
    for k in 0..3 {
        put(0x0c + k * 4, s.pos[k] as u32); // camera = the ship
    }
    put(0x18, frame);
    if s.valid {
        for k in 0..3 {
            b[0x48 + k] = (s.angles[k] >> 17) as u8;
        }
        b[0x4b] = 1;
        // Throttle 0..1024 (−1 = afterburner); the server shows it as `>> 3` in snapshot byte 6.
        b[0x28..0x2c].copy_from_slice(&512u32.to_le_bytes());
    }
    b
}

/// A snapshot record (24 bytes, 28 with flag 0x08).
#[derive(Clone, Debug)]
pub struct Record {
    pub id: u32,
    pub model: u8,
    pub flags: u8,
    /// Throttle / engine-FX byte (`DCB+0xc1`): 0..128 = throttle, 255 = afterburner.
    pub throttle: u8,
    /// Team (`DCB+0xc2`, via the rules' `+0x1c` hook; a `race.cfg` index in deathmatch).
    pub team: u8,
    pub pos: [f64; 3],
    pub vel: [f64; 3],
    pub angles: [u8; 3],
    pub desc: Option<i16>,
}

pub const REC_PAGED: u8 = 0x08;
pub const REC_FAR: u8 = 0x10;

pub fn parse_record(b: &[u8], origin: [i32; 3]) -> Option<(Record, usize)> {
    let flags = *b.get(5)?;
    let len = if flags & REC_PAGED != 0 { 28 } else { 24 };
    if b.len() < len {
        return None;
    }
    let f = |o: usize| f16_decode(u16_at(b, o));
    let r = Record {
        id: u32_at(b, 0),
        model: b[4],
        flags,
        throttle: b[6],
        team: b[7],
        pos: [0, 1, 2].map(|k| (origin[k] as f64 + f(8 + 2 * k) as f64) / UNIT),
        vel: [0, 1, 2].map(|k| f(0xe + 2 * k) as f64 / UNIT),
        angles: [b[0x14], b[0x15], b[0x16]],
        desc: (len == 28).then(|| u16_at(b, 0x1a) as i16).filter(|&d| d >= 0),
    };
    Some((r, len))
}

pub struct Snapshot {
    pub records: Vec<Record>,
    pub page_list: u16,
    pub page_count: u16,
}

/// GAMEDATA: 24-byte status header, then `u16 count, u16 pageList, u16 pageCount, vec3 origin`,
/// then records (`Tach_ReadGameData 0x417dc0`, `Dcb_ApplySnapshot 0x417ce0`).
pub fn parse_gamedata(b: &[u8]) -> Option<Snapshot> {
    let s = b.get(0x18..)?;
    let count = u16_at(s, 0) as usize;
    let origin = [i32_at(s, 6), i32_at(s, 10), i32_at(s, 14)];
    let mut o = 18;
    let mut records = vec![];
    for _ in 0..count {
        let (r, n) = parse_record(s.get(o..)?, origin)?;
        records.push(r);
        o += n;
    }
    Some(Snapshot { records, page_list: u16_at(s, 2), page_count: u16_at(s, 4) })
}

/// A startup blob: `u16 nRecs`, then `{u16 kind, u16 size, ...}`.
pub fn startup_records(blob: &[u8]) -> Vec<(u16, &[u8])> {
    let n = u16_at(blob, 0);
    let mut o = 2;
    let mut out = vec![];
    for _ in 0..n {
        let size = u16_at(blob, o + 2) as usize;
        if size < 4 || o + size > blob.len() {
            break;
        }
        out.push((u16_at(blob, o), &blob[o..o + size]));
        o += size;
    }
    out
}

/// Kind 3 (model table, two names per slot) or kind 2 (descriptor table, one name).
pub fn name_table(rec: &[u8], two_names: bool) -> Vec<(u16, String)> {
    let n = u16_at(rec, 4);
    let mut o = 6;
    let mut out = vec![];
    let next = |o: &mut usize| {
        let s = str_at(rec, *o);
        *o += s.len() + 1;
        s
    };
    for _ in 0..n {
        if o + 2 > rec.len() {
            break;
        }
        let slot = u16_at(rec, o);
        o += 2;
        let a = next(&mut o);
        if two_names {
            let pal = next(&mut o);
            out.push((slot, format!("{a} ({pal})")));
        } else {
            out.push((slot, a));
        }
    }
    out
}

/// The join blob as the server reads it (`Srv_CopyPlayerInfo 0x461100`, mode join hooks).
#[derive(Debug, Clone, Default)]
pub struct JoinInfo {
    /// Team-game ships: GALSPAN side (+0x00), BORA side (+0x20).
    pub galspan_ship: String,
    pub bora_ship: String,
    pub preferred_team: u32,
    pub portrait: String,
    pub systems: u32,
    /// Weapon id + 1 per mount (0 = empty).
    pub mounts: [u16; 8],
    pub voice_port: u16,
    pub game_id: u32,
    pub magic: u32,
    /// Deathmatch: campaign faction (`race.cfg` index) and ship alias.
    pub dm_team: u32,
    pub dm_ship: String,
}

pub fn parse_join_blob(b: &[u8]) -> JoinInfo {
    JoinInfo {
        galspan_ship: b.get(0..0x20).map(cstr).unwrap_or_default(),
        bora_ship: b.get(0x20..0x40).map(cstr).unwrap_or_default(),
        preferred_team: u32_at(b, 0x40),
        portrait: b.get(0x44..0x54).map(cstr).unwrap_or_default(),
        systems: u32_at(b, 0x54),
        mounts: std::array::from_fn(|i| u16_at(b, 0x58 + 2 * i)),
        voice_port: u16_at(b, 0x68),
        game_id: u32_at(b, 0x78),
        magic: u32_at(b, 0x7c),
        dm_team: u32_at(b, 0xa0),
        dm_ship: b.get(0xa4..0xc4).map(cstr).unwrap_or_default(),
    }
}

/// PLAYERDATA as the server reads it (`Tach_SrvReadPlayerData 0x460b30`).
#[derive(Debug, Clone, Copy, Default)]
pub struct PlayerDataIn {
    /// Bit 0 of +0x4b: position, velocity and angles are valid.
    pub has_ship: bool,
    pub pos: [i32; 3],
    pub camera: [i32; 3],
    pub frame: u32,
    pub target: u32,
    pub subtarget: [u16; 2],
    pub speed: i32,
    /// 0..1024, or -1 with the afterburner on.
    pub throttle: i32,
    pub shield_rate: i32,
    /// One-shot interaction request `{objId, action}` (0 = none).
    pub interact: [u32; 2],
    pub vel: [i32; 3],
    /// 16.16 angles, 512 units per turn (the wire carries the top 8 bits).
    pub angles: [i32; 3],
}

pub fn parse_player_data(b: &[u8]) -> Option<PlayerDataIn> {
    if b.len() < 0x4c {
        return None;
    }
    Some(PlayerDataIn {
        has_ship: b[0x4b] & 1 != 0,
        pos: [0, 4, 8].map(|k| i32_at(b, k)),
        camera: [0, 4, 8].map(|k| i32_at(b, 0xc + k)),
        frame: u32_at(b, 0x18),
        target: u32_at(b, 0x1c),
        subtarget: [u16_at(b, 0x20), u16_at(b, 0x22)],
        speed: i32_at(b, 0x24),
        throttle: i32_at(b, 0x28),
        shield_rate: i32_at(b, 0x2c),
        interact: [u32_at(b, 0x34), u32_at(b, 0x38)],
        vel: [0, 4, 8].map(|k| i32_at(b, 0x3c + k)),
        angles: [0, 1, 2].map(|k| (b[0x48 + k] as i32) << 17),
    })
}

/// One entity in a snapshot, server side. Positions are absolute 16.16.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecordOut {
    pub id: u32,
    pub model: u8,
    pub flags: u8,
    pub throttle: u8,
    pub team: u8,
    pub pos: [i32; 3],
    pub vel: [i32; 3],
    /// 16.16 angles, 512 units per turn.
    pub angles: [i32; 3],
    /// Extended (flag 0x08) records: 8.8 scale and descriptor slot (-1 = plain model).
    pub ext: Option<(u16, i16)>,
}

/// `Tach_SrvWriteEntityRecord 0x4614e0` / `...Ext`: 24 bytes, 28 with flag 0x08.
pub fn write_record(out: &mut Vec<u8>, r: &RecordOut, origin: [i32; 3]) {
    out.extend_from_slice(&r.id.to_le_bytes());
    let flags = if r.ext.is_some() { r.flags | REC_PAGED } else { r.flags & !REC_PAGED };
    out.extend_from_slice(&[r.model, flags, r.throttle, r.team]);
    for k in 0..3 {
        out.extend_from_slice(&f16_encode(r.pos[k].wrapping_sub(origin[k])).to_le_bytes());
    }
    for k in 0..3 {
        out.extend_from_slice(&f16_encode(r.vel[k]).to_le_bytes());
    }
    out.extend_from_slice(&[(r.angles[0] >> 17) as u8, (r.angles[1] >> 17) as u8, (r.angles[2] >> 17) as u8, 0]);
    if let Some((scale, desc)) = r.ext {
        out.extend_from_slice(&scale.to_le_bytes());
        out.extend_from_slice(&desc.to_le_bytes());
    }
}

/// GAMEDATA status header (24 bytes, `Tach_SrvWriteGameData 0x461a30`).
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusOut {
    pub target_speed: f32,
    /// Target hull / shield %, credits (0xff = none).
    pub target_meters: [u8; 3],
    pub shield_front: u16,
    pub shield_rear: u16,
    pub hull: u16,
    pub field_12: u16,
    pub one_shot_flags: u32,
}

/// Full GAMEDATA body: status header, snapshot header, records.
pub fn write_gamedata(status: &StatusOut, origin: [i32; 3], page: (u16, u16), records: &[RecordOut]) -> Vec<u8> {
    let mut b = Vec::with_capacity(0x18 + 18 + records.len() * 28);
    b.extend_from_slice(&status.target_speed.to_le_bytes());
    b.extend_from_slice(&[status.target_meters[0], status.target_meters[1], status.target_meters[2], 0]);
    b.extend_from_slice(&status.shield_front.to_le_bytes());
    b.extend_from_slice(&status.shield_rear.to_le_bytes());
    b.extend_from_slice(&status.hull.to_le_bytes());
    b.extend_from_slice(&[0, 0]);
    b.extend_from_slice(&0u16.to_le_bytes());
    b.extend_from_slice(&status.field_12.to_le_bytes());
    b.extend_from_slice(&status.one_shot_flags.to_le_bytes());
    b.extend_from_slice(&(records.len() as u16).to_le_bytes());
    b.extend_from_slice(&page.0.to_le_bytes());
    b.extend_from_slice(&page.1.to_le_bytes());
    for v in origin {
        b.extend_from_slice(&v.to_le_bytes());
    }
    for r in records {
        write_record(&mut b, r, origin);
    }
    b
}

/// `race.cfg` races, the team numbers.
pub const RACES: [&str; 19] = [
    "agt", "bora", "galspan", "pirate", "tourist", "agt_rival", "void_pirate", "skav_pirate",
    "red_pirate", "mad_pirate", "cassitor", "hajod", "malkar", "onrald", "star_patrol",
    "independent_mercenaries", "galspan_mercenaries", "biolith", "cmi",
];

pub fn race(i: u32) -> String {
    RACES.get(i as usize).map_or_else(|| i.to_string(), |s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f16_round_trips_within_precision() {
        for v in [0, 1, 100, -100, 65536, -65536, 3_000_000, -3_000_000, 123_456_789, i32::MAX / 2] {
            let d = f16_decode(f16_encode(v));
            let tol = (v.unsigned_abs() >> 9).max(16) as i64; // ~11 mantissa bits survive
            assert!((d as i64 - v as i64).abs() <= tol, "{v} -> {d}");
        }
        assert_eq!(f16_decode(0), 0);
    }

    #[test]
    fn join_blob_layout() {
        let b = join_blob(&JoinBlob { ship: "BORA_CUTLASS", team: 1, portrait: "", game_id: 0, weapon: 6 });
        assert_eq!(b.len(), 0xc4);
        assert_eq!(&b[0xa4..0xb0], b"BORA_CUTLASS");
        assert_eq!(u32_at(&b, 0x7c), 0x1002);
        assert_eq!(u32_at(&b, 0xa0), 1);
    }

    #[test]
    fn player_data_layout() {
        let s = ShipState { pos: [1 << 16, 2 << 16, 3 << 16], vel: [5, 6, 7], angles: [128 << 16, 0, 0], valid: true };
        let b = player_data(&s, 42);
        assert_eq!(b.len(), 0x4c);
        assert_eq!(i32_at(&b, 4), 2 << 16);
        assert_eq!(i32_at(&b, 0x10), 2 << 16);
        assert_eq!(u32_at(&b, 0x18), 42);
        assert_eq!(i32_at(&b, 0x40), 6);
        assert_eq!((b[0x48], b[0x4b]), (64, 1));
    }

    #[test]
    fn name_tables() {
        let mut r = vec![3, 0, 0, 0, 2, 0];
        r.extend_from_slice(&[7, 0]);
        r.extend_from_slice(b"cutless.pak\0bora.pal\0");
        r.extend_from_slice(&[9, 0]);
        r.extend_from_slice(b"a.pak\0b.pal\0");
        let t = name_table(&r, true);
        assert_eq!(t, vec![(7, "cutless.pak (bora.pal)".into()), (9, "a.pak (b.pal)".into())]);
    }

    #[test]
    fn gamedata_round_trip() {
        let r = RecordOut { id: 7, model: 3, flags: 2, throttle: 64, team: 1, pos: [100 << 16, -(5 << 16), 0], vel: [1000, 0, -1000], angles: [64 << 17, 0, 0], ext: Some((256, 4)) };
        let b = write_gamedata(&StatusOut::default(), [10 << 16, 0, 0], (3, 1), &[r, RecordOut { ext: None, ..r }]);
        let s = parse_gamedata(&b).unwrap();
        assert_eq!((s.records.len(), s.page_list, s.page_count), (2, 3, 1));
        assert_eq!((s.records[0].id, s.records[0].model, s.records[0].team, s.records[0].desc), (7, 3, 1, Some(4)));
        assert!((s.records[0].pos[0] - 100.0).abs() < 0.1 && (s.records[0].pos[1] + 5.0).abs() < 0.1);
        assert_eq!((s.records[1].desc, s.records[1].angles[0]), (None, 64));
    }

    #[test]
    fn player_data_parse() {
        let st = ShipState { pos: [1 << 16, 2 << 16, 3 << 16], vel: [5, 6, 7], angles: [128 << 16, 0, 0], valid: true };
        let p = parse_player_data(&player_data(&st, 42)).unwrap();
        assert!(p.has_ship);
        assert_eq!((p.pos, p.vel, p.frame, p.angles[0], p.throttle), (st.pos, st.vel, 42, 128 << 16, 512));
        let j = parse_join_blob(&join_blob(&JoinBlob { ship: "BORA_CUTLASS", team: 1, portrait: "face1", game_id: 0, weapon: 6 }));
        assert_eq!((j.dm_ship.as_str(), j.dm_team, j.portrait.as_str(), j.mounts[0], j.magic), ("BORA_CUTLASS", 1, "face1", 7, 0x1002));
    }
}
