//! Procedural surface tiles: seamless noise mapped onto a set of colors.
//!
//! `assets/surfaces.ron` gives a [`NoiseRecipe`] per material key and face
//! direction (`all`, or `top` / `side` / `bottom`, with the same fallbacks
//! as `tiles.ron`). While its `enabled` is set, [`crate::tiles`] builds the
//! faces that have a recipe from it, in place of their image tiles. Like the
//! image tiles, the recipes are client-only and never touch a save.
//!
//! [`generate`] makes one tile:
//!
//! 1. Value noise on a lattice of `cells` × `cells · stretch` points that
//!    wraps at the tile's edges, so the tile is seamless. Each of `octaves`
//!    doubles the lattice and multiplies the amplitude by `persistence`.
//! 2. `contrast` spreads the value about its middle, and `speckle` mixes in
//!    per-texel white noise.
//! 3. The value picks a color along `colors`: blended between neighbours, or
//!    in hard bands with `steps`.
//! 4. A `fringe` paints the top rows (the top of the cell on a side tile)
//!    with its own colors down to a ragged edge, like grass over dirt.
//!
//! The Surfaces window ([`surfaces_window`], F11 while the editor is open)
//! edits the recipes live: the edited material's tiles are regenerated and
//! the chunk material's tile array is replaced the same frame. Save writes
//! `surfaces.ron`, Revert reads it again, and Export PNGs writes each recipe's
//! tile to `assets/tiles/noise/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bevy::prelude::*;
use bevy_egui::{EguiContext, EguiPlugin, EguiPrimaryContextPass, PrimaryEguiContext, egui};
use image::{Rgba, RgbaImage};
use rmc_world::materials::MaterialTable;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::editor::EditorState;
use crate::mesher::is_opaque;
use crate::net::ClientMaterials;
use crate::tiles::{self, LoadedTiles, TILES_SUBDIR, TileDir, TileSources};

/// The recipe table, next to `tiles.ron`.
pub const SURFACES_FILE: &str = "surfaces.ron";

/// Where Export PNGs writes, under `assets/tiles/`.
pub const EXPORT_SUBDIR: &str = "noise";

/// Face fields, in [`SurfaceFaces::field`] order.
pub const FACE_NAMES: [&str; 4] = ["all", "top", "side", "bottom"];

/// An sRGB color.
pub type Rgb = (u8, u8, u8);

/// `assets/surfaces.ron`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SurfaceConfig {
    /// Use the recipes; off, every face keeps its image tile.
    pub enabled: bool,
    /// Material key to its faces' recipes.
    pub materials: BTreeMap<String, SurfaceFaces>,
}

impl SurfaceConfig {
    /// Parses `surfaces.ron`. Optional fields take a bare value.
    pub fn parse(text: &str) -> Result<Self, ron::error::SpannedError> {
        ron::Options::default()
            .with_default_extension(ron::extensions::Extensions::IMPLICIT_SOME)
            .from_str(text)
    }

    /// The table as `surfaces.ron` text, which [`SurfaceConfig::parse`] reads
    /// back.
    pub fn to_ron(&self) -> String {
        let pretty = ron::ser::PrettyConfig::new()
            .depth_limit(4)
            .extensions(ron::extensions::Extensions::IMPLICIT_SOME);
        let body = ron::ser::to_string_pretty(self, pretty).expect("surfaces serialize");
        format!(
            "// Procedural surface tiles (rmc_client::surfaces). Edited in the game:\n\
             // F1, then F11. `enabled: false` keeps the image tiles of tiles.ron.\n{body}\n"
        )
    }

