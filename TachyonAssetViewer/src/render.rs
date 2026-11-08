//! OpenGL rendering of a `Scene` inside an egui paint callback.

use crate::math::{self, Mat4};
use crate::model::{PartXform, Scene, SectorFog, SectorLight, Vertex};
use crate::weather::SheetLayer;
use std::sync::Arc;
use crate::pak::BlendMode;
use eframe::glow::{self, HasContext};

const VS: &str = r#"
layout(location = 0) in vec3 a_pos;
layout(location = 1) in vec3 a_normal;
layout(location = 2) in vec2 a_uv;
layout(location = 3) in vec4 a_color;
layout(location = 4) in vec3 a_center;
layout(location = 5) in float a_billboard;
uniform mat4 u_view;
uniform mat4 u_proj;
uniform mat4 u_model;     // animated parts; identity for baked geometry
uniform vec3 u_bbscale;   // billboard offset scale (state scale curves)
out vec3 v_normal;
out vec3 v_world;
out float v_depth;
out vec2 v_uv;
out vec4 v_color;
void main() {
    v_normal = mat3(u_model) * a_normal;
    v_uv = a_uv;
    v_color = a_color;
    // Billboards: the offset is already in camera axes, added after the view transform.
    vec4 vp = a_billboard > 0.5 ? u_view * (u_model * vec4(a_center, 1.0)) + vec4(a_pos * u_bbscale, 0.0)
                                : u_view * (u_model * vec4(a_pos, 1.0));
    v_world = (u_model * vec4(a_billboard > 0.5 ? a_center : a_pos, 1.0)).xyz;
    v_depth = -vp.z;
    gl_Position = u_proj * vp;
}
"#;

const FS: &str = r#"
precision mediump float;
in vec3 v_normal;
in vec3 v_world;
in float v_depth;
in vec2 v_uv;
in vec4 v_color;
uniform sampler2D u_tex;
uniform int u_textured;
uniform vec3 u_light;
uniform int u_lighting;
uniform int u_lightmode;  // 0 viewer headlight, 1 sector (BDF) light
uniform vec3 u_amb;
uniform vec3 u_diffuse;
uniform int u_colorkey;   // discard palette index 0 (texture alpha 0)
uniform int u_mode;       // 0 solid, 1 shadow, 2 additive, 3 glass
uniform float u_emissive; // >= 0: unlit at this brightness (material alpha); < 0: lit
uniform int u_spec_pow;   // 0 none; 1, 2, 3: cos, cos^2, cos^4 of the half-vector
uniform float u_spec_scale;
uniform vec3 u_eye;
uniform int u_fog;        // linear depth fog (BDF [fog])
uniform vec2 u_fog_range;
uniform vec3 u_fog_color;
out vec4 frag;
void main() {
    vec4 c = v_color;
    if (u_textured != 0) {
        vec4 t = texture(u_tex, v_uv);
        if (u_colorkey != 0 && t.a < 0.5) discard;
        c *= t;
    }
    if (u_mode == 1) {
        // Shadow card: darken what's behind wherever the texel isn't the colour key.
        float fs = u_fog != 0 ? clamp((v_depth - u_fog_range.x) / (u_fog_range.y - u_fog_range.x), 0.0, 1.0) : 0.0;
        frag = vec4(0.0, 0.0, 0.0, 0.55 * (1.0 - fs));
        return;
    }
    vec3 shade = vec3(1.0);
    float spec = 0.0;
    if (u_emissive >= 0.0) {
        // FUN_004b4c10: self-illuminated materials ignore the light.
        shade = vec3(u_emissive);
    } else if (u_lighting != 0) {
        if (u_lightmode == 1) {
            // Light_BuildShadeRamp: ambient·RGB + max(N·L, 0)·LIGHT_RGB, clamped to 4..252.
            vec3 n = normalize(v_normal);
            float d = max(dot(n, u_light), 0.0);
            shade = clamp(u_amb + d * u_diffuse, 4.0 / 255.0, 252.0 / 255.0);
            if (u_spec_pow > 0) {
                // FUN_004b49d0 / FUN_004b4b80: grey highlight from the half-vector
                // normalize(V + 2L), added after the texture (vertex specular colour).
                vec3 h = normalize(normalize(u_eye - v_world) + 2.0 * u_light);
                float ch = max(dot(n, h), 0.0);
                float p = u_spec_pow == 1 ? ch : (u_spec_pow == 2 ? ch * ch : ch * ch * ch * ch);
                spec = min(p * u_spec_scale, 1.0);
            }
        } else {
            shade = vec3(0.35 + 0.65 * abs(dot(normalize(v_normal), u_light)));
        }
    }
    vec3 rgb = c.rgb * shade + spec;
    if (u_fog != 0) {
        // FUN_004b47b0: fog = (z - MIN) / (MAX - MIN), to the fog colour. Additive surfaces
        // fade out instead (adding fog colour would brighten).
        float f = clamp((v_depth - u_fog_range.x) / (u_fog_range.y - u_fog_range.x), 0.0, 1.0);
        rgb = u_mode == 2 ? rgb * (1.0 - f) : mix(rgb, u_fog_color, f);
    }
    frag = vec4(rgb, 1.0);
}
"#;

