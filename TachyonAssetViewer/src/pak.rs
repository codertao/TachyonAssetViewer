//! PAK ("3DPK") model parsing. Port of TachyonFileFormats/src/pak.py; spec in
//! decomp/notes/pak.md. Only what the viewer needs: LODs, parts, meshes, textures.
//!
//! Coordinates are returned in **engine axes** (file (x, y, z) -> (x, z, -y)) as f32 units
//! (16.16 fixed point / 65536).

use anyhow::{bail, ensure, Result};
use std::collections::HashMap;

pub const MAT_TEXTURED: u16 = 0x8;
pub const PART_HIDDEN: u32 = 0x80;
/// Part flag 0x40: billboard. `Part_ComputeTransform 0x4b3f90` drops every rotation (object
/// and parents) and keeps only scale, so the mesh's axes are the camera's axes and the part
/// always faces the viewer (e.g. the GOAL gateway orb, cockpit glare).
pub const PART_BILLBOARD: u32 = 0x40;

/// How a material is composited, derived from its u16 flags.
///
/// The precedence follows the special cases in `Pak_LoadTexture 0x4b02d0` (0x4100, 0x6000,
/// 0x2100, 0x4000, 0x110, 0x88, in that order). The meaning of each case was matched against the
/// shipped textures (index-0 coverage, palette brightness) and the 3dfx driver's blend table
/// (`Hw_3dfx.dll` 0x10003b10: modes opaque / src-alpha / additive), so it is **[l]**, not
/// confirmed. See decomp/notes/pak.md "Blend modes".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BlendMode {
    /// Solid, including palette index 0.
    Opaque,
    /// Solid with palette index 0 as holes (flag 0x10): windows, trees, gratings.
    Cutout,
    /// Darkening shadow card (0x88): black, translucent where the texel isn't index 0.
    Shadow,
    /// Glows, flares, thrusters, beacons, explosions: added to what's behind.
    Additive,
}

impl BlendMode {
    pub fn for_material(m: &Material) -> Self {
        Self::from_flags(m.flags)
    }

    /// Whether palette index 0 is transparent in this mode.
    pub fn color_keyed(self) -> bool {
        !matches!(self, BlendMode::Opaque)
    }

    pub fn from_flags(f: u16) -> Self {
        if f & MAT_TEXTURED == 0 {
            // Tex_FlagsToRenderState: untextured + 0x100 → state 0x110102 = blend mode 2
            // (Hw_d3d: SRCALPHA, ONE) with alpha forced to 0xff, i.e. additive. The arena
            // windows are black, so only their specular highlight shows: clear glass.
            if f & 0x100 != 0 {
                BlendMode::Additive
            } else {
                BlendMode::Opaque
            }
        } else if f & 0x4100 == 0x4100 || f & 0x6000 == 0x6000 || f & 0x2100 == 0x2100 || f & 0x4000 != 0 {
            BlendMode::Additive
        } else if f & 0x110 == 0x110 {
            BlendMode::Cutout
        } else if f & 0x88 == 0x88 {
            BlendMode::Shadow
        } else if f & 0x10 != 0 {
            BlendMode::Cutout
        } else {
            BlendMode::Opaque
        }
    }
}

/// Flag 0x8000: drawn full-bright (the loader gives these a separate palette pass; they're the
/// lit windows and glow strips).
pub const MAT_FULLBRIGHT: u16 = 0x8000;

const MESH_STRIDES: [usize; 6] = [0x1C, 0x2C, 0x20, 0x10, 0x0C, 0x10];

#[derive(Debug, Clone)]
pub struct Material {
    pub texture: String,
    pub rgb: [u8; 3],
    pub flags: u16,
    /// +0x15: specular power (1: cos, 2: cos², 3: cos⁴ of the half-vector; `FUN_004b4cf0` →
    /// `FUN_004b4b80`). Non-zero also adds engine render flag 0x10000 (an extra translucent pass).
    pub extra_pass: u8,
    /// +0x14: specular intensity shift (5–9): highlight = 128·cosᵖ << shift >> 7.
    pub spec_shift: u8,
    /// +0x0f: self-illumination. Non-zero means unlit at brightness min(2·alpha, 255)
    /// (`FUN_004b4c10`); 0 means lit.
    pub alpha: u8,
    /// +0x19 / +0x1a: frame count and animation counter slot.
    pub frames: u8,
    pub anim_slot: u8,
}

