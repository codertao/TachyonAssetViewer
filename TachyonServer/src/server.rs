//! The server: Netsock sessions, the engine layer per connection (startup stages, reliable
//! events, round-robin sends) and the game rules hooks. Single-threaded, ticked at 50 Hz.
//! References: `decomp/notes/multiplayer.md`, especially "Server-side startup details".

use crate::map::Map;
use crate::packetlog::PacketLog;
use crate::world::{distance, Entity, Owner, Power, World, ANGLE_MASK, FX};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tachyon_core::mpc::GameType;
use tachyon_core::net::events::{self as ev};
use tachyon_core::net::netsock::{self, ClientMsg};
use tachyon_core::net::packet::{self, Outbox, PacketWriter, TagRing, BUDGET_RELIABLE};
use tachyon_core::net::proto::{self, parse_join_blob, parse_player_data, JoinInfo, StatusOut};
use tachyon_core::net::startup::{self, RosterEntry};
use tachyon_core::weapons;

pub const TICK: Duration = Duration::from_millis(20);
/// Netsock drops a silent connection after 120 s.
const TIMEOUT: Duration = Duration::from_secs(120);
/// Near / far interest radii (`Ent_SetQuadrant 0x4577d0`), 16.16.
const NEAR_RADIUS: i64 = 3750 * FX as i64;
const FAR_RADIUS: i64 = 10000 * FX as i64;
/// Explosion effect radii (`0x535a74`, units), indexed by effect.
const EFFECT_RADIUS: [i64; 12] = [256, 625, 1000, 1000, 5000, 1500, 1500, 5000, 1250, 1250, 1500, 625];
/// The protocol/build magic in the join blob.
const JOIN_MAGIC: u32 = 0x1002;
/// The player ships of `vars.cfg` `ship_type`, preloaded so their model and descriptor slots
/// are the same for every client (see DEVIATIONS.md).
const PLAYER_SHIPS: [&str; 11] = [
    "GALSPAN_PEGASUS", "GALSPAN_POSEIDON", "GALSPAN_ORION", "GALSPAN_ARCHANGEL", "GALSPAN_PHOENIX",
    "BORA_MACE", "BORA_CUTLASS", "BORA_BATTLEAXE", "BORA_WARHAMMER", "BORA_CLAYMORE", "MAKO",
];

pub struct Options {
    pub name: String,
    pub password: String,
    pub max_players: u32,
    pub original_bugs: bool,
}

/// Delivery spec (`NetSrv_QueueEvent`; filter `Tach_SrvEventFilter 0x4620b0`).
#[derive(Clone, Copy, Default)]
pub struct Delivery {
    pub budget: i32,
    pub flags: u16,
    pub radius: i64,
    pub pos: [i32; 3],
    pub player: Option<u16>,
    pub team: i32,
}

pub const D_RADIUS: u16 = 0x01;
pub const D_EXCEPT: u16 = 0x02;
pub const D_ONLY: u16 = 0x04;
pub const D_TEAM: u16 = 0x08;
pub const D_GROUP: u16 = 0x10;
pub const D_QUADRANT: u16 = 0x20;
pub const D_SYNCED: u16 = 0x40;
pub const D_LOADED: u16 = 0x80;

impl Delivery {
    pub fn all() -> Self {
        Delivery { budget: BUDGET_RELIABLE, ..Default::default() }
    }
    pub fn only(slot: u16) -> Self {
        Delivery { budget: BUDGET_RELIABLE, flags: D_ONLY, player: Some(slot), ..Default::default() }
    }
    pub fn except(slot: u16) -> Self {
        Delivery { budget: BUDGET_RELIABLE, flags: D_EXCEPT, player: Some(slot), ..Default::default() }
    }
}

/// One player connection: the Netsock peer plus the engine state (`TachConn`).
pub struct Conn {
    pub addr: SocketAddr,
    pub netsock_id: u32,
    pub slot: u16,
    pub name: String,
    #[allow(dead_code)] // team ships and rules hooks read it from M2 on
    pub join: JoinInfo,
    tx_seq: u32,
    rx_ack: u32,
    outbox: Outbox,
    tags: TagRing,
    /// STARTUPDATA +4 (conn +0x8c), starting at 1.
    stage_wire: u32,
    /// Blob index (conn +0x90).
    stage_idx: u32,
    pub in_startup: bool,
    client_frame: u32,
    pub last_heard: Instant,
    pub kick: Option<u8>,
    kick_sends: u32,
    /// conn +0x13c: 3 placed, 0 in game, 2 loading, 1 empty snapshots.
    pub state: u8,
    pub camera: [i32; 3],
    pub ship: u32,
    /// conn +0x140 (preferred / campaign team).
    pub team: u32,
    pub systems: u32,
    /// `(weapon id, max ammo, ammo)` per mount (conn +0x164, stride 0xc).
    pub mounts: [Option<(u8, u16, u16)>; 8],
    pub portrait: i16,
    chat_flood: i32,
    pub kills: u16,
    pub deaths: u16,
    far_list: Vec<u32>,
    far_cursor: Option<usize>,
}