/// Fog sheets (`FUN_004b9250`): one full-screen triangle per sheet, at the sheet's depth.
const SHEET_VS: &str = r#"
uniform float u_ndc_z;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2)) * 2.0 - 1.0;
    gl_Position = vec4(p, u_ndc_z, 1.0);
}
"#;

const SHEET_FS: &str = r#"
precision mediump float;
uniform sampler2D u_tex;
uniform vec4 u_vp;        // viewport x, y, w, h (pixels)
uniform float u_depth;    // sheet depth (fog-sheet units)
uniform vec2 u_offset;
uniform vec2 u_rot;       // sin, cos of the camera roll
uniform vec3 u_tint;      // FRONT_RGB x fade
out vec4 frag;
void main() {
    // Screen offset from the centre, in 640-wide pixels with y down, as the game computes it.
    vec2 c = u_vp.xy + 0.5 * u_vp.zw;
    vec2 p = vec2(gl_FragCoord.x - c.x, c.y - gl_FragCoord.y) * (640.0 / u_vp.z);
    float k = u_depth * 0.01 / 384.0;
    vec2 uv = vec2(p.x * u_rot.y - p.y * u_rot.x, p.x * u_rot.x + p.y * u_rot.y) * k + u_offset;
    // Two texture stages: the second samples the same texture transposed at a third the
    // frequency, breaking up the tiling.
    vec3 t = texture(u_tex, uv).rgb * texture(u_tex, uv.yx / 3.0).rgb;
    frag = vec4(t * u_tint, 1.0);
}
"#;

/// Per-frame fog state for drawing.
#[derive(Clone, Debug)]
pub struct FogDraw {
    pub fog: SectorFog,
    pub layers: Vec<SheetLayer>,
    /// (sin, cos) of the camera roll.
    pub rot: (f32, f32),
}

/// Per-frame lightning state.
#[derive(Clone, Copy, Debug)]
pub struct Flash {
    pub diffuse_scale: f32,
    /// While a bolt shows: (animation counter, brightness).
    pub bolt: Option<(usize, f32)>,
}

struct GpuBatch {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    count: i32,
    /// One texture, or the frames of an animated material.
    textures: Vec<glow::Texture>,
    fps: f32,
    mode: BlendMode,
    layer: u8,
    unlit: bool,
    part: Option<usize>,
    backdrop: u8,
    emissive: u8,
    spec_pow: u8,
    spec_shift: u8,
}

struct GpuScene {
    batches: Vec<GpuBatch>,
    textures: Vec<glow::Texture>,
}

