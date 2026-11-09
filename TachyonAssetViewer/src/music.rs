//! In-flight music: `Ttfmusic.sbf` (the segments) driven by `TTFMUSIC.BIN` (a compiled script).
//! See `decomp/notes/music.md`.
//!
//! The game streams one segment at a time (`FUN_004b53b0`). When a segment runs out it runs the
//! script (`FUN_004b5b80`), which picks the next one: an interactive score where each script
//! state is a theme (intro, loop bars, ending) and the game's variables (sector, job, hostiles,
//! shields) choose between themes. The viewer has no job or hostiles, so it picks a random
//! theme, lets the script sequence it, and moves on to another when the theme ends or has
//! played for a while.

use crate::audio::Stream;
use anyhow::{bail, ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------------------------
// Ttfmusic.sbf
// ---------------------------------------------------------------------------------------------

/// `SBF0` bank: `+0x14` count, `+0x18` count × 0x20 (name[16], offset, size, read size 0x1000, 0).
/// The audio is headerless IMA ADPCM, 22,050 Hz stereo, 1024-byte blocks.
pub struct Sbf {
    map: memmap2::Mmap,
    pub segs: Vec<(String, usize, usize)>,
}

pub const RATE: u32 = 22050;
const BLOCK: usize = 1024;
const BLOCK_FRAMES: usize = 1 + (BLOCK - 8) / 8 * 8;

impl Sbf {
    pub fn open(path: &Path) -> Result<Sbf> {
        let f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
        let map = unsafe { memmap2::Mmap::map(&f)? };
        ensure!(map.get(0..4) == Some(b"SBF0"), "not an SBF0 file");
        let u32_ = |o: usize| -> Result<usize> {
            Ok(u32::from_le_bytes(map.get(o..o + 4).context("SBF truncated")?.try_into().unwrap()) as usize)
        };
        let n = u32_(0x14)?;
        ensure!(n < 10000, "implausible SBF count");
        let mut segs = Vec::with_capacity(n);
        for i in 0..n {
            let o = 0x18 + i * 0x20;
            let name: String = map.get(o..o + 16).context("SBF truncated")?.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
            let (off, size) = (u32_(o + 16)?, u32_(o + 20)?);
            ensure!(off + size <= map.len(), "SBF segment {name} out of range");
            segs.push((name, off, size));
        }
        Ok(Sbf { map, segs })
    }

    /// Look for `Ttfmusic.sbf` near the asset source. It sits in the game folder beside
    /// `Tachyon.pff`, not inside it.
    pub fn find(source: &Path) -> Option<PathBuf> {
        let base = if source.is_dir() { source.to_path_buf() } else { source.parent()?.to_path_buf() };
        let mut dirs = vec![base.clone()];
        if let Some(p) = base.parent() {
            dirs.push(p.to_path_buf());
            if let Ok(rd) = std::fs::read_dir(p) {
                dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        }
        dirs.iter().find_map(|d| {
            std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).find(|p| {
                p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("ttfmusic.sbf"))
            })
        })
    }
}

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118,
    130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060,
    1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484,
    7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
const INDEX: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// One stereo IMA ADPCM block → interleaved frames in −1..1.
fn decode_block(b: &[u8], out: &mut Vec<[f32; 2]>) {
    out.clear();
    if b.len() < 8 {
        return;
    }
    let mut s = [0i32; 2];
    let mut ix = [0i32; 2];
    for c in 0..2 {
        s[c] = i16::from_le_bytes([b[4 * c], b[4 * c + 1]]) as i32;
        ix[c] = (b[4 * c + 2] as i32).min(88);
    }
    out.push([s[0] as f32 / 32768.0, s[1] as f32 / 32768.0]);
    let mut p = 8;
    let mut tmp = [[0f32; 8]; 2];
    while p + 8 <= b.len() {
        for c in 0..2 {
            for k in 0..4 {
                let v = b[p + 4 * c + k];
                for (j, n) in [v & 15, v >> 4].into_iter().enumerate() {
                    let st = STEPS[ix[c] as usize];
                    let mut d = st >> 3;
                    if n & 4 != 0 {
                        d += st;
                    }
                    if n & 2 != 0 {
                        d += st >> 1;
                    }
                    if n & 1 != 0 {
                        d += st >> 2;
                    }
                    s[c] = if n & 8 != 0 { s[c] - d } else { s[c] + d }.clamp(-32768, 32767);
                    ix[c] = (ix[c] + INDEX[(n & 7) as usize]).clamp(0, 88);
                    tmp[c][2 * k + j] = s[c] as f32 / 32768.0;
                }
            }
        }
        out.extend((0..8).map(|i| [tmp[0][i], tmp[1][i]]));
        p += 8;
    }
}

