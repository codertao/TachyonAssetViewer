//! Turn parsed PAK / OCF data into GPU-ready batches (one per texture / flat colour / animation).

use crate::anim::Rig;
use crate::gate::{self, Anm, GateMode, Role};
use crate::math::{self, Mat3};
use crate::ocf::{self, Ocf};
use crate::pak::{self, Pak};
use crate::source::AssetSource;
use crate::palette::{self, Remap};
use crate::{bdf, spx};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Vertex {
    /// World position, or for billboard vertices the camera-space offset from `center`.
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    /// Billboard pivot in world space (unused otherwise).
    pub center: [f32; 3],
    /// 1.0 for billboard vertices.
    pub billboard: f32,
}

pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

pub struct Batch {
    /// Indices into `Scene::images`: empty for flat colour, one for a static texture, or the
    /// frames of an animated material.
    pub images: Vec<usize>,
    /// Animation rate in frames per second (0 = static).
    pub fps: f32,
    pub mode: pak::BlendMode,
    /// 0 = base, 1 = decal layer of a two-layer material (drawn after the base).
    pub layer: u8,
    /// Skip lighting (full-bright materials and all additive ones).
    pub unlit: bool,
    /// Game shading (`FUN_004b4cf0`): material alpha after load fix-ups (non-zero = unlit at
    /// min(2·alpha, 255)/255), specular power and shift.
    pub emissive: u8,
    pub spec_pow: u8,
    pub spec_shift: u8,
    /// 0 = the model or sector, 1 = backdrop sky, 2 = backdrop image (planet, sun, rock).
    /// Backdrop batches are camera-centred: drawn with the view's rotation only, behind everything.
    pub backdrop: u8,
    /// Animated part this batch belongs to (index into `Animator::frame`'s output). Its
    /// vertices are then in the part's own space; `None` = baked world space.
    pub part: Option<usize>,
    pub verts: Vec<Vertex>,
}

/// A sector's light, from its BDF (`Light_BuildShadeRamp 0x4b37a0`, `FUN_004b4d70`):
/// per channel, `shade = clamp(ambient + max(N·L, 0) · diffuse, 4/255, 252/255)`, times the texel.
#[derive(Clone, Copy, Debug)]
pub struct SectorLight {
    /// GL-space unit vector towards the light.
    pub dir: [f32; 3],
    /// `(AMBIENT_LIGHT / 2) / 256 · RGB / 255`
    pub ambient: [f32; 3],
    /// `LIGHT_RGB / 255`. Lightning sectors scale it per frame (`weather::Lightning`).
    pub diffuse: [f32; 3],
    /// `LIGHTNING_AMBIENT_LIGHT` in lightning sectors.
    pub lightning: Option<i32>,
}

/// A fog sector's settings (BDF `[fog]`), in viewer units.
#[derive(Clone, Copy, Debug)]
pub struct SectorFog {
    /// View-depth range of the linear fade, world units.
    pub min: f32,
    pub max: f32,
    /// Fog / clear colour, 0..1.
    pub color: [f32; 3],
    /// Fog-sheet tint, 0..1.
    pub front: [f32; 3],
    /// Fog-sheet texture (index into `Scene::images`).
    pub image: Option<usize>,
}

/// One placed object in a sector scene.
#[derive(Clone, Debug)]
pub struct SceneObject {
    pub label: String,
    /// GL-space bounding box centre and radius.
    pub center: [f32; 3],
    pub radius: f32,
}

/// Per-frame transform of an animated part.
#[derive(Clone, Copy, Debug)]
pub struct PartXform {
    /// GL-space model matrix (column-major).
    pub model: [f32; 16],
    /// Extra scale for billboard offsets (scale curves).
    pub bbscale: [f32; 3],
    /// Instance intensity (0..1) for self-lit materials (instance flag 0x80; gate centre and
    /// flare).
    pub intensity: Option<f32>,
}

impl PartXform {
    pub const IDENTITY: PartXform = PartXform {
        model: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0],
        bbscale: [1.0; 3],
        intensity: None,
    };
}

/// A gate model's `.anm` drive (see `gate`).
#[derive(Clone)]
struct Drive {
    anm: Arc<Anm>,
    role: Role,
    /// Open / close playback lengths (ticks) of the whole gate, so its models stay in step.
    cycle: (f32, f32),
}

impl Drive {
    fn pose(&self, mode: GateMode, t: f32, salt: u32) -> gate::Pose {
        let mut p = gate::gate_pose(&self.anm, self.role, mode, t, self.cycle);
        if self.role == Role::Flare {
            gate::flare_flicker(&mut p, (t * gate::TICK_HZ) as u32, salt);
        }
        p
    }
}

struct DynInstance {
    rig: Rig,
    basis: Mat3,
    offset: [f32; 3],
    billboard: Vec<bool>,
    drive: Option<Drive>,
}

/// Evaluates the state animation of every animated PAK instance in a scene.
pub struct Animator {
    instances: Vec<DynInstance>,
}

