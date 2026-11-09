# Tachyon Asset Viewer

A desktop viewer for the 3D models in _Tachyon: The Fringe_: single `.PAK` models, `.OCF`
composite models (capital ships, stations) and `.SPX` sectors (every object placed in a sector).

```sh
cargo run --release -- ../pff            # extracted asset directory
cargo run --release -- ../Tachyon.pff    # or the archive itself
```

You can also start it with no argument and type a path into the box at the top left.

- **Left:** every `.PAK`, `.OCF` (orange) and `.SPX` (blue). There's a filter box and
  PAK/OCF/SPX toggles, and the up/down arrow keys step through the list.
- **Right:** the model.
  - Drag to orbit, right- or middle-drag to pan, scroll to zoom, double-click to reset.
  - **W/S** move the orbit point forward/back along the view, **A/D** sideways, **Q/E**
    down/up. The speed is one camera distance per second (Shift: ×4), so it scales with zoom.
  - For sectors: an object list (click one to centre on it) and, for a sub-sector stage like
    `BORA05A.SPX`, an **Include BORA05.SPX** checkbox that also draws the main sector's objects.
  - Toggles for textures, lighting, wireframe, blending and colour key.
  - A LOD picker for PAKs with more than one LOD.
  - An info line (parts, triangles, textures, size) and any load warnings.

For scripted checks: `--select NAME` opens a model, `--camera YAW,PITCH,ZOOM` sets the view
(degrees, degrees, 1 = fit), `--focus N` centres on object N of a sector,
`--gates open|closed|cycle` sets the gate mode, and `--screenshot out.ppm` saves the frame and
exits.
`TAV_TIME=1.5` pins the animation clock (seconds) for repeatable screenshots.

## How models are built

- **PAK:** LOD 0 by default. Each part's vertices are relative to the part's position:
  - The root sits at the origin.
  - Every other part hangs off the root, or off its explicit parent, at that part's mesh pivots
    (`points54`). See `decomp/notes/pak.md` "Part placement".
  - Coordinates are converted from file axes `(x, y, z)` to engine axes `(x, z, -y)`. Textures are the PCX images
  embedded in the PAK, decoded with their own palettes.
- **OCF:** a CBIN file listing PAKs and `part_NN` groups. Part placement matches space.exe
  (`Ocf_ParsePart 0x4094a0`, attach resolver `0x4099d0`, child update `0x408ef0`):
  - Each part's orientation is its own `ANGLES`, relative to the ship. It is **not** combined
    with its parent's.
  - Its offset is `parent.offset + M(parent)·parentPak.attach[i] − M(part)·ownPak.attach[j]`,
    for `ATTACH: parent, i, j`.
  - `M = R(ANGLES)·SCALE`, where R is the engine's Euler convention `Ry(a1)·Rx(−a2)·Rz(a3)`
    (from `0x52d9cd` / `0x4b24d0`). A unit test checks it against a transcription of the
    original code.
  - Check across every OCF: 550 of 563 rotated turrets end up facing away from the part they
    mount on.
- **SPX sector** (`src/spx.rs`, layout from `decomp/notes/formats.md` "SPD / .SPX"):
  - Each placement names a SPED alias, which `desnames.cfg` maps to a `.des`. Player-ship
    aliases missing there fall back to `<alias>.des` or `<alias>.PAK`.
  - The `.des` `[main]` gives the model: `OCF_FILE` if present, else `EXTERIOR_PAK`. Hyper gates
    add `CENTER_PAK` and `FLARE_PAK` (see "Hyper gates" below).
  - The model is placed at the record's position (16.16 world units, the same scale as model
    vertices) with its Euler orientation (512 units per turn, same convention as OCF `ANGLES`).
  - Objects with flag `0x1000` aren't spawned at sector load (triggers spawn them later). They're
    drawn anyway and marked in the object list.
  - Null records (decl index −1) are skipped.
  - A sub-sector's parent is the SPX whose stage table names it (`bora05A.spx` → `BORA05.SPX`).
  - **Backdrop** (`src/bdf.rs`): the sector's `.BDF` (or its parent's, for sub-sectors that leave
    it blank). It's drawn as the game's `FUN_00468440` does:
    - the `SPACE_PAK` sky around the camera at scale 2, rotated by `SPACE_ROT`;
    - each `IMAGE` / `SUN` model at `R(yaw, pitch) · (0, 0, distance)` from the camera, at its
      scale (the sun at distance 1300);
    - both with the view's rotation only, so they stay put as you move;
    - the sky first without depth, then the images, then the sector on a fresh depth buffer.
    - The **Backdrop** checkbox turns it off.
  - Across all 302 sectors, 13,817 of 13,849 placements resolve to a model. The rest are a few
    aliases with no `.des` (SOL_PATROL, POWERUP_*, TUNNEL_ROID, BORA_PIRATE_FIGHTER).
