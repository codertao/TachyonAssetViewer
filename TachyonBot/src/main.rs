//! TachyonBot: a headless Tachyon: The Fringe multiplayer client. Joins a game, flies slow
//! circles and logs what it sees. Protocol notes: `decomp/notes/multiplayer.md`.

mod engine;
mod flight;
use tachyon_core::net::netsock;
use tachyon_core::net::proto;
mod world;

use anyhow::{bail, Context, Result};
use engine::{Engine, Sub};
use proto::*;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use world::{fmt, World};

const USAGE: &str = "usage: TachyonBot <host[:port]> [options]
  --name NAME        player name (default TachyonBot)
  --ship ALIAS       ship desnames alias (default BORA_CUTLASS)
  --team bora|galspan|N   team / race index (default bora)
  --portrait NAME    portrait name (default empty)
  --password PW      session password
  --game-id N|auto   server game id, needed for TCP/IP hosts (shown as ID: N on a dedicated
                     server; `auto` reads it from a space.exe running on this machine)
  --radius R         circle radius in world units (default 1500)
  --period S         seconds per circle (default 40)
  --weapon N         weapon type to fire (default 6)
  --fire-every S     seconds between shots (default 3; 0 = never fire)
  --muzzle U         start shots U world units ahead of the ship's centre (default 12)
  --respawn S        seconds to wait before respawning after death (default 5; 0 = never)
  --track ID         log every snapshot record of entity ID (hex)
  --say <text>       send this chat line (0x2022, to all) once startup completes; repeatable
  --verbose          log every event type, including unhandled ones
  --hexdump          print every datagram";

struct Args {
    host: SocketAddr,
    name: String,
    ship: String,
    team: u32,
    portrait: String,
    password: String,
    game_id: u32,
    radius: f64,
    period: f64,
    respawn: f64,
    weapon: u8,
    fire_every: f64,
    muzzle: f64,
    track: Option<u32>,
    verbose: bool,
    say: Vec<String>,
    hexdump: bool,
}

fn parse_args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let mut host = None;
    let mut a = Args {
        host: "127.0.0.1:3874".parse().unwrap(),
        name: "TachyonBot".into(),
        ship: "BORA_CUTLASS".into(),
        team: 1,
        portrait: String::new(),
        password: String::new(),
        game_id: 0,
        radius: 1500.0,
        period: 40.0,
        respawn: 5.0,
        weapon: 6,
        fire_every: 3.0,
        muzzle: 12.0,
        track: None,
        verbose: false,
        say: vec![],
        hexdump: false,
    };
    while let Some(s) = it.next() {
        let mut val = || it.next().with_context(|| format!("{s} needs a value"));
        match s.as_str() {
            "--name" => a.name = val()?,
            "--ship" => a.ship = val()?,
            "--team" => {
                let v = val()?;
                let t = v.to_lowercase();
                a.team = match RACES.iter().position(|r| *r == t) {
                    Some(i) => i as u32,
                    None => v.parse().with_context(|| format!("unknown team {v}"))?,
                };
            }
            "--portrait" => a.portrait = val()?,
            "--password" => a.password = val()?,
            "--game-id" => {
                let v = val()?;
                a.game_id = if v == "auto" { local_game_id()? } else { v.parse()? };
            }
            "--radius" => a.radius = val()?.parse()?,
            "--period" => a.period = val()?.parse()?,
            "--weapon" => a.weapon = val()?.parse()?,
            "--fire-every" => a.fire_every = val()?.parse()?,
            "--muzzle" => a.muzzle = val()?.parse()?,
            "--respawn" => a.respawn = val()?.parse()?,
            "--track" => a.track = Some(u32::from_str_radix(val()?.trim_start_matches("0x"), 16)?),
            "--say" => a.say.push(val()?),
            "--verbose" => a.verbose = true,
            "--hexdump" => a.hexdump = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            h if host.is_none() && !h.starts_with('-') => host = Some(h.to_string()),
            _ => bail!("unknown argument {s}\n{USAGE}"),
        }
    }
    let h = host.with_context(|| USAGE.to_string())?;
    let h = if h.contains(':') { h } else { format!("{h}:{}", netsock::DEFAULT_PORT) };
    a.host = h.to_socket_addrs()?.next().context("host didn't resolve")?;
    Ok(a)
}