impl Animator {
    /// One entry per animated part; `None` = hidden this frame.
    pub fn frame(&self, t: f32, gates: GateMode) -> Vec<Option<PartXform>> {
        let mut out = vec![];
        for (n, inst) in self.instances.iter().enumerate() {
            let (poses, intensity) = match &inst.drive {
                Some(d) => {
                    let g = d.pose(gates, t, n as u32);
                    // The body's instance has no scale flag (0x10); the centre and flare do.
                    let scale = if d.role == Role::Body { 1.0 } else { g.scale };
                    let rot = (d.role == Role::Body).then_some(&g.rot[..]);
                    (inst.rig.pose_with(t, rot, scale), g.intensity.filter(|_| d.role != Role::Body).map(|i| i.clamp(0.0, 255.0) / 255.0))
                }
                None => (inst.rig.pose(t), None),
            };
            for (i, pp) in poses.into_iter().enumerate() {
                if pp.hidden {
                    out.push(None);
                    continue;
                }
                let tr = to_gl(math::add(inst.basis.mul_vec(pp.pos), inst.offset));
                let (lin, bbscale) = if inst.billboard[i] {
                    (Mat3::IDENTITY, [pp.m.0[0][0], pp.m.0[1][1], pp.m.0[2][2]])
                } else {
                    (inst.basis.mul(&pp.m), [1.0; 3])
                };
                // GL = F·engine·F with F = diag(1, 1, -1).
                let f = [1.0, 1.0, -1.0];
                let mut m = [0.0f32; 16];
                for r in 0..3 {
                    for c in 0..3 {
                        m[c * 4 + r] = lin.0[r][c] * f[r] * f[c];
                    }
                }
                m[12] = tr[0];
                m[13] = tr[1];
                m[14] = tr[2];
                m[15] = 1.0;
                out.push(Some(PartXform { model: m, bbscale, intensity }));
            }
        }
        out
    }
}

pub struct Scene {
    pub images: Vec<Image>,
    pub batches: Vec<Batch>,
    pub bbox_min: [f32; 3],
    pub bbox_max: [f32; 3],
    pub info: Vec<String>,
    pub warnings: Vec<String>,
    pub lod_count: usize,
    /// State names of the PAK (PAK view only).
    pub state_names: Vec<String>,
    /// Placed objects (SPX view), for the camera's object list.
    pub objects: Vec<SceneObject>,
    /// For a sub-sector SPX, the main sector it belongs to.
    pub parent: Option<String>,
    /// The sector's BDF lighting (SPX view).
    pub light: Option<SectorLight>,
    pub fog: Option<SectorFog>,
    pub animator: Option<Animator>,
    /// Hyper gates driven by their `.anm` files.
    pub gates: usize,
}

impl Scene {
    pub fn triangle_count(&self) -> usize {
        self.batches.iter().map(|b| b.verts.len() / 3).sum()
    }
    pub fn has_backdrop(&self) -> bool {
        self.batches.iter().any(|b| b.backdrop != 0)
    }
    pub fn animated(&self) -> bool {
        self.animator.is_some() || self.batches.iter().any(|b| b.images.len() > 1 && b.fps > 0.0)
    }
}

/// Engine axes are left-handed (x right, y up, z forward); GL is right-handed. Flipping z
/// keeps models from appearing mirrored.
fn to_gl(v: [f32; 3]) -> [f32; 3] {
    [v[0], v[1], -v[2]]
}

fn texture_image(t: &pak::Texture, remap: Option<(&Remap, &[bool; 256])>) -> Image {
    // Remap the palette once (protected entries untouched), then expand.
    let mut pal = [[0u8; 3]; 256];
    for (i, p) in pal.iter_mut().enumerate() {
        *p = if t.palette.len() >= 768 { [t.palette[i * 3], t.palette[i * 3 + 1], t.palette[i * 3 + 2]] } else { [i as u8; 3] };
        if let Some((r, protect)) = remap {
            if !protect[i] {
                *p = r.apply(*p);
            }
        }
    }
    let mut rgba = Vec::with_capacity(t.width * t.height * 4);
    for &i in &t.pixels {
        let i = i as usize;
        rgba.extend_from_slice(&pal[i]);
        // Palette index 0 is the colour key (the 3dfx driver uploads it as alpha 0).
        rgba.push(if i == 0 { 0 } else { 255 });
    }
    Image { width: t.width, height: t.height, rgba }
}

/// A standalone 8-bit PCX (RLE, palette in the last 768 bytes), fully opaque.
fn pcx_image(d: &[u8]) -> Option<Image> {
    if d.len() < 128 + 769 || d[0] != 0x0A || d[3] != 8 {
        return None;
    }
    let u16_ = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as usize;
    let (w, h) = (u16_(8).checked_sub(u16_(4))? + 1, u16_(10).checked_sub(u16_(6))? + 1);
    let bpl = u16_(0x42);
    let (pixels, _) = pak::rle_decode(&d[128..d.len() - 769], bpl * h).ok()?;
    let pal = &d[d.len() - 768..];
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let i = *pixels.get(y * bpl + x)? as usize;
            rgba.extend_from_slice(&[pal[i * 3], pal[i * 3 + 1], pal[i * 3 + 2], 255]);
        }
    }
    Some(Image { width: w, height: h, rgba })
}

/// (emissive alpha, specular power, specular shift) after the loader's fix-ups: alpha 0xff
/// becomes 0xfe (`Mesh_Relocate`), and the decal of a two-layer material is forced to 0x7f
/// (`MeshClone_BuildTriList 0x4b0cb0`).
fn shading(m: &pak::Material, layer: u8) -> (u8, u8, u8) {
    let alpha = if layer == 1 {
        0x7f
    } else if m.alpha == 0xff {
        0xfe
    } else {
        m.alpha
    };
    (alpha, m.extra_pass.min(3), m.spec_shift)
}

