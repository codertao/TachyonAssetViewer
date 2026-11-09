//! Tachyon: The Fringe asset viewer.
//!
//! Usage: tachyon-asset-viewer [PFF_DIRECTORY | Tachyon.pff] [--select NAME]
//!        [--camera YAW,PITCH,ZOOM] [--focus N] [--gates open|closed|cycle] [--no-blend] [--no-colorkey] [--screenshot OUT.ppm]
//!
//! `--select` opens a model on startup; `--camera` sets the view (degrees, degrees, zoom
//! factor, 1 = fit); `--screenshot` saves the window after it has rendered
//! and exits (for scripted checks).

mod anim;
mod app;
mod bdf;
mod cbin;
mod gate;
mod math;
mod model;
mod ocf;
mod pak;
mod palette;
mod pff;
mod render;
mod source;
mod spx;
mod weather;

fn main() -> eframe::Result {
    let mut opts = app::StartOptions::default();
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--focus") => opts.focus = args.next().and_then(|s| s.to_string_lossy().parse().ok()),
            Some("--select") => opts.select = args.next().map(|s| s.to_string_lossy().into_owned()),
            Some("--no-blend") => opts.no_blend = true,
            Some("--gates") => {
                opts.gates = match args.next().and_then(|s| s.into_string().ok()).as_deref() {
                    Some("closed") => Some(gate::GateMode::Closed),
                    Some("cycle") => Some(gate::GateMode::Cycle),
                    _ => Some(gate::GateMode::Open),
                }
            }
            Some("--no-colorkey") => opts.no_colorkey = true,
            Some("--camera") => {
                let v: Vec<f32> = args
                    .next()
                    .map(|s| s.to_string_lossy().split(',').filter_map(|x| x.trim().parse().ok()).collect())
                    .unwrap_or_default();
                if v.len() == 3 {
                    opts.camera = Some([v[0], v[1], v[2]]);
                }
            }
            Some("--screenshot") => opts.screenshot = args.next().map(std::path::PathBuf::from),
            _ => opts.path = Some(std::path::PathBuf::from(a)),
        }
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Tachyon Asset Viewer")
            .with_inner_size([1280.0, 800.0]),
        depth_buffer: 24,
        multisampling: 4,
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "Tachyon Asset Viewer",
        options,
        Box::new(move |cc| Ok(Box::new(app::ViewerApp::new(cc, opts)))),
    )
}
