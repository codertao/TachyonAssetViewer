//! egui front end: asset list on the left, 3D view on the right.

use crate::audio::Audio;
use crate::music;
use crate::gate::{self, GateMode};
use crate::pwf::{Bank, Sound};
use std::collections::HashMap;
use crate::math::{self, Mat4};
use crate::model::{self, Animator, Scene, SceneObject, SectorFog, SectorLight};
use crate::render::{DrawParams, Flash, FogDraw, Renderer};
use crate::weather::{FogSheets, Lightning};
use crate::source::AssetSource;
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

struct Camera {
    yaw: f32,
    pitch: f32,
    distance: f32,
    target: [f32; 3],
    /// Distance that frames the current model; zoom limits are relative to it.
    fit: f32,
}

impl Camera {
    fn new() -> Self {
        Self { yaw: 0.6, pitch: 0.35, distance: 5.0, target: [0.0; 3], fit: 5.0 }
    }

    fn frame(&mut self, min: [f32; 3], max: [f32; 3]) {
        self.target = math::scale(math::add(min, max), 0.5);
        let radius = (math::dot(math::sub(max, min), math::sub(max, min))).sqrt() * 0.5;
        self.fit = (radius.max(0.01)) / (FOV / 2.0).tan() * 0.8;
        self.distance = self.fit;
    }

    fn eye(&self) -> [f32; 3] {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        math::add(self.target, math::scale([cp * sy, sp, cp * cy], self.distance))
    }

    fn view(&self) -> Mat4 {
        Mat4::look_at(self.eye(), self.target, [0.0, 1.0, 0.0])
    }

    /// The game draws the backdrop with near 100 / far 12000 (`FUN_00468440`); far is a bit
    /// larger here because the viewer's field of view differs.
    fn proj_backdrop(&self, aspect: f32) -> Mat4 {
        Mat4::perspective(FOV, aspect, 50.0, 30000.0)
    }

    fn proj(&self, aspect: f32) -> Mat4 {
        let near = (self.distance * 0.01).max(1e-4);
        let far = self.distance + self.fit * 4.0;
        Mat4::perspective(FOV, aspect, near, far)
    }

    /// Fly the orbit target: `fwd` along the view direction, `right` sideways, `up` along world
    /// up. Units are camera distances, so the speed follows the zoom.
    fn fly(&mut self, fwd: f32, right: f32, up: f32) {
        let f = math::normalize(math::sub(self.target, self.eye()));
        let r = math::normalize(math::cross(f, [0.0, 1.0, 0.0]));
        let d = math::add(math::add(math::scale(f, fwd), math::scale(r, right)), [0.0, up, 0.0]);
        self.target = math::add(self.target, math::scale(d, self.distance));
    }

    /// Centre on one object, framing its bounds.
    fn focus(&mut self, o: &SceneObject) {
        self.target = o.center;
        self.distance = o.radius / (FOV / 2.0).tan() * 1.2;
    }

    fn pan(&mut self, dx: f32, dy: f32, view_h: f32) {
        let eye = self.eye();
        let f = math::normalize(math::sub(self.target, eye));
        let right = math::normalize(math::cross(f, [0.0, 1.0, 0.0]));
        let up = math::cross(right, f);
        let k = 2.0 * self.distance * (FOV / 2.0).tan() / view_h.max(1.0);
        self.target = math::add(self.target, math::add(math::scale(right, -dx * k), math::scale(up, dy * k)));
    }
}

const FOV: f32 = 0.8;

#[derive(Default)]
pub struct StartOptions {
    pub path: Option<PathBuf>,
    pub select: Option<String>,
    pub screenshot: Option<PathBuf>,
    /// yaw degrees, pitch degrees, zoom factor (1 = fit, 2 = twice as close).
    pub camera: Option<[f32; 3]>,
    /// SPX: centre on object N of the object list (after `camera`'s angles).
    pub focus: Option<usize>,
    pub no_blend: bool,
    pub no_colorkey: bool,
    /// Initial gate mode.
    pub gates: Option<GateMode>,
    pub mute: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Pak,
    Ocf,
    Spx,
}