    /// Reads `dir/surfaces.ron`; `Ok(None)` when there is none.
    pub fn load(dir: &Path) -> Result<Option<Self>, String> {
        let path = dir.join(SURFACES_FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text)
                .map(Some)
                .map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// The recipe of each face direction (top, side, bottom) of `key`, with
    /// the name of the field it came from; none while disabled.
    pub fn faces(&self, key: &str) -> [Option<(&'static str, &NoiseRecipe)>; 3] {
        match self.materials.get(key) {
            Some(faces) if self.enabled => faces.resolved(),
            _ => [None, None, None],
        }
    }
}

/// One material's recipes. `all` is the default for every face; `bottom`
/// falls back to `all`, then `side`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SurfaceFaces {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub all: Option<NoiseRecipe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<NoiseRecipe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<NoiseRecipe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bottom: Option<NoiseRecipe>,
}

impl SurfaceFaces {
    /// The recipe for each face direction (top, side, bottom) and the field
    /// it came from.
    pub fn resolved(&self) -> [Option<(&'static str, &NoiseRecipe)>; 3] {
        let named = |i: usize| self.get(i).map(|r| (FACE_NAMES[i], r));
        let side = named(2).or(named(0));
        [named(1).or(named(0)), side, named(3).or(named(0)).or(side)]
    }

    /// Field `i` of [`FACE_NAMES`].
    pub fn get(&self, i: usize) -> Option<&NoiseRecipe> {
        [&self.all, &self.top, &self.side, &self.bottom][i].as_ref()
    }

    /// Field `i` of [`FACE_NAMES`], mutably.
    pub fn field(&mut self, i: usize) -> &mut Option<NoiseRecipe> {
        match i {
            0 => &mut self.all,
            1 => &mut self.top,
            2 => &mut self.side,
            _ => &mut self.bottom,
        }
    }

    /// Whether the faces have recipes of their own.
    pub fn split(&self) -> bool {
        self.top.is_some() || self.side.is_some() || self.bottom.is_some()
    }
}

/// How one tile is made (see the module docs).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct NoiseRecipe {
    /// The colors the noise runs through, low to high.
    pub colors: Vec<Rgb>,
    /// Lattice cells across the tile at the first octave: the size of the
    /// largest blobs is the tile over `cells`.
    pub cells: u32,
    pub octaves: u32,
    /// Amplitude of each octave over the one before: higher is rougher.
    pub persistence: f32,
    /// Spread of the value about its middle; 1 leaves it.
    pub contrast: f32,
    /// Share of per-texel white noise, 0 to 1.
    pub speckle: f32,
    /// Lattice cells down the tile over those across it: above 1 gives
    /// horizontal bands, below 1 vertical streaks.
    pub stretch: f32,
    /// Hard color bands instead of blends.
    pub steps: bool,
    pub seed: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fringe: Option<Fringe>,
}

impl Default for NoiseRecipe {
    fn default() -> Self {
        Self {
            colors: vec![(96, 96, 96), (128, 128, 128), (160, 160, 160)],
            cells: 4,
            octaves: 4,
            persistence: 0.5,
            contrast: 1.0,
            speckle: 0.15,
            stretch: 1.0,
            steps: false,
            seed: 1,
            fringe: None,
        }
    }
}

impl NoiseRecipe {
    /// A recipe around one color: darker, the color, lighter.
    pub fn from_color([r, g, b]: [u8; 3], seed: u32) -> Self {
        let scale = |f: f32| {
            let c = |v: u8| (f32::from(v) * f).round().clamp(0.0, 255.0) as u8;
            (c(r), c(g), c(b))
        };
        Self {
            colors: vec![scale(0.72), scale(0.9), scale(1.0), scale(1.15)],
            seed,
            ..Self::default()
        }
    }
}

/// A band of other colors along the top of the tile.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Fringe {
    /// How far down the band reaches, as a share of the tile, on average.
    pub depth: f32,
    /// How much the band's lower edge wanders, as a share of `depth`.
    pub ragged: f32,
    pub colors: Vec<Rgb>,
}

impl Default for Fringe {
    fn default() -> Self {
        Self {
            depth: 0.25,
            ragged: 0.5,
            colors: vec![(62, 100, 40), (86, 125, 54), (110, 150, 70)],
        }
    }
}

/// The lowbias32 hash of `a`, `b`, `c` and `d`.
fn hash4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    let mut h = a.wrapping_mul(0x8da6_b343)
        ^ b.wrapping_mul(0xd816_3841)
        ^ c.wrapping_mul(0xcb1a_b31f)
        ^ d.wrapping_mul(0x9e37_79b9);
    h ^= h >> 16;
    h = h.wrapping_mul(0x7feb_352d);
    h ^= h >> 15;
    h = h.wrapping_mul(0x846c_a68b);
    h ^ (h >> 16)
}