/// Read the host's game id (`DAT_005a40b4`, random 1000..9999, checked against join blob
/// +0x78 when the session is advertised, `DAT_005a4040`) from a local Wine space.exe.
#[cfg(target_os = "linux")]
fn local_game_id() -> Result<u32> {
    use std::io::{Read, Seek, SeekFrom};
    for e in std::fs::read_dir("/proc")?.flatten() {
        let Ok(cmd) = std::fs::read(e.path().join("cmdline")) else { continue };
        let exe = cmd.split(|&c| c == 0).next().unwrap_or(&[]);
        if !String::from_utf8_lossy(exe).to_lowercase().ends_with("space.exe") && !String::from_utf8_lossy(exe).to_lowercase().ends_with("space_highres.exe") {
            continue;
        }
        let mut mem = std::fs::File::open(e.path().join("mem")).context("can't read space.exe memory")?;
        let mut rd = |addr: u64| -> Result<u32> {
            let mut b = [0u8; 4];
            mem.seek(SeekFrom::Start(addr))?;
            mem.read_exact(&mut b)?;
            Ok(u32::from_le_bytes(b))
        };
        let (advertised, id) = (rd(0x5a4040)?, rd(0x5a40b4)?);
        if advertised == 0 {
            bail!("space.exe isn't hosting a TCP/IP game (no game id); LAN games use DirectPlay/IPX, which the bot doesn't speak");
        }
        println!("game id {id} (read from space.exe pid {})", e.file_name().to_string_lossy());
        return Ok(id);
    }
    bail!("no space.exe process found")
}

#[cfg(not(target_os = "linux"))]
fn local_game_id() -> Result<u32> {
    bail!("--game-id auto only works on Linux")
}

fn hexdump(dir: &str, b: &[u8]) {
    println!("{dir} {} bytes", b.len());
    for (i, c) in b.chunks(16).enumerate() {
        let hex: Vec<String> = c.iter().map(|x| format!("{x:02x}")).collect();
        let txt: String = c.iter().map(|&x| if (32..127).contains(&x) { x as char } else { '.' }).collect();
        println!("  {:04x}  {:<48} {txt}", i * 16, hex.join(" "));
    }
}

struct Net {
    sock: UdpSocket,
    host: SocketAddr,
    hexdump: bool,
    buf: Vec<u8>,
}

impl Net {
    fn send(&self, msg: &[u8]) {
        if self.hexdump {
            hexdump(">>", msg);
        }
        let _ = self.sock.send_to(&netsock::encode(msg), self.host);
    }

    /// The next valid message from the host, if any is waiting.
    fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            let (n, from) = match self.sock.recv_from(&mut self.buf) {
                Ok(r) => r,
                Err(_) => return None,
            };
            if from.ip() != self.host.ip() {
                continue;
            }
            match netsock::decode(&self.buf[..n]) {
                Some(m) => {
                    if self.hexdump {
                        hexdump("<<", &m);
                    }
                    return Some(m);
                }
                None => eprintln!("dropped a datagram with a bad CRC ({n} bytes)"),
            }
        }
    }
}

