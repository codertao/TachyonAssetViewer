//! Sector palette re-tint, as `Pal_Remap 0x4ad900` applies a BDF's `GAMMA` / `SATURATION` to
//! every remappable model palette (`decomp/notes/formats.md` "Per-sector palette re-tint").
//!
//! Per unprotected palette entry: `luma = (r + 2g + b) / 4`, `c = luma + (c − luma)·sat/128`
//! (`Pal_SaturateColor`), then the gamma curve (`Pal_BuildCurve`). Protected entries (listed as
//! the pixels of the model's `.pal`, a 16×16 PCX; in practice index 0 plus the top ~20 glow colours)
//! are left alone. The RGB multipliers are neutral (0x80) for models: the sector `RGB` tints the
//! lighting instead.

use crate::pak;

/// Signed midtone-bend table at `0x55ea48` in space.exe.
const GAMMA_TABLE: [i8; 256] = [
    0, 15, 21, 25, 28, 31, 33, 35, 37, 39, 40, 42, 43, 45, 46, 47, 48, 49, 50, 51, 51, 52, 53, 54, 54, 55, 55, 56, 56, 57, 57, 58,
    58, 59, 59, 59, 60, 60, 60, 61, 61, 61, 61, 62, 62, 62, 62, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63, 64, 64, 64, 64, 64, 64, 64,
    64, 64, 64, 64, 64, 64, 64, 64, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 62, 62, 62, 62, 62, 62, 62, 61, 61, 61, 61, 61, 61,
    60, 60, 60, 60, 60, 59, 59, 59, 59, 59, 58, 58, 58, 58, 57, 57, 57, 57, 56, 56, 56, 56, 55, 55, 55, 55, 54, 54, 54, 54, 53, 53,
    53, 52, 52, 52, 51, 51, 51, 51, 50, 50, 50, 49, 49, 49, 48, 48, 48, 47, 47, 47, 46, 46, 46, 45, 45, 45, 44, 44, 43, 43, 43, 42,
    42, 42, 41, 41, 40, 40, 40, 39, 39, 39, 38, 38, 37, 37, 37, 36, 36, 35, 35, 35, 34, 34, 33, 33, 33, 32, 32, 31, 31, 31, 30, 30,
    29, 29, 28, 28, 28, 27, 27, 26, 26, 25, 25, 25, 24, 24, 23, 23, 22, 22, 21, 21, 21, 20, 20, 19, 19, 18, 18, 17, 17, 16, 16, 15,
    15, 15, 14, 14, 13, 13, 12, 12, 11, 11, 10, 10, 9, 9, 8, 8, 7, 7, 6, 6, 5, 5, 4, 4, 3, 3, 2, 2, 1, 1, 0, 0,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Remap {
    curve: [u8; 256],
    sat: i32,
}

impl Remap {
    pub fn new(gamma: i32, sat: i32) -> Remap {
        let g = gamma - 0x80;
        let mut curve = [0u8; 256];
        for (i, c) in curve.iter_mut().enumerate() {
            let t = GAMMA_TABLE[i] as i32;
            let v = if g >= 0 {
                ((t * g * 2) >> 8) + i as i32
            } else {
                let k = (-g * 2).min(0xff);
                (i as i32 - ((t * k) >> 8)).max(0)
            };
            *c = v.min(0xff) as u8;
        }
        Remap { curve, sat }
    }

    pub fn is_identity(&self) -> bool {
        self.sat == 0x80 && self.curve.iter().enumerate().all(|(i, &c)| c as usize == i)
    }

    pub fn apply(&self, c: [u8; 3]) -> [u8; 3] {
        let (r, g, b) = (c[0] as i32, c[1] as i32, c[2] as i32);
        let luma = (r + 2 * g + b) >> 2;
        let s = |x: i32| (((x - luma) * self.sat >> 7) + luma).clamp(0, 255) as usize;
        [self.curve[s(r)], self.curve[s(g)], self.curve[s(b)]]
    }
}

/// Protected palette indices from a `.pal` file: the pixel values of its PCX image.
pub fn protect_mask(pal_file: &[u8]) -> Option<[bool; 256]> {
    if pal_file.len() < 128 + 769 || pal_file[0] != 0x0A {
        return None;
    }
    let u16_ = |o: usize| u16::from_le_bytes([pal_file[o], pal_file[o + 1]]) as usize;
    let h = u16_(10).checked_sub(u16_(6))? + 1;
    let bpl = u16_(0x42);
    let (pixels, _) = pak::rle_decode(&pal_file[128..pal_file.len() - 769], bpl * h).ok()?;
    let mut mask = [false; 256];
    for p in pixels {
        mask[p as usize] = true;
    }
    Some(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_is_identity_and_saturation_desaturates() {
        assert!(Remap::new(128, 128).is_identity());
        let grey = Remap::new(128, 0).apply([200, 100, 0]);
        assert_eq!(grey, [100, 100, 100]);
        let bright = Remap::new(160, 128).apply([64, 64, 64]);
        assert!(bright[0] > 64);
    }

    #[test]
    fn bora_pal_protects_glow_range() {
        let Ok(d) = std::fs::read("../pff/BORA.PAL") else { return };
        let m = protect_mask(&d).unwrap();
        assert!(m[0] && m[236] && m[255] && !m[100]);
        assert_eq!(m.iter().filter(|&&x| x).count(), 21);
    }
}