fn tex_key(name: &str) -> String {
    let n = name.trim().to_uppercase();
    match n.rsplit_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => n,
    }
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct BatchKey {
    images: Vec<usize>,
    fps_bits: u32,
    mode: pak::BlendMode,
    layer: u8,
    unlit: bool,
    part: Option<usize>,
    backdrop: u8,
    emissive: u8,
    spec_pow: u8,
    spec_shift: u8,
}

struct Builder {
    images: Vec<Image>,
    /// (pak file, texture key) -> image index
    image_ids: HashMap<(String, String), usize>,
    /// batch key -> index into batches
    batch_ids: HashMap<BatchKey, usize>,
    batches: Vec<Batch>,
    min: [f32; 3],
    max: [f32; 3],
    warnings: Vec<String>,
    billboards: usize,
    /// Animation state used for every PAK.
    state: usize,
    /// Backdrop layer for what's being added (see `Batch::backdrop`); backdrops don't grow the bbox.
    backdrop: u8,
    /// Sector palette re-tint for what's being added (None = original colours).
    remap: Option<Remap>,
    /// Protected palette indices of the PAK being added (from its `.pal`).
    protect: Option<[bool; 256]>,
    dyn_instances: Vec<DynInstance>,
    dyn_parts: usize,
    spinning: usize,
    blinking: usize,
    /// Gate drive for the PAK being added.
    drive: Option<Drive>,
    /// Override for material counter slot 0 (the per-entity counter).
    slot0_fps: Option<f32>,
    gates: usize,
}

impl Builder {
    fn new() -> Self {
        Self {
            images: vec![],
            image_ids: HashMap::new(),
            batch_ids: HashMap::new(),
            batches: vec![],
            min: [f32::MAX; 3],
            max: [f32::MIN; 3],
            warnings: vec![],
            billboards: 0,
            state: 0,
            backdrop: 0,
            remap: None,
            protect: None,
            dyn_instances: vec![],
            dyn_parts: 0,
            spinning: 0,
            blinking: 0,
            drive: None,
            slot0_fps: None,
            gates: 0,
        }
    }

    fn batch(&mut self, key: BatchKey) -> &mut Batch {
        let n = self.batches.len();
        let idx = match self.batch_ids.get(&key) {
            Some(&i) => i,
            None => {
                self.batches.push(Batch {
                    images: key.images.clone(),
                    fps: f32::from_bits(key.fps_bits),
                    mode: key.mode,
                    layer: key.layer,
                    unlit: key.unlit,
                    backdrop: key.backdrop,
                    emissive: key.emissive,
                    spec_pow: key.spec_pow,
                    spec_shift: key.spec_shift,
                    part: key.part,
                    verts: vec![],
                });
                self.batch_ids.insert(key, n);
                n
            }
        };
        &mut self.batches[idx]
    }

    fn grow(&mut self, p: [f32; 3]) {
        if self.backdrop != 0 {
            return;
        }
        for k in 0..3 {
            self.min[k] = self.min[k].min(p[k]);
            self.max[k] = self.max[k].max(p[k]);
        }
    }

    fn warn(&mut self, w: String) {
        if !self.warnings.contains(&w) {
            self.warnings.push(w);
        }
    }