/// Enumerate (best effort), join, create the player. Returns the Netsock player id.
fn connect(net: &mut Net, a: &Args, world: &World) -> Result<u32> {
    // Enumerate: optional, but it shows what we're joining.
    let deadline = Instant::now() + Duration::from_millis(1500);
    net.send(&netsock::enum_request());
    while Instant::now() < deadline {
        if let Some(m) = net.recv()
            && netsock::kind(&m) == netsock::ENUM_REPLY
                && let Some(s) = netsock::parse_session(&m) {
                    world.log(format!(
                        "session \"{}\": {}/{} players{}",
                        s.name,
                        s.players,
                        s.max_players,
                        if s.password { ", password" } else { "" }
                    ));
                    break;
                }
        std::thread::sleep(Duration::from_millis(20));
    }

    world.log(format!("joining {}", net.host));
    let join = netsock::join_request(&a.password);
    let mut last = Instant::now() - Duration::from_secs(5);
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(15) {
            bail!("no answer to the join request");
        }
        if last.elapsed() >= Duration::from_secs(1) {
            net.send(&join);
            last = Instant::now();
        }
        if let Some(m) = net.recv()
            && netsock::kind(&m) == netsock::JOIN_REPLY {
                let (status, reason) = (u16_at(&m, 4), u16_at(&m, 6));
                if status == 1 {
                    break;
                }
                bail!(
                    "join refused: {}",
                    match reason {
                        1 => "wrong password",
                        2 => "wrong application GUID",
                        3 => "session full",
                        _ => "unknown reason",
                    }
                );
            }
        std::thread::sleep(Duration::from_millis(10));
    }

    let blob = join_blob(&JoinBlob { ship: &a.ship, team: a.team, portrait: &a.portrait, game_id: a.game_id, weapon: a.weapon });
    let create = netsock::create_player(1, &a.name, &blob);
    world.log(format!("joined; creating player \"{}\" flying {} for {}", a.name, a.ship, race(a.team)));
    let mut last = Instant::now() - Duration::from_secs(5);
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(15) {
            bail!("no answer to create-player");
        }
        if last.elapsed() >= Duration::from_secs(1) {
            net.send(&create);
            last = Instant::now();
        }
        if let Some(m) = net.recv()
            && netsock::kind(&m) == netsock::CREATE_REPLY {
                if u32_at(&m, 4) != 0 {
                    bail!("create-player failed");
                }
                return Ok(u32_at(&m, 8));
            }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Send destroy-player and close, briefly waiting for the replies.