impl Material {
    /// Slots 0x7d-0x7f with 2 "frames" are two-layer materials: this one is the base and the
    /// next material is a decal drawn over the same faces (lit windows, tinted glass). Every
    /// shipped instance (1,045) has frames == 2 and a decal-style second material.
    pub fn is_layered(&self) -> bool {
        self.frames == 2 && (0x7d..=0x7f).contains(&self.anim_slot)
    }
}

#[derive(Debug, Clone)]
pub struct Face {
    pub material: usize,
    pub v: [usize; 3],
    /// Per-corner indices into `Mesh::normals`.
    pub n: [usize; 3],
}

#[derive(Debug, Clone)]
pub struct Mesh {
    pub materials: Vec<Material>,
    /// Engine-axis positions.
    pub positions: Vec<[f32; 3]>,
    pub uvs: Vec<[f32; 2]>,
    pub faces: Vec<Face>,
    /// Engine-axis unit normals (outward; stored at length 0.5 on disk).
    pub normals: Vec<[f32; 3]>,
    /// Engine-axis attach points.
    pub attach: Vec<[f32; 3]>,
    /// Engine-axis child pivots ("points54"): child `k` of a part using this mesh sits at
    /// part position + part matrix · pivots[k] (`FUN_004b3eb0`).
    pub pivots: Vec<[f32; 3]>,
}

#[derive(Debug, Clone)]
pub struct Part {
    pub mesh_ofs: usize,
    pub flags: u32,
    /// Rotation parent (+0x20). A parent of part 0 is stored as null by `Pak_Relocate`, so it
    /// is `None` here too: the root's rotation never reaches its children.
    pub parent: Option<usize>,
    /// Children (+0x24 first, +0x1b count) that take their position from this part's pivots.
    /// `Pak_Relocate` makes every other part a child of part 0.
    pub first_child: usize,
    pub n_children: usize,
}

/// `Curve_Eval 0x4b25a0` (0x14 bytes).
#[derive(Debug, Clone, Copy, Default)]
pub struct Curve {
    pub mode: u32,
    pub a: f32,
    pub b: f32,
    pub c: f32,
    /// Raw 16.16 rate for the sine mode.
    pub d_raw: i32,
}

/// Part animator (0x2c), run by `Model_UpdateState 0x4b27d0`.
#[derive(Debug, Clone, Copy)]
pub struct PartAnim {
    pub part: usize,
    /// 1 random blink, 2 periodic blink, 4 copy part `other`'s visibility.
    pub mode: u32,
    /// Engine angle units (512 per turn) per second.
    pub rate: [f32; 3],
    pub threshold: u32,
    pub other: u32,
    /// Raw 16.16 blink frequency (cycles per second).
    pub period_raw: i32,
    /// Engine angle units.
    pub base: [f32; 3],
}

#[derive(Debug, Clone)]
pub struct State {
    pub name: String,
    pub part_anims: Vec<PartAnim>,
    /// (slot, frames per second)
    pub counters: Vec<(u8, f32)>,
    /// +0x20: 1 glow curve, 2 uniform scale, 4/8/0x10 scale x/y/z, 0x20/0x40/0x80 paired axes.
    pub effect_flags: u32,
    /// glow, scale x, scale y, scale z
    pub curves: [Curve; 4],
}

#[derive(Debug, Clone)]
pub struct Lod {
    pub distance: f32,
    pub parts: Vec<Part>,
}

#[derive(Debug, Clone)]
pub struct Texture {
    pub name: String,
    pub width: usize,
    pub height: usize,
    /// Palette indices, width*height.
    pub pixels: Vec<u8>,
    /// RGB triples (768 bytes) or empty.
    pub palette: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Pak {
    pub name: String,
    pub flags: u32,
    pub lods: Vec<Lod>,
    pub meshes: HashMap<usize, Mesh>,
    pub textures: Vec<Texture>,
    /// Named animation states (`ON`, `OFF`). Each state's texture counters give
    /// (slot, frames per second): `State_UpdateCounters 0x4b2750` sets counter[slot] = rate × time,
    /// with time in seconds (ticks × 0x10000/50), and `Mat_GetAnimFrame 0x4b4e90` shows
    /// material + counter[slot] % frames.
    pub states: Vec<State>,
}

impl Pak {
    /// Animation rate for a material's counter slot. Slot 0 is the per-entity counter
    /// (entity +0x30), which most entities set to game ticks / 4, i.e. 12.5 fps.
    pub fn slot_fps(&self, state: usize, slot: u8) -> f32 {
        if slot == 0 {
            return 12.5;
        }
        let Some(s) = self.states.get(state) else { return 0.0 };
        s.counters.iter().find(|c| c.0 == slot).map(|c| c.1).unwrap_or(0.0)
    }
}

impl Pak {
    /// LOD 0's root mesh (what `Model_GetLod0Mesh` returns), used for OCF attach points.
    pub fn lod0_root_mesh(&self) -> Option<&Mesh> {
        let p = self.lods.first()?.parts.first()?;
        self.meshes.get(&p.mesh_ofs)
    }