    /// Add one PAK's LOD at the given placement (basis = rotation·scale, then offset).
    fn add_pak(&mut self, pak_file: &str, pak: &Pak, lod: usize, basis: &Mat3, scale: f32, offset: [f32; 3]) {
        let mut tex_ids: HashMap<String, usize> = HashMap::new();
        for t in &pak.textures {
            // Remapped and plain copies of the same texture are different images.
            let tag = if self.remap.is_some() { "|remap" } else { "" };
            let key = (format!("{}{tag}", pak_file.to_uppercase()), tex_key(&t.name));
            let id = match self.image_ids.get(&key) {
                Some(&id) => id,
                None => {
                    let protect = self.protect.unwrap_or([false; 256]);
                    self.images.push(texture_image(t, self.remap.as_ref().map(|r| (r, &protect))));
                    let id = self.images.len() - 1;
                    self.image_ids.insert(key.clone(), id);
                    id
                }
            };
            tex_ids.insert(key.1, id);
        }
        let Some(l) = pak.lods.get(lod) else { return };
        let rig = Rig::new(pak, lod, self.state);
        let drive = self.drive.clone();
        // Bound a gate in its open pose (pylons fanned out, portal at full size).
        let poses = match &drive {
            Some(d) => {
                let g = d.pose(GateMode::Open, 0.0, 0);
                let body = d.role == Role::Body;
                rig.pose_with(0.0, body.then_some(&g.rot[..]), if body { 1.0 } else { g.scale.max(0.0) })
            }
            None => rig.pose(0.0),
        };
        let dynamic = rig.is_animated() || drive.is_some();
        let first_dyn = self.dyn_parts;
        if dynamic {
            self.spinning += rig.spinning_parts();
            self.blinking += rig.blinking_parts();
            self.dyn_parts += l.parts.len();
            self.dyn_instances.push(DynInstance {
                rig,
                basis: *basis,
                offset,
                billboard: l.parts.iter().map(|p| p.flags & pak::PART_BILLBOARD != 0).collect(),
                drive,
            });
        }
        for (pi, part) in l.parts.iter().enumerate() {
            let pose = poses[pi];
            if !dynamic && pose.hidden {
                continue;
            }
            let Some(mesh) = pak.meshes.get(&part.mesh_ofs) else { continue };
            let billboard = part.flags & pak::PART_BILLBOARD != 0;
            let center = to_gl(math::add(basis.mul_vec(pose.pos), offset));
            // World-space points at t = 0. Billboards: scale only, in camera axes around the
            // part's position.
            let world: Vec<[f32; 3]> = if billboard {
                self.billboards += 1;
                mesh.positions.iter().map(|&p| to_gl(math::scale(pose.m.mul_vec(p), scale))).collect()
            } else {
                mesh.positions
                    .iter()
                    .map(|&p| to_gl(math::add(basis.mul_vec(math::add(pose.pos, pose.m.mul_vec(p))), offset)))
                    .collect()
            };
            for &p in &world {
                // For billboards, bound the pivot plus the offset's length in every direction.
                if billboard {
                    let r = math::dot(p, p).sqrt();
                    self.grow(math::add(center, [r, r, r]));
                    self.grow(math::sub(center, [r, r, r]));
                } else {
                    self.grow(p);
                }
            }
            // Animated parts keep part-space vertices; the renderer applies the frame's matrix.
            let (pts, vcenter) = if dynamic {
                let local = mesh
                    .positions
                    .iter()
                    .map(|&p| to_gl(if billboard { math::scale(p, scale) } else { p }))
                    .collect();
                (local, [0.0; 3])
            } else {
                (world, center)
            };
            let part_id = dynamic.then_some(first_dyn + pi);
            let bb = if billboard { 1.0 } else { 0.0 };
            for f in &mesh.faces {
                let p = f.v.map(|i| pts[i]);
                // Per-corner file normals (outward, smooth). Baked parts get them in world space;
                // animated parts keep part space and the shader rotates them. Fallback: the
                // face normal, outward = cross(p1 − p0, p2 − p0) in GL space (the z mirror flips
                // the file winding).
                let face_n = math::normalize(math::cross(math::sub(p[1], p[0]), math::sub(p[2], p[0])));
                let normals = f.n.map(|ni| match mesh.normals.get(ni) {
                    Some(&nf) if dynamic || billboard => to_gl(nf),
                    Some(&nf) => to_gl(math::normalize(basis.mul_vec(pose.m.mul_vec(nf)))),
                    None => face_n,
                });
                let mut layers = vec![(f.material, 0u8)];
                if mesh.materials.get(f.material).is_some_and(|m| m.is_layered()) && f.material + 1 < mesh.materials.len() {
                    layers.push((f.material + 1, 1));
                }
                for (mi, layer) in layers {
                    let (mut key, color) = self.material_key(pak_file, pak, &mesh.materials, mi, layer, &tex_ids);
                    key.part = part_id;
                    key.backdrop = self.backdrop;
                    let batch = self.batch(key);
                    for (j, &vi) in f.v.iter().enumerate() {
                        batch.verts.push(Vertex {
                            pos: p[j],
                            normal: normals[j],
                            uv: mesh.uvs[vi],
                            color,
                            center: vcenter,
                            billboard: bb,
                        });
                    }
                }
            }
        }
    }

    fn material_key(
        &mut self,
        pak_file: &str,
        pak: &Pak,
        mats: &[pak::Material],
        mi: usize,
        layer: u8,
        tex_ids: &HashMap<String, usize>,
    ) -> (BatchKey, [f32; 4]) {
        let flat = |mode| BatchKey {
            images: vec![],
            fps_bits: 0,
            mode,
            layer,
            unlit: false,
            part: None,
            backdrop: 0,
            emissive: 0,
            spec_pow: 0,
            spec_shift: 0,
        };
        let grey = [0.6, 0.6, 0.6, 1.0];
        let Some(m) = mats.get(mi) else { return (flat(pak::BlendMode::Opaque), grey) };
        if m.flags & pak::MAT_TEXTURED == 0 {
            let rgb = self.remap.as_ref().map(|r| r.apply(m.rgb)).unwrap_or(m.rgb);
            let c = [rgb[0] as f32 / 255.0, rgb[1] as f32 / 255.0, rgb[2] as f32 / 255.0, 1.0];
            let mut key = flat(pak::BlendMode::for_material(m));
            (key.emissive, key.spec_pow, key.spec_shift) = shading(m, layer);
            return (key, c);
        }
        // Animated material: this and the following frames-1 materials are the frames.
        let animated = m.frames > 1 && m.anim_slot < 0x7d;
        let frame_mats: &[pak::Material] = if animated {
            &mats[mi..(mi + m.frames as usize).min(mats.len())]
        } else {
            std::slice::from_ref(m)
        };
        let mut images = Vec::with_capacity(frame_mats.len());
        for fm in frame_mats {
            match tex_ids.get(&tex_key(&fm.texture)) {
                Some(&id) => images.push(id),
                None => self.warn(format!("{pak_file}: texture {} not embedded", fm.texture)),
            }
        }
        if images.is_empty() {
            return (flat(pak::BlendMode::Opaque), grey);
        }
        let fps = match (images.len() > 1, m.anim_slot, self.slot0_fps) {
            (false, ..) => 0.0,
            (true, 0, Some(f)) => f,
            (true, slot, _) => pak.slot_fps(self.state, slot),
        };
        let mode = pak::BlendMode::for_material(m);
        let unlit = m.flags & pak::MAT_FULLBRIGHT != 0 || mode == pak::BlendMode::Additive;
        let (emissive, spec_pow, spec_shift) = shading(m, layer);
        let key = BatchKey {
            images,
            fps_bits: fps.to_bits(),
            mode,
            layer,
            unlit,
            part: None,
            backdrop: 0,
            emissive,
            spec_pow,
            spec_shift,
        };
        (key, [1.0; 4])
    }