pub struct ViewerApp {
    source: Option<AssetSource>,
    path_input: String,
    entries: Vec<(String, Kind)>,
    filter: String,
    show_pak: bool,
    show_ocf: bool,
    show_spx: bool,
    /// Draw sector backdrops.
    backdrop: bool,
    /// Light sectors with their BDF sun / ambient instead of the headlight.
    sector_lighting: bool,
    scene_light: Option<SectorLight>,
    /// BDF fog, and whether to draw it.
    fog: Option<SectorFog>,
    show_fog: bool,
    /// Hyper gates in the scene, and what they're doing.
    gates: usize,
    gate_mode: GateMode,
    sound: GateAudio,
    lightning: Option<Lightning>,
    sheets: FogSheets,
    /// Camera eye and axes (forward, right, up) at the last frame, for the fog sheets.
    prev_cam: Option<[[f32; 3]; 4]>,
    has_backdrop: bool,
    /// SPX: also draw the parent sector's objects for a sub-sector stage.
    include_parent: bool,
    parent: Option<String>,
    objects: Vec<SceneObject>,
    selected: Option<String>,
    lod: usize,
    lod_count: usize,
    state: usize,
    state_names: Vec<String>,
    animator: Option<Animator>,
    info: Vec<String>,
    warnings: Vec<String>,
    tri_count: usize,
    error: Option<String>,
    camera: Camera,
    renderer: Option<Arc<Mutex<Renderer>>>,
    lighting: bool,
    wireframe: bool,
    textures: bool,
    blending: bool,
    color_key: bool,
    scroll_to_selected: bool,
    bbox: ([f32; 3], [f32; 3]),
    animated: bool,
    screenshot: Option<PathBuf>,
    frames: u32,
}