/// A Netsock peer that has joined (0x1020) but may not have created a player yet.
struct Peer {
    last_heard: Instant,
    player: Option<u32>,
}

pub struct Server {
    pub opts: Options,
    pub map: Map,
    pub world: World,
    pub conns: Vec<Option<Conn>>,
    peers: HashMap<SocketAddr, Peer>,
    next_netsock_id: u32,
    session: u32,
    session_guid: [u8; 16],
    pub game_frame: u32,
    net_tick: u32,
    portraits: Vec<String>,
    /// Datagrams to send: (address, Netsock message).
    pub outgoing: Vec<(SocketAddr, Vec<u8>)>,
    pub log: Option<PacketLog>,
    rng: u64,
}

fn now_seed() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1) | 1
}

impl Server {
    pub fn new(opts: Options, map: Map, log: Option<PacketLog>) -> Server {
        let slots = opts.max_players.min(map.config.max_players).max(1) as usize;
        let seed = now_seed();
        let mut s = Server {
            opts,
            map,
            world: World::new(),
            conns: (0..slots).map(|_| None).collect(),
            peers: HashMap::new(),
            next_netsock_id: 1,
            session: (seed as u32 & 0xffff) | 1,
            session_guid: std::array::from_fn(|i| (seed >> ((i % 8) * 8)) as u8 ^ i as u8),
            game_frame: 0,
            net_tick: 100,
            portraits: vec![],
            outgoing: vec![],
            log,
            rng: seed,
        };
        s.load_world();
        s
    }