    fn finish(self, mut info: Vec<String>, mut warnings: Vec<String>, lod_count: usize) -> Scene {
        warnings.extend(self.warnings);
        let (bbox_min, bbox_max) = if self.min[0] <= self.max[0] {
            (self.min, self.max)
        } else {
            ([-1.0; 3], [1.0; 3])
        };
        let anims = self.batches.iter().filter(|b| b.images.len() > 1).count();
        if anims > 0 || self.billboards > 0 || self.spinning > 0 || self.blinking > 0 {
            info.push(format!(
                "{anims} animated materials  {} billboard parts  {} spinning parts  {} blinking parts",
                self.billboards, self.spinning, self.blinking
            ));
        }
        let animator = (!self.dyn_instances.is_empty()).then_some(Animator { instances: self.dyn_instances });
        Scene {
            images: self.images,
            batches: self.batches,
            bbox_min,
            bbox_max,
            info,
            warnings,
            lod_count,
            state_names: vec![],
            objects: vec![],
            parent: None,
            light: None,
            fog: None,
            animator,
            gates: self.gates,
        }
    }
}

pub fn load_pak_scene(src: &AssetSource, name: &str, lod: usize, state: usize) -> Result<Scene> {
    let pak = pak::parse(&src.read(name)?)?;
    let mut b = Builder::new();
    b.state = state;
    // A gate model alone still gets its `.anm` pose (otherwise the pylons sit stacked).
    let mut warnings = vec![];
    if let Some(des) = spx::gate_des_for(src, name) {
        let drives = ModelCache::default().gate_drives(src, &des.anims, &mut warnings);
        if let Some(d) = des.models.iter().zip(drives).find(|(m, _)| m.eq_ignore_ascii_case(name)).and_then(|(_, d)| d) {
            b.slot0_fps = (d.role == Role::Center).then_some(gate::TICK_HZ / 2.0);
            b.drive = Some(d);
            b.gates = 1;
        }
    }
    b.add_pak(name, &pak, lod, &Mat3::IDENTITY, 1.0, [0.0; 3]);
    let parts = pak.lods.get(lod).map(|l| l.parts.len()).unwrap_or(0);
    let info = vec![
        format!("'{}'  flags {:#x}", pak.name, pak.flags),
        format!(
            "LOD {}/{} (switch at {:.0})  {} parts  {} textures",
            lod,
            pak.lods.len(),
            pak.lods.get(lod).map(|l| l.distance).unwrap_or(0.0),
            parts,
            pak.textures.len()
        ),
    ];
    let mut scene = b.finish(info, warnings, pak.lods.len());
    scene.state_names = pak.states.iter().map(|s| s.name.clone()).collect();
    Ok(scene)
}

pub fn load_ocf_scene(src: &AssetSource, name: &str) -> Result<Scene> {
    let ocf: Ocf = ocf::parse(&src.read(name)?)?;
    let mut paks: HashMap<String, Pak> = HashMap::new();
    let mut warnings = vec![];
    for p in &ocf.parts {
        let key = p.pak_file.to_uppercase();
        if paks.contains_key(&key) {
            continue;
        }
        match src.read(&p.pak_file).and_then(|d| pak::parse(&d)) {
            Ok(k) => {
                paks.insert(key, k);
            }
            Err(e) => warnings.push(format!("{}: {e}", p.pak_file)),
        }
    }
    let (placements, w) = ocf::place(&ocf, &paks);
    warnings.extend(w);
    let mut b = Builder::new();
    for pl in &placements {
        let pak = &paks[&pl.pak_file.to_uppercase()];
        b.add_pak(&pl.pak_file, pak, 0, &pl.basis, pl.scale, pl.offset);
    }
    let info = vec![format!("'{}'  {} parts  {} distinct paks", ocf.name, ocf.parts.len(), paks.len())];
    Ok(b.finish(info, warnings, 1))
}

/// Parsed models shared across a sector's objects.
#[derive(Default)]
struct ModelCache {
    paks: HashMap<String, Option<Pak>>,
    /// OCF file -> its part placements (or None if it failed to load).
    ocfs: HashMap<String, Option<Vec<ocf::Placement>>>,
    protects: HashMap<String, Option<[bool; 256]>>,
    anms: HashMap<String, Option<Arc<Anm>>>,
}

impl ModelCache {
    fn anm(&mut self, src: &AssetSource, file: &str, warnings: &mut Vec<String>) -> Option<Arc<Anm>> {
        let key = file.to_uppercase();
        self.anms
            .entry(key)
            .or_insert_with(|| match src.read(file).and_then(|d| Anm::parse(&d)) {
                Ok(a) => Some(Arc::new(a)),
                Err(e) => {
                    warnings.push(format!("{file}: {e}"));
                    None
                }
            })
            .clone()
    }