// ---------------------------------------------------------------------------------------------
// TTFMUSIC.BIN: the script and its VM (FUN_004b5b80 / FUN_004b5c10, opcode table 0x55ee5c)
// ---------------------------------------------------------------------------------------------

/// A loaded `SCR0` music script (`load_bin_scr_file 0x4b57e0`); only the first script
/// (`ttf_music`) is used.
pub struct Script {
    bin: Vec<u8>,
    code: usize,
    states: Vec<usize>,
    start: usize,
    consts: usize,
    globals: usize,
    locals: usize,
    imports: Vec<String>,
}

impl Script {
    pub fn parse(bin: Vec<u8>) -> Result<Script> {
        let u32_ = |o: usize| -> Result<usize> {
            Ok(u32::from_le_bytes(bin.get(o..o + 4).context("script truncated")?.try_into().unwrap()) as usize)
        };
        ensure!(bin.get(0..4) == Some(b"SCR0"), "not an SCR0 script");
        ensure!(u32_(8)? >= 1, "no scripts");
        let so = u32_(u32_(0xc)?)?;
        let h = |k: usize| u32_(so + k);
        let n_states = h(0x28)?;
        ensure!(n_states < 1000, "implausible state count");
        let tab = so + h(0x24)?;
        let states = (0..n_states).map(|i| u32_(tab + 4 * i).map(|v| if v != 0 { so + v } else { 0 })).collect::<Result<_>>()?;
        let mut imports = vec![];
        let (mut p, n_imp) = (u32_(0x14)?, u32_(0x10)?);
        for _ in 0..n_imp {
            let l = *bin.get(p).context("import table truncated")? as usize;
            ensure!(l > 0, "bad import table");
            imports.push(bin.get(p + 1..p + l).unwrap_or(&[]).iter().take_while(|&&c| c != 0).map(|&c| c as char).collect());
            p += l;
        }
        Ok(Script {
            code: so + h(0x20)?,
            states,
            start: h(0x2c)?,
            consts: so + h(0x30)?,
            globals: h(0x18)?,
            locals: h(0x1c)?,
            imports,
            bin,
        })
    }

    #[cfg(test)]
    pub fn n_states(&self) -> usize {
        self.states.len()
    }
}

/// The interpreter's state: memory (game variables then script globals, then locals), the
/// current state and resume point, volume and the `GGRnd` generator.
pub struct Vm {
    s: Arc<Script>,
    mem: Vec<u8>,
    pub state: usize,
    ip: usize,
    rng: u32,
    /// `GSV` sets both, `GSDV` only the target (reached by the segment's end).
    pub volume: i32,
    pub target: i32,
}

/// A stack value: a number, or an address in `mem` (for `FSet` and friends).
#[derive(Clone, Copy)]
enum Val {
    N(i32),
    A(usize),
}

impl Val {
    fn n(self) -> i32 {
        match self {
            Val::N(v) => v,
            Val::A(a) => a as i32,
        }
    }
}

enum Step {
    Next,
    /// Stop running until the current segment finishes (`yield`, `ret`, `goto`).
    Yield,
    Play(usize),
}

impl Vm {
    pub fn new(s: Arc<Script>, seed: u32) -> Vm {
        let mem = vec![0; s.globals + s.locals];
        let state = s.start.min(s.states.len().saturating_sub(1));
        let ip = s.states.get(state).copied().unwrap_or(0);
        Vm { s, mem, state, ip, rng: seed | 1, volume: 255, target: 255 }
    }