impl ViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: StartOptions) -> Self {
        let path = opts.path;
        let mut error = None;
        let renderer = match cc.gl.as_ref().map(|gl| Renderer::new(gl)) {
            Some(Ok(r)) => Some(Arc::new(Mutex::new(r))),
            Some(Err(e)) => {
                error = Some(format!("shader setup failed: {e}"));
                None
            }
            None => {
                error = Some("no OpenGL context".into());
                None
            }
        };
        let mut app = Self {
            source: None,
            path_input: path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            entries: vec![],
            filter: String::new(),
            show_pak: true,
            show_ocf: true,
            show_spx: true,
            backdrop: true,
            sector_lighting: true,
            scene_light: None,
            fog: None,
            show_fog: true,
            gates: 0,
            gate_mode: opts.gates.unwrap_or(GateMode::Open),
            sound: {
                let on = !opts.mute && opts.screenshot.is_none();
                GateAudio { on, music_on: on, ..Default::default() }
            },
            lightning: None,
            sheets: FogSheets::default(),
            prev_cam: None,
            has_backdrop: false,
            include_parent: true,
            parent: None,
            objects: vec![],
            selected: None,
            lod: 0,
            lod_count: 0,
            state: 0,
            state_names: vec![],
            animator: None,
            info: vec![],
            warnings: vec![],
            tri_count: 0,
            error,
            camera: Camera::new(),
            renderer,
            lighting: true,
            wireframe: false,
            textures: true,
            blending: !opts.no_blend,
            color_key: !opts.no_colorkey,
            scroll_to_selected: false,
            bbox: ([-1.0; 3], [1.0; 3]),
            animated: false,
            screenshot: opts.screenshot,
            frames: 0,
        };
        if path.is_some() {
            app.open_source();
        }
        if let Some(sel) = opts.select {
            // Match case-insensitively against the listing.
            let name = app
                .entries
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(&sel))
                .map(|(n, _)| n.clone())
                .unwrap_or(sel);
            app.load(&name, 0, true);
            app.scroll_to_selected = true;
        }
        if let Some([yaw, pitch, zoom]) = opts.camera {
            app.camera.yaw = yaw.to_radians();
            app.camera.pitch = pitch.to_radians().clamp(-1.55, 1.55);
            app.camera.distance = app.camera.fit / zoom.max(0.01);
        }
        if let Some(o) = opts.focus.and_then(|i| app.objects.get(i).cloned()) {
            let zoom = opts.camera.map(|c| c[2]).unwrap_or(1.0);
            app.camera.focus(&o);
            app.camera.distance /= zoom.max(0.01);
        }
        app
    }

    fn open_source(&mut self) {
        let path = PathBuf::from(self.path_input.trim());
        match AssetSource::open(&path) {
            Ok(src) => {
                let mut entries: Vec<(String, Kind)> = src
                    .list(&[".PAK", ".OCF", ".SPX"])
                    .into_iter()
                    .map(|n| {
                        let u = n.to_uppercase();
                        let k = if u.ends_with(".OCF") {
                            Kind::Ocf
                        } else if u.ends_with(".SPX") {
                            Kind::Spx
                        } else {
                            Kind::Pak
                        };
                        (n, k)
                    })
                    .collect();
                entries.sort_by_key(|(n, _)| n.to_uppercase());
                self.entries = entries;
                self.source = Some(src);
                self.error = None;
                self.selected = None;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
    }

    fn load(&mut self, name: &str, lod: usize, reframe: bool) {
        let Some(src) = &self.source else { return };
        // A new model starts in state 0; LOD changes keep the state.
        if reframe {
            self.state = 0;
        }
        let upper = name.to_uppercase();
        let result = if upper.ends_with(".OCF") {
            model::load_ocf_scene(src, name)
        } else if upper.ends_with(".SPX") {
            model::load_spx_scene(src, name, self.include_parent)
        } else {
            model::load_pak_scene(src, name, lod, self.state)
        };
        match result {
            Ok(scene) => self.show(scene, lod, reframe),
            Err(e) => {
                self.error = Some(format!("{name}: {e:#}"));
                self.info.clear();
                self.warnings.clear();
            }
        }
        self.selected = Some(name.to_string());
    }

    fn show(&mut self, mut scene: Scene, lod: usize, reframe: bool) {
        self.bbox = (scene.bbox_min, scene.bbox_max);
        self.animated = scene.animated();
        self.animator = scene.animator.take();
        self.state_names = std::mem::take(&mut scene.state_names);
        self.objects = std::mem::take(&mut scene.objects);
        self.has_backdrop = scene.has_backdrop();
        self.scene_light = scene.light;
        self.fog = scene.fog;
        self.lightning = scene.light.and_then(|l| l.lightning).map(Lightning::new);
        self.sheets = FogSheets::default();
        self.prev_cam = None;
        self.gates = scene.gates;
        self.sound.set_scene(std::mem::take(&mut scene.gate_sounds));
        // Flares flicker and cycling gates move.
        self.animated |= self.lightning.is_some() || self.fog.is_some() || self.gates > 0;
        self.parent = scene.parent.take();
        if reframe {
            self.camera.frame(scene.bbox_min, scene.bbox_max);
        }
        self.lod = lod;
        self.lod_count = scene.lod_count;
        self.info = scene.info.clone();
        self.warnings = scene.warnings.clone();
        self.tri_count = scene.triangle_count();
        let size = math::sub(scene.bbox_max, scene.bbox_min);
        self.info.push(format!(
            "{} triangles  {} textures  size {:.2} x {:.2} x {:.2}",
            self.tri_count,
            scene.images.len(),
            size[0],
            size[1],
            size[2]
        ));
        self.error = None;
        if let Some(r) = &self.renderer {
            r.lock().unwrap().set_scene(scene);
        }
    }

    fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_uppercase();
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, (n, k))| {
                (match k {
                    Kind::Pak => self.show_pak,
                    Kind::Ocf => self.show_ocf,
                    Kind::Spx => self.show_spx,
                }) && (f.is_empty() || n.to_uppercase().contains(&f))
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The Music checkbox, the theme picker and what's playing.
    fn music_controls(&mut self, ui: &mut egui::Ui) {
        let snd = &mut self.sound;
        let now = snd.now_playing();
        ui.horizontal(|ui| {
            let tip = match &snd.music_error {
                Some(e) => format!("Music unavailable: {e}"),
                None => "In-flight music: Ttfmusic.sbf segments sequenced by the game's TTFMUSIC.BIN \
                         script, with every game variable (hostiles, sector, job...) zero. The chosen \
                         theme plays; when the script ends it, the script runs on in the terminal \
                         states for a while, then the theme starts again."
                    .into(),
            };
            ui.checkbox(&mut snd.music_on, "Music").on_hover_text(tip);
            let home = snd.music_state.unwrap_or(music::THEMES[0]);
            let label = |s: usize| {
                snd.music_labels.iter().find(|l| l.0 == s).map_or_else(|| format!("state {s}"), |l| l.1.clone())
            };
            let mut pick = home;
            // Until the music has loaded once, offer the themes by number.
            let entries: Vec<(usize, String)> = if snd.music_labels.is_empty() {
                music::THEMES.iter().map(|&s| (s, format!("state {s}"))).collect()
            } else {
                snd.music_labels.clone()
            };
            egui::ComboBox::from_id_salt("music_state")
                .selected_text(label(home))
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for (s, l) in &entries {
                        ui.selectable_value(&mut pick, *s, l);
                    }
                })
                .response
                .on_hover_text("Script state to play, named by its first segment");
            if pick != home {
                snd.music_state = Some(pick);
                if let Some(m) = &snd.music {
                    if let Ok(mut n) = m.lock() {
                        n.select = Some(pick);
                    }
                }
            }
        });
        if let Some(n) = now {
            let seg = if n.segment.is_empty() { "(silence)" } else { n.segment.as_str() };
            let mut text = format!("state {} · {seg}", n.state);
            if n.terminal_secs > 0.0 {
                text += &format!(" · theme {} again in {:.0} s", n.home, (music::TERMINAL_SECONDS - n.terminal_secs).max(0.0));
            }
            ui.small(text);
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            // Right-to-left so the button is placed first and the text box takes exactly the
            // remaining width. Sizing the box from available_width() minus a guess overflowed by
            // a few pixels, and the resizable panel grew to fit on every repaint.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let open = ui.button("Open").clicked();
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.path_input)
                        .hint_text("pff directory or Tachyon.pff")
                        .desired_width(f32::INFINITY),
                );
                let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if open || enter {
                    self.open_source();
                }
            });
        });
        if let Some(src) = &self.source {
            ui.small(format!(
                "{} ({}), {} models",
                if src.is_archive() { "archive" } else { "directory" },
                src.path.display(),
                self.entries.len()
            ));
            self.music_controls(ui);
        }
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Filter");
            ui.add(egui::TextEdit::singleline(&mut self.filter).desired_width(f32::INFINITY));
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.show_pak, "PAK");
            ui.checkbox(&mut self.show_ocf, "OCF");
            ui.checkbox(&mut self.show_spx, "SPX");
        });
        ui.separator();

        let visible = self.visible();
        // Up/down arrow keys step through the list (when no text field has focus).
        let step = ui.ctx().input(|i| {
            if i.key_pressed(egui::Key::ArrowDown) {
                1
            } else if i.key_pressed(egui::Key::ArrowUp) {
                -1
            } else {
                0
            }
        });
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if step != 0 && !typing && !visible.is_empty() {
            let cur = self
                .selected
                .as_ref()
                .and_then(|s| visible.iter().position(|&i| &self.entries[i].0 == s));
            let next = match cur {
                Some(c) => (c as i64 + step).clamp(0, visible.len() as i64 - 1) as usize,
                None => 0,
            };
            let name = self.entries[visible[next]].0.clone();
            self.load(&name, 0, true);
            self.scroll_to_selected = true;
        }

        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 4.0;
        let mut clicked = None;
        let scroll = std::mem::take(&mut self.scroll_to_selected);
        let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
        if scroll {
            // show_rows only lays out visible rows, so scroll by computed offset instead.
            if let Some(pos) = self
                .selected
                .as_ref()
                .and_then(|s| visible.iter().position(|&i| &self.entries[i].0 == s))
            {
                let pitch = row_h + ui.spacing().item_spacing.y;
                let view_h = ui.available_height();
                area = area.vertical_scroll_offset((pos as f32 * pitch - view_h / 2.0).max(0.0));
            }
        }
        area.show_rows(ui, row_h, visible.len(), |ui, range| {
            for &i in &visible[range] {
                let (name, kind) = &self.entries[i];
                let sel = self.selected.as_deref() == Some(name.as_str());
                let label = match kind {
                    Kind::Pak => egui::RichText::new(name),
                    Kind::Ocf => egui::RichText::new(name).color(ui.visuals().warn_fg_color),
                    Kind::Spx => egui::RichText::new(name).color(egui::Color32::from_rgb(110, 180, 255)),
                };
                let r = ui.selectable_label(sel, label);
                if r.clicked() {
                    clicked = Some(name.clone());
                }
            }
        });
        if let Some(n) = clicked {
            self.load(&n, 0, true);
        }
    }

    fn view(&mut self, ui: &mut egui::Ui) {
        // Header line: info + options.
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.textures, "Textures");
            ui.checkbox(&mut self.blending, "Blending")
                .on_hover_text("Per-material blend modes: additive glows, shadow cards, cutouts");
            ui.checkbox(&mut self.color_key, "Colour key")
                .on_hover_text("Treat palette index 0 as transparent");
            ui.checkbox(&mut self.lighting, "Lighting");
            ui.checkbox(&mut self.wireframe, "Wireframe");
            if self.lod_count > 1 {
                let mut lod = self.lod;
                egui::ComboBox::from_label("LOD")
                    .selected_text(lod.to_string())
                    .show_ui(ui, |ui| {
                        for l in 0..self.lod_count {
                            ui.selectable_value(&mut lod, l, l.to_string());
                        }
                    });
                if lod != self.lod {
                    if let Some(n) = self.selected.clone() {
                        self.load(&n, lod, false);
                    }
                }
            }
            if self.state_names.len() > 1 {
                let mut st = self.state;
                egui::ComboBox::from_label("State")
                    .selected_text(self.state_names[st.min(self.state_names.len() - 1)].clone())
                    .show_ui(ui, |ui| {
                        for (i, n) in self.state_names.iter().enumerate() {
                            ui.selectable_value(&mut st, i, n);
                        }
                    });
                if st != self.state {
                    self.state = st;
                    if let Some(n) = self.selected.clone() {
                        self.load(&n, self.lod, false);
                    }
                }
            }
            if self.scene_light.is_some() {
                ui.checkbox(&mut self.sector_lighting, "Sector light")
                    .on_hover_text("Light from the BDF's sun (or lightning), ambient and colours, as the game does");
            }
            if self.has_backdrop {
                ui.checkbox(&mut self.backdrop, "Backdrop").on_hover_text("The sector's .BDF sky, planets and suns");
            }
            if self.gates > 0 {
                egui::ComboBox::from_id_salt("gates")
                    .selected_text(match self.gate_mode {
                        GateMode::Open => "Gates open",
                        GateMode::Closed => "Gates closed",
                        GateMode::Cycle => "Gates cycling",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.gate_mode, GateMode::Open, "Open");
                        ui.selectable_value(&mut self.gate_mode, GateMode::Closed, "Closed");
                        ui.selectable_value(&mut self.gate_mode, GateMode::Cycle, "Cycle (open / close animations)");
                    })
                    .response
                    .on_hover_text(
                        "Hyper gates' .anm poses. In the game a mission variable decides open or closed; \
                         most gates start open",
                    );
            }
            if !self.sound.gates.is_empty() {
                ui.checkbox(&mut self.sound.on, "Sound").on_hover_text(
                    "Gate sounds from SPACE.PWF: each gate's ambient loop, plus the open / close \
                     animation sounds while cycling",
                );
            }
            if self.fog.is_some() {
                ui.checkbox(&mut self.show_fog, "Fog")
                    .on_hover_text("The BDF's [fog]: depth fade to the fog colour, plus drifting fog sheets");
            }
            if let Some(p) = self.parent.clone() {
                if ui
                    .checkbox(&mut self.include_parent, format!("Include {p}"))
                    .on_hover_text("Also draw the main sector's objects; the game loads both")
                    .changed()
                {
                    if let Some(n) = self.selected.clone() {
                        self.load(&n, 0, false);
                    }
                }
            }
            if ui.button("Reset view").clicked() {
                self.reset_view();
            }
        });
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        if let Some(n) = &self.selected {
            ui.strong(n);
        }
        for l in &self.info {
            ui.label(l);
        }
        if !self.objects.is_empty() {
            let mut focus = None;
            egui::CollapsingHeader::new(format!("{} objects (click to centre)", self.objects.len()))
                .id_salt("objects")
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().id_salt("objlist").max_height(160.0).show(ui, |ui| {
                        for (i, o) in self.objects.iter().enumerate() {
                            if ui.small_button(&o.label).clicked() {
                                focus = Some(i);
                            }
                        }
                    });
                });
            if let Some(i) = focus {
                let o = self.objects[i].clone();
                self.camera.focus(&o);
            }
        }
        if !self.warnings.is_empty() {
            egui::CollapsingHeader::new(format!("{} warnings", self.warnings.len()))
                .id_salt("warnings")
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                        for w in &self.warnings {
                            ui.small(w);
                        }
                    });
                });
        }

        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let pointer_on_view = resp.hovered();
        if resp.dragged_by(egui::PointerButton::Primary) {
            let d = resp.drag_delta();
            self.camera.yaw -= d.x * 0.01;
            self.camera.pitch = (self.camera.pitch + d.y * 0.01).clamp(-1.55, 1.55);
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            self.camera.pan(d.x, d.y, rect.height());
        }
        if resp.double_clicked() {
            self.reset_view();
        }
        if pointer_on_view {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            let zoom = ui.input(|i| i.zoom_delta());
            if scroll != 0.0 {
                self.camera.distance *= (-scroll * 0.002).exp();
            }
            if zoom != 1.0 {
                self.camera.distance /= zoom;
            }
            let fit = self.camera.fit;
            self.camera.distance = self.camera.distance.clamp(fit * 1e-5, fit * 20.0);
        }
        // Keyboard flying (when no text box has focus): W/S forward/back along the view, A/D
        // sideways, Q/E down/up. Speed is one camera distance per second (Shift: x4), so it
        // follows the zoom.
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        if !typing {
            let (fwd, right, up, fast, dt) = ui.input(|i| {
                let k = |a: egui::Key, b: egui::Key| (i.key_down(a) as i32 - i.key_down(b) as i32) as f32;
                (
                    k(egui::Key::W, egui::Key::S),
                    k(egui::Key::D, egui::Key::A),
                    k(egui::Key::E, egui::Key::Q),
                    i.modifiers.shift,
                    i.stable_dt.min(0.1),
                )
            });
            if fwd != 0.0 || right != 0.0 || up != 0.0 {
                let s = dt * if fast { 4.0 } else { 1.0 };
                self.camera.fly(fwd * s, right * s, up * s);
                ui.ctx().request_repaint();
            }
        }

        let Some(renderer) = self.renderer.clone() else { return };
        // TAV_TIME pins the animation clock (seconds) for reproducible screenshots.
        let time = std::env::var("TAV_TIME")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or_else(|| ui.input(|i| i.time) as f32);
        if let Some(src) = &self.source {
            self.sound.update_music(src);
            self.sound.update(src, time, self.gate_mode, self.camera.eye(), self.camera.target);
        }
        let flash = self.lightning.as_mut().map(|l| {
            l.advance_to(time);
            Flash { diffuse_scale: l.diffuse_scale(), bolt: l.bolt() }
        });
        let fog = match (self.fog, self.show_fog) {
            (Some(f), true) => {
                // Feed the camera's motion to the fog sheets, as the game does each frame.
                let eye = self.camera.eye();
                let fwd = math::normalize(math::sub(self.camera.target, eye));
                let right = math::normalize(math::cross(fwd, [0.0, 1.0, 0.0]));
                let up = math::cross(right, fwd);
                if let Some([pe, pf, pr, pu]) = self.prev_cam {
                    let d = math::sub(eye, pe);
                    self.sheets.translate(math::dot(d, pr), math::dot(d, pu), math::dot(d, pf));
                    let turn = std::f32::consts::TAU;
                    self.sheets.rotate(math::dot(fwd, pr).asin() / turn, math::dot(fwd, pu).asin() / turn, 0.0);
                }
                self.prev_cam = Some([eye, fwd, right, up]);
                Some(FogDraw { fog: f, layers: self.sheets.layers(), rot: self.sheets.rot() })
            }
            _ => None,
        };
        let background = fog.as_ref().map_or([0.06, 0.06, 0.09], |f| f.fog.color);
        let params = DrawParams {
            view: self.camera.view(),
            proj: self.camera.proj(rect.width() / rect.height().max(1.0)),
            proj_backdrop: self.camera.proj_backdrop(rect.width() / rect.height().max(1.0)),
            backdrop: self.backdrop && self.has_backdrop,
            time,
            parts: Arc::new(self.animator.as_ref().map(|a| a.frame(time, self.gate_mode)).unwrap_or_default()),
            sprites: Arc::new(self.animator.as_ref().map(|a| a.sprites(time, self.gate_mode)).unwrap_or_default()),
            light: math::sub(self.camera.eye(), self.camera.target),
            sector_light: if self.sector_lighting { self.scene_light } else { None },
            eye: self.camera.eye(),
            lighting: self.lighting,
            wireframe: self.wireframe,
            textures: self.textures,
            blending: self.blending,
            color_key: self.color_key,
            background,
            flash,
            fog,
        };
        let cb = egui_glow::CallbackFn::new(move |_info, painter| {
            renderer.lock().unwrap().paint(painter.gl(), &params);
        });
        ui.painter_at(rect).add(egui::PaintCallback { rect, callback: Arc::new(cb) });
        ui.painter().text(
            rect.left_bottom() + egui::vec2(8.0, -8.0),
            egui::Align2::LEFT_BOTTOM,
            "drag: orbit   right/middle drag: pan   wheel: zoom   W/S A/D Q/E: fly (shift: faster)   double-click: reset   up/down: next model",
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(140),
        );
    }

    fn reset_view(&mut self) {
        self.camera = Camera::new();
        self.camera.frame(self.bbox.0, self.bbox.1);
    }
}