    fn rand(&mut self) -> u32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 32) as u32
    }

    /// Register the map's descriptors in SPX order, then the player ships.
    fn load_world(&mut self) {
        for d in self.map.decls.iter().flatten() {
            self.world.tables.add_des(d);
        }
        for alias in PLAYER_SHIPS {
            if let Ok(d) = self.map.registry.resolve(&self.map.src, alias) {
                self.world.tables.add_des(&d);
            }
        }
        // World statics (asteroids, stations, powerups) are spawned in M2.
    }

    pub fn player_count(&self) -> usize {
        self.conns.iter().flatten().count()
    }

    fn session_record(&self) -> Vec<u8> {
        netsock::session_record(&self.opts.name, self.conns.len() as u32, self.player_count() as u32, !self.opts.password.is_empty(), self.session_guid)
    }

    fn send_raw(&mut self, addr: SocketAddr, msg: Vec<u8>) {
        self.outgoing.push((addr, msg));
    }

    // ---- Netsock layer ----

    pub fn on_datagram(&mut self, addr: SocketAddr, msg: &[u8]) {
        let Some(m) = netsock::parse_client(msg) else { return };
        if let Some(p) = self.peers.get_mut(&addr) {
            p.last_heard = Instant::now();
        }
        if let Some(c) = self.conns.iter_mut().flatten().find(|c| c.addr == addr) {
            c.last_heard = Instant::now();
        }
        match m {
            ClientMsg::Enum { guid_ok: true } => {
                let r = self.session_record();
                self.send_raw(addr, netsock::enum_reply(&r));
            }
            ClientMsg::Enum { .. } => {}
            ClientMsg::Join { guid_ok, password } => {
                let refuse = if !guid_ok {
                    Some(netsock::REFUSE_GUID)
                } else if !self.opts.password.is_empty() && password != self.opts.password {
                    Some(netsock::REFUSE_PASSWORD)
                } else if !self.peers.contains_key(&addr) && self.conns.iter().all(Option::is_some) {
                    Some(netsock::REFUSE_FULL)
                } else {
                    None
                };
                match refuse {
                    Some(r) => self.send_raw(addr, netsock::join_reply(false, r)),
                    None => {
                        self.peers.entry(addr).or_insert(Peer { last_heard: Instant::now(), player: None });
                        self.send_raw(addr, netsock::join_reply(true, 0));
                    }
                }
            }
            ClientMsg::CreatePlayer { name, data, .. } => {
                if !self.peers.contains_key(&addr) {
                    return;
                }
                // A repeated 0x1030 gets the existing id back.
                if let Some(id) = self.peers[&addr].player {
                    self.send_raw(addr, netsock::create_reply(0, id));
                    return;
                }
                let Some(slot) = self.conns.iter().position(Option::is_none) else {
                    self.send_raw(addr, netsock::create_reply(1, 0));
                    return;
                };
                let id = self.next_netsock_id;
                self.next_netsock_id += 1;
                self.peers.get_mut(&addr).unwrap().player = Some(id);
                self.send_raw(addr, netsock::create_reply(0, id));
                let data = data.to_vec();
                self.player_joined(addr, id, slot as u16, &name, &data);
            }
            ClientMsg::DestroyPlayer { player_id } => {
                self.send_raw(addr, netsock::destroy_reply());
                if let Some(slot) = self.slot_of(addr).filter(|&s| self.conns[s as usize].as_ref().unwrap().netsock_id == player_id) {
                    self.remove_player(slot, "left");
                }
                if let Some(p) = self.peers.get_mut(&addr) {
                    p.player = None;
                }
            }
            ClientMsg::Close => {
                self.send_raw(addr, netsock::close_reply());
                if let Some(slot) = self.slot_of(addr) {
                    self.remove_player(slot, "left");
                }
                self.peers.remove(&addr);
            }
            ClientMsg::GameData(p) => {
                if let Some(slot) = self.slot_of(addr) {
                    let p = p.to_vec();
                    self.on_packet(slot, &p);
                }
            }
            ClientMsg::Other(_) => {}
        }
    }

    fn slot_of(&self, addr: SocketAddr) -> Option<u16> {
        self.conns.iter().flatten().find(|c| c.addr == addr).map(|c| c.slot)
    }

    pub fn conn(&self, slot: u16) -> Option<&Conn> {
        self.conns.get(slot as usize).and_then(Option::as_ref)
    }

    fn conn_mut(&mut self, slot: u16) -> Option<&mut Conn> {
        self.conns.get_mut(slot as usize).and_then(Option::as_mut)
    }

    // ---- events ----

    fn passes(&self, c: &Conn, d: &Delivery) -> bool {
        if d.flags & D_LOADED != 0 && c.in_startup {
            return false;
        }
        if d.flags & D_SYNCED != 0 && c.state != 0 {
            return false;
        }
        if d.flags & D_RADIUS != 0 && distance(c.camera, d.pos) > d.radius {
            return false;
        }
        if d.flags & D_EXCEPT != 0 && d.player == Some(c.slot) {
            return false;
        }
        if d.flags & D_ONLY != 0 && d.player != Some(c.slot) {
            return false;
        }
        let ship_team = self.world.get(c.ship).map_or(-1, |e| e.team as i32);
        if d.flags & D_TEAM != 0 && ship_team != d.team {
            return false;
        }
        if d.flags & D_GROUP != 0 && c.team as i32 != d.team {
            return false;
        }
        // One quadrant per map for now, so D_QUADRANT always passes.
        true
    }

    pub fn queue(&mut self, kind: u16, body: Vec<u8>, d: Delivery) {
        let targets: Vec<u16> = self.conns.iter().flatten().filter(|c| self.passes(c, &d)).map(|c| c.slot).collect();
        for s in targets {
            let c = self.conn_mut(s).unwrap();
            c.outbox.queue(kind, body.clone(), d.budget);
        }
    }

    pub fn text_to(&mut self, d: Delivery, color: u16, s: &str) {
        self.queue(ev::EV_TEXT, ev::text(0, color, s), d);
    }

    fn roster_entry(&self, c: &Conn) -> RosterEntry {
        RosterEntry {
            slot: c.slot,
            portrait: c.portrait,
            a: 0,
            b: 0,
            team: self.world.get(c.ship).map_or(0xff, |e| e.team),
            name: c.name.clone(),
        }
    }

    fn scoreboard(&mut self) {
        let mut rows: Vec<(u16, u16, u16)> = self.conns.iter().flatten().map(|c| (c.slot, c.kills, c.deaths)).collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.2.cmp(&b.2)));
        self.queue(ev::EV_SCOREBOARD, ev::scoreboard(&rows), Delivery::all());
    }

    fn portrait_slot(&mut self, name: &str) -> i16 {
        if name.is_empty() {
            return -1;
        }
        if let Some(i) = self.portraits.iter().position(|p| p.eq_ignore_ascii_case(name)) {
            return i as i16;
        }
        if self.portraits.len() >= 64 {
            return -1;
        }
        self.portraits.push(name.to_string());
        (self.portraits.len() - 1) as i16
    }

    // ---- join / leave (`Tach_SrvPlacePlayer 0x461280`, `Tach_SrvRemovePlayer 0x461400`) ----

    fn player_joined(&mut self, addr: SocketAddr, netsock_id: u32, slot: u16, name: &str, blob: &[u8]) {
        let join = parse_join_blob(blob);
        let name = if name.is_empty() { "JAKE_LOGAN".to_string() } else { name.to_string() };
        let kick = (join.magic != JOIN_MAGIC).then_some(5u8);
        let mut c = Conn {
            addr,
            netsock_id,
            slot,
            name: name.clone(),
            join: join.clone(),
            tx_seq: 1,
            rx_ack: 0,
            outbox: Outbox::new(0x1000),
            tags: TagRing::default(),
            stage_wire: 1,
            stage_idx: 0,
            in_startup: true,
            client_frame: 0,
            last_heard: Instant::now(),
            kick,
            kick_sends: 0,
            state: 3,
            camera: [0; 3],
            ship: 0,
            team: join.preferred_team,
            systems: join.systems,
            mounts: [None; 8],
            portrait: -1,
            chat_flood: 0,
            kills: 0,
            deaths: 0,
            far_list: vec![],
            far_cursor: None,
        };
        // Loadout: each mount at its max ammo (`MpRules_CopyLoadout`, then `MpRules_ClearLoadout`
        // copies max into the ammo counter).
        for (i, &m) in join.mounts.iter().enumerate() {
            if m == 0 {
                continue;
            }
            if let Some(w) = weapons::by_id((m - 1) as u8) {
                let max = if join.systems & 0x10_0000 != 0 { w.ammo + (w.ammo >> 2) } else { w.ammo } as u16;
                c.mounts[i] = Some((w.id, max, max));
            }
        }
        self.conns[slot as usize] = Some(c);

        // The mode's join hook picks team and ship, then spawn.
        let (team, alias) = match self.map.config.game_type {
            GameType::BaseWars => {
                // Placeholder until M6: team from the preferred side, the matching team ship.
                if join.preferred_team == 1 { (1u32, join.bora_ship.clone()) } else { (2u32, join.galspan_ship.clone()) }
            }
            _ => (join.dm_team, join.dm_ship.clone()),
        };
        let des = match self.map.registry.resolve(&self.map.src, &alias) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{name}: ship {alias:?}: {e}");
                self.map.registry.resolve(&self.map.src, "BORA_CUTLASS").expect("BORA_CUTLASS")
            }
        };
        let (desc_slot, model) = self.world.tables.add_des(&des).unwrap_or((0, None));
        let id = self.world.alloc_id();
        let (pos, angles) = self.dm_spawn_pos();
        let power = Power::for_des(&des);
        self.world.spawn(Entity {
            id,
            owner: Owner::Player(slot),
            des: des.clone(),
            desc_slot,
            model: model.unwrap_or(0),
            pos,
            vel: [0; 3],
            angles,
            net_pos: pos,
            net_angles: angles,
            state_flags: 5,
            throttle: 0,
            team: team as u8,
            net_flags: 0,
            scale: FX,
            power,
            target: 0,
        });
        let portrait_name = join.portrait.clone();
        let portrait = self.portrait_slot(&portrait_name);
        let is_bw = self.map.config.game_type == GameType::BaseWars;
        {
            let c = self.conn_mut(slot).unwrap();
            c.ship = id;
            c.camera = pos;
            c.portrait = portrait;
            if !is_bw {
                c.team = team;
            }
        }
        self.queue(ev::EV_TEAM_SHIP, ev::team_ship(&des.file, desc_slot), Delivery::all());
        if portrait >= 0 {
            self.queue(ev::EV_PORTRAIT, ev::portrait(&portrait_name, portrait as u16), Delivery::all());
        }
        self.text_to(Delivery::except(slot), ev::COLOR_JOIN, &format!("{name} is entering the game"));
        let e = self.roster_entry(self.conn(slot).unwrap());
        self.queue(ev::EV_ROSTER, ev::roster(&e), Delivery::all());
        self.scoreboard();
        println!("{name} joined from {addr} as slot {slot}, flying {} ({}), team {team}", des.file, alias);
    }

    /// `DM_PickSpawnPos`: (0, 625, 2500) rotated about Y by a random yaw, facing the origin.
    fn dm_spawn_pos(&mut self) -> ([i32; 3], [i32; 3]) {
        let yaw = (self.rand() as i32) & ANGLE_MASK;
        let a = yaw as f64 / (512.0 * 65536.0) * std::f64::consts::TAU;
        let (x, z) = (2500.0 * a.sin(), 2500.0 * a.cos());
        let pos = [(x * 65536.0) as i32, 625 * FX, (z * 65536.0) as i32];
        (pos, face_toward(pos, [0; 3]))
    }

    pub fn remove_player(&mut self, slot: u16, why: &str) {
        let Some(c) = self.conns[slot as usize].take() else { return };
        if let Some(p) = self.peers.get_mut(&c.addr) {
            p.player = None;
        }
        if let Some(e) = self.world.remove(c.ship) {
            let pos = e.pos;
            let mut body = Vec::with_capacity(0x16);
            for v in pos {
                body.extend_from_slice(&v.to_le_bytes());
            }
            body.extend_from_slice(&[0; 8]);
            body.extend_from_slice(&[3, 0]);
            self.queue(ev::EV_DESTROYED, body, Delivery { budget: 4, flags: D_SYNCED | D_QUADRANT | D_RADIUS, radius: EFFECT_RADIUS[3] * FX as i64, pos, ..Default::default() });
        }
        self.text_to(Delivery::all(), ev::COLOR_JOIN, &format!("{} has left the game", c.name));
        self.queue(ev::EV_ROSTER_CLEARED, ev::roster_cleared(slot), Delivery::all());
        self.scoreboard();
        println!("{} ({}) {why}", c.name, c.addr);
    }

    pub fn kick(&mut self, slot: u16, reason: u8) {
        if let Some(c) = self.conn_mut(slot) {
            c.kick = Some(reason);
        }
    }

    // ---- engine packets in ----

    fn on_packet(&mut self, slot: u16, p: &[u8]) {
        if let Some(l) = &mut self.log {
            l.packet(true, slot, p);
        }
        let Some((seq, ack, subs)) = packet::parse(p) else { return };
        {
            let c = self.conn_mut(slot).unwrap();
            if c.rx_ack != 0 && seq <= c.rx_ack {
                return;
            }
            c.rx_ack = seq;
            c.outbox.on_ack(ack);
        }
        for s in subs {
            match s.kind {
                packet::MSG_PLAYERDATA => self.on_player_data(slot, s.body),
                packet::EV_STARTUP_ACK => {
                    let c = self.conn_mut(slot).unwrap();
                    if c.tags.first_time(s.tag.unwrap_or(0)) && proto::u16_at(s.body, 0) == 1 {
                        let stage = proto::u32_at(s.body, 2);
                        self.on_startup_ack(slot, stage);
                    }
                }
                k if k >= 8 => {
                    let c = self.conn_mut(slot).unwrap();
                    if c.tags.first_time(s.tag.unwrap_or(0)) {
                        self.on_event(slot, k, s.body);
                    }
                }
                _ => {}
            }
        }
    }

    /// `NetSrv_HandlePlayerData` + `Tach_SrvReadPlayerData 0x460b30`.
    fn on_player_data(&mut self, slot: u16, b: &[u8]) {
        let (frame, session) = (proto::u32_at(b, 4), proto::u32_at(b, 8));
        if session != self.session {
            return;
        }
        let Some(pd) = b.get(12..).and_then(parse_player_data) else { return };
        let c = self.conn_mut(slot).unwrap();
        if frame <= c.client_frame {
            return;
        }
        c.client_frame = frame;
        c.camera = pd.camera;
        let ship = c.ship;
        if let Some(e) = self.world.get_mut(ship) {
            if pd.has_ship {
                e.vel = pd.vel;
                e.net_pos = pd.pos;
                e.net_angles = pd.angles;
            }
            e.target = pd.target;
            e.power.shield_rate = pd.shield_rate;
            e.throttle = (pd.throttle >> 3) as u8;
        }
    }

    /// `NetSrv_AdvanceStartupStage` + `Tach_SrvStartupStageDone 0x462080`.
    fn on_startup_ack(&mut self, slot: u16, stage: u32) {
        let c = self.conn_mut(slot).unwrap();
        if !c.in_startup || stage != c.stage_wire {
            return;
        }
        let done = c.stage_idx == 5;
        c.stage_wire += 1;
        c.stage_idx += 1;
        if done {
            c.in_startup = false;
            c.state = 0;
            let name = c.name.clone();
            self.queue(ev::EV_STARTUP_DONE, ev::startup_done(), Delivery::only(slot));
            println!("{name} finished loading");
        }
    }

    fn on_event(&mut self, slot: u16, kind: u16, b: &[u8]) {
        match kind {
            ev::CL_CHAT => self.on_chat(slot, b),
            ev::CL_DATA_ACK => {
                // Static pages arrive in M2; kind 2 only marks the conn as in the quadrant.
                let _ = ev::parse_data_ack(b);
            }
            ev::CL_ENTER_WORLD => {
                if let Some(c) = self.conn_mut(slot) {
                    c.state = 0;
                }
            }
            ev::CL_READY => {
                if let Some(c) = self.conn_mut(slot) {
                    c.state = 2;
                }
            }
            ev::CL_QUADRANT_ENTERED => {}
            ev::CL_LOADOUT => self.on_loadout(slot, b),
            _ => {
                if let Some(l) = &mut self.log {
                    l.note(&format!("slot {slot}: unhandled event {kind:#06x} ({} bytes)", b.len()));
                }
            }
        }
    }

    /// 0x2028 `{u32 systems, u16 n, {u16 weapon+1, u16 ammo}[n]}`, applied as sent.
    fn on_loadout(&mut self, slot: u16, b: &[u8]) {
        let systems = proto::u32_at(b, 0);
        let n = proto::u16_at(b, 4) as usize;
        let c = self.conn_mut(slot).unwrap();
        c.systems = systems;
        for i in 0..8 {
            c.mounts[i] = None;
            if i < n {
                let (w, ammo) = (proto::u16_at(b, 6 + 4 * i), proto::u16_at(b, 8 + 4 * i));
                if w != 0 {
                    let max = weapons::by_id((w - 1) as u8).map_or(ammo, |d| d.ammo as u16);
                    c.mounts[i] = Some(((w - 1) as u8, max, ammo));
                }
            }
        }
    }

    /// `TachSrv_OnChat 0x460600`. Lines starting with `/` are server commands: echoed to the
    /// sender like any chat, answered, and not relayed (the original's command hook
    /// `Stub_ReturnZeroB` is a stub).
    fn on_chat(&mut self, slot: u16, b: &[u8]) {
        let (mode, target, text) = ev::parse_chat(b);
        let c = self.conn_mut(slot).unwrap();
        let flooded = c.chat_flood > 0;
        c.chat_flood += 50;
        if flooded {
            return;
        }
        let name = c.name.clone();
        let is_command = text.trim_start().starts_with('/') || self.command_text(&text).is_some();
        match mode {
            0 => {
                self.text_to(Delivery::only(slot), ev::COLOR_CHAT, &text);
                if is_command {
                    let reply = crate::console::run_command(self, &text, Some(slot));
                    for line in reply {
                        self.text_to(Delivery::only(slot), ev::COLOR_INFO, &line);
                    }
                } else {
                    self.text_to(Delivery::except(slot), ev::COLOR_CHAT, &text);
                    println!("chat (slot {slot}, {name}) {text}");
                }
            }
            1 => {
                if self.conn(target).is_some() {
                    self.text_to(Delivery::only(target), ev::COLOR_PRIVATE, &text);
                }
            }
            4 => self.text_to(Delivery { budget: BUDGET_RELIABLE, flags: D_TEAM, team: target as i32, ..Default::default() }, ev::COLOR_TEAM, &text),
            8 => self.text_to(Delivery { budget: BUDGET_RELIABLE, flags: D_GROUP, team: target as i32, ..Default::default() }, ev::COLOR_GROUP, &text),
            _ => {}
        }
    }

    /// The client prefixes chat with "NAME: "; a command may follow that prefix.
    fn command_text<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.split_once(": ").map(|(_, t)| t.trim_start()).filter(|t| t.starts_with('/'))
    }

    pub fn strip_chat_prefix<'a>(&self, text: &'a str) -> &'a str {
        self.command_text(text).unwrap_or(text.trim_start())
    }

    // ---- tick ----

    pub fn tick(&mut self) {
        // Per-player think: smoothing filter.
        let ids: Vec<u32> = self.world.order.clone();
        for id in ids {
            if let Some(e) = self.world.get_mut(id)
                && matches!(e.owner, Owner::Player(_))
            {
                e.smooth_step();
            }
        }
        for c in self.conns.iter_mut().flatten() {
            if c.chat_flood > 0 {
                c.chat_flood -= 1;
            }
        }
        // Timeouts.
        let now = Instant::now();
        let dead: Vec<u16> = self.conns.iter().flatten().filter(|c| now.duration_since(c.last_heard) > TIMEOUT).map(|c| c.slot).collect();
        for s in dead {
            self.remove_player(s, "timed out");
        }
        self.peers.retain(|_, p| now.duration_since(p.last_heard) <= TIMEOUT || p.player.is_some());
        self.send_round();
        self.game_frame = self.game_frame.wrapping_add(1);
        self.net_tick = self.net_tick.wrapping_add(1);
    }

    /// `NetSrv_SendRound 0x4b8650`: slots with `(slot & 15) == (tick & 15)`.
    fn send_round(&mut self) {
        let phase = self.net_tick & 15;
        let slots: Vec<u16> = self.conns.iter().flatten().filter(|c| (c.slot as u32 & 15) == phase).map(|c| c.slot).collect();
        let mut kicked = vec![];
        for s in slots {
            let p = self.build_packet(s);
            let c = self.conn_mut(s).unwrap();
            if c.kick.is_some() {
                c.kick_sends += 1;
                if c.kick_sends >= 4 {
                    kicked.push(s);
                }
            }
            let addr = c.addr;
            if let Some(l) = &mut self.log {
                l.packet(false, s, &p);
            }
            self.send_raw(addr, netsock::game_data(&p));
        }
        for s in kicked {
            self.remove_player(s, "kicked");
        }
    }

    fn build_packet(&mut self, slot: u16) -> Vec<u8> {
        let (seq, ack, kick, in_startup, stage_wire, stage_idx) = {
            let c = self.conn(slot).unwrap();
            (c.tx_seq, c.rx_ack, c.kick, c.in_startup, c.stage_wire, c.stage_idx)
        };
        let mut w = PacketWriter::new(seq, ack);
        if let Some(r) = kick {
            w.sub(packet::MSG_KICK, &[r, 0, 0, 0]);
        } else if in_startup {
            let mut b = stage_wire.to_le_bytes().to_vec();
            b.extend_from_slice(&self.session.to_le_bytes());
            b.extend_from_slice(&(slot as u32).to_le_bytes());
            b.extend_from_slice(&self.startup_blob(slot, stage_idx));
            w.sub(packet::MSG_STARTUPDATA, &b);
        } else {
            let mut info = self.net_tick.to_le_bytes().to_vec();
            info.extend_from_slice(&self.session.to_le_bytes());
            info.extend_from_slice(&self.game_frame.to_le_bytes());
            w.sub(packet::MSG_PLAYERINFO, &info);
            let gd = self.game_data(slot);
            w.sub(packet::MSG_GAMEDATA, &gd);
            let c = self.conn_mut(slot).unwrap();
            c.outbox.write_into(&mut w, seq);
        }
        self.conn_mut(slot).unwrap().tx_seq += 1;
        w.finish()
    }

    fn startup_blob(&self, slot: u16, idx: u32) -> Vec<u8> {
        let c = self.conn(slot).unwrap();
        let rec = match idx {
            0 => startup::model_table(&self.world.tables.model_entries()),
            1 => startup::desc_table(&self.world.tables.desc_entries()),
            2 => {
                let e = self.world.get(c.ship);
                let k1 = startup::rules_info(&startup::RulesInfo {
                    bdf: &self.map.spx.backdrop,
                    region: "NOVAWORLD",
                    entity_id: c.ship,
                    rules_value: 1,
                    region_index: 9,
                    quadrant_id: self.map.spx.sector_id as u16,
                    slot,
                    max_slots: self.conns.len() as u16,
                    game_mode: if self.map.config.game_type == GameType::BaseWars { 0 } else { 1 },
                });
                let mut mounts = [(0u16, 0u16); 8];
                for (i, m) in c.mounts.iter().enumerate() {
                    if let Some((w, max, _)) = m {
                        mounts[i] = (*w as u16 + 1, *max);
                    }
                }
                let k4 = startup::player_init(&startup::PlayerInit {
                    des_name: e.map_or("", |e| e.des.file.as_str()),
                    team: c.team as u16,
                    systems: c.systems,
                    pos: e.map_or([0; 3], |e| e.pos),
                    angles: e.map_or([0; 3], |e| e.angles),
                    mounts,
                });
                return startup::blob(&[k1, k4]);
            }
            3 => startup::portraits(&self.portraits.iter().enumerate().map(|(i, n)| (n.clone(), i as u16)).collect::<Vec<_>>()),
            4 => startup::roster(&self.conns.iter().flatten().map(|c| self.roster_entry(c)).collect::<Vec<_>>()),
            _ => startup::session_info(0, 0, 0),
        };
        startup::blob(&[rec])
    }

    /// `Tach_SrvWriteGameData 0x461a30` + `Tach_SrvWriteSnapshot 0x461870`.
    fn game_data(&mut self, slot: u16) -> Vec<u8> {
        let (camera, ship, state) = {
            let c = self.conn(slot).unwrap();
            (c.camera, c.ship, c.state)
        };
        let me = self.world.get(ship);
        let mut status = StatusOut { target_meters: [0xff; 3], ..Default::default() };
        let mut target = 0;
        if let Some(e) = me {
            status.shield_front = e.power.shield_front.clamp(0, 0xffff) as u16;
            status.shield_rear = e.power.shield_rear.clamp(0, 0xffff) as u16;
            status.hull = e.power.hull.clamp(0, 0xffff) as u16;
            target = e.target;
            if let Some(t) = self.world.get(target) {
                status.target_meters = [pct(t.power.hull, t.power.hull_max), pct(t.power.shield_front, t.power.shield_max), 0xff];
            }
        }
        let mut recs = vec![];
        if state != 1 {
            let visible = |e: &Entity| e.id != ship && e.net_flags & 1 == 0;
            // Near: within the near radius, at most 8 (2 while loading).
            let mut near: Vec<(i64, u32)> = self.world.order.iter().filter_map(|id| self.world.get(*id)).filter(|e| visible(e)).map(|e| (distance(camera, e.pos), e.id)).filter(|(d, _)| *d < NEAR_RADIUS).collect();
            if !self.opts.original_bugs {
                near.sort_by_key(|(d, _)| *d);
            }
            let cap = if state == 2 { 2 } else { 8 };
            let mut sent: Vec<u32> = near.iter().take(cap).map(|(_, id)| *id).collect();
            // Your target, if not already in.
            if target != 0 && !sent.contains(&target) && self.world.get(target).is_some_and(|e| visible(e)) {
                sent.push(target);
            }
            for id in &sent {
                recs.push(self.world.get(*id).unwrap().record(0, false));
            }
            // Far: two more per snapshot from a cursor over everything within the far radius.
            let far_ids = self.far_records(slot, camera, ship, &sent);
            for id in far_ids {
                recs.push(self.world.get(id).unwrap().record(proto::REC_FAR, false));
            }
        }
        proto::write_gamedata(&status, camera, (0, 0), &recs)
    }

    /// `Tach_SrvBuildFarList` / `Tach_SrvWriteFarRecords`.
    fn far_records(&mut self, slot: u16, camera: [i32; 3], ship: u32, sent: &[u32]) -> Vec<u32> {
        let rebuild = self.conn(slot).unwrap().far_cursor.is_none();
        if rebuild {
            let list: Vec<u32> = self.world.order.iter().filter_map(|id| self.world.get(*id)).filter(|e| e.id != ship && e.net_flags & 1 == 0 && distance(e.pos, camera) < FAR_RADIUS).map(|e| e.id).collect();
            let c = self.conn_mut(slot).unwrap();
            c.far_cursor = (!list.is_empty()).then_some(0);
            c.far_list = list;
        }
        let mut out = vec![];
        loop {
            let c = self.conn(slot).unwrap();
            let Some(i) = c.far_cursor else { break };
            if out.len() >= 2 {
                break;
            }
            if i >= c.far_list.len() {
                self.conn_mut(slot).unwrap().far_cursor = None;
                break;
            }
            let id = c.far_list[i];
            if !sent.contains(&id) && self.world.get(id).is_some_and(|e| e.net_flags & 1 == 0) {
                out.push(id);
            }
            self.conn_mut(slot).unwrap().far_cursor = Some(i + 1);
        }
        out
    }

    pub fn shutdown(&mut self) {
        let addrs: Vec<SocketAddr> = self.peers.keys().copied().collect();
        for a in addrs {
            self.send_raw(a, netsock::server_closing());
        }
    }

    pub fn status_lines(&self, with_addresses: bool) -> Vec<String> {
        let mut v = vec![format!(
            "{} on {} ({}): {}/{} players, frame {}",
            self.opts.name,
            self.map.config.name,
            self.map.config.game_type_name,
            self.player_count(),
            self.conns.len(),
            self.game_frame
        )];
        for c in self.conns.iter().flatten() {
            let ship = self.world.get(c.ship).map_or("-".to_string(), |e| e.des.file.clone());
            let st = if c.in_startup { format!("loading {}/6", c.stage_idx) } else { "in game".into() };
            let addr = if with_addresses { c.addr.to_string() } else { String::new() };
            v.push(format!("  slot {:2} {:<16} {:<14} K{} D{} {st} {addr}", c.slot, c.name, ship, c.kills, c.deaths));
        }
        v
    }
}