fn leave(net: &mut Net, player_id: u32) {
    for (msg, reply) in [(netsock::destroy_player(player_id), netsock::DESTROY_REPLY), (netsock::close(), 0x1041)] {
        let start = Instant::now();
        let mut last = start - Duration::from_secs(1);
        'wait: while start.elapsed() < Duration::from_secs(2) {
            if last.elapsed() >= Duration::from_millis(250) {
                net.send(&msg);
                last = Instant::now();
            }
            while let Some(m) = net.recv() {
                if netsock::kind(&m) == reply {
                    break 'wait;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

struct Bot {
    engine: Engine,
    world: World,
    circle: flight::Circle,
    verbose: bool,
    say: Vec<String>,
    respawn_after: Option<Instant>,
    respawn_delay: f64,
    kicked: Option<u8>,
    weapon: u8,
    armed: bool,
    /// Highest server frame seen in PLAYERINFO (`FUN_00417e50` keeps the same max in
    /// `DAT_005b39cc`); receivers age 0x1002 shots against it.
    server_frame: u32,
}

impl Bot {
    fn me(&self) -> [f64; 3] {
        self.circle.position()
    }

    fn start_flying(&mut self, pos: [f64; 3], why: &str) {
        self.circle.start_at(pos);
        self.world.log(format!(
            "{why}: at {}, flying circles of radius {:.0} at {:.0} u/s",
            fmt(pos),
            self.circle.radius(),
            self.circle.speed()
        ));
    }

    fn handle(&mut self, subs: Vec<Sub>) {
        for s in subs {
            match s {
                Sub::Startup { stage, session, player, blob } => {
                    self.world.log(format!("startup stage {stage} (session {session}, player {player:#x})"));
                    self.on_startup(blob);
                }
                Sub::PlayerInfo { game_frame, .. } => self.server_frame = self.server_frame.max(game_frame),
                Sub::Kick(r) => {
                    self.world.log(format!(
                        "KICKED, reason {r}{}",
                        match r {
                            4 => " (wrong game id: try --game-id)",
                            5 => " (protocol magic mismatch)",
                            _ => "",
                        }
                    ));
                    self.kicked = Some(r);
                }
                Sub::GameData(b) => {
                    if let Some(snap) = parse_gamedata(b) {
                        let me = self.me();
                        self.world.apply_snapshot(&snap.records, me);
                        if snap.page_list != 0 {
                            self.engine.queue(0x2008, ack_body(1, snap.page_list as u32, snap.page_count));
                        }
                    }
                }
                Sub::Event { kind, body } => self.on_event(kind, body),
            }
        }
    }

    fn on_startup(&mut self, blob: &[u8]) {
        for (kind, rec) in startup_records(blob) {
            match kind {
                3 => {
                    let t = name_table(rec, true);
                    self.world.log(format!("  model table: {} entries", t.len()));
                    self.world.tables.models.extend(t);
                }
                2 => {
                    let t = name_table(rec, false);
                    self.world.log(format!("  descriptor table: {} entries", t.len()));
                    self.world.tables.descs.extend(t.into_iter().map(|(k, v)| (k as i16, v)));
                }
                1 => {
                    let id = u32_at(rec, 0x22);
                    self.world.me = Some(id);
                    self.world.log(format!(
                        "  you are entity {id:#010x}, slot {}, in {} / {} (quadrant {})",
                        u16_at(rec, 0x2e),
                        str_at(rec, 4),
                        str_at(rec, 0x12),
                        u16_at(rec, 0x2c)
                    ));
                }
                4 => {
                    let pos = vec3_at(rec, 0x18);
                    self.world.log(format!("  player state: race {} at {}", race(u16_at(rec, 0x12) as u32), fmt(pos)));
                    self.start_flying(pos, "ship placed");
                }
                5 => {
                    // `s16 count @+4`, then entries `{u16 slot, ..., char name[] @+8}` padded to
                    // even (`FUN_00417690`).
                    let mut o = 6;
                    for _ in 0..u16_at(rec, 4) {
                        let name = str_at(rec, o + 8);
                        let slot = u16_at(rec, o);
                        o += (name.len() + 1 + 9 + 1) & !1;
                        self.world.log(format!("  roster: slot {slot} {name}"));
                        self.world.roster.insert(slot, name);
                    }
                }
                7 => self.world.log("  session settings"),
                k => self.world.log(format!("  startup record kind {k} ({} bytes)", rec.len())),
            }
        }
    }

    fn on_event(&mut self, kind: u16, b: &[u8]) {
        let now = Instant::now();
        let me = self.me();
        match kind {
            0x1111 => {
                self.world.log("startup complete");
                self.engine.queue(0x2012, vec![0, 0]);
                // Chat 0x2022 `{u16 mode (0 = all), u16 target, char text[]}`, as the client
                // sends it: prefixed with the sender's name.
                for line in std::mem::take(&mut self.say) {
                    let mut b = vec![0, 0, 0, 0];
                    b.extend_from_slice(line.as_bytes());
                    b.push(0);
                    if b.len() & 1 != 0 {
                        b.push(0);
                    }
                    self.world.log(format!("saying {line:?}"));
                    self.engine.queue(0x2022, b);
                }
                // The server only relays a shot if the slot holds a weapon with ammo
                // (`TachSrv_OnFire`), and the join blob path leaves the ammo counter at 0, so
                // send a loadout (0x2028, applied as sent by `FUN_004607b0`).
                self.engine.queue(0x2028, loadout_body(0, &[(self.weapon, 30000)]));
                self.armed = true;
            }
            0x1007 => {
                let pos = vec3_at(b, 0);
                self.world.log(format!("entering quadrant {} \"{}\"", u16_at(b, 0x12), str_at(b, 0x16)));
                self.start_flying(pos, "spawned");
                self.engine.queue(0x2008, ack_body(2, 0, 0));
            }
            0x1012 => {
                let pos = vec3_at(b, 0);
                self.start_flying(pos, "teleported");
            }
            0x1106 => {
                let origin = [0, 4, 8].map(|k| i32_at(b, 0x1c + k));
                if let Some((r, _)) = parse_record(b, origin) {
                    self.world.apply_record(r, now, me, "SPAWN* ");
                }
            }
            0x1011 => self.world.remove(u32_at(b, 0), "despawned"),
            0x1003 => {
                let (dead, by) = (u32_at(b, 0xc), u32_at(b, 0x10));
                if Some(dead) == self.world.me {
                    self.world.log(format!("we exploded (killer/related {by:#010x})"));
                } else {
                    self.world.remove(dead, &format!("destroyed, effect {}, related {by:#010x}", b.get(0x14).unwrap_or(&0)));
                }
            }
            0x1114 => self.world.set_team(u32_at(b, 0), u16_at(b, 4) as u8),
            0x1005 => self.world.log("we took damage"),
            0x1006 => {
                self.world.log("WE WERE DESTROYED");
                if self.respawn_delay > 0.0 {
                    self.respawn_after = Some(now + Duration::from_secs_f64(self.respawn_delay));
                }
            }
            0x1001 => self.world.log(format!("MESSAGE: {}", str_at(b, 4))),
            0x1110 => {
                let (slot, name) = (u16_at(b, 0), str_at(b, 8));
                self.world.log(format!("PLAYER slot {slot}: {name}"));
                self.world.roster.insert(slot, name);
            }
            0x1029 => {
                let slot = u16_at(b, 0);
                let name = self.world.roster.remove(&slot).unwrap_or_default();
                self.world.log(format!("PLAYER slot {slot} left ({name})"));
            }
            0x1108 => self.world.log(format!("portrait {} for {}", u16_at(b, 14), str_at(b, 0))),
            0x1107 => self.world.log(format!("team ship descriptor {} (class {})", str_at(b, 0), u16_at(b, 14))),
            0x1112 => {
                let n = u16_at(b, 0) as usize;
                let rows: Vec<String> = (0..n.min(8))
                    .map(|i| {
                        let o = 2 + i * 6;
                        let slot = u16_at(b, o);
                        let who = self.world.roster.get(&slot).cloned().unwrap_or_else(|| format!("slot {slot}"));
                        format!("{who} {}/{}", u16_at(b, o + 2), u16_at(b, o + 4))
                    })
                    .collect();
                self.world.log(format!("SCORES (kills/deaths): {}", rows.join(", ")));
            }
            0x1032 => self.world.log(format!("ROUND RESULT {} vs {}: {}", str_at(b, 0), str_at(b, 16), u16_at(b, 0x20))),
            0x1119 => self.world.log("round over"),
            0x1120 => self.world.log("new round"),
            0x1033 => {
                self.engine.session = u32_at(b, 0);
                self.world.log(format!("session id now {}", self.engine.session));
            }
            0x1002 => {
                let pos = vec3_at(b, 0);
                let range = world::dist(pos, me);
                let shooter = u32_at(b, 0x18);
                // The age a receiver fast-forwards by (`CliOn_ProjectileFired`). Proj_CreateBolt and
                // Proj_SpawnRemoteVisual both call Proj_Launch with it, so it's applied twice.
                let age = (self.server_frame & 0xffff) as i32 - u16_at(b, 0x12) as i32;
                // Where the shot starts relative to the shooter's ship as we last saw it, split
                // into along its heading and sideways (yaw u8: 256 per turn, Ry turns +z to +x).
                let rel = self.world.entities.get(&shooter).map(|e| {
                    let yaw = e.rec.angles[0] as f64 * std::f64::consts::TAU / 256.0;
                    let d = [pos[0] - e.rec.pos[0], pos[1] - e.rec.pos[1], pos[2] - e.rec.pos[2]];
                    let ahead = d[0] * yaw.sin() + d[2] * yaw.cos();
                    let total = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
                    format!(" | from shooter's ship: {total:.0} ({ahead:.0} ahead)")
                });
                self.world.log(format!(
                    "SHOT weapon {} by {shooter:#010x} from {} dist={range:.0} age={age}{}",
                    b.get(0x16).unwrap_or(&0),
                    fmt(pos),
                    rel.unwrap_or_default()
                ));
            }
            0x1009 => self.world.log(format!("gate {:#010x} state {}", u32_at(b, 0), u16_at(b, 4))),
            0x1015 => self.world.log(format!("nav target {:#010x} \"{}\"", u32_at(b, 0), str_at(b, 8))),
            0x1016 => self.world.log(format!("ORDER: {}", str_at(b, 8))),
            0x1115 => self.world.log(format!("voice {}", str_at(b, 0))),
            0x1010 => self.world.log(format!("scene {}", str_at(b, 0))),
            _ => {
                if self.verbose {
                    self.world.log(format!("event {kind:#06x} ({} bytes)", b.len()));
                }
            }
        }
    }
}

/// 0x2028 body: `u32 systemsBits, u16 n, {u16 weapon + 1, u16 ammo}[n]` (slot order).
fn loadout_body(systems: u32, slots: &[(u8, u16)]) -> Vec<u8> {
    let mut v = systems.to_le_bytes().to_vec();
    v.extend_from_slice(&(slots.len() as u16).to_le_bytes());
    for &(w, ammo) in slots {
        v.extend_from_slice(&(w as u16 + 1).to_le_bytes());
        v.extend_from_slice(&ammo.to_le_bytes());
    }
    v
}

/// 0x2001 body (`NetCli_SendFire 0x415bf0`), 0x1c bytes: `vec3 pos, u16 angles[3] (high
/// halves), u16 clientFrame @0x12, u8 slot @0x14, u8 weapon @0x16, u8 volume @0x17`.
/// Receivers scale the shot sound by `volume * 2 / 256` (`Proj_SpawnRemoteVisual` →
/// `FUN_00466620`); `Weapon_TryFire` uses 0x80 = full, so 0 is silent.
///
/// A real client sends its bolt's position after `Proj_Launch`: the mount point plus
/// (model `+0x30` + one tick of bolt speed) forward, not the ship's centre. `muzzle` moves the
/// shot that far along the heading (yaw only; the bot flies level).
fn fire_body(ship: &ShipState, muzzle: f64, frame: u32, slot: u8, weapon: u8) -> Vec<u8> {
    let mut v = vec![0u8; 0x1c];
    let yaw = ship.angles[0] as f64 / UNIT * std::f64::consts::TAU / 512.0;
    let fwd = [yaw.sin(), 0.0, yaw.cos()];
    for k in 0..3 {
        let p = ship.pos[k].wrapping_add((fwd[k] * muzzle * UNIT) as i32);
        v[k * 4..k * 4 + 4].copy_from_slice(&p.to_le_bytes());
        v[0xc + k * 2..0xe + k * 2].copy_from_slice(&((ship.angles[k] as u32 >> 16) as u16).to_le_bytes());
    }
    v[0x12..0x14].copy_from_slice(&(frame as u16).to_le_bytes());
    v[0x14] = slot;
    v[0x16] = weapon;
    v[0x17] = 0x80;
    v
}

/// 0x2008 body: `u16 kind, u32 a, u16 b`.
fn ack_body(kind: u16, a: u32, b: u16) -> Vec<u8> {
    let mut v = kind.to_le_bytes().to_vec();
    v.extend_from_slice(&a.to_le_bytes());
    v.extend_from_slice(&b.to_le_bytes());
    v
}

fn main() -> Result<()> {
    let a = parse_args()?;
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_nonblocking(true)?;
    let mut net = Net { sock, host: a.host, hexdump: a.hexdump, buf: vec![0; 0x10000] };
    let world = World::new();
    let player_id = connect(&mut net, &a, &world)?;
    world.log(format!("player created (netsock id {player_id:#x}); waiting for startup data"));

    let quit = Arc::new(AtomicBool::new(false));
    {
        let q = quit.clone();
        let _ = ctrlc_hook(move || q.store(true, Ordering::SeqCst));
    }

    let mut world = world;
    world.track = a.track;
    let mut bot = Bot {
        engine: Engine::new(),
        world,
        circle: flight::Circle::new(a.radius, a.period),
        verbose: a.verbose,
        say: a.say.iter().map(|l| format!("{}: {l}", a.name)).collect(),
        respawn_after: None,
        respawn_delay: a.respawn,
        kicked: None,
        weapon: a.weapon,
        armed: false,
        server_frame: 0,
    };
    let tick = Duration::from_millis(20);
    let mut next = Instant::now();
    let mut ticks: u64 = 0;
    let mut last_heard = Instant::now();
    let mut ship = ShipState::default();
    while !quit.load(Ordering::SeqCst) {
        while let Some(m) = net.recv() {
            last_heard = Instant::now();
            match netsock::kind(&m) {
                netsock::GAME_DATA => {
                    if let Some(p) = netsock::game_payload(&m)
                        && let Some(subs) = bot.engine.receive(p) {
                            bot.handle(subs);
                        }
                }
                netsock::SERVER_CLOSING => {
                    bot.world.log("server closed the session");
                    return Ok(());
                }
                k => {
                    if a.verbose {
                        bot.world.log(format!("netsock message {k:#06x}"));
                    }
                }
            }
        }
        if let Some(r) = bot.kicked {
            bail!("kicked by the server (reason {r})");
        }
        if last_heard.elapsed() > Duration::from_secs(30) {
            bail!("nothing from the server for 30 s");
        }
        if bot.respawn_after.is_some_and(|t| Instant::now() >= t) {
            bot.respawn_after = None;
            bot.world.log("requesting respawn");
            bot.engine.queue(0x2011, vec![0, 0]);
        }

        let fire_ticks = (a.fire_every * 50.0) as u64;
        if bot.armed && bot.circle.active() && fire_ticks > 0 && ticks.is_multiple_of(fire_ticks) {
            bot.engine.queue(0x2001, fire_body(&ship, a.muzzle, bot.engine.frame, 0, a.weapon));
            if a.verbose {
                //bot.world.log(format!("fired weapon {}", a.weapon));
            }
        }
        if bot.circle.active() {
            ship = bot.circle.tick();
        }
        // The game sends every 16 ticks, counting from half an interval in.
        if ticks % 16 == 8 {
            let pd = player_data(&ship, bot.engine.frame);
            let p = bot.engine.build(&pd);
            net.send(&netsock::game_data(&p));
        }
        if ticks.is_multiple_of(50 * 30) && ticks > 0 {
            bot.world.log(bot.world.summary());
        }
        bot.engine.frame = bot.engine.frame.wrapping_add(1);
        ticks += 1;
        next += tick;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    bot.world.log("leaving");
    leave(&mut net, player_id);
    Ok(())
}

/// Ctrl-C → callback, without extra crates: a thread watching stdin closing isn't reliable,
/// so on Unix we poll a self-pipe set by a signal handler.
#[cfg(unix)]
fn ctrlc_hook(f: impl Fn() + Send + 'static) -> Result<()> {
    use std::sync::atomic::AtomicBool;
    static HIT: AtomicBool = AtomicBool::new(false);
    extern "C" fn on_sigint(_: i32) {
        HIT.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    unsafe extern "C" {
        fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
    }
    unsafe {
        signal(2, on_sigint);
    }
    std::thread::spawn(move || loop {
        if HIT.load(std::sync::atomic::Ordering::SeqCst) {
            f();
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    });
    Ok(())
}

#[cfg(not(unix))]
fn ctrlc_hook(_f: impl Fn() + Send + 'static) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod fire_tests {
    use super::*;

    #[test]
    fn muzzle_offset_follows_heading() {
        // Yaw a quarter turn (128 of 512): Ry(+90°) turns +z to +x, so "ahead" is +x.
        let ship = ShipState { pos: [0; 3], vel: [0; 3], angles: [128 << 16, 0, 0], valid: true };
        let v = fire_body(&ship, 12.0, 0, 0, 6);
        let p = vec3_at(&v, 0);
        assert!((p[0] - 12.0).abs() < 1e-3 && p[1].abs() < 1e-3 && p[2].abs() < 1e-3, "{p:?}");
        assert_eq!(u16_at(&v, 0xc), 128);
    }
}