impl eframe::App for ViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::SidePanel::left("assets")
            .resizable(true)
            .default_width(280.0)
            .show(ctx, |ui| self.side_panel(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.view(ui));
        if self.animated {
            ctx.request_repaint();
        }
        if let Some(out) = self.screenshot.clone() {
            self.frames += 1;
            let shot_frame = std::env::var("TAV_SHOT_FRAME").ok().and_then(|s| s.parse().ok()).unwrap_or(5);
            if self.frames == shot_frame {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }
            let shot = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    egui::Event::Screenshot { image, .. } => Some(image.clone()),
                    _ => None,
                })
            });
            if let Some(img) = shot {
                let mut ppm = format!("P6\n{} {}\n255\n", img.size[0], img.size[1]).into_bytes();
                for p in &img.pixels {
                    ppm.extend_from_slice(&[p.r(), p.g(), p.b()]);
                }
                if let Err(e) = std::fs::write(&out, ppm) {
                    eprintln!("screenshot: {e}");
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        }
    }
}

/// Gate sounds: a 2D mix of each gate's ambient loop and its keyframe sounds, from the camera.
#[derive(Default)]
struct GateAudio {
    on: bool,
    gates: Vec<model::GateSound>,
    /// Opened on first use; `None` inside once that failed.
    audio: Option<Option<Audio>>,
    bank: Option<Option<Bank>>,
    samples: HashMap<String, Option<Arc<Sound>>>,
    last: Option<f32>,
    playing: bool,
    /// Background music, and what it's playing.
    music_on: bool,
    music: Option<Arc<Mutex<music::NowPlaying>>>,
    music_error: Option<String>,
    /// The theme picked in the UI (`None`: the first of `music::THEMES`).
    music_state: Option<usize>,
    /// The picker's entries, filled when the music first starts.
    music_labels: Vec<(usize, String)>,
}