    /// `Pak_GetAttachPoint`: out-of-range indices fall back to point 0, or the origin.
    pub fn attach_point(&self, idx: usize) -> [f32; 3] {
        match self.lod0_root_mesh() {
            Some(m) if !m.attach.is_empty() => *m.attach.get(idx).unwrap_or(&m.attach[0]),
            _ => [0.0; 3],
        }
    }
}

struct R<'a>(&'a [u8]);

impl R<'_> {
    fn u8(&self, o: usize) -> Result<u8> {
        self.0.get(o).copied().ok_or_else(|| anyhow::anyhow!("read past end at {o:#x}"))
    }
    fn bytes(&self, o: usize, n: usize) -> Result<&[u8]> {
        self.0.get(o..o + n).ok_or_else(|| anyhow::anyhow!("read past end at {o:#x}+{n}"))
    }
    fn u16(&self, o: usize) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes(o, 2)?.try_into().unwrap()))
    }
    fn u32(&self, o: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes(o, 4)?.try_into().unwrap()))
    }
    fn us(&self, o: usize) -> Result<usize> {
        Ok(self.u32(o)? as usize)
    }
    fn fx(&self, o: usize) -> Result<f32> {
        Ok(i32::from_le_bytes(self.bytes(o, 4)?.try_into().unwrap()) as f32 / 65536.0)
    }
    fn cname(&self, o: usize, n: usize) -> Result<String> {
        let b = self.bytes(o, n)?;
        let s: String = b.iter().take_while(|&&c| c != 0).map(|&c| c as char).collect();
        Ok(s.trim_end_matches(' ').to_string())
    }
    /// File-axis fixed-point xyz -> engine-axis f32.
    fn vec_engine(&self, o: usize) -> Result<[f32; 3]> {
        let (x, y, z) = (self.fx(o)?, self.fx(o + 4)?, self.fx(o + 8)?);
        Ok([x, z, -y])
    }
}

/// PCX-style RLE. Returns (pixels, bytes consumed).
pub fn rle_decode(src: &[u8], n: usize) -> Result<(Vec<u8>, usize)> {
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while out.len() < n {
        let b = *src.get(i).ok_or_else(|| anyhow::anyhow!("RLE data ran out"))?;
        if b < 0xC0 {
            out.push(b);
            i += 1;
        } else {
            let v = *src.get(i + 1).ok_or_else(|| anyhow::anyhow!("RLE data ran out"))?;
            let run = ((b - 0xC0) as usize).max(1);
            out.extend(std::iter::repeat_n(v, run));
            i += 2;
        }
    }
    out.truncate(n);
    Ok((out, i))
}

