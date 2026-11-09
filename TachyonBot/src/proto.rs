//! Tachyon's game layer: the join blob, PLAYERDATA, startup records, snapshot records.
//! Layouts are from `decomp/notes/multiplayer.md`.

use crate::netsock::{cstr, fixed_str};

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
#[cfg(test)]
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
}