    pub fn goto(&mut self, state: usize) {
        if let Some(&ip) = self.s.states.get(state) {
            if ip != 0 {
                self.state = state;
                self.ip = ip;
            }
        }
    }

    fn rand(&mut self) -> u32 {
        let r = self.rng.rotate_left(11).wrapping_add(self.rng).rotate_left(2);
        self.rng = r;
        r
    }

    fn rd(&self, a: usize) -> i32 {
        self.mem.get(a..a + 4).map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap()))
    }

    fn wr(&mut self, a: usize, v: i32) {
        if let Some(b) = self.mem.get_mut(a..a + 4) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }

    fn b(&self, o: usize) -> Result<u8> {
        self.s.bin.get(o).copied().context("script ip out of range")
    }

    fn u32_at(&self, o: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.s.bin.get(o..o + 4).context("script ip out of range")?.try_into().unwrap()))
    }

    /// Run one scheduling pass (`FUN_004b5c10`): up to 32 instructions, then on while the stack
    /// holds anything. Returns the segment the script chose, if it chose one.
    pub fn run(&mut self) -> Result<Option<usize>> {
        let mut stack: Vec<Val> = Vec::with_capacity(16);
        let mut budget = 0x20i32;
        loop {
            match self.op(&mut stack)? {
                Step::Play(n) => return Ok(Some(n)),
                Step::Yield => return Ok(None),
                Step::Next => {}
            }
            budget -= 1;
            if budget <= 0 && stack.is_empty() {
                return Ok(None);
            }
            if budget < -100_000 {
                bail!("music script runaway");
            }
        }
    }

    fn op(&mut self, st: &mut Vec<Val>) -> Result<Step> {
        let s = self.s.clone();
        let mut ip = self.ip;
        let o = self.b(ip)?;
        ip += 1;
        let pop = |st: &mut Vec<Val>| st.pop().unwrap_or(Val::N(0));
        let (g, l) = (0usize, s.globals);
        match o {
            1 => {
                st.push(Val::N(self.b(ip)? as i32));
                ip += 1;
            }
            2 => {
                st.push(Val::N(self.u32_at(ip)? as i32));
                ip += 4;
            }
            3 | 4 => {
                let base = if o == 3 { g } else { l };
                st.push(Val::N(self.rd(base + self.b(ip)? as usize)));
                ip += 1;
            }
            5 | 6 => {
                let base = if o == 5 { g } else { l };
                st.push(Val::A(base + self.b(ip)? as usize));
                ip += 2;
            }
            7 => {
                st.push(Val::N(self.u32_at(s.consts + 4 * self.b(ip)? as usize)? as i32));
                ip += 1;
            }
            8 | 9 => {
                let base = if o == 8 { g } else { l };
                let v = pop(st).n();
                self.wr(base + self.b(ip)? as usize, v);
                ip += 1;
            }
            10 | 11 => {
                // memcpy n bytes from a popped address
                let base = if o == 10 { g } else { l };
                let src = pop(st).n() as usize;
                let (dst, n) = (base + self.b(ip)? as usize, self.b(ip + 1)? as usize);
                for k in 0..n {
                    let v = self.mem.get(src + k).copied().unwrap_or(0);
                    if let Some(d) = self.mem.get_mut(dst + k) {
                        *d = v;
                    }
                }
                ip += 2;
            }
            12 => st.push(Val::N(0)),
            15 => st.clear(),
            16..=25 | 28 | 29 | 32..=37 => {
                let (y, x) = (pop(st).n(), pop(st).n());
                let t = |c: bool| if c { -1 } else { 0 };
                st.push(Val::N(match o {
                    16 => x.wrapping_add(y),
                    17 => x.wrapping_sub(y),
                    18 => x.wrapping_mul(y),
                    19 => x.checked_div(y).unwrap_or(0),
                    20 => x.checked_rem(y).unwrap_or(0),
                    21 | 23 => x & y,
                    22 | 24 => x | y,
                    25 => x ^ y,
                    28 => x.wrapping_shl(y as u32),
                    29 => x.wrapping_shr(y as u32),
                    32 => t(x == y),
                    33 => t(x != y),
                    34 => t(x >= y),
                    35 => t(x <= y),
                    36 => t(x > y),
                    _ => t(x < y),
                }));
            }
            26 => {
                let v = pop(st).n();
                st.push(Val::N(v.wrapping_neg()));
            }
            27 => {
                let v = pop(st).n();
                st.push(Val::N(!v));
            }
            30 => {
                let v = pop(st).n();
                st.push(Val::N(if v == 0 { -1 } else { 0 }));
            }
            40..=43 => {
                let a = if o < 42 { g } else { l } + self.b(ip)? as usize;
                if let Some(m) = self.mem.get_mut(a) {
                    *m = if o % 2 == 0 { m.wrapping_add(1) } else { m.wrapping_sub(1) };
                }
                ip += 1;
            }
            48 => ip = s.code + self.u32_at(ip)? as usize,
            49 | 50 => {
                let v = pop(st).n();
                let t = s.code + self.u32_at(ip)? as usize;
                ip += 4;
                if (v == 0) == (o == 49) {
                    ip = t;
                }
            }
            53 => {
                // switch: [count][?][entry size][total size], then count entries of one op each
                let v = pop(st).n();
                let (cnt, es, tl) = (self.b(ip)? as i32, self.b(ip + 2)? as usize, self.b(ip + 3)? as usize);
                let after = ip + tl - 1;
                if v < 0 || v >= cnt {
                    ip = after;
                } else {
                    let e = ip + 4 + es * v as usize;
                    if self.b(e)? == 0 {
                        ip = after;
                    } else {
                        self.ip = e;
                        let r = self.op(st)?;
                        if self.ip == e + es {
                            self.ip = after;
                        }
                        return Ok(r);
                    }
                }
            }
            57 | 58 => {
                self.ip = ip;
                return Ok(Step::Yield);
            }
            59 => {
                let n = self.b(ip)? as usize;
                self.goto(n);
                if self.state != n {
                    bail!("goto to missing state {n}");
                }
                return Ok(Step::Yield);
            }
            61 => {
                self.ip = ip + 2;
                return Ok(Step::Play(u16::from_le_bytes([self.b(ip)?, self.b(ip + 1)?]) as usize));
            }
            62 => {
                self.ip = ip + 1;
                return Ok(Step::Play(self.b(ip)? as usize));
            }
            63 => {
                self.goto(s.start);
                return Ok(Step::Yield);
            }
            64 => {
                // Natives (table 0x55ed28) see the stack top; their arguments stay pushed.
                let name = s.imports.get(self.b(ip)? as usize).map(String::as_str).unwrap_or("");
                ip += 1;
                let top = |k: usize| st.len().checked_sub(k + 1).map(|i| st[i]).unwrap_or(Val::N(0));
                let v = match name {
                    "GGRnd" => {
                        let (lo, hi) = (top(1).n(), top(0).n());
                        let r = (self.rand() & 0xffff) as i32;
                        let span = hi.wrapping_sub(lo).wrapping_add(1);
                        lo + if span != 0 { r % span } else { 0 }
                    }
                    "GSV" => {
                        self.volume = top(0).n().clamp(0, 255);
                        self.target = self.volume;
                        0
                    }
                    "GSDV" => {
                        self.target = top(0).n().clamp(0, 255);
                        0
                    }
                    "FSet" | "FClear" | "FIsSet" | "FIsClear" => {
                        let a = match top(0) {
                            Val::A(a) => a,
                            Val::N(_) => usize::MAX,
                        };
                        let (m, cur) = (top(1).n(), self.rd(a));
                        match name {
                            "FSet" => {
                                self.wr(a, cur | m);
                                0
                            }
                            "FClear" => {
                                self.wr(a, cur & !m);
                                0
                            }
                            "FIsSet" => if cur & m == m { -1 } else { 0 },
                            _ => if cur & m == 0 { -1 } else { 0 },
                        }
                    }
                    // GEcho prints; TStart/TStop aren't in the exe's table (they push 0).
                    _ => 0,
                };
                st.push(Val::N(v));
            }
            // nops: 0, 13, 14, 31, 38, 39, 44–47, 54, 55, 60 (51 / 52 / 56 are unused here)
            0 | 13 | 14 | 31 | 38 | 39 | 44..=47 | 54 | 55 | 60 => {}
            _ => bail!("unsupported music opcode {o} at {:#x}", ip - 1 - s.code),
        }
        self.ip = ip;
        Ok(Step::Next)
    }
}