- **Handedness:** the engine is left-handed (x right, y up, z forward), so z is flipped for
  OpenGL.

## Transparency

Each textured material gets a blend mode from its flags (`BlendMode::from_flags` in `src/pak.rs`):
- **opaque**;
- **cutout:** flag `0x10`; palette index 0 is a hole;
- **additive:** the `0x2100` / `0x4000` / `0x4100` / `0x6000` combos: glows, thrusters, beacons,
  explosions;
- **shadow:** `0x88`; darkens what's behind;
- untextured faces with flag `0x100` are additive too, as the D3D driver draws them. The black
  arena windows vanish except for their specular glints, which is how the game draws glass.

Flag `0x8000` draws full-bright (headlight mode). Palette index 0 is transparent in cutout, additive and shadow
materials; plain opaque textures keep it solid.

**Two-layer materials:** materials on animation slot 125–127 with 2 frames draw the next material
as a decal over the same faces. This is how stations get lit windows and tinted dome glass. Decals
are self-lit at full brightness, as the loader forces their alpha to 0x7f. Draw order is solid, then shadows, then additive; the blended passes
don't write depth. The **Blending** and **Colour key** checkboxes turn each part off for comparison.

## Sector lighting and tint

With an SPX loaded, **Sector light** (on by default) replaces the viewer's headlight with the
game's lighting (`decomp/notes/formats.md` "Sector lighting"):
- per channel `shade = clamp(ambient·RGB + max(N·L, 0)·LIGHT_RGB, 4, 252)`, times the texel, with
  `ambient = AMBIENT_LIGHT / 2` and `L` = the direction of the BDF's `SUN` image (or
  `LIGHTNING_LIGHT_SRC`; lightning sectors use the dim between-flash level);
- materials with alpha (`+0x0f`) ≠ 0 are self-lit at `min(2·alpha, 255)` and ignore the light:
  glow strips, windows, emitters, two-layer decals;
- materials with `+0x15` set get a grey specular highlight (power `+0x15`, strength `+0x14`) from
  the half-vector `normalize(V + 2L)`;
- `GAMMA` and `SATURATION` remap every model palette (`Pal_Remap`), skipping the indices listed in
  the model's `.pal` (index 0 and the top ~20 glow colours). Backdrop models aren't remapped.

Normals are the per-corner normals from the file. Without a sector (or with **Sector light** off),
models use the headlight; self-lit materials still use their alpha brightness.

**Lightning sectors** run the game's 50 Hz flash simulation: about one flash every 4 seconds,
12–23 ticks of random light levels that then fade back to `LIGHTNING_AMBIENT_LIGHT`. During a
flash, `LIGHTNING_PAK` (BORA5, BORA7, EARTH05) shows one random bolt frame over the sky. Thunder
and the game's flash screen effect aren't reproduced.

**Fog sectors** (BDF `[fog]`; **Fog** checkbox): no sky; the view clears to the fog colour, and
geometry fades linearly to it between `MIN` and `MAX` view depth (additive glows fade out). Up to
five additive fog sheets textured with the BDF's `PCX` and tinted by `FRONT_RGB` sit in front of
the camera. They slide as it turns, show parallax as it strafes, and drift past and recycle as it
flies forward, as in `FUN_004b9250`.

## Animation and billboards

- **Animated textures:** a material with `frames > 1` and slot < 125 cycles through its frame
  materials at `counter % frames`. For each slot, the rate comes from the selected state's counter
  table (frames per second). Slot 0 is driven by the entity, and it's almost always 12.5 fps.
  The view repaints continuously while anything is animated.