    /// Drives for a gate's models (one per model; `None` for non-gate models).
    fn gate_drives(&mut self, src: &AssetSource, anims: &[Option<(String, Role)>], warnings: &mut Vec<String>) -> Vec<Option<Drive>> {
        let loaded: Vec<Option<(Arc<Anm>, Role)>> =
            anims.iter().map(|a| a.as_ref().and_then(|(f, r)| Some((self.anm(src, f, warnings)?, *r)))).collect();
        let dur = |open: bool| {
            loaded
                .iter()
                .flatten()
                .map(|(a, r)| {
                    let name = match (r, open) {
                        (Role::Body, true) => "GATE_OPEN",
                        (Role::Body, false) => "GATE_CLOSE",
                        (_, true) => "GATEWAY_OPEN",
                        (_, false) => "GATEWAY_CLOSE",
                    };
                    a.duration(name)
                })
                .fold(0.0f32, f32::max)
        };
        let cycle = (dur(true), dur(false));
        loaded.into_iter().map(|l| l.map(|(anm, role)| Drive { anm, role, cycle })).collect()
    }

    /// Protected palette indices for a PAK: from its same-named `.pal`, else none.
    fn protect(&mut self, src: &AssetSource, pak_file: &str) -> Option<[bool; 256]> {
        let stem = pak_file.rsplit_once('.').map(|(s, _)| s).unwrap_or(pak_file).to_uppercase();
        if let Some(m) = self.protects.get(&stem) {
            return *m;
        }
        let m = src.read(&format!("{stem}.PAL")).ok().and_then(|d| palette::protect_mask(&d));
        self.protects.insert(stem, m);
        m
    }

    fn pak(&mut self, src: &AssetSource, file: &str, warnings: &mut Vec<String>) -> Option<&Pak> {
        let key = file.to_uppercase();
        if !self.paks.contains_key(&key) {
            let p = match src.read(file).and_then(|d| pak::parse(&d)) {
                Ok(p) => Some(p),
                Err(e) => {
                    warnings.push(format!("{file}: {e}"));
                    None
                }
            };
            self.paks.insert(key.clone(), p);
        }
        self.paks[&key].as_ref()
    }

    fn ocf(&mut self, src: &AssetSource, file: &str, warnings: &mut Vec<String>) -> Option<Vec<ocf::Placement>> {
        let key = file.to_uppercase();
        if !self.ocfs.contains_key(&key) {
            let placed = match src.read(file).and_then(|d| ocf::parse(&d)) {
                Ok(o) => {
                    let mut paks = HashMap::new();
                    for p in &o.parts {
                        if let Some(k) = self.pak(src, &p.pak_file, warnings) {
                            paks.insert(p.pak_file.to_uppercase(), k.clone());
                        }
                    }
                    let (pl, w) = ocf::place(&o, &paks);
                    warnings.extend(w.into_iter().map(|w| format!("{file}: {w}")));
                    Some(pl)
                }
                Err(e) => {
                    warnings.push(format!("{file}: {e}"));
                    None
                }
            };
            self.ocfs.insert(key.clone(), placed);
        }
        self.ocfs[&key].clone()
    }
}

impl Builder {
    /// Add a PAK or OCF at a world rotation / position. Returns false if nothing was drawn.
    fn add_model(&mut self, src: &AssetSource, cache: &mut ModelCache, file: &str, rot: &Mat3, pos: [f32; 3], warnings: &mut Vec<String>) -> bool {
        if file.to_uppercase().ends_with(".OCF") {
            let Some(placements) = cache.ocf(src, file, warnings) else { return false };
            for pl in &placements {
                if self.remap.is_some() {
                    self.protect = cache.protect(src, &pl.pak_file);
                }
                let Some(pak) = cache.pak(src, &pl.pak_file, warnings) else { continue };
                let basis = rot.mul(&pl.basis);
                let offset = math::add(pos, rot.mul_vec(pl.offset));
                self.add_pak(&pl.pak_file, pak, 0, &basis, pl.scale, offset);
            }
            !placements.is_empty()
        } else {
            if self.remap.is_some() {
                self.protect = cache.protect(src, file);
            }
            let Some(pak) = cache.pak(src, file, warnings) else { return false };
            self.add_pak(file, pak, 0, rot, 1.0, pos);
            true
        }
    }
}