pub struct Renderer {
    program: glow::Program,
    sheet_program: glow::Program,
    scene: Option<GpuScene>,
    pending: Option<Scene>,
}

#[derive(Clone)]
pub struct DrawParams {
    pub view: Mat4,
    pub proj: Mat4,
    /// Projection for the camera-centred backdrop pass (its own near/far, like the game's).
    pub proj_backdrop: Mat4,
    /// Draw the sector backdrop (sky and distant images).
    pub backdrop: bool,
    /// Seconds, for texture animation.
    pub time: f32,
    /// This frame's transforms for animated parts (`None` = hidden).
    pub parts: Arc<Vec<Option<PartXform>>>,
    pub light: [f32; 3],
    /// Camera position (GL world), for specular highlights.
    pub eye: [f32; 3],
    /// Use the sector's BDF light instead of the headlight.
    pub sector_light: Option<SectorLight>,
    pub lighting: bool,
    pub wireframe: bool,
    pub textures: bool,
    /// Use per-material blend modes; off draws everything solid.
    pub blending: bool,
    pub color_key: bool,
    pub background: [f32; 3],
    pub flash: Option<Flash>,
    /// Fog (drawn only when set; the caller decides whether fog is on).
    pub fog: Option<FogDraw>,
}

fn shader_header() -> &'static str {
    if cfg!(target_arch = "wasm32") {
        "#version 300 es\n"
    } else {
        "#version 330 core\n"
    }
}