- **State animation** (`src/anim.rs`, from `Model_UpdateState 0x4b27d0`):
  - Part animators spin parts at `base + rate × t` (512 units per turn).
  - They blink parts randomly, periodically (staggered phases give chasing lights), or by copying
    another part.
  - Scale curves pulse the object's scale.
  - Animated PAK instances draw each part as its own batch with a per-frame matrix. Everything
    else stays baked.
  - A **State** picker appears for PAKs with more than one state (ARMLAZR, LAZCORE: `ON`/`OFF`).
- **Billboards:** parts with flag `0x40` drop all rotation and keep only scale, as
  `Part_ComputeTransform 0x4b3f90` does. Their mesh is laid out in camera axes around the part's
  position, e.g. the green orb (`GOALGWAY.PAK`) in `GOAL.OCF`.

### Hyper gates

`gate.des` (`STARGATE.PAK`) and `megagate.des` (`MEGAGATE.PAK`) are the only `.des` files of type
`HYPER_GATE`. Each draws three models at the same position and orientation, each with its own
`.anm` (`src/gate.rs`; `decomp/notes/animation.md` "Gates"):
- **Body** (`EXTERIOR_PAK` + `ANIMFILE`). Its three pylon bases are one mesh stacked three times
  at rest; the anm's first keyframe turns two of them by ±120° about the gate axis. That is why
  gates used to show a single pylon.
- **Portal** (`CENTER_PAK` + `CENTER_ANIMFILE`): a billboard sphere whose anm sets its scale and
  self-lit intensity (0 when closed). Its 16-frame texture runs at 25 fps (the game advances its
  counter every other tick).
- **Flare** (`FLARE_PAK` + `FLARE_ANIMFILE`): a billboard streak, scaled the same way. While lit
  it flickers: every tick a random intensity of 128–255 and ±0.125 on the scale.

The game poses a gate every frame from the **first keyframe** of an animation
(`Anim_ApplyPose`): `GATE_OPEN` / `GATEWAY_OPEN` when the gate is closed, `GATE_CLOSE` /
`GATEWAY_CLOSE` when it's open. Each animation starts from the pose it leaves.
- **Open:** the pylon arms fold back against the bases and the centre is clear. This matches the
  open collision mesh, which has nothing within 28 units of the axis.
- **Closed:** the arms swing in to form a ring around the centre.

Whether a gate is open depends on a mission variable named in its placement. With no variable,
or a value below 1, it's open. 132 of the 175 gate placements use a variable the viewer can't
evaluate, so a **Gates** picker chooses between:
- **Open** (the default);
- **Closed**;
- **Cycle:** plays the open animation, holds 3 s, plays the close animation, holds 3 s, and
  repeats. Keyframes move linearly over `TIME`, then hold for `PAUSE`, as `Anim_StepFrame` does.
  Opening takes about 16 s, including the anm's built-in 6 s wait.

**Particle streams** (cycle mode only, since they exist only while a gate opens or closes).
Both gate body anms have `particle_path`s (stargate: 20 for opening and 20 for closing; Mega Gate:
14 and 8):
- **Opening:** blue smoke (`smoke.pak`) and violet sparks (`vlt_part.pak`) stream from three
  points on each pylon tip to the centre during the 6 s wait, then burst outwards from the centre.
- **Closing:** the streams return, and there's a second burst at the end.

Each path's rules, from `Anim_StepParticlePath` / `Anim_EmitParticle`:
- **Emission:** after `WAIT`, each tick of `LIFETIME` emits a particle with probability
  `(volume + 1) / 50`.
- **Flight:** the particle flies in a straight line from the source part's attach point to the
  destination's, taken in the pose at its emission tick. Each end is jittered by ±spread/2.
- **Scale and brightness:** scale and intensity blend from source to destination over the
  particle's own life.
- **Path values:** every path value (volume, life, scale, intensity, spread) moves from its
  `_START` to its `_END` across the path's lifetime.

The particles are evaluated from the clock rather than simulated, so `TAV_TIME` screenshots stay
repeatable.

