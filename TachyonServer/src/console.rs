//! Console and chat commands. The console reads stdin on a thread; chat commands arrive as
//! 0x2022 text starting with `/` (after the client's "NAME: " prefix). Anyone may use them.

use crate::server::{Delivery, Server};
use std::sync::mpsc::{channel, Receiver};
use tachyon_core::net::events as ev;

pub fn spawn_stdin() -> Receiver<String> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut line = String::new();
        while stdin.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
            if tx.send(line.trim().to_string()).is_err() {
                break;
            }
            line.clear();
        }
    });
    rx
}

const HELP: &[&str] = &[
    "/status                        players and map",
    "/say <text>                    message everyone (console)",
    "/kick <slot>                   drop a player",
    "/ai add <ship> [team] [skill] [count]   (milestone M5)",
    "/ai remove <id|all>, /ai list           (milestone M5)",
    "/quit                          stop the server (console only)",
];

/// Run one command line; returns reply lines. `from` is the chat sender's slot (None = console).
pub fn run_command(s: &mut Server, line: &str, from: Option<u16>) -> Vec<String> {
    let line = s.strip_chat_prefix(line).trim().to_string();
    let line = line.strip_prefix('/').unwrap_or(&line);
    let mut args = line.split_whitespace();
    let cmd = args.next().unwrap_or("").to_ascii_lowercase();
    let rest: Vec<&str> = args.collect();
    match cmd.as_str() {
        "" | "help" | "?" => HELP.iter().map(|s| s.to_string()).collect(),
        "status" | "players" => s.status_lines(from.is_none()),
        "say" if from.is_none() => {
            let text = format!("SERVER: {}", rest.join(" "));
            s.text_to(Delivery::all(), ev::COLOR_INFO, &text);
            vec![text]
        }
        "kick" => match rest.first().and_then(|a| a.parse::<u16>().ok()) {
            Some(slot) if s.conn(slot).is_some() => {
                s.kick(slot, 1);
                vec![format!("kicking slot {slot}")]
            }
            _ => vec!["usage: /kick <slot>".into()],
        },
        "ai" => vec!["AI pilots arrive in milestone M5".into()],
        "map" => vec![format!("map {} ({}); switching maps isn't supported yet", s.map.config.name, s.map.config.game_type_name)],
        _ => vec![format!("unknown command /{cmd} (try /help)")],
    }
}
