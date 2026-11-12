//! Startup stage blobs, server side (`Tach_SrvWriteStartupStage 0x461f80`; layouts in
//! `decomp/notes/multiplayer.md` "Server-side startup details"). A blob is `u16 nRecs` plus
//! records `{u16 kind, u16 size, ...}`; `blob()` wraps finished records.

use crate::net::netsock::fixed_str;

pub fn blob(records: &[Vec<u8>]) -> Vec<u8> {
    let mut b = (records.len() as u16).to_le_bytes().to_vec();
    for r in records {
        b.extend_from_slice(r);
    }
    b
}

fn finish(mut r: Vec<u8>) -> Vec<u8> {
    if r.len() & 1 != 0 {
        r.push(0);
    }
    let n = r.len() as u16;
    r[2..4].copy_from_slice(&n.to_le_bytes());
    r
}

fn cstr_into(r: &mut Vec<u8>, s: &str) {
    r.extend_from_slice(s.as_bytes());
    r.push(0);
}

/// Kind 3 (stage 0): `{u16 slot, char pak[], char pal[]}` per used slot.
pub fn model_table(entries: &[(u16, String, String)]) -> Vec<u8> {
    let mut r = vec![3, 0, 0, 0];
    r.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (slot, pak, pal) in entries {
        r.extend_from_slice(&slot.to_le_bytes());
        cstr_into(&mut r, pak);
        cstr_into(&mut r, pal);
    }
    finish(r)
}

/// Kind 2 (stage 1): `{u16 slot, char desName[]}` per used slot.
pub fn desc_table(entries: &[(u16, String)]) -> Vec<u8> {
    let mut r = vec![2, 0, 0, 0];
    r.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for (slot, name) in entries {
        r.extend_from_slice(&slot.to_le_bytes());
        cstr_into(&mut r, name);
    }
    finish(r)
}

/// Kind 1 (0x34, `TachSrv_WriteRulesInfo 0x461cf0`).
pub struct RulesInfo<'a> {
    pub bdf: &'a str,
    pub region: &'a str,
    pub entity_id: u32,
    pub rules_value: u32,
    pub region_index: u16,
    pub quadrant_id: u16,
    pub slot: u16,
    pub max_slots: u16,
    pub game_mode: u16,
}

pub fn rules_info(i: &RulesInfo) -> Vec<u8> {
    let mut r = vec![0u8; 0x34];
    r[0..2].copy_from_slice(&1u16.to_le_bytes());
    r[2..4].copy_from_slice(&0x34u16.to_le_bytes());
    r[4..0x12].copy_from_slice(&fixed_str(i.bdf, 14));
    r[0x12..0x22].copy_from_slice(&fixed_str(i.region, 16));
    r[0x22..0x26].copy_from_slice(&i.entity_id.to_le_bytes());
    r[0x26..0x2a].copy_from_slice(&i.rules_value.to_le_bytes());
    r[0x2a..0x2c].copy_from_slice(&i.region_index.to_le_bytes());
    r[0x2c..0x2e].copy_from_slice(&i.quadrant_id.to_le_bytes());
    r[0x2e..0x30].copy_from_slice(&i.slot.to_le_bytes());
    r[0x30..0x32].copy_from_slice(&i.max_slots.to_le_bytes());
    r[0x32..0x34].copy_from_slice(&i.game_mode.to_le_bytes());
    r
}

/// Kind 4 (0x50, `MpRules_InitPlayerShip 0x44ffe0`).
pub struct PlayerInit<'a> {
    pub des_name: &'a str,
    pub team: u16,
    pub systems: u32,
    pub pos: [i32; 3],
    pub angles: [i32; 3],
    /// `(weapon id + 1, max ammo)` per mount; (0, 0) = empty.
    pub mounts: [(u16, u16); 8],
}

pub fn player_init(p: &PlayerInit) -> Vec<u8> {
    let mut r = vec![0u8; 0x50];
    r[0..2].copy_from_slice(&4u16.to_le_bytes());
    r[2..4].copy_from_slice(&0x50u16.to_le_bytes());
    r[4..0x12].copy_from_slice(&fixed_str(p.des_name, 14));
    r[0x12..0x14].copy_from_slice(&p.team.to_le_bytes());
    r[0x14..0x18].copy_from_slice(&p.systems.to_le_bytes());
    for k in 0..3 {
        r[0x18 + 4 * k..0x1c + 4 * k].copy_from_slice(&p.pos[k].to_le_bytes());
        r[0x24 + 4 * k..0x28 + 4 * k].copy_from_slice(&p.angles[k].to_le_bytes());
    }
    for (i, (w, a)) in p.mounts.iter().enumerate() {
        r[0x30 + 4 * i..0x32 + 4 * i].copy_from_slice(&w.to_le_bytes());
        r[0x32 + 4 * i..0x34 + 4 * i].copy_from_slice(&a.to_le_bytes());
    }
    r
}