// ---------------------------------------------------------------------------------------------
// Player
// ---------------------------------------------------------------------------------------------

/// Themes (script states) that are in-flight music. Left out: the dispatcher and silent
/// bookkeeping states (0–3, 20, 22, 23, 44, 45, 54, 56), cutscene and story stingers
/// (21, 37–42), and duplicates that only differ in what game state they wait for.
pub const THEMES: &[usize] = &[4, 5, 6, 7, 8, 9, 10, 11, 13, 14, 16, 17, 19, 24, 28, 30, 31, 33, 46, 47, 49, 50, 51];
/// States that mean "this theme is over" (back to the dispatcher or the generic filler).
const ENDED: &[usize] = &[0, 1, 2, 3, 44, 45, 56];
/// Move on after this long in one theme.
const THEME_SECONDS: f32 = 240.0;

/// What's playing, for the UI.
#[derive(Clone, Default)]
pub struct NowPlaying {
    pub theme: usize,
    pub segment: String,
}

pub struct Player {
    sbf: Arc<Sbf>,
    vm: Vm,
    seg: Option<usize>,
    block: usize,
    frames: Vec<[f32; 2]>,
    pos: f64,
    /// Volume ramp across the segment (0..255).
    vol_from: f32,
    vol_to: f32,
    theme_frames: usize,
    pub now: Arc<Mutex<NowPlaying>>,
    pub gain: Arc<Mutex<f32>>,
}

