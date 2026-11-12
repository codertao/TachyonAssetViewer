//! Netsock.dll's UDP layer: CRC framing and the session messages (enumerate, join, create
//! player, leave) plus the 0x1080 game-data wrapper. See `decomp/notes/multiplayer.md`,
//! "Netsock session protocol".

/// Tachyon's Netsock application GUID (space.exe `0x52e8c0`).
pub const APP_GUID: [u8; 16] =
    [0xa1, 0xf8, 0xaf, 0x01, 0x89, 0x07, 0xd3, 0x11, 0xaf, 0x07, 0x00, 0xc0, 0xf0, 0x30, 0x12, 0x85];
/// `netsock_Init` default port.
pub const DEFAULT_PORT: u16 = 3874;

pub const ENUM: u16 = 0x1010;
pub const ENUM_REPLY: u16 = 0x1011;
pub const JOIN: u16 = 0x1020;
pub const JOIN_REPLY: u16 = 0x1021;
pub const CREATE_PLAYER: u16 = 0x1030;
pub const CREATE_REPLY: u16 = 0x1031;
pub const DESTROY_PLAYER: u16 = 0x1032;
pub const DESTROY_REPLY: u16 = 0x1033;
pub const CLOSE: u16 = 0x1040;
pub const CLOSE_REPLY: u16 = 0x1041;
pub const SERVER_CLOSING: u16 = 0x1052;
pub const GAME_DATA: u16 = 0x1080;

/// MSB-first CRC-32 (poly 0x04c11db7), table at Netsock `0x10007234`.
const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 24;
        let mut k = 0;
        while k < 8 {
            c = if c & 0x8000_0000 != 0 { (c << 1) ^ 0x04c1_1db7 } else { c << 1 };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}
static CRC: [u32; 256] = crc_table();

/// Init 0xffffffff, no final xor.
pub fn crc(data: &[u8]) -> u32 {
    data.iter().fold(0xffff_ffffu32, |c, &b| (c << 8) ^ CRC[((c >> 24) as u8 ^ b) as usize])
}

/// Message → datagram (`FUN_10005020`). For messages of 32+ bytes, bit 0 of the first 32 bytes
/// carries the CRC and the header holds the original bits.
pub fn encode(msg: &[u8]) -> Vec<u8> {
    let c = crc(msg);
    let mut out = Vec::with_capacity(msg.len() + 4);
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(msg);
    let mut head = c;
    if msg.len() >= 32 {
        head = 0;
        for i in 0..32 {
            let b = &mut out[4 + i];
            head |= ((*b & 1) as u32) << i;
            *b = (*b & !1) | ((c >> i) & 1) as u8;
        }
    }
    out[..4].copy_from_slice(&head.to_le_bytes());
    out
}

/// Datagram → message, or None if the CRC doesn't match (`FUN_100052e0`, plain form only).
pub fn decode(dgram: &[u8]) -> Option<Vec<u8>> {
    if dgram.len() <= 4 {
        return None;
    }
    let head = u32::from_le_bytes(dgram[..4].try_into().unwrap());
    let mut msg = dgram[4..].to_vec();
    let mut want = head;
    if msg.len() >= 32 {
        want = 0;
        for (i, b) in msg.iter_mut().take(32).enumerate() {
            want |= ((*b & 1) as u32) << i;
            *b = (*b & !1) | ((head >> i) & 1) as u8;
        }
    }
    (crc(&msg) == want).then_some(msg)
}

/// `u16 type, u16 length` + body.
pub fn message(kind: u16, body: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(4 + body.len());
    m.extend_from_slice(&kind.to_le_bytes());
    m.extend_from_slice(&((4 + body.len()) as u16).to_le_bytes());
    m.extend_from_slice(body);
    m
}

pub fn enum_request() -> Vec<u8> {
    message(ENUM, &APP_GUID)
}

pub fn join_request(password: &str) -> Vec<u8> {
    let mut b = APP_GUID.to_vec();
    b.extend_from_slice(&fixed_str(password, 64));
    message(JOIN, &b)
}

