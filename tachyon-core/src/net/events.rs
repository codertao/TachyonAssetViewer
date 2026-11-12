//! App event bodies, server → client (`0x10xx/0x11xx`) and the client → server ones the server
//! reads. Layouts: `decomp/notes/multiplayer.md` "App event catalogue".

use crate::net::netsock::{cstr, fixed_str};
use crate::net::proto::{u16_at, u32_at};
use crate::net::startup::{roster_entry, RosterEntry};

pub const EV_TEXT: u16 = 0x1001;
pub const EV_FIRED: u16 = 0x1002;
pub const EV_DESTROYED: u16 = 0x1003;
pub const EV_YOU_DIED: u16 = 0x1006;
pub const EV_ENTER_QUADRANT: u16 = 0x1007;
pub const EV_DESPAWN: u16 = 0x1011;
pub const EV_TELEPORT: u16 = 0x1012;
pub const EV_ENTITY_OP: u16 = 0x1014;
pub const EV_ROSTER_CLEARED: u16 = 0x1029;
pub const EV_SESSION_BUMP: u16 = 0x1033;
pub const EV_TEAM_SHIP: u16 = 0x1107;
pub const EV_PORTRAIT: u16 = 0x1108;
pub const EV_ROSTER: u16 = 0x1110;
pub const EV_STARTUP_DONE: u16 = 0x1111;
pub const EV_SCOREBOARD: u16 = 0x1112;
pub const EV_AMMO: u16 = 0x1124;

pub const CL_FIRE: u16 = 0x2001;
pub const CL_HIT: u16 = 0x2003;
pub const CL_FIRE_GUIDED: u16 = 0x2004;
pub const CL_POWER: u16 = 0x2005;
pub const CL_ENTER_WORLD: u16 = 0x2007;
pub const CL_DATA_ACK: u16 = 0x2008;
pub const CL_COLLISION: u16 = 0x2009;
pub const CL_READY: u16 = 0x2010;
pub const CL_RESPAWN: u16 = 0x2011;
pub const CL_QUADRANT_ENTERED: u16 = 0x2012;
pub const CL_CHAT: u16 = 0x2022;
pub const CL_PART_HIT: u16 = 0x2027;
pub const CL_LOADOUT: u16 = 0x2028;

/// Text colours (plain palette indices; `Gfx_DefineColor` stores `i` at `0x5a6320 + i`).
pub const COLOR_CHAT: u16 = 0;
pub const COLOR_INFO: u16 = 1;
pub const COLOR_ALERT: u16 = 2;
pub const COLOR_JOIN: u16 = 4;
pub const COLOR_GROUP: u16 = 9;
pub const COLOR_PRIVATE: u16 = 11;
pub const COLOR_TEAM: u16 = 12;

/// 0x1001 `{u16 kind, u16 color, char text[]}`; size `(strlen + 7) & !1` (`SrvSend_TextMsg`).
/// kind 0 = HUD line.
pub fn text(kind: u16, color: u16, s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| c.is_ascii()).take(254).collect();
    let mut b = kind.to_le_bytes().to_vec();
    b.extend_from_slice(&color.to_le_bytes());
    b.extend_from_slice(s.as_bytes());
    b.push(0);
    b.resize((s.len() + 7) & !1, 0);
    b
}

pub fn parse_text(b: &[u8]) -> (u16, u16, String) {
    (u16_at(b, 0), u16_at(b, 2), b.get(4..).map(cstr).unwrap_or_default())
}

/// 0x2022 `{u16 mode, u16 target, char text[]}`: 0 all, 1 one player, 4 team, 8 group.
pub fn parse_chat(b: &[u8]) -> (u16, u16, String) {
    parse_text(b)
}

pub fn chat(mode: u16, target: u16, s: &str) -> Vec<u8> {
    text(mode, target, s)
}

/// 0x1107 `{char desName[14], u16 descSlot}`.
pub fn team_ship(des_name: &str, slot: u16) -> Vec<u8> {
    let mut b = fixed_str(des_name, 14);
    b.extend_from_slice(&slot.to_le_bytes());
    b
}

/// 0x1108 `{char name[14], u16 portraitSlot}`.
pub fn portrait(name: &str, slot: u16) -> Vec<u8> {
    team_ship(name, slot)
}

pub fn roster(e: &RosterEntry) -> Vec<u8> {
    roster_entry(e)
}

pub fn roster_cleared(slot: u16) -> Vec<u8> {
    slot.to_le_bytes().to_vec()
}

/// 0x1112 `u16 n (≤ 8), {u16 slot, u16 kills, u16 deaths}[n]`, best first.
pub fn scoreboard(rows: &[(u16, u16, u16)]) -> Vec<u8> {
    let n = rows.len().min(8);
    let mut b = (n as u16).to_le_bytes().to_vec();
    for (s, k, d) in &rows[..n] {
        for v in [s, k, d] {
            b.extend_from_slice(&v.to_le_bytes());
        }
    }
    b
}

pub fn startup_done() -> Vec<u8> {
    vec![0, 0]
}

pub fn session_bump(session: u32) -> Vec<u8> {
    session.to_le_bytes().to_vec()
}

/// 0x2008: kind 1 = static page ack `{u16 1, u32 pageId, u16 count}`, kind 2 = ready.
pub fn parse_data_ack(b: &[u8]) -> (u16, u32, u16) {
    (u16_at(b, 0), u32_at(b, 2), u16_at(b, 6))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_sizes() {
        assert_eq!(text(0, 0, "abcd").len(), 10);
        assert_eq!(text(0, 0, "abcde").len(), 12);
        assert_eq!(parse_text(&text(0, 11, "hi there")), (0, 11, "hi there".into()));
        assert_eq!(scoreboard(&[(1, 2, 3)]), [1, 0, 1, 0, 2, 0, 3, 0]);
    }
}
