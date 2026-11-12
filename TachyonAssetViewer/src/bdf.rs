//! `.BDF` backdrop scenes (CBIN): the sky model and the distant planets, stars and rocks around a
//! sector. `Bdf_ParseScene 0x4679f0`, `Bdf_ParseImageEntry 0x467840`, drawn by `FUN_00468440`.
//!
//! - `SPACE_PAK: pak, pak, pal` is drawn around the camera at scale 2, rotated by
//!   `SPACE_ROT: yaw, pitch` (degrees).
//! - `[images] IMAGE / SUN: pak, pal, yaw, pitch, distance, scale` places a model at
//!   `R(yaw, pitch) · (0, 0, distance)` from the camera, at that uniform scale. `SUN` is always at
//!   distance 1300 (the parser passes 0x5140000).
//! - Both follow the camera (position + camera), so they never get closer.
//! - `[fog]` sectors draw neither (see `Fog`); `LIGHTNING_PAK` flashes over the sky (`weather.rs`).

use crate::cbin::{self, Value};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct BdfImage {
    pub pak: String,
    /// Degrees.
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub scale: f32,
    /// A `SUN` entry (the light source and lens-flare origin).
    pub sun: bool,
}

#[derive(Debug, Clone)]
pub struct Bdf {
    pub space_pak: Option<String>,
    /// Degrees: yaw, pitch.
    pub space_rot: [f32; 2],
    pub images: Vec<BdfImage>,
    /// Palette re-tint for models (`Pal_Remap`): 128 = neutral.
    pub gamma: i32,
    pub saturation: i32,
    /// Sector tint: multiplies the ambient term of the shade ramp.
    pub rgb: [i32; 3],
    /// Light colour (default 255, 255, 255).
    pub light_rgb: [i32; 3],
    pub ambient: i32,
    /// Lightning sectors light from `LIGHTNING_LIGHT_SRC` (yaw, pitch degrees) instead of the
    /// sun, with diffuse scaled by `LIGHTNING_AMBIENT_LIGHT / 512` between flashes.
    pub lightning: bool,
    pub lightning_ambient: i32,
    pub lightning_src: [f32; 2],
    /// `LIGHTNING_PAK`: drawn over the sky during flashes (`FUN_00468440`).
    pub lightning_pak: Option<String>,
    /// `[fog]` group (`Bdf_ParseScene` +0xdc..+0x124).
    pub fog: Option<Fog>,
}

/// A fog sector: no sky or images; the screen clears to `rgb`, geometry fades linearly to `rgb`
/// between `min` and `max` (view depth), and textured fog sheets drift past the camera.
#[derive(Debug, Clone)]
pub struct Fog {
    pub min: f32,
    pub max: f32,
    /// Fog and clear colour (`FUN_004ad0c0` → driver colour slot 3, `FUN_004abd30`).
    pub rgb: [i32; 3],
    /// Fog-sheet tint (`FUN_004b91a0`), default 0x80.
    pub front_rgb: [i32; 3],
    /// Fog-sheet texture (default `fog1_01.pcx`).
    pub pcx: String,
}

impl Default for Bdf {
    fn default() -> Self {
        Bdf {
            space_pak: None,
            space_rot: [0.0; 2],
            images: vec![],
            gamma: 128,
            saturation: 128,
            rgb: [128; 3],
            light_rgb: [255; 3],
            ambient: 0,
            lightning: false,
            lightning_ambient: 0,
            lightning_src: [0.0; 2],
            lightning_pak: None,
            fog: None,
        }
    }
}

impl Bdf {
    /// Engine-axis unit vector towards the light: the `SUN` image's direction
    /// (`FUN_0048a960` → `FUN_004b3a10`), overridden by the lightning source when lightning is on.
    pub fn light_dir(&self) -> [f32; 3] {
        let (yaw, pitch) = if self.lightning {
            (self.lightning_src[0], self.lightning_src[1])
        } else if let Some(s) = self.images.iter().find(|i| i.sun) {
            (s.yaw, s.pitch)
        } else {
            (0.0, 0.0)
        };
        crate::math::Mat3::from_engine_euler_deg([yaw, pitch, 0.0]).mul_vec([0.0, 0.0, 1.0])
    }
}

/// `FUN_00468440` draws the sky model with this scale.
pub const SKY_SCALE: f32 = 2.0;
/// `Bdf_ParseImageEntry` gives the `SUN` entry this fixed distance.
pub const SUN_DISTANCE: f32 = 1300.0;