/// 0x1030: `u32 requestNo, {name[128], dataPtr, dataSize, custId[16]}, data`.
pub fn create_player(request: u32, name: &str, data: &[u8]) -> Vec<u8> {
    let mut b = request.to_le_bytes().to_vec();
    b.extend_from_slice(&fixed_str(name, 128));
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&(data.len() as u32).to_le_bytes());
    b.extend_from_slice(&[0; 16]);
    b.extend_from_slice(data);
    message(CREATE_PLAYER, &b)
}

pub fn destroy_player(player_id: u32) -> Vec<u8> {
    message(DESTROY_PLAYER, &player_id.to_le_bytes())
}

pub fn close() -> Vec<u8> {
    message(CLOSE, &[])
}

/// 0x1080: length = payload + 12, an unwritten u32, the payload, then 4 bytes the length
/// counts but nobody reads.
pub fn game_data(payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(payload.len() + 12);
    m.extend_from_slice(&GAME_DATA.to_le_bytes());
    m.extend_from_slice(&((payload.len() + 12) as u16).to_le_bytes());
    m.extend_from_slice(&[0; 4]);
    m.extend_from_slice(payload);
    m.extend_from_slice(&[0; 4]);
    m
}

/// The engine packet inside a 0x1080 message.
pub fn game_payload(msg: &[u8]) -> Option<&[u8]> {
    let len = u16::from_le_bytes(msg.get(2..4)?.try_into().ok()?) as usize;
    msg.get(8..len.checked_sub(4)?.min(msg.len()))
}

pub fn kind(msg: &[u8]) -> u16 {
    msg.get(..2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]))
}

pub fn fixed_str(s: &str, n: usize) -> Vec<u8> {
    let mut v = s.as_bytes()[..s.len().min(n - 1)].to_vec();
    v.resize(n, 0);
    v
}

pub fn cstr(b: &[u8]) -> String {
    b.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect()
}

/// A session from an enumerate reply.
#[derive(Debug)]
pub struct Session {
    pub name: String,
    pub players: u32,
    pub max_players: u32,
    pub password: bool,
}

pub fn parse_session(msg: &[u8]) -> Option<Session> {
    let r = msg.get(4..4 + 0x100)?;
    let u = |o: usize| u32::from_le_bytes(r[o..o + 4].try_into().unwrap());
    (r[0xe0..0xf0] == APP_GUID).then(|| Session {
        name: cstr(&r[0x10..0x90]),
        players: u(0xc),
        max_players: u(8),
        password: u(4) & 8 != 0,
    })
}

// ---- server side ----

/// The 0x100-byte session record (`NetSock_HostSession 0x4ab570`). space.exe passes 0 for the
/// four game-data dwords. `password` is blanked in enum replies, as Netsock does.
pub fn session_record(name: &str, max_players: u32, players: u32, password: bool, session_guid: [u8; 16]) -> Vec<u8> {
    let mut r = vec![0u8; 0x100];
    r[0..4].copy_from_slice(&0x100u32.to_le_bytes());
    r[4..8].copy_from_slice(&(7u32 | if password { 8 } else { 0 }).to_le_bytes());
    r[8..12].copy_from_slice(&max_players.to_le_bytes());
    r[12..16].copy_from_slice(&players.to_le_bytes());
    r[0x10..0x90].copy_from_slice(&fixed_str(name, 128));
    r[0xd0..0xe0].copy_from_slice(&session_guid);
    r[0xe0..0xf0].copy_from_slice(&APP_GUID);
    r
}

pub fn enum_reply(record: &[u8]) -> Vec<u8> {
    message(ENUM_REPLY, record)
}

/// Join refusal reasons for 0x1021.
pub const REFUSE_PASSWORD: u16 = 1;
pub const REFUSE_GUID: u16 = 2;
pub const REFUSE_FULL: u16 = 3;