fn parse_mesh(r: &R, m: usize) -> Result<Mesh> {
    ensure!(r.bytes(m, 4)? == b"3DO1", "mesh @{m:#x}: bad magic");
    let mut cnt = [0usize; 6];
    let mut ofs = [0usize; 6];
    for k in 0..6 {
        cnt[k] = r.us(m + 0x2C + 4 * k)?;
        ofs[k] = r.us(m + 0x44 + 4 * k)?;
    }
    for k in 0..5 {
        ensure!(
            ofs[k] + cnt[k] * MESH_STRIDES[k] == ofs[k + 1],
            "mesh @{m:#x}: sections don't tile"
        );
    }
    let mut materials = Vec::with_capacity(cnt[0]);
    for k in 0..cnt[0] {
        let o = m + ofs[0] + k * 0x1C;
        materials.push(Material {
            texture: r.cname(o, 12)?,
            rgb: [r.u8(o + 0x10)?, r.u8(o + 0x11)?, r.u8(o + 0x12)?],
            flags: r.u16(o + 0x16)?,
            extra_pass: r.u8(o + 0x15)?,
            spec_shift: r.u8(o + 0x14)?,
            alpha: r.u8(o + 0x0F)?,
            frames: r.u8(o + 0x19)?,
            anim_slot: r.u8(o + 0x1A)?,
        });
    }
    let mut positions = Vec::with_capacity(cnt[1]);
    let mut uvs = Vec::with_capacity(cnt[1]);
    for k in 0..cnt[1] {
        let o = m + ofs[1] + k * 0x2C;
        positions.push(r.vec_engine(o)?);
        uvs.push([r.fx(o + 0x20)?, r.fx(o + 0x24)?]);
    }
    let mut faces = Vec::with_capacity(cnt[2]);
    for k in 0..cnt[2] {
        let o = m + ofs[2] + k * 0x20;
        let mat = r.us(o + 4)?;
        let mut v = [0usize; 3];
        for (j, vv) in v.iter_mut().enumerate() {
            let x = r.us(o + 8 + 4 * j)?;
            ensure!(x % 0x2C == 0 && x / 0x2C < cnt[1], "mesh @{m:#x} face {k}: bad vertex");
            *vv = x / 0x2C;
        }
        ensure!(mat % 0x1C == 0 && mat / 0x1C < cnt[0].max(1), "mesh @{m:#x} face {k}: bad material");
        let mut n = [0usize; 3];
        for (j, nn) in n.iter_mut().enumerate() {
            let x = r.us(o + 0x14 + 4 * j)?;
            *nn = if x % 0x10 == 0 && x / 0x10 < cnt[3] { x / 0x10 } else { usize::MAX };
        }
        faces.push(Face { material: mat / 0x1C, v, n });
    }
    let mut normals = Vec::with_capacity(cnt[3]);
    for k in 0..cnt[3] {
        let v = r.vec_engine(m + ofs[3] + k * 0x10)?;
        let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
        normals.push(if l > 0.0 { [v[0] / l, v[1] / l, v[2] / l] } else { [0.0, 1.0, 0.0] });
    }
    let mut pivots = Vec::with_capacity(cnt[4]);
    for k in 0..cnt[4] {
        pivots.push(r.vec_engine(m + ofs[4] + k * 0xC)?);
    }
    let mut attach = Vec::with_capacity(cnt[5]);
    for k in 0..cnt[5] {
        attach.push(r.vec_engine(m + ofs[5] + k * 0x10)?);
    }
    Ok(Mesh { materials, positions, uvs, faces, normals, attach, pivots })
}