fn pct(v: i32, max: i32) -> u8 {
    if max <= 0 { 0xff } else { (v.max(0) as i64 * 100 / max as i64).min(100) as u8 }
}

/// (yaw, pitch, 0) facing from `from` toward `to`, 16.16 angles at 512 per turn. The original
/// (`DM_PickSpawnPos` → `Vec_DirAngles(pos, origin)`: yaw = Math_Atan2(dx, dz), pitch =
/// Math_Atan2(dy, -|dxz|)) depends on `Math_Atan2`'s argument convention, which isn't pinned
/// down yet [?]; this aims the nose at the target with yaw = atan2(dx, dz) and no pitch, which the
/// client then owns anyway.
pub fn face_toward(from: [i32; 3], to: [i32; 3]) -> [i32; 3] {
    let d: [f64; 3] = std::array::from_fn(|k| to[k] as f64 - from[k] as f64);
    let yaw = d[0].atan2(d[2]);
    let pitch = 0.0;
    let unit = |a: f64| ((a / std::f64::consts::TAU * 512.0 * 65536.0) as i64 as i32) & ANGLE_MASK;
    [unit(yaw), unit(pitch), 0]
}


#[cfg(test)]
mod tests {
    use super::*;
    use tachyon_core::net::proto::{join_blob, player_data, startup_records, JoinBlob, ShipState};
    use tachyon_core::source::AssetSource;