impl Player {
    pub fn new(sbf: Arc<Sbf>, script: Arc<Script>, seed: u32) -> Player {
        let mut p = Player {
            sbf,
            vm: Vm::new(script, seed),
            seg: None,
            block: 0,
            frames: vec![],
            pos: 0.0,
            vol_from: 255.0,
            vol_to: 255.0,
            theme_frames: 0,
            now: Arc::default(),
            gain: Arc::new(Mutex::new(0.6)),
        };
        p.new_theme();
        p
    }

    fn new_theme(&mut self) {
        let mut t = THEMES[(self.vm.rand() as usize >> 3) % THEMES.len()];
        if t == self.vm.state {
            t = THEMES[(THEMES.iter().position(|&x| x == t).unwrap_or(0) + 1) % THEMES.len()];
        }
        self.vm.goto(t);
        self.theme_frames = 0;
        if let Ok(mut n) = self.now.lock() {
            n.theme = t;
        }
    }

    /// The current segment ran out: ask the script for the next.
    fn next_segment(&mut self) {
        if self.theme_frames as f32 > THEME_SECONDS * RATE as f32 {
            self.new_theme();
        }
        for _ in 0..256 {
            if ENDED.contains(&self.vm.state) {
                self.new_theme();
            }
            match self.vm.run() {
                Ok(Some(n)) if n < self.sbf.segs.len() => {
                    self.start(n);
                    return;
                }
                // The game plays segment 0 for an index past the end (FUN_004b5320).
                Ok(Some(_)) => {
                    self.start(0);
                    return;
                }
                Ok(None) => {}
                Err(e) => {
                    eprintln!("music: {e}");
                    self.new_theme();
                }
            }
        }
        self.new_theme();
    }

    fn start(&mut self, n: usize) {
        self.seg = Some(n);
        self.block = 0;
        self.vol_from = self.vm.volume as f32;
        self.vol_to = self.vm.target as f32;
        // The game steps the volume towards the target across the segment's reads.
        self.vm.volume = self.vm.target;
        self.frames.clear();
        if let Ok(mut now) = self.now.lock() {
            now.segment = self.sbf.segs[n].0.clone();
        }
    }