/// 0x1021 `{u16 status (1 accept, 2 refuse), u16 reason, u32}`.
pub fn join_reply(accept: bool, reason: u16) -> Vec<u8> {
    let mut b = (if accept { 1u16 } else { 2 }).to_le_bytes().to_vec();
    b.extend_from_slice(&reason.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    message(JOIN_REPLY, &b)
}

/// 0x1031 `{u32 result, u32 playerId}`.
pub fn create_reply(result: u32, player_id: u32) -> Vec<u8> {
    let mut b = result.to_le_bytes().to_vec();
    b.extend_from_slice(&player_id.to_le_bytes());
    message(CREATE_REPLY, &b)
}

pub fn destroy_reply() -> Vec<u8> {
    message(DESTROY_REPLY, &0u32.to_le_bytes())
}

pub fn close_reply() -> Vec<u8> {
    message(CLOSE_REPLY, &0u32.to_le_bytes())
}

pub fn server_closing() -> Vec<u8> {
    message(SERVER_CLOSING, &[])
}

/// A parsed client → server session message.
#[derive(Debug)]
pub enum ClientMsg<'a> {
    Enum { guid_ok: bool },
    Join { guid_ok: bool, password: String },
    CreatePlayer { request: u32, name: String, data: &'a [u8] },
    DestroyPlayer { player_id: u32 },
    Close,
    GameData(&'a [u8]),
    Other(u16),
}

pub fn parse_client(msg: &[u8]) -> Option<ClientMsg<'_>> {
    let k = kind(msg);
    let body = msg.get(4..)?;
    Some(match k {
        ENUM => ClientMsg::Enum { guid_ok: body.get(..16)? == APP_GUID },
        JOIN => ClientMsg::Join { guid_ok: body.get(..16)? == APP_GUID, password: cstr(body.get(16..80)?) },
        CREATE_PLAYER => {
            let request = u32::from_le_bytes(body.get(..4)?.try_into().ok()?);
            let name = cstr(body.get(4..0x84)?);
            let size = u32::from_le_bytes(body.get(0x88..0x8c)?.try_into().ok()?) as usize;
            // data starts at message +0xa0 = body +0x9c
            let data = body.get(0x9c..0x9c + size).or_else(|| body.get(0x9c..))?;
            ClientMsg::CreatePlayer { request, name, data }
        }
        DESTROY_PLAYER => ClientMsg::DestroyPlayer { player_id: u32::from_le_bytes(body.get(..4)?.try_into().ok()?) },
        CLOSE => ClientMsg::Close,
        GAME_DATA => ClientMsg::GameData(game_payload(msg)?),
        k => ClientMsg::Other(k),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_matches_table() {
        assert_eq!(CRC[1], 0x04c1_1db7);
        assert_eq!(CRC[255], 0xb1f7_40b4);
    }

    #[test]
    fn round_trip_short_and_long() {
        for n in [5usize, 31, 32, 33, 200] {
            let msg: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            let d = encode(&msg);
            assert_eq!(decode(&d).as_deref(), Some(&msg[..]), "len {n}");
            let mut bad = d.clone();
            bad[n / 2 + 4] ^= 0x10;
            assert!(decode(&bad).is_none());
        }
    }

    #[test]
    fn game_data_wrapping() {
        let m = game_data(&[1, 2, 3]);
        assert_eq!(m.len(), 15);
        assert_eq!(u16::from_le_bytes([m[2], m[3]]), 15);
        assert_eq!(game_payload(&m), Some(&[1u8, 2, 3][..]));
    }

    #[test]
    fn server_messages_round_trip() {
        let rec = session_record("Test", 16, 2, true, [7; 16]);
        let s = parse_session(&enum_reply(&rec)).unwrap();
        assert_eq!((s.name.as_str(), s.players, s.max_players, s.password), ("Test", 2, 16, true));
        match parse_client(&create_player(3, "Bob", &[1, 2, 3])).unwrap() {
            ClientMsg::CreatePlayer { request, name, data } => assert_eq!((request, name.as_str(), data), (3, "Bob", &[1u8, 2, 3][..])),
            m => panic!("{m:?}"),
        }
        match parse_client(&join_request("pw")).unwrap() {
            ClientMsg::Join { guid_ok, password } => assert!(guid_ok && password == "pw"),
            m => panic!("{m:?}"),
        }
        match parse_client(&game_data(&[9, 8, 7])).unwrap() {
            ClientMsg::GameData(p) => assert_eq!(p, [9, 8, 7]),
            m => panic!("{m:?}"),
        }
    }
}