/// `h` as a value in `[0, 1]`.
fn unit(h: u32) -> f32 {
    (h >> 8) as f32 / 16_777_215.0
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Value noise at `(u, v)` in `[0, 1)²` on a lattice of `nx` × `ny` cells
/// that wraps at 1, so the result is seamless. `layer` picks the octave.
fn wrapped_value(seed: u32, layer: u32, u: f32, v: f32, nx: u32, ny: u32) -> f32 {
    let (x, y) = (u * nx as f32, v * ny as f32);
    let (x0, y0) = (x.floor(), y.floor());
    let (tx, ty) = (smooth(x - x0), smooth(y - y0));
    let (ix, iy) = (x0 as u32 % nx, y0 as u32 % ny);
    let at = |i: u32, j: u32| unit(hash4(seed, layer, i % nx, j % ny));
    let top = at(ix, iy) + (at(ix + 1, iy) - at(ix, iy)) * tx;
    let bottom = at(ix, iy + 1) + (at(ix + 1, iy + 1) - at(ix, iy + 1)) * tx;
    top + (bottom - top) * ty
}

/// Fractal value noise of `recipe` at `(u, v)`, in `[0, 1]`.
fn fractal(recipe: &NoiseRecipe, u: f32, v: f32) -> f32 {
    let nx = recipe.cells.max(1);
    let ny = ((nx as f32 * recipe.stretch).round() as u32).max(1);
    let (mut sum, mut total, mut amp) = (0.0, 0.0, 1.0);
    for o in 0..recipe.octaves.max(1) {
        sum += amp * wrapped_value(recipe.seed, o, u, v, nx << o, ny << o);
        total += amp;
        amp *= recipe.persistence.max(0.0);
    }
    if total > 0.0 { sum / total } else { 0.5 }
}

/// The color at `t` in `[0, 1]` along `colors`.
pub fn palette(colors: &[Rgb], t: f32, steps: bool) -> Rgb {
    match colors {
        [] => (255, 0, 255),
        [c] => *c,
        _ => {
            let n = colors.len();
            let t = t.clamp(0.0, 1.0);
            if steps {
                return colors[((t * n as f32) as usize).min(n - 1)];
            }
            let x = t * (n - 1) as f32;
            let i = (x as usize).min(n - 2);
            let f = x - i as f32;
            let (a, b) = (colors[i], colors[i + 1]);
            let mix =
                |p: u8, q: u8| (f32::from(p) + (f32::from(q) - f32::from(p)) * f).round() as u8;
            (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
        }
    }
}

/// The tile `recipe` makes, `size` texels square (see the module docs).
pub fn generate(recipe: &NoiseRecipe, size: u32) -> RgbaImage {
    let size = size.max(1);
    let s = size as f32;
    // The fringe's lower edge per column, in texels.
    let edge: Vec<f32> = match &recipe.fringe {
        Some(f) => (0..size)
            .map(|x| {
                let u = x as f32 / s;
                let w = wrapped_value(recipe.seed ^ 0xf1, 0, u, 0.0, 8, 1) * 0.6
                    + wrapped_value(recipe.seed ^ 0xf1, 1, u, 0.0, 32, 1) * 0.4;
                f.depth * s * (1.0 + f.ragged * (2.0 * w - 1.0))
            })
            .collect(),
        None => Vec::new(),
    };
    RgbaImage::from_fn(size, size, |x, y| {
        let (u, v) = ((x as f32 + 0.5) / s, (y as f32 + 0.5) / s);
        let mut t = fractal(recipe, u, v);
        t = 0.5 + (t - 0.5) * recipe.contrast;
        let white = unit(hash4(recipe.seed, 0xffff, x, y));
        t += (white - t) * recipe.speckle.clamp(0.0, 1.0);
        let colors = match (&recipe.fringe, edge.get(x as usize)) {
            (Some(f), Some(&e)) if (y as f32) < e => &f.colors,
            _ => &recipe.colors,
        };
        let (r, g, b) = palette(colors, t, recipe.steps);
        Rgba([r, g, b, 255])
    })
}

/// The tile layer name for `key`'s recipe in field `field`.
pub fn layer_name(key: &str, field: &str) -> String {
    format!("noise:{key}:{field}")
}

/// The Surfaces window's state.
#[derive(Resource, Clone, Debug, Default)]
pub struct SurfaceDesigner {
    pub open: bool,
    /// The recipes being edited; what the tiles show.
    pub config: SurfaceConfig,
    /// The material being edited.
    pub selected: Option<String>,
    /// The face field being edited ([`FACE_NAMES`] index).
    pub face: usize,
    /// The last save, revert or export, for the status line.
    pub status: String,
}

/// What the window asks for this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SurfaceRequests {
    /// A recipe of `selected` changed: regenerate its tiles.
    pub edited: bool,
    /// Which faces have recipes, or `enabled`, changed: rebuild every tile.
    pub rebuild: bool,
    pub save: bool,
    pub revert: bool,
    pub export: bool,
}

/// Materials that are drawn with tiles: the opaque ones, by key.
pub fn surface_materials(table: &MaterialTable) -> Vec<(String, [u8; 3])> {
    table
        .iter()
        .filter(|d| is_opaque(d))
        .map(|d| (d.key.clone(), [d.color[0], d.color[1], d.color[2]]))
        .collect()
}

fn color_row(ui: &mut egui::Ui, colors: &mut Vec<Rgb>, changed: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        let mut remove = None;
        for (i, c) in colors.iter_mut().enumerate() {
            let mut rgb = [c.0, c.1, c.2];
            let r = ui.color_edit_button_srgb(&mut rgb);
            if r.changed() {
                *c = (rgb[0], rgb[1], rgb[2]);
                *changed = true;
            }
            if r.secondary_clicked() && remove.is_none() {
                remove = Some(i);
            }
        }
        if let Some(i) = remove
            && colors.len() > 1
        {
            colors.remove(i);
            *changed = true;
        }
        if ui.small_button("+").on_hover_text("Add a color").clicked() {
            let last = colors.last().copied().unwrap_or((128, 128, 128));
            colors.push(last);
            *changed = true;
        }
    });
    ui.label(egui::RichText::new("Right-click a color to remove it.").small());
}

fn recipe_ui(ui: &mut egui::Ui, r: &mut NoiseRecipe) -> bool {
    let mut changed = false;
    ui.label("Colors (low to high)");
    color_row(ui, &mut r.colors, &mut changed);
    let mut slider = |ui: &mut egui::Ui, w: egui::Slider<'_>| {
        changed |= ui.add(w).changed();
    };
    slider(
        ui,
        egui::Slider::new(&mut r.cells, 1..=32).text("Blob count"),
    );
    slider(ui, egui::Slider::new(&mut r.octaves, 1..=6).text("Octaves"));
    slider(
        ui,
        egui::Slider::new(&mut r.persistence, 0.0..=1.0).text("Roughness"),
    );
    slider(
        ui,
        egui::Slider::new(&mut r.contrast, 0.0..=3.0).text("Contrast"),
    );
    slider(
        ui,
        egui::Slider::new(&mut r.speckle, 0.0..=1.0).text("Speckle"),
    );
    slider(
        ui,
        egui::Slider::new(&mut r.stretch, 0.125..=8.0)
            .logarithmic(true)
            .text("Stretch (bands)"),
    );
    changed |= ui.checkbox(&mut r.steps, "Hard color bands").changed();
    ui.horizontal(|ui| {
        changed |= ui
            .add(egui::DragValue::new(&mut r.seed).prefix("Seed "))
            .changed();
        if ui.button("Reroll").clicked() {
            r.seed = hash4(r.seed, 0x5eed, 0, 0);
            changed = true;
        }
    });
    let mut has_fringe = r.fringe.is_some();
    if ui
        .checkbox(&mut has_fringe, "Fringe along the top edge")
        .changed()
    {
        r.fringe = has_fringe.then(Fringe::default);
        changed = true;
    }
    if let Some(f) = &mut r.fringe {
        ui.indent("fringe", |ui| {
            changed |= ui
                .add(egui::Slider::new(&mut f.depth, 0.0..=1.0).text("Depth"))
                .changed();
            changed |= ui
                .add(egui::Slider::new(&mut f.ragged, 0.0..=1.0).text("Ragged"))
                .changed();
            ui.label("Fringe colors");
            color_row(ui, &mut f.colors, &mut changed);
        });
    }
    changed
}

