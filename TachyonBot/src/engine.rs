//! The engine packet carried in 0x1080 messages: `u32 seq, u32 ack`, then sub-messages
//! `{u16 type, u16 size, ...}`. App events add a `u32 tag` and ride in every packet until the
//! server acks them (`NetCli_*` at space.exe `0x4b8970..0x4b8fd0`).

pub const MSG_GAMEDATA: u16 = 2;
pub const MSG_PLAYERDATA: u16 = 3;
pub const MSG_STARTUPDATA: u16 = 4;
pub const MSG_PLAYERINFO: u16 = 6;
pub const MSG_KICK: u16 = 7;
pub const EV_STARTUP_ACK: u16 = 0x103;

pub struct OutEvent {
    tag: u32,
    kind: u16,
    body: Vec<u8>,
    budget: i32,
    /// The seq of the first packet this rode in (0 = not sent yet).
    first_seq: u32,
}

/// One received sub-message.
pub enum Sub<'a> {
    GameData(&'a [u8]),
    Startup { stage: u32, session: u32, player: u32, blob: &'a [u8] },
    #[allow(dead_code)]
    /// `{u32 netTick @4, u32 session @8}` (`NetSrv_WritePlayerInfo`), then the game's
    /// `u32 gameFrame @0xc` (`FUN_00461b20`). The net tick runs 100 ahead of the game frame;
    /// shots (0x1002) are stamped with, and aged against, the game frame.
    PlayerInfo { net_tick: u32, session: u32, game_frame: u32 },
    Kick(u8),
    /// An app event, already de-duplicated.
    Event { kind: u16, body: &'a [u8] },
}

pub struct Engine {
    seq: u32,
    ack: u32,
    next_tag: u32,
    events: Vec<OutEvent>,
    seen: [u32; 128],
    seen_at: usize,
    pub session: u32,
    pub player: u32,
    pub frame: u32,
    stage: Option<u32>,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}

impl Engine {
    pub fn new() -> Engine {
        Engine {
            seq: 1,
            ack: 0,
            next_tag: 0x1000,
            events: vec![],
            seen: [u32::MAX; 128],
            seen_at: 0,
            session: u32::MAX,
            player: 0,
            frame: 100,
            stage: None,
        }
    }

    /// Queue a reliable event (budget 0x800 ≈ until acked).
    pub fn queue(&mut self, kind: u16, body: Vec<u8>) {
        self.events.push(OutEvent { tag: self.next_tag, kind, body, budget: 0x800, first_seq: 0 });
        self.next_tag = self.next_tag.wrapping_add(2);
    }

    #[cfg(test)]
    pub fn pending_events(&self) -> usize {
        self.events.len()
    }

    /// Build the next packet: header, PLAYERDATA with `player_data` (0x4c bytes), events.
    pub fn build(&mut self, player_data: &[u8]) -> Vec<u8> {
        let mut p = Vec::with_capacity(256);
        p.extend_from_slice(&self.seq.to_le_bytes());
        p.extend_from_slice(&self.ack.to_le_bytes());
        p.extend_from_slice(&MSG_PLAYERDATA.to_le_bytes());
        p.extend_from_slice(&((16 + player_data.len()) as u16).to_le_bytes());
        p.extend_from_slice(&self.player.to_le_bytes());
        p.extend_from_slice(&self.frame.to_le_bytes());
        p.extend_from_slice(&self.session.to_le_bytes());
        p.extend_from_slice(player_data);
        for e in &mut self.events {
            if e.first_seq == 0 {
                e.first_seq = self.seq;
            }
            p.extend_from_slice(&e.kind.to_le_bytes());
            p.extend_from_slice(&((8 + e.body.len()) as u16).to_le_bytes());
            p.extend_from_slice(&e.tag.to_le_bytes());
            p.extend_from_slice(&e.body);
            e.budget -= 1;
        }
        self.events.retain(|e| e.budget > 0);
        self.seq += 1;
        p
    }

    /// Split a server packet into sub-messages; None if it's old or a duplicate.
    pub fn receive<'a>(&mut self, p: &'a [u8]) -> Option<Vec<Sub<'a>>> {
        if p.len() < 8 {
            return None;
        }
        let (seq, ack) = (u32_at(p, 0), u32_at(p, 4));
        if self.ack != 0 && seq <= self.ack {
            return None;
        }
        self.ack = seq;
        self.events.retain(|e| e.first_seq == 0 || e.first_seq > ack);
        let mut subs = vec![];
        let mut o = 8;
        while o + 4 <= p.len() {
            let (kind, size) = (u16_at(p, o), u16_at(p, o + 2) as usize);
            if size < 4 || o + size > p.len() {
                break;
            }
            let m = &p[o..o + size];
            match kind {
                MSG_GAMEDATA => subs.push(Sub::GameData(&m[4..])),
                MSG_STARTUPDATA if size >= 16 => {
                    let (stage, session, player) = (u32_at(m, 4), u32_at(m, 8), u32_at(m, 12));
                    self.player = player;
                    if self.stage != Some(stage) {
                        self.stage = Some(stage);
                        self.session = session;
                        let mut ack = 1u16.to_le_bytes().to_vec();
                        ack.extend_from_slice(&stage.to_le_bytes());
                        self.queue(EV_STARTUP_ACK, ack);
                        subs.push(Sub::Startup { stage, session, player, blob: &m[16..] });
                    }
                }
                MSG_PLAYERINFO => subs.push(Sub::PlayerInfo { net_tick: u32_at(m, 4), session: u32_at(m, 8), game_frame: u32_at(m, 12) }),
                MSG_KICK => subs.push(Sub::Kick(m.get(4).copied().unwrap_or(0))),
                k if k >= 8 && size >= 8 => {
                    let tag = u32_at(m, 4);
                    if !self.seen.contains(&tag) {
                        self.seen[self.seen_at] = tag;
                        self.seen_at = (self.seen_at + 1) % 128;
                        subs.push(Sub::Event { kind, body: &m[8..] });
                    }
                }
                _ => {}
            }
            o += size;
        }
        Some(subs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_packet(seq: u32, ack: u32, subs: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut p = seq.to_le_bytes().to_vec();
        p.extend_from_slice(&ack.to_le_bytes());
        for (k, b) in subs {
            p.extend_from_slice(&k.to_le_bytes());
            p.extend_from_slice(&((4 + b.len()) as u16).to_le_bytes());
            p.extend_from_slice(b);
        }
        p
    }

    #[test]
    fn events_retire_on_ack_and_dedupe() {
        let mut e = Engine::new();
        e.queue(0x2012, vec![0, 0]);
        let p = e.build(&[0; 0x4c]);
        assert_eq!(p.len(), 8 + 92 + 10);
        assert_eq!(e.pending_events(), 1);
        let _ = e.build(&[0; 0x4c]); // still rides along
        // Server acks seq 1 → retired.
        e.receive(&server_packet(1, 1, &[])).unwrap();
        assert_eq!(e.pending_events(), 0);
        // The same event tag twice is delivered once; an old seq is dropped.
        let ev = |tag: u32| {
            let mut b = tag.to_le_bytes().to_vec();
            b.extend_from_slice(&[1, 2]);
            (0x1111u16, b)
        };
        let (p2, p3) = (server_packet(2, 1, &[ev(9)]), server_packet(3, 1, &[ev(9)]));
        assert_eq!(e.receive(&p2).unwrap().len(), 1);
        assert!(e.receive(&p3).unwrap().is_empty());
        assert!(e.receive(&server_packet(3, 1, &[])).is_none());
    }

    #[test]
    fn startup_stage_is_acked_once() {
        let mut e = Engine::new();
        let mut body = vec![];
        for v in [0u32, 77, 5] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        let n = e.receive(&server_packet(1, 0, &[(4, body.clone())])).unwrap().len();
        assert_eq!(n, 1);
        assert_eq!((e.session, e.player, e.pending_events()), (77, 5, 1));
        let n = e.receive(&server_packet(2, 0, &[(4, body)])).unwrap().len();
        assert_eq!((n, e.pending_events()), (0, 1));
    }
}
