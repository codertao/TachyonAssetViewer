//! `.PWF` sound banks (`SPACE.PWF` for flight, `MENUS.PWF` for the front end), opened by
//! `FUN_0049cf30` (`SPACE.PWF` from `FUN_00431a50`).
//!
//! ```text
//! +0x00 u32 header size (0x1c)   +0x04 "PWF2"   +0x08 u32 sample count   +0x0c u32 trigger count
//! +0x10 u32 total size
//! then count × 0x34: name[0x18], u32 offset, u32 size, u32 volume (0xff00 = full), 4 × u32 0
//! then triggers × 0xc ("SNDTRIG_TRIGGERS"), then the samples: plain RIFF WAVs.
//! ```
//! Samples are looked up by name (`FUN_0049d150`). Nearly all are PCM mono at 22,050 Hz,
//! 8- or 16-bit; one is MS-ADPCM (not decoded here).

use anyhow::{ensure, Result};
use std::collections::HashMap;

pub struct Bank {
    data: Vec<u8>,
    /// Upper-case name → (offset, size, volume 0..1).
    entries: HashMap<String, (usize, usize, f32)>,
}

/// A decoded mono sample.
pub struct Sound {
    pub rate: u32,
    pub samples: Vec<f32>,
    /// The bank entry's volume, 0..1.
    pub volume: f32,
}

impl Bank {
    pub fn parse(data: Vec<u8>) -> Result<Bank> {
        let u32_ = |o: usize| -> Result<u32> {
            Ok(u32::from_le_bytes(data.get(o..o + 4).ok_or_else(|| anyhow::anyhow!("PWF truncated at {o:#x}"))?.try_into().unwrap()))
        };
        ensure!(data.get(4..8) == Some(b"PWF2"), "not a PWF2 sound bank");
        let (hs, n) = (u32_(0)? as usize, u32_(8)? as usize);
        ensure!(n < 10000, "implausible sample count");
        let mut entries = HashMap::new();
        for i in 0..n {
            let o = hs + i * 0x34;
            let name: String = data.get(o..o + 0x18).unwrap_or(&[]).iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
            let (off, size, vol) = (u32_(o + 0x18)? as usize, u32_(o + 0x1c)? as usize, u32_(o + 0x20)?);
            if off + size <= data.len() {
                entries.insert(name.to_uppercase(), (off, size, vol as f32 / 0xff00 as f32));
            }
        }
        Ok(Bank { data, entries })
    }

    pub fn sound(&self, name: &str) -> Option<Sound> {
        let &(off, size, volume) = self.entries.get(&name.to_uppercase())?;
        let (rate, samples) = decode_wav(&self.data[off..off + size])?;
        Some(Sound { rate, samples, volume })
    }
}

/// PCM RIFF WAV → (rate, mono samples in −1..1).
fn decode_wav(w: &[u8]) -> Option<(u32, Vec<f32>)> {
    if w.get(0..4)? != b"RIFF" || w.get(8..12)? != b"WAVE" {
        return None;
    }
    let (mut fmt, mut data) = (None, None);
    let mut p = 12;
    while p + 8 <= w.len() {
        let len = u32::from_le_bytes(w[p + 4..p + 8].try_into().ok()?) as usize;
        let body = w.get(p + 8..(p + 8 + len).min(w.len()))?;
        match &w[p..p + 4] {
            b"fmt " if body.len() >= 16 => fmt = Some(body),
            b"data" => data = Some(body),
            _ => {}
        }
        p += 8 + len + (len & 1);
    }
    let (fmt, data) = (fmt?, data?);
    let le16 = |o: usize| u16::from_le_bytes([fmt[o], fmt[o + 1]]);
    let (tag, channels, rate, bits) = (le16(0), le16(2).max(1) as usize, u32::from_le_bytes(fmt[4..8].try_into().ok()?), le16(14));
    if tag != 1 {
        return None;
    }
    let frame: Vec<f32> = match bits {
        8 => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        16 => data.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0).collect(),
        _ => return None,
    };
    let samples = frame.chunks_exact(channels).map(|c| c.iter().sum::<f32>() / channels as f32).collect();
    Some((rate, samples))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::AssetSource;

    #[test]
    fn space_bank_has_gate_sounds() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let Ok(d) = src.read("SPACE.PWF") else { return };
        let b = Bank::parse(d).unwrap();
        assert_eq!(b.entries.len(), 334);
        let s = b.sound("GATE_OPEN_1").unwrap();
        assert_eq!(s.rate, 22050);
        assert!(s.samples.len() > 300_000 && s.volume == 1.0);
        let r = b.sound("gate_rumble").unwrap();
        assert!((r.volume - 0xaa as f32 / 0xff as f32).abs() < 1e-3);
    }
}