fn num(v: Option<&Value>) -> f32 {
    v.and_then(|v| v.as_f32()).unwrap_or(0.0)
}

fn text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Str(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

pub fn parse(d: &[u8]) -> Result<Bdf> {
    let groups = cbin::parse(d)?;
    let mut b = Bdf::default();
    let mut fog = Fog { min: 0.0, max: 0.0, rgb: [0; 3], front_rgb: [0x80; 3], pcx: "fog1_01.pcx".into() };
    let mut fog_on = false;
    for g in &groups {
        let gname = g.name.to_uppercase();
        for it in &g.items {
            let key = it.key.to_uppercase();
            let v = &it.values;
            match (gname.as_str(), key.as_str()) {
                ("MAIN", "SPACE_PAK") => b.space_pak = text(v.first()),
                ("MAIN", "SPACE_ROT") => b.space_rot = [num(v.first()), num(v.get(1))],
                ("MAIN", "GAMMA") => b.gamma = num(v.first()) as i32,
                ("MAIN", "SATURATION") => b.saturation = num(v.first()) as i32,
                ("MAIN", "RGB") => b.rgb = [0, 1, 2].map(|i| num(v.get(i)) as i32),
                ("MAIN", "LIGHT_RGB") => b.light_rgb = [0, 1, 2].map(|i| num(v.get(i)) as i32),
                ("MAIN", "AMBIENT_LIGHT") => b.ambient = num(v.first()) as i32,
                ("MAIN", "LIGHTNING") => b.lightning = num(v.first()) != 0.0,
                ("MAIN", "LIGHTNING_AMBIENT_LIGHT") => b.lightning_ambient = num(v.first()) as i32,
                ("MAIN", "LIGHTNING_LIGHT_SRC") => b.lightning_src = [num(v.first()), num(v.get(1))],
                ("MAIN", "LIGHTNING_PAK") => b.lightning_pak = text(v.first()),
                ("FOG", "ENABLED") => fog_on = num(v.first()) != 0.0,
                ("FOG", "MIN") => fog.min = num(v.first()),
                ("FOG", "MAX") => fog.max = num(v.first()),
                ("FOG", "RGB") => fog.rgb = [0, 1, 2].map(|i| num(v.get(i)) as i32),
                ("FOG", "FRONT_RGB") => fog.front_rgb = [0, 1, 2].map(|i| num(v.get(i)) as i32),
                ("FOG", "PCX") => {
                    if let Some(s) = text(v.first()) {
                        fog.pcx = s;
                    }
                }
                ("IMAGES", "IMAGE") | ("IMAGES", "SUN") => {
                    let Some(pak) = text(v.first()) else { continue };
                    let sun = key == "SUN";
                    b.images.push(BdfImage {
                        pak,
                        yaw: num(v.get(2)),
                        pitch: num(v.get(3)),
                        distance: if sun { SUN_DISTANCE } else { num(v.get(4)) },
                        scale: num(v.get(5)),
                        sun,
                    });
                }
                _ => {}
            }
        }
    }
    if fog_on {
        b.fog = Some(fog);
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::AssetSource;

    #[test]
    fn every_bdf_parses() {
        let Ok(src) = AssetSource::open(std::path::Path::new("../../pff")) else { return };
        let mut n = 0;
        for name in src.list(&[".BDF"]) {
            let b = parse(&src.read(&name).unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
            for img in &b.images {
                assert!(src.read(&img.pak).is_ok(), "{name}: {} missing", img.pak);
            }
            n += 1;
        }
        assert!(n > 80, "{n}");
        let m = parse(&src.read("mnu.bdf").unwrap()).unwrap();
        assert_eq!(m.space_pak.as_deref(), Some("frontneb.pak"));
        assert_eq!(m.space_rot, [98.0, 342.0]);
        assert_eq!(m.images.len(), 5);
        assert!(m.images[4].sun && m.images[4].distance == SUN_DISTANCE);
        let f = parse(&src.read("BORA6.BDF").unwrap()).unwrap().fog.unwrap();
        assert_eq!((f.min, f.max, f.rgb, f.pcx.as_str()), (1000.0, 5000.0, [180, 60, 20], "foglorng.pcx"));
        let z = parse(&src.read("BORA5.BDF").unwrap()).unwrap();
        assert!(z.fog.is_none() && z.lightning_pak.as_deref() == Some("gold4zap.pak"));
    }
}