/// One roster entry (kind 5 and event 0x1110, `Roster_FillEntry 0x45d9f0`):
/// `{u16 slot, u16 portrait, u8 a, u8 b, u8 team, u8 0, char name[]}`, `(strlen + 11) & !1` bytes.
#[derive(Debug, Clone)]
pub struct RosterEntry {
    pub slot: u16,
    /// Portrait slot (`conn+0x1c8`, -1 = none).
    pub portrait: i16,
    /// DCB +0xfc / conn +0x10c; meaning unknown, 0 in deathmatch as far as seen.
    pub a: u8,
    pub b: u8,
    /// 0xff = no ship.
    pub team: u8,
    pub name: String,
}

pub fn roster_entry(e: &RosterEntry) -> Vec<u8> {
    let mut r = e.slot.to_le_bytes().to_vec();
    r.extend_from_slice(&e.portrait.to_le_bytes());
    r.extend_from_slice(&[e.a, e.b, e.team, 0]);
    let name: String = e.name.chars().take(31).collect();
    cstr_into(&mut r, &name);
    r.resize((name.len() + 11) & !1, 0);
    r
}

/// Kind 5 (stage 4).
pub fn roster(entries: &[RosterEntry]) -> Vec<u8> {
    let mut r = vec![5, 0, 0, 0];
    r.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    for e in entries {
        r.extend_from_slice(&roster_entry(e));
    }
    finish(r)
}

/// Kind 6 (stage 3): `u32 n`, then `{char name[14], u16 slot}`.
pub fn portraits(entries: &[(String, u16)]) -> Vec<u8> {
    let mut r = vec![6, 0, 0, 0];
    r.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, slot) in entries {
        r.extend_from_slice(&fixed_str(name, 14));
        r.extend_from_slice(&slot.to_le_bytes());
    }
    finish(r)
}

/// Kind 7 (stage 5, 0xc): voice server; ip 0 disables voice chat.
pub fn session_info(voice_ip: u32, voice_port: u16, flag: u8) -> Vec<u8> {
    let mut r = vec![7, 0, 0x0c, 0];
    r.extend_from_slice(&voice_ip.to_le_bytes());
    r.extend_from_slice(&voice_port.to_le_bytes());
    r.extend_from_slice(&[flag, 0]);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::proto::{name_table, startup_records, str_at, u16_at, u32_at};

    #[test]
    fn blobs_parse_back() {
        let b = blob(&[
            model_table(&[(0, "cutless.pak".into(), "bora.pal".into()), (5, "a.pak".into(), "b.pal".into())]),
            desc_table(&[(1, "CUTLASS.DES".into())]),
        ]);
        let recs = startup_records(&b);
        assert_eq!(recs.len(), 2);
        assert_eq!(name_table(recs[0].1, true), vec![(0, "cutless.pak (bora.pal)".into()), (5, "a.pak (b.pal)".into())]);
        assert_eq!(name_table(recs[1].1, false), vec![(1, "CUTLASS.DES".into())]);
        assert!(recs.iter().all(|(_, r)| r.len() % 2 == 0));

        let k1 = rules_info(&RulesInfo { bdf: "BORA5.BDF", region: "NOVAWORLD", entity_id: 0x41, rules_value: 1, region_index: 9, quadrant_id: 3, slot: 2, max_slots: 32, game_mode: 1 });
        assert_eq!((str_at(&k1, 4).as_str(), str_at(&k1, 0x12).as_str(), u32_at(&k1, 0x22), u16_at(&k1, 0x2e)), ("BORA5.BDF", "NOVAWORLD", 0x41, 2));

        // Roster entries pad like the client's reader: (strlen + 11) & !1.
        let r = roster(&[RosterEntry { slot: 3, portrait: -1, a: 0, b: 0, team: 1, name: "Bob".into() }, RosterEntry { slot: 4, portrait: -1, a: 0, b: 0, team: 1, name: "Alice".into() }]);
        let mut o = 6;
        let mut names = vec![];
        for _ in 0..u16_at(&r, 4) {
            let name = str_at(&r, o + 8);
            o += (name.len() + 1 + 10) & !1;
            names.push(name);
        }
        assert_eq!(names, ["Bob", "Alice"]);
        assert_eq!(o, r.len());
        assert_eq!(session_info(0, 0, 0).len(), 0xc);
        assert_eq!(player_init(&PlayerInit { des_name: "CUTLASS.DES", team: 1, systems: 0, pos: [0; 3], angles: [0; 3], mounts: [(0, 0); 8] }).len(), 0x50);
    }
}