impl Renderer {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        unsafe {
            let program = Self::link(gl, VS, FS)?;
            let sheet_program = Self::link(gl, SHEET_VS, SHEET_FS)?;
            Ok(Self { program, sheet_program, scene: None, pending: None })
        }
    }

    unsafe fn link(gl: &glow::Context, vs: &str, fs: &str) -> Result<glow::Program, String> {
        {
            let program = gl.create_program()?;
            let mut shaders = vec![];
            for (ty, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
                let s = gl.create_shader(ty)?;
                gl.shader_source(s, &format!("{}{}", shader_header(), src));
                gl.compile_shader(s);
                if !gl.get_shader_compile_status(s) {
                    return Err(gl.get_shader_info_log(s));
                }
                gl.attach_shader(program, s);
                shaders.push(s);
            }
            gl.link_program(program);
            if !gl.get_program_link_status(program) {
                return Err(gl.get_program_info_log(program));
            }
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            Ok(program)
        }
    }

    /// Queue a scene for upload on the next paint (the GL context is current there).
    pub fn set_scene(&mut self, scene: Scene) {
        self.pending = Some(scene);
    }

    unsafe fn upload(&mut self, gl: &glow::Context, scene: Scene) {
        if let Some(old) = self.scene.take() {
            Self::free(gl, old);
        }
        let mut textures = Vec::with_capacity(scene.images.len());
        for img in &scene.images {
            let t = gl.create_texture().expect("texture");
            gl.bind_texture(glow::TEXTURE_2D, Some(t));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                img.width as i32,
                img.height as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&img.rgba)),
            );
            // Nearest filtering keeps the original chunky 8-bit look.
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::NEAREST as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::NEAREST as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::REPEAT as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::REPEAT as i32);
            textures.push(t);
        }
        let stride = std::mem::size_of::<Vertex>() as i32;
        let mut batches = Vec::with_capacity(scene.batches.len());
        for b in &scene.batches {
            if b.verts.is_empty() {
                continue;
            }
            let vao = gl.create_vertex_array().expect("vao");
            let vbo = gl.create_buffer().expect("vbo");
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            let bytes = std::slice::from_raw_parts(b.verts.as_ptr() as *const u8, b.verts.len() * stride as usize);
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
            for (loc, size, ofs) in [(0u32, 3, 0), (1, 3, 12), (2, 2, 24), (3, 4, 32), (4, 3, 48), (5, 1, 60)] {
                gl.enable_vertex_attrib_array(loc);
                gl.vertex_attrib_pointer_f32(loc, size, glow::FLOAT, false, stride, ofs);
            }
            batches.push(GpuBatch {
                vao,
                vbo,
                count: b.verts.len() as i32,
                textures: b.images.iter().map(|&i| textures[i]).collect(),
                fps: b.fps,
                mode: b.mode,
                layer: b.layer,
                unlit: b.unlit,
                part: b.part,
                backdrop: b.backdrop,
                emissive: b.emissive,
                spec_pow: b.spec_pow,
                spec_shift: b.spec_shift,
            });
        }
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        // Draw order: solid, then shadows, then additive (blended passes don't write depth).
        // Within a pass, decal layers go after their bases (same depth; LEQUAL lets them win).
        batches.sort_by_key(|b| (b.mode, b.layer));
        self.scene = Some(GpuScene { batches, textures });
    }

    unsafe fn free(gl: &glow::Context, s: GpuScene) {
        for b in s.batches {
            gl.delete_vertex_array(b.vao);
            gl.delete_buffer(b.vbo);
        }
        for t in s.textures {
            gl.delete_texture(t);
        }
    }

    /// Additive (`SRCALPHA, ONE`, alpha 0xff), depth-tested against the world, no depth write.
    unsafe fn draw_sheets(&self, gl: &glow::Context, p: &DrawParams, f: &FogDraw, tex: glow::Texture) {
        #[cfg(not(target_arch = "wasm32"))]
        gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
        gl.use_program(Some(self.sheet_program));
        let loc = |n: &str| gl.get_uniform_location(self.sheet_program, n);
        let mut vp = [0i32; 4];
        gl.get_parameter_i32_slice(glow::VIEWPORT, &mut vp);
        gl.uniform_4_f32(loc("u_vp").as_ref(), vp[0] as f32, vp[1] as f32, vp[2].max(1) as f32, vp[3].max(1) as f32);
        gl.uniform_2_f32(loc("u_rot").as_ref(), f.rot.0, f.rot.1);
        gl.uniform_1_i32(loc("u_tex").as_ref(), 0);
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(tex));
        gl.enable(glow::DEPTH_TEST);
        gl.depth_mask(false);
        gl.enable(glow::BLEND);
        gl.blend_func(glow::ONE, glow::ONE);
        // An empty VAO: the vertex shader makes the triangle from gl_VertexID.
        let vao = gl.create_vertex_array().ok();
        gl.bind_vertex_array(vao);
        let m = &p.proj.0;
        for l in &f.layers {
            // Forward flight moves sheets 0.4 depth units per world unit.
            let z = l.depth * 2.5;
            let (zc, wc) = (-m[10] * z + m[14], z);
            gl.uniform_1_f32(loc("u_ndc_z").as_ref(), (zc / wc).clamp(-1.0, 1.0));
            gl.uniform_1_f32(loc("u_depth").as_ref(), l.depth);
            gl.uniform_2_f32(loc("u_offset").as_ref(), l.offset[0], l.offset[1]);
            let t = f.fog.front.map(|c| c * l.fade);
            gl.uniform_3_f32(loc("u_tint").as_ref(), t[0], t[1], t[2]);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
        gl.bind_vertex_array(None);
        if let Some(v) = vao {
            gl.delete_vertex_array(v);
        }
        gl.use_program(Some(self.program));
    }

    pub fn paint(&mut self, gl: &glow::Context, p: &DrawParams) {
        unsafe {
            if let Some(scene) = self.pending.take() {
                self.upload(gl, scene);
            }
            gl.clear_color(p.background[0], p.background[1], p.background[2], 1.0);
            gl.clear_depth_f32(1.0);
            gl.clear(glow::COLOR_BUFFER_BIT | glow::DEPTH_BUFFER_BIT);
            let Some(scene) = &self.scene else { return };
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LEQUAL);
            gl.depth_mask(true);
            gl.disable(glow::CULL_FACE);
            gl.disable(glow::BLEND);
            #[cfg(not(target_arch = "wasm32"))]
            if p.wireframe {
                gl.polygon_mode(glow::FRONT_AND_BACK, glow::LINE);
            }
            gl.use_program(Some(self.program));
            let loc = |n: &str| gl.get_uniform_location(self.program, n);
            let u_view = loc("u_view");
            let u_proj = loc("u_proj");
            let (l, mode, amb, dif) = match &p.sector_light {
                Some(s) => {
                    let k = p.flash.map_or(1.0, |f| f.diffuse_scale);
                    (s.dir, 1, s.ambient, s.diffuse.map(|d| d * k))
                }
                None => (math::normalize(p.light), 0, [0.0; 3], [0.0; 3]),
            };
            gl.uniform_3_f32(loc("u_light").as_ref(), l[0], l[1], l[2]);
            gl.uniform_1_i32(loc("u_lightmode").as_ref(), mode);
            gl.uniform_3_f32(loc("u_amb").as_ref(), amb[0], amb[1], amb[2]);
            gl.uniform_3_f32(loc("u_diffuse").as_ref(), dif[0], dif[1], dif[2]);
            gl.uniform_1_i32(loc("u_tex").as_ref(), 0);
            let u_colorkey = loc("u_colorkey");
            gl.active_texture(glow::TEXTURE0);
            let u_textured = loc("u_textured");
            let u_lighting = loc("u_lighting");
            let u_mode = loc("u_mode");
            let u_model = loc("u_model");
            let u_bbscale = loc("u_bbscale");
            let u_emissive = loc("u_emissive");
            let u_spec_pow = loc("u_spec_pow");
            let u_spec_scale = loc("u_spec_scale");
            gl.uniform_3_f32(loc("u_eye").as_ref(), p.eye[0], p.eye[1], p.eye[2]);
            let u_fog = loc("u_fog");
            if let Some(f) = &p.fog {
                gl.uniform_2_f32(loc("u_fog_range").as_ref(), f.fog.min, f.fog.max.max(f.fog.min + 1.0));
                let c = f.fog.color;
                gl.uniform_3_f32(loc("u_fog_color").as_ref(), c[0], c[1], c[2]);
            }
            let bolt = p.flash.and_then(|f| f.bolt);
            // The backdrop follows the camera: the view's rotation without its translation.
            let mut view_rot = p.view;
            view_rot.0[12] = 0.0;
            view_rot.0[13] = 0.0;
            view_rot.0[14] = 0.0;
            // Passes, as FUN_00468440 orders them: sky (never occludes), lightning over it during
            // flashes, distant images, then the world on a fresh depth buffer.
            for pass in [1u8, 3, 2, 0] {
                if pass != 0 && !p.backdrop || pass == 3 && bolt.is_none() {
                    continue;
                }
                gl.uniform_1_i32(u_fog.as_ref(), (pass == 0 && p.fog.is_some()) as i32);
                if pass == 0 {
                    gl.uniform_matrix_4_f32_slice(u_view.as_ref(), false, &p.view.0);
                    gl.uniform_matrix_4_f32_slice(u_proj.as_ref(), false, &p.proj.0);
                    gl.depth_mask(true);
                    gl.clear(glow::DEPTH_BUFFER_BIT);
                } else {
                    gl.uniform_matrix_4_f32_slice(u_view.as_ref(), false, &view_rot.0);
                    gl.uniform_matrix_4_f32_slice(u_proj.as_ref(), false, &p.proj_backdrop.0);
                }
                for b in scene.batches.iter().filter(|b| b.backdrop == pass) {
                    let xf = match b.part {
                        None => PartXform::IDENTITY,
                        Some(i) => match p.parts.get(i) {
                            Some(Some(x)) => *x,
                            Some(None) => continue, // blinked off
                            None => PartXform::IDENTITY,
                        },
                    };
                    gl.uniform_matrix_4_f32_slice(u_model.as_ref(), false, &xf.model);
                    gl.uniform_3_f32(u_bbscale.as_ref(), xf.bbscale[0], xf.bbscale[1], xf.bbscale[2]);
                    // Mat_GetAnimFrame: frame = counter % frames, counter = fps × seconds.
                    // The lightning overlay's counter is the flash's random bolt number.
                    let frame = if pass == 3 {
                        bolt.map_or(0, |(n, _)| n) % b.textures.len().max(1)
                    } else if b.textures.len() > 1 && b.fps > 0.0 {
                        ((p.time * b.fps) as usize) % b.textures.len()
                    } else {
                        0
                    };
                    let tex = if p.textures { b.textures.get(frame).copied() } else { None };
                    let mode = if p.blending && p.textures { b.mode } else { BlendMode::Opaque };
                    match mode {
                        BlendMode::Opaque | BlendMode::Cutout => {
                            gl.disable(glow::BLEND);
                            gl.depth_mask(true);
                            gl.uniform_1_i32(u_mode.as_ref(), 0);
                        }
                        BlendMode::Shadow => {
                            gl.enable(glow::BLEND);
                            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                            gl.depth_mask(false);
                            gl.uniform_1_i32(u_mode.as_ref(), 1);
                        }
                        BlendMode::Additive => {
                            gl.enable(glow::BLEND);
                            gl.blend_func(glow::ONE, glow::ONE);
                            gl.depth_mask(false);
                            gl.uniform_1_i32(u_mode.as_ref(), 2);
                        }
                    }
                    if pass == 1 || pass == 3 {
                        // The sky is a shell around the camera; nothing should test against it.
                        gl.depth_mask(false);
                    }
                    let keyed = p.color_key && p.textures && (b.mode.color_keyed() || !p.blending);
                    gl.uniform_1_i32(u_colorkey.as_ref(), keyed as i32);
                    let lit = p.lighting && pass != 1 && !(b.unlit && p.blending);
                    gl.uniform_1_i32(u_lighting.as_ref(), lit as i32);
                    // Material alpha = self-illumination at 2·alpha (0xff: full). With the sector
                    // light this follows the game exactly (alpha 0 is lit, even additive glows);
                    // the headlight keeps additive / full-bright materials at full brightness.
                    let emissive = if pass == 3 {
                        // Instance flag 0x80: self-lit materials take the flash level.
                        bolt.map_or(1.0, |(_, k)| k)
                    } else if !p.lighting || pass == 1 {
                        -1.0
                    } else if b.emissive != 0 {
                        if b.emissive >= 0xfe { 1.0 } else { 2.0 * b.emissive as f32 / 255.0 }
                    } else if p.sector_light.is_none() && b.unlit && p.blending {
                        1.0
                    } else {
                        -1.0
                    };
                    gl.uniform_1_f32(u_emissive.as_ref(), emissive);
                    gl.uniform_1_i32(u_spec_pow.as_ref(), b.spec_pow as i32);
                    gl.uniform_1_f32(u_spec_scale.as_ref(), (1u32 << b.spec_shift.min(16)) as f32 / 255.0);
                    gl.uniform_1_i32(u_textured.as_ref(), tex.is_some() as i32);
                    gl.bind_texture(glow::TEXTURE_2D, tex);
                    gl.bind_vertex_array(Some(b.vao));
                    gl.draw_arrays(glow::TRIANGLES, 0, b.count);
                }
            }
            if let (Some(f), true) = (&p.fog, p.textures) {
                if let Some(&tex) = f.fog.image.and_then(|i| scene.textures.get(i)) {
                    self.draw_sheets(gl, p, f, tex);
                }
            }
            gl.disable(glow::BLEND);
            gl.depth_mask(true);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            #[cfg(not(target_arch = "wasm32"))]
            gl.polygon_mode(glow::FRONT_AND_BACK, glow::FILL);
            gl.disable(glow::DEPTH_TEST);
        }
    }
}