Opening `STARGATE.PAK`, `GATE_WAY.PAK` and the other gate models on their own also uses their
anm. Not reproduced: the gate sounds and the scripted camera fly-by.

### Models with state animation

Spinning parts (30 PAKs):

| PAK | used by |
|---|---|
| IBDBEACN (beacon heads; they also blink) | SBASE_N.OCF |
| REACTORA (reactor arcs; they also blink) | SBASE_B.OCF |
| SINPLS, SINZAP | GMINE1.OCF |
| ARMLAZR | DISPUT01 / FRONTI00 jobs |
| CRNADEVC | CRNADEVC.ITM, FRONTI02 job |
| RADSCREN | RADSCREN.ITM, BORA01 / FRONTI05 jobs |
| LAZCORE, RIPSTAR2, WCRYSTAL | their `.des` files |
| STAR_YEL, STAR_RED, STAR_GRN, STARDISC, ROCKPT3 | system backdrops (`.BDF`) |
| FRONTNEB | main menu backdrop (`mnu.bdf`) |
| BEAMEMIT, CORONA, EXPLODE1, EXPLODE5, MISSDISB, SBEAM, TORPABST, TORPBLST, TORPFRTS, TORPSECO, TORPSHLD, TRANSP02, WRPTUBE2 | named in space.exe (weapons, explosions, warp effects) |
| CORONAM | nothing found |

Blinking parts (26 PAKs):
- chasing arrows (BCARAROS, GCARAROS, TRAROWS, TYARROWS);
- landing-bay lights (GCBAY1, IHUBBAY, IVGBAY, RIPSTAR, RIPSTAR2);
- sparks, welding and lightning (BFLASH, HBWELD*, SPRK2–4, LTNING*);
- GASGNT02, SGPOD, SINZAP, REACTORA, TORPFRTS, WRPTUBE2, IBDBEACN.

## Layout

| file | what |
|---|---|
| `src/source.rs` | directory or memory-mapped `.pff`; case-insensitive lookup |
| `src/pff.rs` | PFF entry table |
| `src/pak.rs` | PAK parser (port of `TachyonFileFormats/src/pak.py`) |
| `src/cbin.rs` | CBIN decrypt and group/item/param walk |
| `src/ocf.rs` | OCF parse and part placement |
| `src/bdf.rs` | BDF backdrop parse (sky, images, lightning, fog) |
| `src/gate.rs` | `.anm` parse and playback, hyper gate poses |
| `src/weather.rs` | Lightning flash simulation and fog-sheet camera tracking |
| `src/spx.rs` | SPX sector parse, `desnames.cfg` / `.des` model lookup, parent sector |
| `examples/cbindump.rs` | prints any CBIN file (`cargo run --release --example cbindump -- FILE`) |
| `src/model.rs` | builds the GPU batches (one per texture, or per part for animated PAKs) |
| `src/anim.rs` | part tree, part animators, scale curves |
| `src/render.rs` | OpenGL (glow) renderer inside an egui paint callback |
| `src/app.rs` | egui UI and orbit camera |

`cargo test --release` parses all 1,160 shipped PAKs and all 91 OCFs (checking every
referenced PAK and ATTACH parent exists), and builds all 302 sectors. It also checks that a file read from `Tachyon.pff`
matches the extracted copy, and the Euler convention. The tests that need game data skip if
`../pff` or `../Tachyon.pff` are missing.

## Not done yet

- **Blend modes are a best reading, not confirmed** (`decomp/notes/pak.md` "Blend modes"):
  - shadow cards use a fixed 55% darkening.
- The state glow-intensity curve isn't applied. Random blinks re-roll at a fixed 20 Hz rather than
  every game update.
- `.des`-based models (ships with `connect` attachments) aren't assembled. Open their PAKs
  directly.
- Sectors don't draw the flight-time space dust (`moveroid.pak` specks,
  `decomp/notes/formats.md` "Space debris"). Backdrops skip the lens flare. The BDF's
  `CLOUD_RGB`, `BACK_RGB` and `DOA` fog keys aren't drawn (the game's cloud
  routine is an empty stub). Backdrop images are lit by the sector light
  like models; the game's handling of them is unconfirmed.