/// Draws the Surfaces window on `ctx` while `designer.open`; returns what
/// it asks for. `materials` are the keys and colors of the tiled
/// materials ([`surface_materials`]).
pub fn surfaces_window(
    ctx: &egui::Context,
    designer: &mut SurfaceDesigner,
    materials: &[(String, [u8; 3])],
) -> SurfaceRequests {
    let mut req = SurfaceRequests::default();
    if !designer.open {
        return req;
    }
    let mut open = true;
    egui::Window::new("Surfaces")
        .open(&mut open)
        .default_pos([340.0, 40.0])
        .default_width(320.0)
        .vscroll(true)
        .show(ctx, |ui| {
            req.rebuild |= ui
                .checkbox(&mut designer.config.enabled, "Use noise surfaces")
                .on_hover_text("Off: every face shows its image tile from tiles.ron")
                .changed();
            ui.horizontal(|ui| {
                req.save = ui
                    .button("Save")
                    .on_hover_text(format!("Write {SURFACES_FILE}"))
                    .clicked();
                req.revert = ui
                    .button("Revert")
                    .on_hover_text(format!("Read {SURFACES_FILE} again"))
                    .clicked();
                req.export = ui
                    .button("Export PNGs")
                    .on_hover_text(format!("Write each tile to tiles/{EXPORT_SUBDIR}/"))
                    .clicked();
            });
            if !designer.status.is_empty() {
                ui.label(egui::RichText::new(&designer.status).small());
            }
            ui.separator();
            let selected = designer
                .selected
                .clone()
                .or_else(|| materials.first().map(|m| m.0.clone()))
                .unwrap_or_default();
            egui::ComboBox::from_label("Material")
                .selected_text(&selected)
                .show_ui(ui, |ui| {
                    for (key, _) in materials {
                        let mark = if designer.config.materials.contains_key(key) {
                            "● "
                        } else {
                            "   "
                        };
                        if ui
                            .selectable_label(*key == selected, format!("{mark}{key}"))
                            .clicked()
                        {
                            designer.selected = Some(key.clone());
                            designer.face = 0;
                        }
                    }
                });
            let key = designer.selected.clone().unwrap_or(selected);
            designer.selected = Some(key.clone());
            let color = materials
                .iter()
                .find(|m| m.0 == key)
                .map_or([128; 3], |m| m.1);
            ui.separator();
            let Some(faces) = designer.config.materials.get_mut(&key) else {
                ui.label("This material shows its image tile.");
                if ui.button("Add a noise recipe").clicked() {
                    let seed = hash4(key.len() as u32, color[0].into(), color[1].into(), 7);
                    designer.config.materials.insert(
                        key.clone(),
                        SurfaceFaces {
                            all: Some(NoiseRecipe::from_color(color, seed)),
                            ..SurfaceFaces::default()
                        },
                    );
                    req.rebuild = true;
                }
                return;
            };
            let mut split = faces.split();
            if ui
                .checkbox(&mut split, "Separate top, side and bottom")
                .changed()
            {
                if split {
                    let base = faces.all.take().unwrap_or_default();
                    faces.top = Some(base.clone());
                    faces.side = Some(base.clone());
                    faces.bottom = Some(base);
                    designer.face = 1;
                } else {
                    let base = faces
                        .side
                        .take()
                        .or(faces.top.take())
                        .or(faces.bottom.take());
                    *faces = SurfaceFaces {
                        all: base,
                        ..SurfaceFaces::default()
                    };
                    designer.face = 0;
                }
                req.rebuild = true;
            }
            if split {
                if designer.face == 0 {
                    designer.face = 1;
                }
                ui.horizontal(|ui| {
                    for (i, name) in FACE_NAMES.iter().enumerate().skip(1) {
                        ui.selectable_value(&mut designer.face, i, *name);
                    }
                });
            } else {
                designer.face = 0;
            }
            let slot = faces.field(designer.face);
            match slot {
                Some(recipe) => req.edited |= recipe_ui(ui, recipe),
                None => {
                    if ui.button("Add a recipe for this face").clicked() {
                        *slot = Some(NoiseRecipe::from_color(color, 1));
                        req.rebuild = true;
                    }
                }
            }
            ui.separator();
            if ui.button("Remove this material's recipe").clicked() {
                designer.config.materials.remove(&key);
                req.rebuild = true;
            }
        });
    designer.open = open;
    req
}

