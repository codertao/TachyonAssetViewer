//! `--log-packets <file>`: every engine packet in and out, decoded to sub-messages with a hex
//! dump, so a failed real-client test can be diagnosed from the log alone.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;
use tachyon_core::net::packet;

pub struct PacketLog {
    out: BufWriter<File>,
    start: Instant,
}

impl PacketLog {
    pub fn create(path: &Path) -> std::io::Result<Self> {
        Ok(PacketLog { out: BufWriter::new(File::create(path)?), start: Instant::now() })
    }

    pub fn note(&mut self, s: &str) {
        let t = self.start.elapsed().as_secs_f64();
        let _ = writeln!(self.out, "{t:9.3} # {s}");
    }

    pub fn packet(&mut self, incoming: bool, slot: u16, p: &[u8]) {
        let t = self.start.elapsed().as_secs_f64();
        let dir = if incoming { "<-" } else { "->" };
        let Some((seq, ack, subs)) = packet::parse(p) else {
            let _ = writeln!(self.out, "{t:9.3} {dir} slot {slot}: short packet {}", hex(p));
            return;
        };
        let _ = writeln!(self.out, "{t:9.3} {dir} slot {slot}: seq {seq} ack {ack}, {} bytes", p.len());
        for s in subs {
            let tag = s.tag.map_or(String::new(), |t| format!(" tag {t:#x}"));
            let _ = writeln!(self.out, "            {:#06x}{tag} [{}] {}", s.kind, s.body.len(), hex(&s.body[..s.body.len().min(96)]));
        }
        let _ = self.out.flush();
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}