    fn server() -> Option<Server> {
        let src = AssetSource::open(std::path::Path::new("../../pff")).ok()?;
        let cfg = tachyon_core::mpc::find(&src, "DM_01").ok()?;
        let map = Map::load(src, cfg).ok()?;
        Some(Server::new(Options { name: "t".into(), password: String::new(), max_players: 8, original_bugs: false }, map, None))
    }

    /// Drives a fake client through join, the six startup stages and into the game, and checks
    /// a second client then shows up in the first one's snapshots.
    #[test]
    fn join_handshake_and_snapshots() {
        let Some(mut s) = server() else { return };
        let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let blob = join_blob(&JoinBlob { ship: "BORA_CUTLASS", team: 1, portrait: "", game_id: 0, weapon: 6 });
        for a in [addr, addr2] {
            s.on_datagram(a, &netsock::join_request(""));
            s.on_datagram(a, &netsock::create_player(1, "P", &blob));
        }
        let mut seqs = [1u32, 1];
        let mut stages_seen = [0u32; 2];
        let mut saw_other = false;
        let mut tag = 0x1000;
        for _ in 0..16 * 10 {
            s.tick();
            for (a, msg) in std::mem::take(&mut s.outgoing) {
                let i = if a == addr { 0 } else { 1 };
                let Some(ClientMsg::GameData(p)) = netsock::parse_client(&msg) else { continue };
                let (seq, _, subs) = packet::parse(p).unwrap();
                let mut w = PacketWriter::new(seqs[i], seq);
                seqs[i] += 1;
                for sub in subs {
                    match sub.kind {
                        packet::MSG_STARTUPDATA => {
                            let stage = proto::u32_at(sub.body, 0);
                            assert!(!startup_records(&sub.body[12..]).is_empty());
                            stages_seen[i] = stages_seen[i].max(stage);
                            let mut b = 1u16.to_le_bytes().to_vec();
                            b.extend_from_slice(&stage.to_le_bytes());
                            w.event(packet::EV_STARTUP_ACK, tag, &b);
                            tag += 2;
                        }
                        packet::MSG_GAMEDATA if i == 0 => {
                            let snap = proto::parse_gamedata(sub.body).unwrap();
                            saw_other |= !snap.records.is_empty();
                        }
                        _ => {}
                    }
                }
                let mut pd = 0u32.to_le_bytes().to_vec();
                pd.extend_from_slice(&(seqs[i] * 16).to_le_bytes());
                pd.extend_from_slice(&s.session.to_le_bytes());
                pd.extend_from_slice(&player_data(&ShipState { pos: [0, 625 << 16, (2000 + 100 * i as i32) << 16], valid: true, ..Default::default() }, seqs[i] * 16));
                w.sub(packet::MSG_PLAYERDATA, &pd);
                s.on_datagram(a, &netsock::game_data(&w.finish()));
            }
        }
        assert_eq!(stages_seen, [6, 6]);
        assert!(s.conns.iter().flatten().all(|c| !c.in_startup));
        assert!(saw_other, "the second player never appeared in the first one's snapshots");
    }
}