/// Opens and closes the Surfaces window (F11 while the editor is open),
/// draws it, and turns what it asks into new tiles, saves and exports.
pub struct SurfacesPlugin;

impl Plugin for SurfacesPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<EguiPlugin>() {
            app.add_plugins(EguiPlugin::default());
        }
        app.init_resource::<SurfaceDesigner>()
            .add_systems(Update, toggle_designer)
            .add_systems(EguiPrimaryContextPass, draw_designer.run_if(designer_shown));
    }
}

fn designer_shown(designer: Res<SurfaceDesigner>, editor: Option<Res<EditorState>>) -> bool {
    designer.open && editor.is_some_and(|e| e.open)
}

/// The editor's Surfaces action (F11) opens the window on the recipes the
/// tiles were built from, or closes it.
fn toggle_designer(
    editor: Option<ResMut<EditorState>>,
    sources: Option<Res<TileSources>>,
    mut designer: ResMut<SurfaceDesigner>,
) {
    let requested = editor.is_some_and(|mut e| std::mem::take(&mut e.surfaces_toggle_requested));
    if !requested {
        return;
    }
    designer.open = !designer.open;
    if designer.open
        && let Some(sources) = sources
    {
        designer.config = sources.surfaces.clone();
    }
}

fn draw_designer(world: &mut World) {
    let mut contexts = world.query_filtered::<&mut EguiContext, With<PrimaryEguiContext>>();
    let Ok(mut context) = contexts.single_mut(world) else {
        return;
    };
    let ctx = context.get_mut().clone();
    let materials = world
        .get_resource::<ClientMaterials>()
        .map(|m| surface_materials(&m.0))
        .unwrap_or_default();
    let req = surfaces_window(
        &ctx,
        &mut world.resource_mut::<SurfaceDesigner>(),
        &materials,
    );
    apply_requests(world, req);
}

/// Carries out `req` for the [`SurfaceDesigner`]: regenerates or rebuilds
/// the tiles from its recipes, saves, reverts or exports. Needs a
/// [`TileDir`]; without the loaded tiles, edits wait for the next load.
pub fn apply_requests(world: &mut World, req: SurfaceRequests) {
    if req == SurfaceRequests::default() {
        return;
    }
    let Some(dir) = world.get_resource::<TileDir>().map(|d| d.0.clone()) else {
        return;
    };
    if req.revert {
        let status = match SurfaceConfig::load(&dir) {
            Ok(config) => {
                world.resource_mut::<SurfaceDesigner>().config = config.unwrap_or_default();
                format!("Reverted to {SURFACES_FILE}")
            }
            Err(e) => format!("Revert failed: {e}"),
        };
        world.resource_mut::<SurfaceDesigner>().status = status;
    }
    if req.edited || req.rebuild || req.revert {
        retile(world, &dir, req.rebuild || req.revert);
    }
    if req.save {
        let text = world.resource::<SurfaceDesigner>().config.to_ron();
        let path = dir.join(SURFACES_FILE);
        let status = match std::fs::write(&path, text) {
            Ok(()) => {
                info!("surfaces saved to {}", path.display());
                format!("Saved {SURFACES_FILE}")
            }
            Err(e) => format!("Save failed: {e}"),
        };
        world.resource_mut::<SurfaceDesigner>().status = status;
    }
    if req.export {
        let size = world
            .get_resource::<TileSources>()
            .map_or(tiles::DEFAULT_TILE_SIZE, |s| s.config.size);
        let out = dir.join(TILES_SUBDIR).join(EXPORT_SUBDIR);
        let status = match export(&world.resource::<SurfaceDesigner>().config, &out, size) {
            Ok(n) => format!("Exported {n} tiles to tiles/{EXPORT_SUBDIR}/"),
            Err(e) => format!("Export failed: {e}"),
        };
        world.resource_mut::<SurfaceDesigner>().status = status;
    }
}

/// Gives the tiles the designer's recipes: the selected material's noise
/// layers regenerated in place, or (`rebuild`, or a layer is missing) the
/// whole set built again, reusing the decoded image tiles.
fn retile(world: &mut World, dir: &Path, rebuild: bool) {
    let designer = world.resource::<SurfaceDesigner>().clone();
    let Some(mut sources) = world.get_resource::<TileSources>().cloned() else {
        return;
    };
    sources.surfaces = designer.config.clone();
    let previous = world.get_resource::<LoadedTiles>().map(|t| t.0.clone());
    let mut patched = None;
    if !rebuild && let (Some(mut set), Some(key)) = (previous.clone(), designer.selected.as_ref()) {
        let faces = designer.config.materials.get(key);
        let recipes: Vec<(&str, &NoiseRecipe)> = (0..FACE_NAMES.len())
            .filter_map(|i| Some((FACE_NAMES[i], faces?.get(i)?)))
            .collect();
        let size = set.size;
        let all_found = recipes.iter().all(|(field, recipe)| {
            !designer.config.enabled
                || set.replace_layer(&layer_name(key, field), &generate(recipe, size))
        });
        if all_found {
            patched = Some(set);
        }
    }
    let set = match patched {
        Some(set) => set,
        None => match tiles::TileSet::build_with(
            &sources.config,
            &dir.join(TILES_SUBDIR),
            &sources.surfaces,
            previous.as_ref(),
        ) {
            Ok(set) => set,
            Err(e) => {
                warn!("surfaces: {e}");
                return;
            }
        },
    };
    world.insert_resource(sources);
    tiles::install(world, set);
}

