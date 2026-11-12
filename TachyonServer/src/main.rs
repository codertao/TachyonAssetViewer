//! tachyon-server: a console dedicated multiplayer server for Tachyon: The Fringe.
//! Plan and milestones: `/home/matt/.claude/plans/sunny-doodling-forest.md`; deliberate
//! differences from the original host: `DEVIATIONS.md`.

mod console;
mod map;
mod packetlog;
mod server;
mod world;

use anyhow::{anyhow, bail, Context, Result};
use std::net::UdpSocket;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tachyon_core::net::netsock;
use tachyon_core::source::AssetSource;

const USAGE: &str = "\
usage: tachyon-server --assets <Tachyon.pff|dir> --map <name> [options]
       tachyon-server --assets <Tachyon.pff|dir> --list-maps

options:
  --port <n>            UDP port (default 2766, the in-game Network host port)
  --name <s>            session name shown in the game list (default \"Tachyon Server\")
  --max-players <n>     cap below the map's MAX_PLAYERS
  --password <s>        session password
  --original-bugs       reproduce the original host's known bugs (see DEVIATIONS.md)
  --log-packets <file>  write every packet in and out to <file>
  --join-page <host[:port]>
                        print a Startup.htm that makes a stock client join this server from
                        the Network (NovaWorld) screen, then exit (port defaults to --port)
";

#[derive(Debug)]
pub struct Args {
    pub assets: PathBuf,
    pub map: Option<String>,
    pub list_maps: bool,
    pub port: u16,
    pub name: String,
    pub max_players: Option<u32>,
    pub password: String,
    pub original_bugs: bool,
    pub log_packets: Option<PathBuf>,
    pub join_page: Option<String>,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args> {
    let mut a = Args {
        assets: PathBuf::new(),
        map: None,
        list_maps: false,
        port: 2766,
        name: "Tachyon Server".into(),
        max_players: None,
        password: String::new(),
        original_bugs: false,
        log_packets: None,
        join_page: None,
    };
    let mut assets = None;
    while let Some(arg) = it.next() {
        let mut val = || it.next().ok_or_else(|| anyhow!("{arg} needs a value"));
        match arg.as_str() {
            "--assets" => assets = Some(PathBuf::from(val()?)),
            "--map" => a.map = Some(val()?),
            "--list-maps" => a.list_maps = true,
            "--port" => a.port = val()?.parse().context("--port")?,
            "--name" => a.name = val()?,
            "--max-players" => a.max_players = Some(val()?.parse().context("--max-players")?),
            "--password" => a.password = val()?,
            "--original-bugs" => a.original_bugs = true,
            "--log-packets" => a.log_packets = Some(PathBuf::from(val()?)),
            "--join-page" => a.join_page = Some(val()?),
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            _ => bail!("unknown argument {arg:?}\n\n{USAGE}"),
        }
    }
    if a.join_page.is_some() {
        return Ok(a);
    }
    a.assets = assets.ok_or_else(|| anyhow!("--assets is required\n\n{USAGE}"))?;
    if !a.list_maps && a.map.is_none() {
        bail!("--map is required (or --list-maps)\n\n{USAGE}");
    }
    Ok(a)
}

fn list_maps(src: &AssetSource) {
    let (maps, bad) = tachyon_core::mpc::list(src);
    println!("{:<12} {:<11} {:>4}  {:<16} {}", "MAP", "TYPE", "MAX", "SPX", "TITLE");
    for m in &maps {
        let note = if src.read(&m.main_spx).is_err() { "  (SPX missing)" } else { "" };
        println!("{:<12} {:<11} {:>4}  {:<16} {}{note}", m.name, m.game_type_name, m.max_players, m.main_spx, m.title);
    }
    for b in bad {
        println!("unreadable: {b}");
    }
}

fn main() -> Result<()> {
    let args = parse_args(std::env::args().skip(1))?;
    if let Some(h) = &args.join_page {
        let hp = if h.contains(':') { h.clone() } else { format!("{h}:{}", args.port) };
        let page = tachyon_core::net::novaworld::join_page(&hp, 1000, &args.name)
            .ok_or_else(|| anyhow!("{hp:?} is too long to encode (21 characters at most)"))?;
        print!("{page}");
        return Ok(());
    }
    let src = AssetSource::open(&args.assets).with_context(|| format!("opening assets {}", args.assets.display()))?;
    if args.list_maps {
        list_maps(&src);
        return Ok(());
    }
    let cfg = tachyon_core::mpc::find(&src, args.map.as_deref().unwrap())?;
    let map = map::Map::load(src, cfg)?;
    for w in &map.warnings {
        eprintln!("warning: {w}");
    }
    println!("{}", map.summary());
    let log = match &args.log_packets {
        Some(p) => Some(packetlog::PacketLog::create(p).with_context(|| format!("creating {}", p.display()))?),
        None => None,
    };
    let opts = server::Options {
        name: args.name.clone(),
        password: args.password.clone(),
        max_players: args.max_players.unwrap_or(u32::MAX),
        original_bugs: args.original_bugs,
    };
    let mut srv = server::Server::new(opts, map, log);
    run(&mut srv, args.port)
}

fn run(srv: &mut server::Server, port: u16) -> Result<()> {
    let sock = UdpSocket::bind(("0.0.0.0", port)).with_context(|| format!("binding UDP port {port}"))?;
    sock.set_nonblocking(true)?;
    println!("listening on UDP {port}, {} player slots; type /help", srv.conns.len());
    let stdin = console::spawn_stdin();
    let quit = Arc::new(AtomicBool::new(false));
    {
        let q = quit.clone();
        let _ = ctrlc_hook(move || q.store(true, Ordering::SeqCst));
    }
    let mut buf = vec![0u8; 0x10000];
    let mut next = Instant::now();
    while !quit.load(Ordering::SeqCst) {
        loop {
            match sock.recv_from(&mut buf) {
                Ok((n, addr)) => {
                    if let Some(msg) = netsock::decode(&buf[..n]) {
                        srv.on_datagram(addr, &msg);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                // Windows reports ICMP port-unreachable from an earlier send as a recv error.
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
                Err(e) => return Err(e.into()),
            }
        }
        while let Ok(line) = stdin.try_recv() {
            if line.trim_start_matches('/').eq_ignore_ascii_case("quit") {
                quit.store(true, Ordering::SeqCst);
                break;
            }
            if line.is_empty() {
                continue;
            }
            let line = if line.starts_with('/') { line } else { format!("/say {line}") };
            for l in console::run_command(srv, &line, None) {
                println!("{l}");
            }
        }
        let now = Instant::now();
        if now >= next {
            srv.tick();
            next += server::TICK;
            if now > next + server::TICK * 50 {
                next = now; // fell far behind (suspended?): don't try to catch up
            }
        }
        for (addr, msg) in srv.outgoing.drain(..) {
            let _ = sock.send_to(&netsock::encode(&msg), addr);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    srv.shutdown();
    for (addr, msg) in srv.outgoing.drain(..) {
        let _ = sock.send_to(&netsock::encode(&msg), addr);
    }
    println!("server stopped");
    Ok(())
}

/// SIGINT without an extra crate: a thread can't catch it portably, so rely on the default
/// (process exit) and offer /quit for a clean shutdown.
fn ctrlc_hook(_f: impl Fn() + Send + 'static) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Result<Args> {
        parse_args(s.split_whitespace().map(String::from))
    }

    #[test]
    fn cli() {
        let a = args("--assets ../pff --map DM_01 --port 3000 --original-bugs").unwrap();
        assert_eq!((a.map.as_deref(), a.port, a.original_bugs), (Some("DM_01"), 3000, true));
        assert!(args("--assets ../pff").is_err());
        assert!(args("--map DM_01").is_err());
        assert!(args("--assets x --list-maps").unwrap().list_maps);
        assert!(args("--assets x --map a --bogus").is_err());
    }
}