/// Sky model plus distant images, camera-centred (see `bdf`).
fn add_backdrop(b: &mut Builder, src: &AssetSource, cache: &mut ModelCache, bd: &bdf::Bdf, warnings: &mut Vec<String>) {
    // FUN_00468440 draws no sky or images in fog sectors.
    if bd.fog.is_some() {
        return;
    }
    if let Some(sky) = &bd.space_pak {
        b.backdrop = 1;
        let rot = Mat3::from_engine_euler_deg([bd.space_rot[0], bd.space_rot[1], 0.0]).scaled(bdf::SKY_SCALE);
        if let Some(pak) = cache.pak(src, sky, warnings) {
            b.add_pak(sky, pak, 0, &rot, bdf::SKY_SCALE, [0.0; 3]);
        }
        // The lightning overlay is the sky's twin: same rotation and scale, drawn only during
        // flashes (backdrop pass 3).
        if let (true, Some(zap)) = (bd.lightning, &bd.lightning_pak) {
            b.backdrop = 3;
            if let Some(pak) = cache.pak(src, zap, warnings) {
                b.add_pak(zap, pak, 0, &rot, bdf::SKY_SCALE, [0.0; 3]);
            }
        }
    }
    b.backdrop = 2;
    for img in &bd.images {
        let dir = Mat3::from_engine_euler_deg([img.yaw, img.pitch, 0.0]);
        let pos = dir.mul_vec([0.0, 0.0, img.distance]);
        if let Some(pak) = cache.pak(src, &img.pak, warnings) {
            b.add_pak(&img.pak, pak, 0, &Mat3::IDENTITY.scaled(img.scale), img.scale, pos);
        }
    }
    b.backdrop = 0;
}