impl GateAudio {
    fn audio(&mut self) -> Option<&Audio> {
        self.audio.get_or_insert_with(Audio::new).as_ref()
    }

    /// Start or stop the music to match the checkbox.
    fn update_music(&mut self, src: &AssetSource) {
        match (self.music_on, self.music.is_some()) {
            (true, false) if self.music_error.is_none() => {
                let player = (|| -> anyhow::Result<music::Player> {
                    let path = music::Sbf::find(&src.path).ok_or_else(|| {
                        anyhow::anyhow!("Ttfmusic.sbf not found (it's in the game folder, beside Tachyon.pff)")
                    })?;
                    let sbf = Arc::new(music::Sbf::open(&path)?);
                    let script = Arc::new(music::Script::parse(src.read("TTFMUSIC.BIN")?)?);
                    let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u32);
                    Ok(music::Player::new(sbf, script, seed, self.music_state.unwrap_or(music::THEMES[0])))
                })();
                match (player, self.audio().is_some()) {
                    (Ok(p), true) => {
                        if self.music_labels.is_empty() {
                            self.music_labels = p.state_labels();
                        }
                        self.music = Some(p.now.clone());
                        if let Some(a) = self.audio() {
                            a.set_music(Some(Box::new(p)));
                        }
                    }
                    (Ok(_), false) => self.music_error = Some("no audio output".into()),
                    (Err(e), _) => self.music_error = Some(format!("{e:#}")),
                }
            }
            (false, true) => {
                if let Some(Some(a)) = &self.audio {
                    a.set_music(None);
                }
                self.music = None;
            }
            _ => {}
        }
    }

    fn now_playing(&self) -> Option<music::NowPlaying> {
        self.music.as_ref().and_then(|m| m.lock().ok().map(|n| n.clone()))
    }

    fn set_scene(&mut self, gates: Vec<model::GateSound>) {
        self.gates = gates;
        self.last = None;
        if let Some(Some(a)) = &self.audio {
            a.stop_one_shots();
            a.set_loops(&[]);
        }
    }

    fn sample(&mut self, src: &AssetSource, name: &str) -> Option<Arc<Sound>> {
        let bank = self.bank.get_or_insert_with(|| src.read("SPACE.PWF").ok().and_then(|d| Bank::parse(d).ok()));
        let bank = bank.as_ref()?;
        self.samples.entry(name.to_uppercase()).or_insert_with(|| bank.sound(name).map(Arc::new)).clone()
    }

    fn update(&mut self, src: &AssetSource, t: f32, mode: GateMode, eye: [f32; 3], target: [f32; 3]) {
        if !self.on || self.gates.is_empty() {
            if self.playing {
                if let Some(Some(a)) = &self.audio {
                    a.stop_one_shots();
                    a.set_loops(&[]);
                }
                self.playing = false;
            }
            self.last = None;
            return;
        }
        if self.audio().is_none() {
            return;
        }
        self.playing = true;
        let fwd = math::normalize(math::sub(target, eye));
        let right = math::normalize(math::cross(fwd, [0.0, 1.0, 0.0]));
        // A paused or jumping clock (TAV_TIME, a long stall) fires nothing.
        let prev = self.last.replace(t).filter(|&p| t > p && t - p < 0.5);
        let mut loops = vec![];
        let mut shots = vec![];
        for (i, g) in self.gates.clone().iter().enumerate() {
            let d = math::sub(g.pos, eye);
            let dist = math::dot(d, d).sqrt();
            let pan = if dist > 1e-3 { math::dot(d, right) / dist } else { 0.0 };
            if let Some(r) = &g.ring {
                if dist < r.max {
                    if let Some(s) = self.sample(src, &r.sample) {
                        let vol = (r.min / dist.max(1e-3)).min(1.0);
                        loops.push((i as u64, s, vol, pan, r.pitch));
                    }
                }
            }
            if let (GateMode::Cycle, Some(p)) = (mode, prev) {
                // Keyframe sounds: 2D, 255 − distance/2 out of 255.
                let vol = ((255.0 - dist / 2.0) / 255.0).max(0.0);
                let (o0, c0) = gate::cycle_ticks(p, g.cycle);
                let (o1, c1) = gate::cycle_ticks(t, g.cycle);
                let hit = |a: f32, b: f32, at: f32| if a <= b { at > a && at <= b } else { at > a || at <= b };
                for (at, name) in &g.open {
                    if hit(o0, o1, *at) {
                        shots.push((name.clone(), vol, pan));
                    }
                }
                for (at, name) in &g.close {
                    if c1 >= 0.0 && c0 <= c1 && hit(c0, c1, *at) {
                        shots.push((name.clone(), vol, pan));
                    }
                }
            }
        }
        let shots: Vec<_> = shots.into_iter().filter_map(|(n, v, p)| Some((self.sample(src, &n)?, v, p))).collect();
        let Some(Some(a)) = &self.audio else { return };
        a.set_loops(&loops);
        for (s, v, p) in shots {
            if v > 0.0 {
                a.play(s, v, p, 1.0);
            }
        }
    }
}