pub fn parse(d: &[u8]) -> Result<Pak> {
    let r = R(d);
    ensure!(d.len() >= 0x60 && &d[..4] == b"3DPK", "not a 3DPK file");
    let name = r.cname(8, 32)?;
    let flags = r.u32(0x28)?;
    let n_lods = r.us(0x2C)?;
    ensure!(r.u32(0x38)? == 0x60, "unexpected header size");
    let main_end = r.us(0x3C)?;
    let states_ofs = r.us(0x4C)?;
    ensure!(n_lods < 64, "implausible LOD count {n_lods}");

    let mut lods = Vec::with_capacity(n_lods);
    let mut meshes = HashMap::new();
    for l in 0..n_lods {
        let distance = r.fx(0x60 + l * 8)?;
        let go = r.us(0x60 + l * 8 + 4)?;
        let nparts = r.us(go + 4)?;
        let pofs = r.us(go + 8)?;
        ensure!(nparts < 4096, "implausible part count");
        let mut parts = Vec::with_capacity(nparts);
        for p in 0..nparts {
            let e = go + pofs + p * 0x2C;
            let mesh_ofs = r.us(e)?;
            // Hierarchy offsets are byte offsets into this part array.
            let parent = r.us(e + 0x20)? / 0x2C;
            let first = r.us(e + 0x24)? / 0x2C;
            let mut part = Part {
                mesh_ofs,
                flags: r.u32(e + 0x10)?,
                parent: (parent != 0 && parent < nparts).then_some(parent),
                first_child: first,
                n_children: if first != 0 { r.u8(e + 0x1B)? as usize } else { 0 },
            };
            // Pak_Relocate: the root gets every other part as a child.
            if p == 0 && nparts > 1 {
                part.first_child = 1;
                part.n_children = nparts - 1;
            }
            part.n_children = part.n_children.min(nparts.saturating_sub(part.first_child));
            parts.push(part);
            if let std::collections::hash_map::Entry::Vacant(v) = meshes.entry(mesh_ofs) {
                v.insert(parse_mesh(&r, mesh_ofs)?);
            }
        }
        lods.push(Lod { distance, parts });
    }

    let mut textures = Vec::new();
    let mut o = main_end;
    if o < d.len() {
        let n = r.us(o)?;
        o += 4;
        for _ in 0..n {
            let tname = r.cname(o, 12)?;
            let npal = r.u16(o + 12)? as usize;
            let w = r.us(o + 16)?;
            let h = r.us(o + 20)?;
            let size = r.us(o + 24)?;
            ensure!(w * h <= 4096 * 4096, "texture {tname}: implausible size {w}x{h}");
            let data = r.bytes(o + 0x1C, size)?;
            let (pixels, used) = rle_decode(data, w * h)?;
            // 84 shipped files keep the PCX palette marker 0x0C after the RLE body.
            if used != size && !(used + 1 == size && data[used] == 0x0C) {
                bail!("texture {tname}: RLE used {used} of {size} bytes");
            }
            let palette = r.bytes(o + 0x1C + size, 3 * npal)?.to_vec();
            textures.push(Texture { name: tname, width: w, height: h, pixels, palette });
            o += 0x1C + size + 3 * npal;
        }
    }
    let mut states = Vec::new();
    if flags & 4 != 0 {
        let n_states = r.us(states_ofs + 0x10)?;
        let eo = r.us(states_ofs + 0x14)?;
        ensure!(n_states < 64, "implausible state count");
        for s in 0..n_states {
            let e = eo + s * 0x74;
            let (npa, pa, nc, co) = (r.us(e)?, r.us(e + 4)?, r.us(e + 8)?, r.us(e + 0xC)?);
            ensure!(npa < 4096 && nc < 256, "implausible state sizes");
            let mut part_anims = Vec::with_capacity(npa);
            for k in 0..npa {
                let a = pa + k * 0x2C;
                part_anims.push(PartAnim {
                    part: r.us(a)?,
                    mode: r.u32(a + 4)?,
                    rate: [r.fx(a + 8)?, r.fx(a + 0xC)?, r.fx(a + 0x10)?],
                    threshold: r.u32(a + 0x14)?,
                    other: r.u32(a + 0x18)?,
                    period_raw: r.u32(a + 0x1C)? as i32,
                    base: [r.fx(a + 0x20)?, r.fx(a + 0x24)?, r.fx(a + 0x28)?],
                });
            }
            let mut counters = Vec::with_capacity(nc);
            for k in 0..nc {
                let slot = r.u32(co + k * 8)?;
                counters.push((slot.min(255) as u8, r.fx(co + k * 8 + 4)?));
            }
            let curve = |o: usize| -> Result<Curve> {
                Ok(Curve {
                    mode: r.u32(o)?,
                    a: r.fx(o + 4)?,
                    b: r.fx(o + 8)?,
                    c: r.fx(o + 0xC)?,
                    d_raw: r.u32(o + 0x10)? as i32,
                })
            };
            states.push(State {
                name: r.cname(e + 0x10, 16)?,
                part_anims,
                counters,
                effect_flags: r.u32(e + 0x20)?,
                curves: [curve(e + 0x24)?, curve(e + 0x38)?, curve(e + 0x4C)?, curve(e + 0x60)?],
            });
        }
    }
    Ok(Pak { name, flags, lods, meshes, textures, states })
}

#[cfg(test)]
mod tests {
    use super::*;
    const PFF_DIR: &str = "../../pff";

    #[test]
    fn blend_mode_classes() {
        use BlendMode::*;
        for (f, m) in [
            (0x0000, Opaque), (0x0100, Additive), (0x0008, Opaque), (0x0048, Opaque), (0x8008, Opaque), (0x0808, Opaque),
            (0x0018, Cutout), (0x0058, Cutout), (0x0118, Cutout),
            (0x0088, Shadow),
            (0x2108, Additive), (0x2148, Additive), (0xa148, Additive), (0xa948, Additive),
            (0x4008, Additive), (0x4048, Additive), (0xc008, Additive), (0x6018, Additive),
        ] {
            assert_eq!(BlendMode::from_flags(f), m, "flags {f:#06x}");
        }
    }

    #[test]
    fn parses_every_shipped_pak() {
        let Ok(rd) = std::fs::read_dir(PFF_DIR) else { return };
        let mut ok = 0;
        let mut bad = vec![];
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_uppercase();
            if !n.ends_with(".PAK") {
                continue;
            }
            let d = std::fs::read(e.path()).unwrap();
            match parse(&d) {
                Ok(p) => {
                    for t in &p.textures {
                        assert_eq!(t.pixels.len(), t.width * t.height);
                    }
                    ok += 1
                }
                Err(err) => bad.push(format!("{n}: {err}")),
            }
        }
        assert!(bad.is_empty(), "{} failures: {:?}", bad.len(), &bad[..bad.len().min(10)]);
        assert_eq!(ok, 1160);
    }
}