/// Writes every recipe's tile to `out` as `<key>_<field>.png`; returns how
/// many.
pub fn export(config: &SurfaceConfig, out: &Path, size: u32) -> Result<usize, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut n = 0;
    for (key, faces) in &config.materials {
        for (i, field) in FACE_NAMES.iter().enumerate() {
            if let Some(recipe) = faces.get(i) {
                let path: PathBuf = out.join(format!("{key}_{field}.png"));
                generate(recipe, size)
                    .save(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                n += 1;
            }
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::{TileConfig, TileSet};

    const ASSETS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");
    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rmc_surfaces_{name}_{}", std::process::id()));
        std::fs::create_dir_all(dir.join(TILES_SUBDIR)).unwrap();
        dir
    }

    fn rgb(p: &Rgba<u8>) -> [i32; 3] {
        [p[0].into(), p[1].into(), p[2].into()]
    }

    fn diff(a: &Rgba<u8>, b: &Rgba<u8>) -> i32 {
        let (a, b) = (rgb(a), rgb(b));
        (0..3).map(|i| (a[i] - b[i]).abs()).sum()
    }

    fn smooth_recipe() -> NoiseRecipe {
        NoiseRecipe {
            colors: vec![(0, 0, 0), (255, 255, 255)],
            speckle: 0.0,
            ..NoiseRecipe::default()
        }
    }

    #[test]
    fn noise_tiles_are_seamless() {
        for stretch in [1.0, 3.0, 0.5] {
            let recipe = NoiseRecipe {
                stretch,
                ..smooth_recipe()
            };
            let img = generate(&recipe, 64);
            let n = 64;
            // Across the wrap, neighbours differ no more than inside the tile.
            let mut inner = 0;
            let mut wrap = 0;
            for i in 0..n {
                inner = inner.max(diff(img.get_pixel(n / 2, i), img.get_pixel(n / 2 + 1, i)));
                inner = inner.max(diff(img.get_pixel(i, n / 2), img.get_pixel(i, n / 2 + 1)));
                wrap = wrap.max(diff(img.get_pixel(n - 1, i), img.get_pixel(0, i)));
                wrap = wrap.max(diff(img.get_pixel(i, n - 1), img.get_pixel(i, 0)));
            }
            let interior = (1..n - 1)
                .flat_map(|x| (0..n).map(move |y| (x, y)))
                .map(|(x, y)| diff(img.get_pixel(x, y), img.get_pixel(x + 1, y)))
                .max()
                .unwrap();
            assert!(
                wrap <= interior.max(inner),
                "stretch {stretch}: wrap step {wrap} > interior {interior}"
            );
        }
    }

    #[test]
    fn noise_is_deterministic_and_follows_the_seed() {
        let a = generate(&NoiseRecipe::default(), 32);
        assert_eq!(a, generate(&NoiseRecipe::default(), 32));
        let other = NoiseRecipe {
            seed: 2,
            ..NoiseRecipe::default()
        };
        assert_ne!(a, generate(&other, 32));
    }

    #[test]
    fn hard_bands_use_only_the_palette() {
        let colors = vec![(10, 20, 30), (100, 50, 0), (200, 200, 90)];
        let recipe = NoiseRecipe {
            colors: colors.clone(),
            steps: true,
            speckle: 0.3,
            contrast: 2.0,
            ..NoiseRecipe::default()
        };
        let img = generate(&recipe, 32);
        let mut seen = std::collections::BTreeSet::new();
        for p in img.pixels() {
            let c = (p[0], p[1], p[2]);
            assert!(colors.contains(&c), "{c:?} is not in the palette");
            seen.insert(c);
        }
        assert!(seen.len() > 1, "more than one band shows");
        assert_eq!(palette(&colors, 0.0, false), colors[0]);
        assert_eq!(palette(&colors, 1.0, false), colors[2]);
        assert_eq!(palette(&colors, 0.25, false), (55, 35, 15));
    }

    #[test]
    fn fringe_colors_the_top_rows() {
        let recipe = NoiseRecipe {
            colors: vec![(100, 70, 40)],
            fringe: Some(Fringe {
                depth: 0.25,
                ragged: 0.5,
                colors: vec![(50, 160, 50)],
            }),
            ..NoiseRecipe::default()
        };
        let img = generate(&recipe, 64);
        // Depth 0.25 ± half: rows above 8 are all fringe, rows from 24 none.
        for x in 0..64 {
            for y in 0..8 {
                assert_eq!(rgb(img.get_pixel(x, y)), [50, 160, 50], "({x}, {y})");
            }
            for y in 24..64 {
                assert_eq!(rgb(img.get_pixel(x, y)), [100, 70, 40], "({x}, {y})");
            }
        }
        let depths: std::collections::BTreeSet<u32> = (0..64)
            .map(|x| {
                (0..64)
                    .take_while(|&y| img.get_pixel(x, y)[1] == 160)
                    .count() as u32
            })
            .collect();
        assert!(depths.len() > 2, "the edge is ragged: {depths:?}");
    }

    #[test]
    fn faces_fall_back_like_tiles_and_only_while_enabled() {
        let r = |seed| NoiseRecipe {
            seed,
            ..NoiseRecipe::default()
        };
        let mut config = SurfaceConfig {
            enabled: true,
            materials: BTreeMap::from([(
                "topsoil".to_owned(),
                SurfaceFaces {
                    top: Some(r(1)),
                    side: Some(r(2)),
                    ..SurfaceFaces::default()
                },
            )]),
        };
        let fields = config.faces("topsoil").map(|f| f.map(|(n, _)| n));
        assert_eq!(fields, [Some("top"), Some("side"), Some("side")]);
        assert_eq!(config.faces("stone").map(|f| f.is_some()), [false; 3]);
        config.enabled = false;
        assert_eq!(config.faces("topsoil").map(|f| f.is_some()), [false; 3]);
    }

    #[test]
    fn config_round_trips_through_ron() {
        let config = SurfaceConfig {
            enabled: true,
            materials: BTreeMap::from([
                (
                    "stone".to_owned(),
                    SurfaceFaces {
                        all: Some(NoiseRecipe::default()),
                        ..SurfaceFaces::default()
                    },
                ),
                (
                    "topsoil".to_owned(),
                    SurfaceFaces {
                        side: Some(NoiseRecipe {
                            fringe: Some(Fringe::default()),
                            ..NoiseRecipe::default()
                        }),
                        ..SurfaceFaces::default()
                    },
                ),
            ]),
        };
        let text = config.to_ron();
        assert_eq!(SurfaceConfig::parse(&text).unwrap(), config, "{text}");
        assert!(!text.contains("Some("), "bare optional values: {text}");
    }

    #[test]
    fn shipped_surfaces_cover_tiled_materials() {
        let config = SurfaceConfig::load(Path::new(ASSETS_DIR))
            .expect("assets/surfaces.ron parses")
            .expect("assets/surfaces.ron exists");
        let table = MaterialTable::load(MATERIALS_PATH).unwrap();
        for (key, faces) in &config.materials {
            assert!(table.id(key).is_some(), "unknown material {key:?}");
            assert!(
                faces.resolved().iter().all(Option::is_some),
                "{key}: every face has a recipe"
            );
        }
        assert!(!config.materials.is_empty());
    }

    fn noise_set(enabled: bool, dir: &Path) -> (TileConfig, SurfaceConfig, TileSet) {
        image::RgbaImage::from_pixel(8, 8, Rgba([200, 10, 10, 255]))
            .save(dir.join("tiles/a.png"))
            .unwrap();
        let config = TileConfig {
            size: 16,
            materials: BTreeMap::from([
                (
                    "stone".to_owned(),
                    tiles::FaceFiles {
                        all: Some("a.png".into()),
                        ..default()
                    },
                ),
                (
                    "dirt".to_owned(),
                    tiles::FaceFiles {
                        all: Some("a.png".into()),
                        ..default()
                    },
                ),
            ]),
        };
        let surfaces = SurfaceConfig {
            enabled,
            materials: BTreeMap::from([(
                "dirt".to_owned(),
                SurfaceFaces {
                    top: Some(NoiseRecipe::default()),
                    side: Some(NoiseRecipe {
                        seed: 9,
                        ..NoiseRecipe::default()
                    }),
                    ..SurfaceFaces::default()
                },
            )]),
        };
        let set = TileSet::build_with(&config, &dir.join(TILES_SUBDIR), &surfaces, None).unwrap();
        (config, surfaces, set)
    }

    #[test]
    fn tile_set_uses_noise_layers_while_enabled() {
        let dir = scratch_dir("set");
        let (_, _, on) = noise_set(true, &dir);
        assert_eq!(on.layers, ["noise:dirt:top", "noise:dirt:side", "a.png"]);
        assert_eq!(on.faces["dirt"], [Some(0), Some(1), Some(1)]);
        assert_eq!(on.faces["stone"], [Some(2); 3]);
        let top = on.layer("noise:dirt:top").unwrap();
        assert_eq!(
            &top[..16 * 16 * 4],
            generate(&NoiseRecipe::default(), 16).as_raw()
        );

        let (_, _, off) = noise_set(false, &dir);
        assert_eq!(off.layers, ["a.png"]);
        assert_eq!(off.faces["dirt"], [Some(0); 3]);
        std::fs::remove_dir_all(dir).ok();
    }

    fn designer_world(dir: &Path) -> World {
        let (config, surfaces, set) = noise_set(true, dir);
        let mut world = World::new();
        world.insert_resource(TileDir(dir.to_path_buf()));
        world.insert_resource(TileSources {
            config,
            surfaces: surfaces.clone(),
        });
        world.insert_resource(LoadedTiles(set));
        world.insert_resource(SurfaceDesigner {
            open: true,
            config: surfaces,
            selected: Some("dirt".into()),
            face: 1,
            status: String::new(),
        });
        world
    }

    #[test]
    fn edits_regenerate_tiles_save_and_revert() {
        let dir = scratch_dir("edit");
        let mut world = designer_world(&dir);
        let before = world.resource::<LoadedTiles>().0.clone();

        // A color change regenerates the edited face's layer in place.
        world
            .resource_mut::<SurfaceDesigner>()
            .config
            .materials
            .get_mut("dirt")
            .unwrap()
            .top
            .as_mut()
            .unwrap()
            .colors = vec![(1, 2, 3)];
        apply_requests(
            &mut world,
            SurfaceRequests {
                edited: true,
                ..default()
            },
        );
        let after = world.resource::<LoadedTiles>().0.clone();
        assert_eq!(after.layers, before.layers, "patched in place");
        assert!(
            after.layer("noise:dirt:top").unwrap()[..4] == [1, 2, 3, 255],
            "the new color shows"
        );
        assert_eq!(after.layer("a.png"), before.layer("a.png"));
        assert_eq!(
            world.resource::<TileSources>().surfaces,
            world.resource::<SurfaceDesigner>().config
        );

        // Save, then turn the noise off: the image tile comes back.
        apply_requests(
            &mut world,
            SurfaceRequests {
                save: true,
                ..default()
            },
        );
        let saved = SurfaceConfig::load(&dir).unwrap().unwrap();
        assert_eq!(saved, world.resource::<SurfaceDesigner>().config);
        world.resource_mut::<SurfaceDesigner>().config.enabled = false;
        apply_requests(
            &mut world,
            SurfaceRequests {
                rebuild: true,
                ..default()
            },
        );
        assert_eq!(world.resource::<LoadedTiles>().0.layers, ["a.png"]);

        // Revert reads the save: noise again, with the saved color.
        apply_requests(
            &mut world,
            SurfaceRequests {
                revert: true,
                ..default()
            },
        );
        let set = &world.resource::<LoadedTiles>().0;
        assert_eq!(set.faces["dirt"], [Some(0), Some(1), Some(1)]);
        assert_eq!(set.layer("noise:dirt:top").unwrap()[..4], [1, 2, 3, 255]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn export_writes_a_png_per_recipe() {
        let dir = scratch_dir("export");
        let (_, surfaces, _) = noise_set(true, &dir);
        let out = dir.join("noise");
        assert_eq!(export(&surfaces, &out, 16), Ok(2));
        let top = image::open(out.join("dirt_top.png")).unwrap().to_rgba8();
        assert_eq!(top, generate(&NoiseRecipe::default(), 16));
        std::fs::remove_dir_all(dir).ok();
    }

    fn frame(ctx: &egui::Context, designer: &mut SurfaceDesigner) -> SurfaceRequests {
        let materials = vec![
            ("dirt".to_owned(), [121, 85, 58]),
            ("stone".to_owned(), [128, 128, 128]),
        ];
        let mut req = SurfaceRequests::default();
        let out = ctx.run_ui(egui::RawInput::default(), |ui| {
            req = surfaces_window(ui.ctx(), designer, &materials);
        });
        // No renderer takes the font texture here.
        out.drop_without_applying_deltas();
        req
    }

    #[test]
    fn window_draws_only_while_open_and_changes_nothing_by_itself() {
        let ctx = egui::Context::default();
        let dir = scratch_dir("window");
        let (_, surfaces, _) = noise_set(true, &dir);
        let mut designer = SurfaceDesigner {
            config: surfaces.clone(),
            ..default()
        };
        assert_eq!(frame(&ctx, &mut designer), SurfaceRequests::default());
        designer.open = true;
        for _ in 0..3 {
            assert_eq!(frame(&ctx, &mut designer), SurfaceRequests::default());
        }
        assert!(designer.open);
        assert_eq!(designer.config, surfaces, "drawing alone edits nothing");
        assert_eq!(designer.selected.as_deref(), Some("dirt"));
        assert_eq!(designer.face, 1, "split faces start on top");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn surfaces_action_toggles_the_window_on_the_loaded_recipes() {
        use bevy::ecs::system::RunSystemOnce;
        let dir = scratch_dir("toggle");
        let mut world = designer_world(&dir);
        world.resource_mut::<SurfaceDesigner>().open = false;
        world.resource_mut::<SurfaceDesigner>().config = SurfaceConfig::default();
        let mut editor = EditorState {
            open: true,
            ..default()
        };
        editor.apply(crate::editor::EditorAction::ToggleSurfaces, None);
        world.insert_resource(editor);
        world.run_system_once(toggle_designer).unwrap();
        let designer = world.resource::<SurfaceDesigner>();
        assert!(designer.open);
        assert_eq!(designer.config, world.resource::<TileSources>().surfaces);
        assert!(!world.resource::<EditorState>().surfaces_toggle_requested);
        world.run_system_once(toggle_designer).unwrap();
        assert!(
            world.resource::<SurfaceDesigner>().open,
            "one press, one toggle"
        );
        world
            .resource_mut::<EditorState>()
            .apply(crate::editor::EditorAction::ToggleSurfaces, None);
        world.run_system_once(toggle_designer).unwrap();
        assert!(!world.resource::<SurfaceDesigner>().open);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn surface_materials_are_the_opaque_ones() {
        let table = MaterialTable::load(MATERIALS_PATH).unwrap();
        let keys: Vec<String> = surface_materials(&table)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert!(keys.contains(&"stone".to_owned()));
        assert!(!keys.contains(&"water".to_owned()), "liquids draw no tiles");
        assert!(!keys.contains(&"air".to_owned()));
    }
}