    fn seg_blocks(&self) -> usize {
        self.seg.map_or(0, |n| self.sbf.segs[n].2 / BLOCK)
    }

    /// A decoded frame of the current block.
    fn frame(&self, i: usize) -> Option<[f32; 2]> {
        self.frames.get(i).copied()
    }

    fn load_block(&mut self) {
        loop {
            if self.seg.is_none() || self.block >= self.seg_blocks() {
                self.next_segment();
                if self.seg_blocks() == 0 {
                    self.seg = None;
                    continue;
                }
            }
            let (_, off, _) = self.sbf.segs[self.seg.unwrap()];
            let o = off + self.block * BLOCK;
            decode_block(&self.sbf.map[o..o + BLOCK], &mut self.frames);
            self.block += 1;
            if !self.frames.is_empty() {
                return;
            }
        }
    }
}

impl Stream for Player {
    fn fill(&mut self, out: &mut [f32], rate: u32) {
        let step = RATE as f64 / rate as f64;
        let gain = self.gain.lock().map_or(0.6, |g| *g);
        for f in out.chunks_exact_mut(2) {
            if self.frames.is_empty() {
                self.load_block();
            }
            let i = self.pos as usize;
            let fr = (self.pos - i as f64) as f32;
            let a = self.frame(i).unwrap_or([0.0; 2]);
            // Interpolate into the next block's first frame only when it's already decoded.
            let b = self.frame(i + 1).unwrap_or(a);
            let blocks = self.seg_blocks().max(1) as f32;
            let t = ((self.block as f32 - 1.0) + i as f32 / BLOCK_FRAMES as f32) / blocks;
            let v = gain * (self.vol_from + (self.vol_to - self.vol_from) * t.clamp(0.0, 1.0)) / 255.0;
            f[0] += (a[0] + (b[0] - a[0]) * fr) * v;
            f[1] += (a[1] + (b[1] - a[1]) * fr) * v;
            self.pos += step;
            if self.pos as usize >= self.frames.len() {
                self.pos -= self.frames.len() as f64;
                self.theme_frames += self.frames.len();
                self.frames.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::AssetSource;

    #[test]
    fn script_sequences_themes() {
        let Ok(src) = AssetSource::open(Path::new("../pff")) else { return };
        let Some(sbf) = Sbf::find(Path::new("../pff")).and_then(|p| Sbf::open(&p).ok()) else { return };
        let s = Arc::new(Script::parse(src.read("TTFMUSIC.BIN").unwrap()).unwrap());
        assert_eq!(s.n_states(), 57);
        assert_eq!(sbf.segs.len(), 451);
        let play = |state: usize, n: usize| {
            let mut vm = Vm::new(s.clone(), 0xbabeface);
            vm.goto(state);
            let mut out = vec![];
            for _ in 0..4000 {
                if let Some(k) = vm.run().unwrap() {
                    out.push(sbf.segs[k].0.clone());
                    if out.len() == n {
                        break;
                    }
                }
            }
            out
        };
        // From the start the dispatcher, with no sector or job, falls to the generic filler.
        assert_eq!(play(0, 3), ["wharmp1", "wharmp2", "wharmp3"]);
        assert_eq!(play(4, 4), ["f1i1a", "f1i1b", "f1i1c01", "f1i1c02"]);
        assert_eq!(play(24, 3), ["BnDInt", "BnD01", "BnD02"]);
        assert_eq!(play(46, 2), ["MQin", "MQLoop1"]);
        // Every theme picks segments without errors.
        for &t in THEMES {
            assert!(!play(t, 6).is_empty(), "theme {t}");
        }
        // ADPCM decode: block headers continue the previous block's last sample.
        let (_, off, _) = sbf.segs[0];
        let mut a = vec![];
        decode_block(&sbf.map[off + 10 * BLOCK..off + 11 * BLOCK], &mut a);
        assert_eq!(a.len(), BLOCK_FRAMES);
        let mut b = vec![];
        decode_block(&sbf.map[off + 11 * BLOCK..off + 12 * BLOCK], &mut b);
        assert!((a.last().unwrap()[0] - b[0][0]).abs() < 0.01);
    }
}