/// A sector: every placed object's model at its position and orientation. With
/// `include_parent`, a sub-sector stage (`bora05A.spx`) also draws its main sector's objects
/// (`BORA05.SPX`), as the game loads both.
pub fn load_spx_scene(src: &AssetSource, name: &str, include_parent: bool) -> Result<Scene> {
    let main = spx::parse(&src.read(name)?)?;
    let mut warnings = vec![];
    let mut files = vec![];
    let parent = spx::find_parent(src, name);
    if include_parent {
        if let Some(p) = &parent {
            match src.read(p).and_then(|d| spx::parse(&d)) {
                Ok(s) => files.push((p.clone(), s)),
                Err(e) => warnings.push(format!("{p}: {e}")),
            }
        }
    }
    files.push((name.to_string(), main.clone()));
    let mut des = spx::DesResolver::new(src)?;
    let mut cache = ModelCache::default();
    let mut b = Builder::new();
    let mut objects = vec![];
    let (mut placed, mut deferred) = (0, 0);
    // Backdrop: a sub-sector stage usually leaves it blank and uses its main sector's.
    let backdrop = files.iter().rev().map(|(_, s)| s.backdrop.clone()).find(|b| !b.is_empty()).or_else(|| {
        parent
            .as_ref()
            .and_then(|p| src.read(p).ok())
            .and_then(|d| spx::parse(&d).ok())
            .map(|s| s.backdrop)
            .filter(|b| !b.is_empty())
    });
    let bdf = backdrop.as_ref().and_then(|n| match src.read(n).and_then(|d| bdf::parse(&d)) {
        Ok(bd) => Some(bd),
        Err(e) => {
            warnings.push(format!("{n}: {e}"));
            None
        }
    });
    // Models get the sector's gamma / saturation (backdrop paks are loaded unremapped).
    if let Some(bd) = &bdf {
        let r = Remap::new(bd.gamma, bd.saturation);
        b.remap = (!r.is_identity()).then_some(r);
    }
    for (file, s) in &files {
        for p in &s.placements {
            // Null records (kind 0) carry decl index -1.
            let Some(alias) = s.decls.get(p.decl).cloned() else { continue };
            let (models, drives) = match des.resolve(src, &alias) {
                Ok(m) => {
                    let d = cache.gate_drives(src, &m.anims, &mut warnings);
                    (m.models, d)
                }
                Err(e) => {
                    b.warn(format!("{file}: {e}"));
                    continue;
                }
            };
            let rot = Mat3::from_engine_euler_deg(p.orient.map(|a| a as f32 * 360.0 / 512.0));
            // Track this object's own bounds for the object list.
            let (min0, max0) = (b.min, b.max);
            b.min = [f32::MAX; 3];
            b.max = [f32::MIN; 3];
            let mut drew = false;
            b.gates += drives.iter().any(|d| d.is_some()) as usize;
            for (m, d) in models.iter().zip(drives) {
                // The centre's think advances its texture counter every other tick
                // (`FUN_00425d60`): 25 fps.
                b.slot0_fps = d.as_ref().filter(|d| d.role == Role::Center).map(|_| gate::TICK_HZ / 2.0);
                b.drive = d;
                drew |= b.add_model(src, &mut cache, m, &rot, p.pos, &mut warnings);
            }
            b.drive = None;
            b.slot0_fps = None;
            let (omin, omax) = if b.min[0] <= b.max[0] { (b.min, b.max) } else { (to_gl(p.pos), to_gl(p.pos)) };
            for k in 0..3 {
                b.min[k] = min0[k].min(omin[k]);
                b.max[k] = max0[k].max(omax[k]);
            }
            if !drew {
                continue;
            }
            placed += 1;
            deferred += p.deferred() as usize;
            let d = math::sub(omax, omin);
            let src_tag = if files.len() > 1 && file != name { format!("  [{file}]") } else { String::new() };
            objects.push(SceneObject {
                label: format!(
                    "{alias}  #{}{}{}{src_tag}",
                    p.id,
                    if p.group >= 0 { format!("  group {}", p.group) } else { String::new() },
                    if p.deferred() { "  (spawned by trigger)" } else { "" }
                ),
                center: math::scale(math::add(omin, omax), 0.5),
                radius: (math::dot(d, d).sqrt() * 0.5).max(1.0),
            });
        }
    }
    b.remap = None;
    b.protect = None;
    if let Some(bd) = &bdf {
        add_backdrop(&mut b, src, &mut cache, bd, &mut warnings);
    }
    let mut info = vec![format!(
        "{}  backdrop {}  {} objects drawn ({} spawned later by triggers)",
        main.quad,
        backdrop.as_deref().unwrap_or("none"),
        placed,
        deferred
    )];
    match (&parent, include_parent) {
        (Some(p), true) => info.push(format!("sub-sector of {p}: its objects are included")),
        (Some(p), false) => info.push(format!("sub-sector of {p} (not included)")),
        _ => {}
    }
    if !main.stages.is_empty() {
        info.push(format!("stages: {}", main.stages.join(", ")));
    }
    let mut scene = b.finish(info, warnings, 1);
    scene.objects = objects;
    scene.parent = parent;
    scene.light = bdf.as_ref().map(|bd| {
        let amb = (bd.ambient >> 1) as f32 / 256.0;
        SectorLight {
            dir: to_gl(bd.light_dir()),
            ambient: bd.rgb.map(|c| amb * c as f32 / 255.0),
            diffuse: bd.light_rgb.map(|c| c as f32 / 255.0),
            lightning: bd.lightning.then_some(bd.lightning_ambient),
        }
    });
    if let Some(f) = bdf.as_ref().and_then(|bd| bd.fog.as_ref()) {
        let image = match src.read(&f.pcx).ok().and_then(|d| pcx_image(&d)) {
            Some(img) => {
                scene.images.push(img);
                Some(scene.images.len() - 1)
            }
            None => {
                scene.warnings.push(format!("fog texture {} not found", f.pcx));
                None
            }
        };
        let c = |v: [i32; 3]| v.map(|x| x.clamp(0, 255) as f32 / 255.0);
        scene.fog = Some(SectorFog { min: f.min, max: f.max, color: c(f.rgb), front: c(f.front_rgb), image });
        scene.info.push(format!("fog: {}..{}  RGB {:?}  sheets {} tint {:?}", f.min, f.max, f.rgb, f.pcx, f.front_rgb));
    }
    if let Some(bd) = &bdf {
        scene.info.push(format!(
            "lighting: {}  ambient {}  RGB {:?}  light {:?}  gamma {}  saturation {}",
            if bd.lightning { "lightning" } else { "sun" },
            bd.ambient,
            bd.rgb,
            bd.light_rgb,
            bd.gamma,
            bd.saturation
        ));
    }
    Ok(scene)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The animator's per-part GL matrix at t = 0 must reproduce the geometry the static path
    /// bakes, under a rotated, scaled and offset placement (as in an OCF).
    #[test]
    fn animated_parts_match_baked_pose() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../pff")) else { return };
        let basis = Mat3::from_engine_euler_deg([30.0, -20.0, 45.0]).scaled(2.5);
        let offset = [10.0, -4.0, 7.0];
        for name in ["SINZAP.PAK", "TORPFRTS.PAK", "STARDISC.PAK", "TRANSP02.PAK", "IBDBEACN.PAK"] {
            let pak = pak::parse(&src.read(name).unwrap()).unwrap();
            let mut b = Builder::new();
            b.add_pak(name, &pak, 0, &basis, 2.5, offset);
            let anim = Animator { instances: std::mem::take(&mut b.dyn_instances) };
            let xf = anim.frame(0.0, GateMode::Open);
            let poses = Rig::new(&pak, 0, 0).pose(0.0);
            let mut checked = 0;
            for batch in &b.batches {
                let i = batch.part.expect("animated instance batches carry a part");
                let part = &pak.lods[0].parts[i];
                if part.flags & pak::PART_BILLBOARD != 0 {
                    continue;
                }
                let Some(Some(x)) = xf.get(i) else { continue };
                for v in &batch.verts {
                    let m = &x.model;
                    let p = v.pos;
                    let got = [0, 1, 2].map(|r| m[r] * p[0] + m[4 + r] * p[1] + m[8 + r] * p[2] + m[12 + r]);
                    let want = to_gl(math::add(
                        // Part-space vertices are GL-axis engine positions; to_gl is its own inverse.
                        basis.mul_vec(math::add(poses[i].pos, poses[i].m.mul_vec(to_gl(p)))),
                        offset,
                    ));
                    assert!((0..3).all(|k| (got[k] - want[k]).abs() < 1e-3 * (1.0 + want[k].abs())), "{name} part {i}: {got:?} vs {want:?}");
                    checked += 1;
                }
            }
            assert!(checked > 0, "{name}: nothing checked");
        }
    }
}

#[cfg(test)]
mod spx_tests {
    use super::*;

    /// Every shipped sector builds; report the slowest and the warning count.
    #[test]
    fn every_spx_scene_builds() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../pff")) else { return };
        let (mut worst, mut worst_name, mut warns, mut n) = (0.0f64, String::new(), 0, 0);
        for name in src.list(&[".SPX"]) {
            let t = std::time::Instant::now();
            let s = load_spx_scene(&src, &name, true).unwrap_or_else(|e| panic!("{name}: {e}"));
            let dt = t.elapsed().as_secs_f64();
            if dt > worst {
                worst = dt;
                worst_name = name.clone();
            }
            warns += s.warnings.len();
            n += 1;
        }
        eprintln!("{n} sectors, slowest {worst_name} {worst:.3}s, {warns} warnings");
    }
}
