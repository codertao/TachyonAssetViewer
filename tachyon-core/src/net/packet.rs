//! The engine packet carried in Netsock 0x1080 messages, both directions:
//! `u32 seq, u32 ack`, then sub-messages `{u16 type, u16 size, ...}` (size includes the 4-byte
//! sub-header). Types below 8 are engine messages; 8 and up are app events with a `u32 tag`
//! after the sub-header, resent in every packet until acked (`multiplayer.md` "Reliability").

pub const MSG_GAMEDATA: u16 = 2;
pub const MSG_PLAYERDATA: u16 = 3;
pub const MSG_STARTUPDATA: u16 = 4;
pub const MSG_PLAYERINFO: u16 = 6;
pub const MSG_KICK: u16 = 7;
pub const EV_STARTUP_ACK: u16 = 0x103;

/// Resend budget meaning "until acked".
pub const BUDGET_RELIABLE: i32 = 0x800;
/// Fire-and-forget events.
pub const BUDGET_UNRELIABLE: i32 = 4;

pub fn u16_at(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]))
}
pub fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map_or(0, |s| u32::from_le_bytes(s.try_into().unwrap()))
}

/// Builds one packet.
pub struct PacketWriter {
    pub buf: Vec<u8>,
}

impl PacketWriter {
    pub fn new(seq: u32, ack: u32) -> Self {
        let mut buf = Vec::with_capacity(512);
        buf.extend_from_slice(&seq.to_le_bytes());
        buf.extend_from_slice(&ack.to_le_bytes());
        PacketWriter { buf }
    }

    /// An engine sub-message: `{u16 kind, u16 size}` + body.
    pub fn sub(&mut self, kind: u16, body: &[u8]) {
        self.buf.extend_from_slice(&kind.to_le_bytes());
        self.buf.extend_from_slice(&((4 + body.len()) as u16).to_le_bytes());
        self.buf.extend_from_slice(body);
    }

    /// An app event: `{u16 kind, u16 size, u32 tag}` + body.
    pub fn event(&mut self, kind: u16, tag: u32, body: &[u8]) {
        self.buf.extend_from_slice(&kind.to_le_bytes());
        self.buf.extend_from_slice(&((8 + body.len()) as u16).to_le_bytes());
        self.buf.extend_from_slice(&tag.to_le_bytes());
        self.buf.extend_from_slice(body);
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.len() <= 8
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// One raw sub-message. `body` excludes the 4-byte sub-header (and, for events, the tag).
#[derive(Debug, Clone, Copy)]
pub struct RawSub<'a> {
    pub kind: u16,
    pub tag: Option<u32>,
    pub body: &'a [u8],
}

/// `(seq, ack, subs)`, or None if the packet is shorter than its header. Parsing stops at the
/// first malformed sub-message.
pub fn parse(p: &[u8]) -> Option<(u32, u32, Vec<RawSub<'_>>)> {
    if p.len() < 8 {
        return None;
    }
    let mut subs = vec![];
    let mut o = 8;
    while o + 4 <= p.len() {
        let (kind, size) = (u16_at(p, o), u16_at(p, o + 2) as usize);
        if size < 4 || o + size > p.len() {
            break;
        }
        let m = &p[o..o + size];
        if kind >= 8 {
            if size < 8 {
                break;
            }
            subs.push(RawSub { kind, tag: Some(u32_at(m, 4)), body: &m[8..] });
        } else {
            subs.push(RawSub { kind, tag: None, body: &m[4..] });
        }
        o += size;
    }
    Some((u32_at(p, 0), u32_at(p, 4), subs))
}

/// The receiver's 128-entry ring of recently seen event tags.
pub struct TagRing {
    seen: [u32; 128],
    at: usize,
}

impl Default for TagRing {
    fn default() -> Self {
        TagRing { seen: [u32::MAX; 128], at: 0 }
    }
}

impl TagRing {
    /// True if `tag` is new (and remembers it).
    pub fn first_time(&mut self, tag: u32) -> bool {
        if self.seen.contains(&tag) {
            return false;
        }
        self.seen[self.at] = tag;
        self.at = (self.at + 1) % 128;
        true
    }
}

/// A queued outgoing event.
#[derive(Debug, Clone)]
pub struct OutEvent {
    pub tag: u32,
    pub kind: u16,
    pub body: Vec<u8>,
    pub budget: i32,
    /// The seq of the first packet this rode in (0 = not sent yet).
    pub first_seq: u32,
}

/// Per-peer list of pending events. Tags step by 2, as the game's message pool does.
pub struct Outbox {
    pub events: Vec<OutEvent>,
    next_tag: u32,
}

impl Outbox {
    pub fn new(first_tag: u32) -> Self {
        Outbox { events: vec![], next_tag: first_tag }
    }

    pub fn queue(&mut self, kind: u16, body: Vec<u8>, budget: i32) -> u32 {
        let tag = self.next_tag;
        self.events.push(OutEvent { tag, kind, body, budget, first_seq: 0 });
        self.next_tag = self.next_tag.wrapping_add(2);
        tag
    }

    /// Copy every pending event into `w` (sent in packet `seq`), spend one unit of budget each
    /// and drop the exhausted ones.
    pub fn write_into(&mut self, w: &mut PacketWriter, seq: u32) {
        for e in &mut self.events {
            if e.first_seq == 0 {
                e.first_seq = seq;
            }
            w.event(e.kind, e.tag, &e.body);
            e.budget -= 1;
        }
        self.events.retain(|e| e.budget > 0);
    }

    /// The peer acked `ack`: retire everything first sent at or before it.
    pub fn on_ack(&mut self, ack: u32) {
        self.events.retain(|e| e.first_seq == 0 || e.first_seq > ack);
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_retire() {
        let mut ob = Outbox::new(0x1000);
        ob.queue(0x1111, vec![1, 2, 3], BUDGET_RELIABLE);
        ob.queue(0x1002, vec![9], 1);
        let mut w = PacketWriter::new(5, 4);
        w.sub(MSG_PLAYERINFO, &[0; 12]);
        ob.write_into(&mut w, 5);
        let p = w.finish();
        let (seq, ack, subs) = parse(&p).unwrap();
        assert_eq!((seq, ack, subs.len()), (5, 4, 3));
        assert_eq!((subs[1].kind, subs[1].tag, subs[1].body), (0x1111, Some(0x1000), &[1u8, 2, 3][..]));
        assert_eq!(subs[2].tag, Some(0x1002));
        // Budget-1 event is gone already; the reliable one retires on ack 5.
        assert_eq!(ob.len(), 1);
        ob.on_ack(4);
        assert_eq!(ob.len(), 1);
        ob.on_ack(5);
        assert!(ob.is_empty());
        let mut r = TagRing::default();
        assert!(r.first_time(0x1000));
        assert!(!r.first_time(0x1000));
    }
}
