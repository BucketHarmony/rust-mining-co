//! Data maps: heat maps of the soil and water over the terrain, picked from a
//! light panel on the right of the screen (docs/OVERLAYS.md).
//!
//! `assets/overlays.ron` lists the maps ([`MapOverlays`]). M steps to the
//! next map and Shift+M to the previous one; past the last (or before the
//! first), the maps turn off. The picked map's [`ColorSource`] goes to
//! [`MeshColorSource`], so the chunk meshes take its colors. A layer map
//! reads the layer in each cell; a column map (the water table, runoff)
//! derives one value per cell column ([`crate::mesher::ColumnField`]).
//!
//! The panel lists every map with its color, marks the picked one, and
//! shows its legend: a color bar between the two ends of its scale, and the
//! value at the targeted cell. On the runoff map, arrows show which way the
//! water runs off around the camera ([`RunoffArrows`]); they are drawn only
//! in apps with Bevy's `GizmoPlugin`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use bevy::gizmos::GizmoPlugin;
use bevy::prelude::*;
use rmc_world::coords::{CELL_SIZE_M, CellPos};
use rmc_world::layers::LayerId;
use rmc_world::world::World;
use serde::Deserialize;

use crate::eco::ClientEco;
use crate::hud::{HudState, TemperatureUnit, shown_unit};
use crate::mesher::{ColorSource, Colormap, ColumnField, runoff};
use crate::meshpipe::MeshColorSource;
use crate::render::GameCamera;
use crate::replica::Replica;
use crate::ui::theme::parse_hex;
use crate::ui::{ColorToken, contrast_ratio};
use crate::ui::{HudAnchor, HudSlot, TextRole, UiFonts, UiScreens, UiText, UiTheme};

/// Steps to the next map; with Shift, to the previous one.
pub const MAP_KEY: KeyCode = KeyCode::KeyM;

/// Frames between re-reads of the runoff arrows while the camera stays in
/// one column and the replica changes.
const ARROW_REFRESH_FRAMES: u32 = 30;

#[derive(Debug, thiserror::Error)]
pub enum OverlayError {
    #[error("reading {0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error("parsing {0}: {1}")]
    Parse(PathBuf, Box<ron::error::SpannedError>),
    #[error("{0}: {1}")]
    Invalid(PathBuf, String),
}

/// What a map reads.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub enum MapSource {
    /// A substance layer, by its `substances.ron` key.
    Substance(String),
    Moisture,
    /// 0.1 °C.
    Temperature,
    Light,
    Growth,
    Biomass,
    Regolith,
    /// [`crate::mesher::water_table_depth`], searched to the range's end.
    WaterTable,
    /// The drop of [`runoff`]; the runoff map also draws its arrows.
    Runoff,
}

/// Where a map's value comes from, once the substance keys are resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Probe {
    Layer(LayerId),
    Column(ColumnField),
}

impl MapSource {
    fn probe(&self, eco: Option<&ClientEco>) -> Option<Probe> {
        Some(match self {
            Self::Substance(key) => Probe::Layer(eco?.substances.layer(key)?),
            Self::Moisture => Probe::Layer(LayerId::Moisture),
            Self::Temperature => Probe::Layer(LayerId::Temperature),
            Self::Light => Probe::Layer(LayerId::Light),
            Self::Growth => Probe::Layer(LayerId::Growth),
            Self::Biomass => Probe::Layer(LayerId::Biomass),
            Self::Regolith => Probe::Layer(LayerId::Regolith),
            Self::WaterTable => Probe::Column(ColumnField::WaterTable),
            Self::Runoff => Probe::Column(ColumnField::Runoff),
        })
    }
}

/// One data map.
#[derive(Clone, Debug, PartialEq)]
pub struct MapDef {
    pub key: String,
    pub label: String,
    pub source: MapSource,
    pub colormap: Colormap,
    /// The names of the scale's two ends.
    pub legend: (String, String),
}

impl MapDef {
    /// What colors the meshes while this map is picked; `None` until the
    /// eco tables name its substance.
    pub fn color_source(&self, eco: Option<&ClientEco>) -> Option<ColorSource> {
        Some(match self.source.probe(eco)? {
            Probe::Layer(id) => ColorSource::Layer(id, self.colormap),
            Probe::Column(field) => ColorSource::Column(field, self.colormap),
        })
    }

    /// The map's value at `cell` (a column map reads the cell's column).
    pub fn value_at(&self, world: &World, cell: CellPos, eco: Option<&ClientEco>) -> Option<i16> {
        Some(match self.source.probe(eco)? {
            Probe::Layer(id) => {
                let v = world.layer(cell, id);
                v.clamp(i16::MIN.into(), i16::MAX.into()) as i16
            }
            Probe::Column(field) => field.value(world, cell.0.x, cell.0.z, self.colormap.max),
        })
    }

    /// `value` as the panel reads it out.
    pub fn readout(&self, value: i16, unit: TemperatureUnit) -> String {
        let max = self.colormap.max;
        match self.source {
            MapSource::Temperature => unit.format(value),
            MapSource::WaterTable if value == 0 => "at the surface".into(),
            MapSource::WaterTable if value >= max => format!("deeper than {max} cells"),
            MapSource::WaterTable if value == 1 => "1 cell down".into(),
            MapSource::WaterTable => format!("{value} cells down"),
            MapSource::Runoff if value == 0 => "flat".into(),
            MapSource::Runoff => format!("drops {value}"),
            _ => value.to_string(),
        }
    }

    /// The map's own color: the end of its scale that is not near white.
    pub fn swatch(&self) -> [u8; 3] {
        let pale = |c: [u8; 3]| c.iter().all(|&v| v >= PALE);
        if pale(self.colormap.high) {
            self.colormap.low
        } else {
            self.colormap.high
        }
    }
}

/// Channels at or above this read as near white ([`MapDef::swatch`]).
const PALE: u8 = 0xE0;

/// The panel's look.
#[derive(Clone, Debug, PartialEq)]
pub struct MapPanelLook {
    pub width: f32,
    pub radius: f32,
    pub swatch: f32,
    pub surface: Color,
    pub text: Color,
    pub text_dim: Color,
    pub border: Color,
    pub selected: Color,
    pub selected_text: Color,
    pub shadow: Color,
}

/// The runoff arrows' look and reach.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrowLook {
    /// Columns from the camera's column.
    pub radius: i32,
    /// Columns between arrows.
    pub spacing: i32,
    /// Cells above the ground.
    pub lift: f32,
    pub color: Color,
}

/// `assets/overlays.ron`. See the module docs.
#[derive(Resource, Clone, Debug, PartialEq)]
pub struct MapOverlays {
    pub panel: MapPanelLook,
    pub arrows: ArrowLook,
    /// Swatches in the legend's color bar.
    pub legend_steps: usize,
    pub maps: Vec<MapDef>,
}

#[derive(Deserialize)]
struct OverlaysFile {
    panel: PanelFile,
    arrows: ArrowsFile,
    legend_steps: usize,
    maps: Vec<MapFile>,
}

#[derive(Deserialize)]
struct PanelFile {
    width: f32,
    radius: f32,
    swatch: f32,
    surface: String,
    text: String,
    text_dim: String,
    border: String,
    selected: String,
    selected_text: String,
    shadow: String,
}

#[derive(Deserialize)]
struct ArrowsFile {
    radius: i32,
    spacing: i32,
    lift: f32,
    color: String,
}

#[derive(Deserialize)]
struct MapFile {
    key: String,
    label: String,
    source: MapSource,
    range: (i16, i16),
    low: String,
    high: String,
    legend: (String, String),
}

enum ParseError {
    Ron(Box<ron::error::SpannedError>),
    Invalid(String),
}

fn color(name: &str, hex: &str) -> Result<Color, ParseError> {
    parse_hex(hex).ok_or_else(|| ParseError::Invalid(format!("{name}: bad color {hex:?}")))
}

fn rgb(name: &str, hex: &str) -> Result<[u8; 3], ParseError> {
    let c = color(name, hex)?.to_srgba().to_u8_array();
    Ok([c[0], c[1], c[2]])
}

impl MapOverlays {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, OverlayError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|e| OverlayError::Io(path.into(), e))?;
        Self::parse(&text).map_err(|e| match e {
            ParseError::Ron(e) => OverlayError::Parse(path.into(), e),
            ParseError::Invalid(why) => OverlayError::Invalid(path.into(), why),
        })
    }

    fn parse(text: &str) -> Result<Self, ParseError> {
        let f: OverlaysFile = ron::from_str(text).map_err(|e| ParseError::Ron(Box::new(e)))?;
        let p = &f.panel;
        let panel = MapPanelLook {
            width: p.width,
            radius: p.radius,
            swatch: p.swatch,
            surface: color("panel.surface", &p.surface)?,
            text: color("panel.text", &p.text)?,
            text_dim: color("panel.text_dim", &p.text_dim)?,
            border: color("panel.border", &p.border)?,
            selected: color("panel.selected", &p.selected)?,
            selected_text: color("panel.selected_text", &p.selected_text)?,
            shadow: color("panel.shadow", &p.shadow)?,
        };
        let a = &f.arrows;
        if a.radius < 0 || a.spacing < 1 {
            return Err(ParseError::Invalid(
                "arrows: radius must not be negative and spacing must be at least 1".into(),
            ));
        }
        let arrows = ArrowLook {
            radius: a.radius,
            spacing: a.spacing,
            lift: a.lift,
            color: color("arrows.color", &a.color)?,
        };
        if f.legend_steps < 2 {
            return Err(ParseError::Invalid(
                "legend_steps must be at least 2".into(),
            ));
        }
        if f.maps.is_empty() {
            return Err(ParseError::Invalid("no maps".into()));
        }
        let mut keys = BTreeSet::new();
        let mut maps = Vec::with_capacity(f.maps.len());
        for m in f.maps {
            if !keys.insert(m.key.clone()) {
                return Err(ParseError::Invalid(format!("map {:?} twice", m.key)));
            }
            let (min, max) = m.range;
            if min >= max {
                return Err(ParseError::Invalid(format!(
                    "map {:?}: range {min}..{max} is empty",
                    m.key
                )));
            }
            let colormap = Colormap {
                min,
                max,
                low: rgb(&m.key, &m.low)?,
                high: rgb(&m.key, &m.high)?,
            };
            maps.push(MapDef {
                key: m.key,
                label: m.label,
                source: m.source,
                colormap,
                legend: m.legend,
            });
        }
        Ok(Self {
            panel,
            arrows,
            legend_steps: f.legend_steps,
            maps,
        })
    }
}

/// The picked map, by index into [`MapOverlays::maps`]; `None`: off.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MapOverlay {
    pub active: Option<usize>,
}

/// The map after `active` among `len` maps (`back`: before it): off, then
/// the first (last) map, through to the last (first), then off again.
pub fn step_map(active: Option<usize>, len: usize, back: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match (active, back) {
        (None, false) => Some(0),
        (None, true) => Some(len - 1),
        (Some(i), false) => (i + 1 < len).then_some(i + 1),
        (Some(i), true) => i.checked_sub(1),
    }
}

/// One runoff arrow: from a column's surface toward the lower neighbour
/// its water runs to, in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunoffArrow {
    /// The column's top cell.
    pub from: CellPos,
    /// The neighbour's top cell.
    pub to: CellPos,
    pub drop: i16,
}

/// Every runoff arrow within `radius` columns of `centre`, every `spacing`
/// columns (on the grid of multiples of `spacing`), in x-then-z order.
pub fn runoff_arrows(
    world: &World,
    centre: (i32, i32),
    radius: i32,
    spacing: i32,
) -> Vec<RunoffArrow> {
    let spacing = spacing.max(1);
    // The first multiple of `spacing` at or past `v`.
    let snap = |v: i32| (v + spacing - 1).div_euclid(spacing) * spacing;
    let (cx, cz) = centre;
    let mut out = Vec::new();
    let mut x = snap(cx - radius);
    while x <= cx + radius {
        let mut z = snap(cz - radius);
        while z <= cz + radius {
            if let Some(([dx, dz], drop)) = runoff(world, x, z) {
                let top = i32::from(world.surface_y(x, z));
                out.push(RunoffArrow {
                    from: CellPos::new(x, top, z),
                    to: CellPos::new(x + dx, top - i32::from(drop), z + dz),
                    drop,
                });
            }
            z += spacing;
        }
        x += spacing;
    }
    out
}

/// The runoff arrows around the camera, while the runoff map is picked.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct RunoffArrows(pub Vec<RunoffArrow>);

/// The panel's root.
#[derive(Component, Clone, Copy, Debug)]
pub struct MapPanel;

/// The panel's row for map `.0`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapRow(pub usize);

/// The label text of [`MapRow`] `.0`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MapRowLabel(pub usize);

/// The legend, shown while a map is picked.
#[derive(Component, Clone, Copy, Debug)]
pub struct MapLegend;

/// Swatch `.0` of the legend's color bar.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LegendStep(pub usize);

/// The legend's texts.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum LegendText {
    Title,
    Low,
    High,
    /// The value at the targeted cell.
    Here,
}

#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MapOverlaySystems;

/// Steps through the maps and draws their panel. The app inserts
/// [`MapOverlays`]; without it the plugin does nothing.
pub struct MapOverlayPlugin;

impl Plugin for MapOverlayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<MapOverlay>()
            .init_resource::<RunoffArrows>()
            .init_resource::<MeshColorSource>()
            .add_systems(
                Update,
                (
                    map_keys,
                    follow_map,
                    spawn_map_panel,
                    update_map_panel,
                    update_readout,
                    collect_arrows,
                )
                    .chain()
                    .in_set(MapOverlaySystems)
                    .run_if(resource_exists::<MapOverlays>),
            );
        if app.is_plugin_added::<GizmoPlugin>() {
            app.add_systems(
                Update,
                draw_arrows
                    .after(MapOverlaySystems)
                    .run_if(resource_exists::<MapOverlays>),
            );
        }
    }
}

fn map_keys(
    keys: Option<Res<ButtonInput<KeyCode>>>,
    screens: Option<Res<UiScreens>>,
    overlays: Res<MapOverlays>,
    mut map: ResMut<MapOverlay>,
) {
    let Some(keys) = keys else {
        return;
    };
    if !keys.just_pressed(MAP_KEY) || screens.is_some_and(|s| s.blocks_play()) {
        return;
    }
    let back = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    map.active = step_map(map.active, overlays.maps.len(), back);
    info!(map = ?map.active.and_then(|i| overlays.maps.get(i)).map(|m| &m.key), "data map picked");
}

/// Writes the picked map's color source when the pick changes, so the
/// editor's layer overlay may set it between picks. A substance map waits
/// for the eco tables.
fn follow_map(
    map: Res<MapOverlay>,
    overlays: Res<MapOverlays>,
    eco: Option<Res<ClientEco>>,
    mut source: ResMut<MeshColorSource>,
    mut last: Local<Option<Option<usize>>>,
) {
    if *last == Some(map.active) {
        return;
    }
    let want = match map.active {
        None => Some(ColorSource::Material),
        Some(i) => overlays
            .maps
            .get(i)
            .and_then(|m| m.color_source(eco.as_deref())),
    };
    let Some(want) = want else {
        return;
    };
    *last = Some(map.active);
    if source.0 != want {
        source.0 = want;
    }
}

fn panel_text(
    theme: &UiTheme,
    fonts: &UiFonts,
    role: TextRole,
    color: Color,
    value: impl Into<String>,
) -> impl Bundle {
    (
        // The kit restyles the font with the UI scale; the color is the
        // panel's own.
        UiText {
            role,
            color: ColorToken::Text,
        },
        Text::new(value),
        TextFont::from_font_size(theme.font_size(role)).with_font(fonts.get(role)),
        TextColor(color),
    )
}

fn srgb([r, g, b]: [u8; 3]) -> Color {
    Color::srgb_u8(r, g, b)
}

fn spawn_map_panel(
    mut commands: Commands,
    theme: Option<Res<UiTheme>>,
    fonts: Option<Res<UiFonts>>,
    overlays: Res<MapOverlays>,
    existing: Query<(), With<MapPanel>>,
) {
    let (Some(theme), Some(fonts)) = (theme, fonts) else {
        return;
    };
    if !existing.is_empty() {
        return;
    }
    let (s, look) = (&theme.space, &overlays.panel);
    let rows: Vec<Entity> = overlays
        .maps
        .iter()
        .enumerate()
        .map(|(i, m)| {
            commands
                .spawn((
                    MapRow(i),
                    Node {
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        column_gap: Val::Px(s.sm),
                        padding: UiRect::axes(Val::Px(s.sm), Val::Px(s.xs)),
                        border_radius: BorderRadius::all(Val::Px(look.radius / 2.0)),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                    children![
                        (
                            Node {
                                width: Val::Px(look.swatch),
                                height: Val::Px(look.swatch),
                                border: UiRect::all(Val::Px(theme.border)),
                                border_radius: BorderRadius::all(Val::Px(look.swatch / 2.0)),
                                ..default()
                            },
                            BackgroundColor(srgb(m.swatch())),
                            BorderColor::all(look.border),
                        ),
                        (
                            MapRowLabel(i),
                            panel_text(&theme, &fonts, TextRole::Body, look.text, m.label.clone()),
                        ),
                    ],
                ))
                .id()
        })
        .collect();
    let steps: Vec<Entity> = (0..overlays.legend_steps)
        .map(|k| {
            commands
                .spawn((
                    LegendStep(k),
                    Node {
                        flex_grow: 1.0,
                        height: Val::Px(look.swatch),
                        ..default()
                    },
                    BackgroundColor(Color::NONE),
                ))
                .id()
        })
        .collect();
    let bar = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                border: UiRect::all(Val::Px(theme.border)),
                border_radius: BorderRadius::all(Val::Px(look.swatch / 2.0)),
                overflow: Overflow::clip(),
                ..default()
            },
            BorderColor::all(look.border),
        ))
        .add_children(&steps)
        .id();
    let ends = commands
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            },
            children![
                (
                    LegendText::Low,
                    panel_text(&theme, &fonts, TextRole::Caption, look.text_dim, "")
                ),
                (
                    LegendText::High,
                    panel_text(&theme, &fonts, TextRole::Caption, look.text_dim, "")
                ),
            ],
        ))
        .id();
    let title = commands
        .spawn((
            LegendText::Title,
            panel_text(&theme, &fonts, TextRole::Label, look.text, ""),
        ))
        .id();
    let here = commands
        .spawn((
            LegendText::Here,
            panel_text(&theme, &fonts, TextRole::Body, look.text, ""),
        ))
        .id();
    let legend = commands
        .spawn((
            MapLegend,
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(s.xs),
                padding: UiRect::top(Val::Px(s.sm)),
                border: UiRect::top(Val::Px(theme.border)),
                ..default()
            },
            BorderColor::all(look.border),
        ))
        .add_children(&[title, bar, ends, here])
        .id();
    let list = commands
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(s.xs / 2.0),
            ..default()
        })
        .add_children(&rows)
        .id();
    let heading = commands
        .spawn(panel_text(
            &theme,
            &fonts,
            TextRole::Label,
            look.text,
            "Data maps",
        ))
        .id();
    let hint = commands
        .spawn(panel_text(
            &theme,
            &fonts,
            TextRole::Caption,
            look.text_dim,
            "M next · Shift+M back",
        ))
        .id();
    commands
        .spawn((
            MapPanel,
            HudSlot(HudAnchor::Right),
            Node {
                width: Val::Px(look.width),
                display: Display::None,
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(s.sm),
                padding: UiRect::all(Val::Px(s.md)),
                border: UiRect::all(Val::Px(theme.border)),
                border_radius: BorderRadius::all(Val::Px(look.radius)),
                ..default()
            },
            BackgroundColor(look.surface),
            BorderColor::all(look.border),
            BoxShadow::new(
                look.shadow,
                Val::ZERO,
                Val::Px(s.xs),
                Val::ZERO,
                Val::Px(s.lg),
            ),
            Pickable::IGNORE,
        ))
        .add_children(&[heading, hint, list, legend]);
}

#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn update_map_panel(
    map: Res<MapOverlay>,
    overlays: Res<MapOverlays>,
    added: Query<(), Added<MapPanel>>,
    mut panels: Query<&mut Node, With<MapPanel>>,
    mut rows: Query<(&MapRow, &mut BackgroundColor), Without<LegendStep>>,
    mut labels: Query<(&MapRowLabel, &mut TextColor), Without<LegendText>>,
    mut steps: Query<(&LegendStep, &mut BackgroundColor), Without<MapRow>>,
    mut texts: Query<(&LegendText, &mut Text)>,
) {
    if !map.is_changed() && added.is_empty() {
        return;
    }
    let look = &overlays.panel;
    let picked = map.active.and_then(|i| overlays.maps.get(i));
    for mut node in &mut panels {
        node.display = if picked.is_some() {
            Display::Flex
        } else {
            Display::None
        };
    }
    for (row, mut bg) in &mut rows {
        let on = map.active == Some(row.0);
        bg.set_if_neq(BackgroundColor(if on {
            look.selected
        } else {
            Color::NONE
        }));
    }
    for (label, mut color) in &mut labels {
        let on = map.active == Some(label.0);
        color.set_if_neq(TextColor(if on { look.selected_text } else { look.text }));
    }
    let Some(m) = picked else {
        return;
    };
    let cmap = &m.colormap;
    let last = overlays.legend_steps.saturating_sub(1).max(1) as i32;
    for (step, mut bg) in &mut steps {
        let span = i32::from(cmap.max) - i32::from(cmap.min);
        let v = i32::from(cmap.min) + span * step.0 as i32 / last;
        bg.set_if_neq(BackgroundColor(srgb(cmap.rgb(v as i16))));
    }
    for (which, mut text) in &mut texts {
        let value = match which {
            LegendText::Title => m.label.clone(),
            LegendText::Low => m.legend.0.clone(),
            LegendText::High => m.legend.1.clone(),
            LegendText::Here => continue,
        };
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// Writes the picked map's value at the targeted cell into the legend.
fn update_readout(
    map: Res<MapOverlay>,
    overlays: Res<MapOverlays>,
    hud: Option<Res<HudState>>,
    replica: Option<Res<Replica>>,
    eco: Option<Res<ClientEco>>,
    unit: Option<Res<TemperatureUnit>>,
    mut texts: Query<(&LegendText, &mut Text)>,
) {
    let value = map
        .active
        .and_then(|i| overlays.maps.get(i))
        .zip(hud.as_ref().and_then(|h| h.target))
        .zip(replica.as_deref())
        .and_then(|((m, cell), replica)| {
            let v = m.value_at(replica.world(), cell, eco.as_deref())?;
            Some(m.readout(v, shown_unit(unit.as_deref())))
        });
    let line = value.map_or_else(|| "Here: —".to_string(), |v| format!("Here: {v}"));
    for (which, mut text) in &mut texts {
        if *which == LegendText::Here && text.0 != line {
            text.0 = line.clone();
        }
    }
}

/// Re-reads the runoff arrows around the camera while the runoff map is
/// picked: when the camera moves to another column, or now and then while
/// the replica changes.
#[allow(clippy::too_many_arguments)]
fn collect_arrows(
    map: Res<MapOverlay>,
    overlays: Res<MapOverlays>,
    replica: Option<Res<Replica>>,
    cameras: Query<&GlobalTransform, With<GameCamera>>,
    mut arrows: ResMut<RunoffArrows>,
    mut at: Local<Option<(i32, i32)>>,
    mut since: Local<u32>,
) {
    let on = map
        .active
        .and_then(|i| overlays.maps.get(i))
        .is_some_and(|m| m.source == MapSource::Runoff);
    let (Some(replica), Ok(camera), true) = (replica, cameras.single(), on) else {
        if !arrows.0.is_empty() {
            arrows.0.clear();
        }
        *at = None;
        return;
    };
    let p = camera.translation() / CELL_SIZE_M;
    let column = (p.x.floor() as i32, p.z.floor() as i32);
    *since = since.saturating_add(1);
    let stale = *at != Some(column)
        || map.is_changed()
        || (replica.is_changed() && *since >= ARROW_REFRESH_FRAMES);
    if !stale {
        return;
    }
    *at = Some(column);
    *since = 0;
    let a = &overlays.arrows;
    arrows.0 = runoff_arrows(replica.world(), column, a.radius, a.spacing);
}

fn draw_arrows(arrows: Res<RunoffArrows>, overlays: Res<MapOverlays>, mut gizmos: Gizmos) {
    let a = &overlays.arrows;
    let at = |c: CellPos| {
        Vec3::new(
            c.0.x as f32 + 0.5,
            c.0.y as f32 + 1.0 + a.lift,
            c.0.z as f32 + 0.5,
        ) * CELL_SIZE_M
    };
    for arrow in &arrows.0 {
        let (from, to) = (at(arrow.from), at(arrow.to));
        // Level with the column's top, a cell long: the slope reads from
        // the colors underneath.
        let end = Vec3::new(to.x, from.y, to.z);
        gizmos.arrow(from, end, a.color);
    }
}

/// The contrast of `text` over `surface`, with `surface` composited over
/// `under` (the panel's surface may be translucent).
pub fn contrast_over(text: Color, surface: Color, under: Color) -> f32 {
    let (s, u) = (surface.to_srgba(), under.to_srgba());
    let mix = |a: f32, b: f32| a * s.alpha + b * (1.0 - s.alpha);
    let flat = Color::srgb(
        mix(s.red, u.red),
        mix(s.green, u.green),
        mix(s.blue, u.blue),
    );
    contrast_ratio(text, flat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::UiKitPlugin;
    use crate::ui::theme::tests::THEME_PATH;
    use rmc_world::chunk::Chunk;
    use rmc_world::coords::{CellAabb, ChunkPos, LocalPos};
    use rmc_world::layers::SubstanceLayers;
    use rmc_world::materials::MaterialTable;
    use std::sync::Arc;

    const OVERLAYS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/overlays.ron");
    const MATERIALS_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/materials.ron");

    fn overlays() -> MapOverlays {
        MapOverlays::load(OVERLAYS_PATH).expect("assets/overlays.ron loads")
    }

    fn index(o: &MapOverlays, source: &MapSource) -> usize {
        o.maps
            .iter()
            .position(|m| &m.source == source)
            .unwrap_or_else(|| panic!("a {source:?} map"))
    }

    #[test]
    fn the_shipped_maps_load_and_read_clearly() {
        let o = overlays();
        for source in [
            MapSource::Substance("n".into()),
            MapSource::Substance("p".into()),
            MapSource::Substance("k".into()),
            MapSource::Moisture,
            MapSource::WaterTable,
            MapSource::Runoff,
        ] {
            index(&o, &source);
        }
        // Each map has a color of its own.
        let swatches: BTreeSet<[u8; 3]> = o.maps.iter().map(MapDef::swatch).collect();
        assert_eq!(swatches.len(), o.maps.len(), "one color per map");
        // The panel is light and its text reads, over a dark or a light
        // scene behind it.
        let p = &o.panel;
        let s = p.surface.to_srgba();
        assert!(s.red.min(s.green).min(s.blue) > 0.9, "a white panel");
        for under in [Color::BLACK, Color::WHITE] {
            for (name, text, surface) in [
                ("text", p.text, p.surface),
                ("text_dim", p.text_dim, p.surface),
                ("selected_text", p.selected_text, p.selected),
            ] {
                let c = contrast_over(text, surface, under);
                assert!(c >= 4.5, "{name}: contrast {c}");
            }
        }
    }

    #[test]
    fn bad_tables_are_refused() {
        let good = std::fs::read_to_string(OVERLAYS_PATH).unwrap();
        assert!(MapOverlays::parse(&good).is_ok());
        let bad = |from: &str, to: &str| {
            assert!(good.contains(from), "{from:?} is in the file");
            let text = good.replacen(from, to, 1);
            assert!(MapOverlays::parse(&text).is_err(), "{to:?} is refused");
        };
        bad("high: \"#1E8E3E\"", "high: \"green\"");
        bad("range: (0, 64)", "range: (64, 64)");
        bad("key: \"phosphorus\"", "key: \"nitrogen\"");
        bad("legend_steps: 8", "legend_steps: 1");
        bad("spacing: 2", "spacing: 0");
        let start = good.find("maps: [").unwrap();
        let end = good.rfind(']').unwrap();
        let empty = format!("{}maps: []{}", &good[..start], &good[end + 1..]);
        assert!(MapOverlays::parse(&empty).is_err(), "no maps");
    }

    #[test]
    fn m_steps_through_the_maps_and_back_to_off() {
        let mut at = None;
        let mut seen = Vec::new();
        for _ in 0..4 {
            at = step_map(at, 3, false);
            seen.push(at);
        }
        assert_eq!(seen, [Some(0), Some(1), Some(2), None]);
        assert_eq!(step_map(None, 3, true), Some(2));
        assert_eq!(step_map(Some(0), 3, true), None);
        assert_eq!(step_map(None, 0, false), None);
    }

    #[test]
    fn readouts_name_their_units() {
        let o = overlays();
        let map = |s: MapSource| &o.maps[index(&o, &s)];
        let table = map(MapSource::WaterTable);
        let max = table.colormap.max;
        let unit = TemperatureUnit::Celsius;
        assert_eq!(table.readout(0, unit), "at the surface");
        assert_eq!(table.readout(3, unit), "3 cells down");
        assert_eq!(table.readout(max, unit), format!("deeper than {max} cells"));
        assert_eq!(map(MapSource::Runoff).readout(0, unit), "flat");
        assert_eq!(map(MapSource::Runoff).readout(2, unit), "drops 2");
        assert_eq!(
            map(MapSource::Temperature).readout(215, unit),
            unit.format(215)
        );
        assert_eq!(map(MapSource::Moisture).readout(40, unit), "40");
    }

    fn eco() -> ClientEco {
        use rmc_world::layers::N_SUBSTANCES;
        use rmc_world::query::{PhRule, ToxProfile};
        ClientEco {
            substances: SubstanceLayers::new(["n", "p", "k"]).unwrap(),
            gases: Vec::new(),
            ph: PhRule {
                acid: None,
                lime: None,
                ph_shift: 0,
            },
            player_tox: ToxProfile::new([None; N_SUBSTANCES]),
            plant_tox: ToxProfile::new([None; N_SUBSTANCES]),
            tint: Vec::new(),
            gas_tint: Vec::new(),
            flammable: Vec::new(),
            plants: Vec::new(),
            plant_looks: Vec::new(),
            deficiency: Vec::new(),
        }
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_message::<AppExit>()
            .add_plugins(UiKitPlugin {
                theme: UiTheme::load(THEME_PATH).unwrap(),
                assets_dir: None,
            })
            .insert_resource(overlays())
            .add_plugins(MapOverlayPlugin);
        app.update();
        app
    }

    fn tap(app: &mut App, key: KeyCode, shift: bool) {
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        if shift {
            keys.press(KeyCode::ShiftLeft);
        }
        keys.press(key);
        app.update();
        let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
        keys.release_all();
        keys.clear();
        app.update();
    }

    fn source(app: &App) -> ColorSource {
        app.world().resource::<MeshColorSource>().0
    }

    #[test]
    fn m_picks_a_map_that_colors_the_meshes() {
        let mut app = app();
        let o = overlays();
        assert_eq!(source(&app), ColorSource::Material);

        tap(&mut app, MAP_KEY, false);
        assert_eq!(app.world().resource::<MapOverlay>().active, Some(0));
        // The first map is a substance's: it waits for the eco tables.
        assert!(matches!(o.maps[0].source, MapSource::Substance(_)));
        assert_eq!(source(&app), ColorSource::Material);
        app.insert_resource(eco());
        app.update();
        let n = eco().substances.layer("n").unwrap();
        assert_eq!(source(&app), ColorSource::Layer(n, o.maps[0].colormap));

        // Shift+M from the first map turns the maps off.
        tap(&mut app, MAP_KEY, true);
        assert_eq!(app.world().resource::<MapOverlay>().active, None);
        assert_eq!(source(&app), ColorSource::Material);

        // A column map colors by its column field.
        let table = index(&o, &MapSource::WaterTable);
        app.world_mut().resource_mut::<MapOverlay>().active = Some(table);
        app.update();
        assert_eq!(
            source(&app),
            ColorSource::Column(ColumnField::WaterTable, o.maps[table].colormap)
        );

        // Between picks, the editor's overlay may hold the meshes.
        let editor = ColorSource::layer(LayerId::Light);
        app.world_mut().resource_mut::<MeshColorSource>().0 = editor;
        app.update();
        assert_eq!(source(&app), editor);
    }

    #[test]
    fn the_panel_lists_the_maps_and_marks_the_picked_one() {
        let mut app = app();
        let o = overlays();
        let display = |app: &mut App| {
            let w = app.world_mut();
            w.query_filtered::<&Node, With<MapPanel>>()
                .single(w)
                .expect("one panel")
                .display
        };
        assert_eq!(display(&mut app), Display::None, "hidden while off");
        let w = app.world_mut();
        let rows = w.query::<&MapRow>().iter(w).count();
        assert_eq!(rows, o.maps.len());

        let moisture = index(&o, &MapSource::Moisture);
        app.world_mut().resource_mut::<MapOverlay>().active = Some(moisture);
        app.update();
        assert_eq!(display(&mut app), Display::Flex);
        let w = app.world_mut();
        for (row, bg) in w.query::<(&MapRow, &BackgroundColor)>().iter(w) {
            let want = if row.0 == moisture {
                o.panel.selected
            } else {
                Color::NONE
            };
            assert_eq!(bg.0, want, "row {}", row.0);
        }
        let cmap = o.maps[moisture].colormap;
        let mut steps: Vec<(usize, Color)> = w
            .query::<(&LegendStep, &BackgroundColor)>()
            .iter(w)
            .map(|(s, bg)| (s.0, bg.0))
            .collect();
        steps.sort_by_key(|s| s.0);
        assert_eq!(steps.len(), o.legend_steps);
        assert_eq!(steps[0].1, srgb(cmap.low), "the bar starts at the low end");
        assert_eq!(
            steps.last().unwrap().1,
            srgb(cmap.high),
            "and ends at the high end"
        );
        let texts: Vec<(LegendText, String)> = w
            .query::<(&LegendText, &Text)>()
            .iter(w)
            .map(|(l, t)| (*l, t.0.clone()))
            .collect();
        let text = |which| texts.iter().find(|t| t.0 == which).unwrap().1.clone();
        assert_eq!(text(LegendText::Title), o.maps[moisture].label);
        assert_eq!(text(LegendText::Low), o.maps[moisture].legend.0);
        assert_eq!(text(LegendText::High), o.maps[moisture].legend.1);
        assert_eq!(text(LegendText::Here), "Here: —", "no target, no value");
    }

    #[test]
    fn runoff_arrows_point_downhill() {
        let t = Arc::new(MaterialTable::load(MATERIALS_PATH).unwrap());
        let (loam, air) = (t.id("loam").unwrap(), t.id("air").unwrap());
        let mut world = World::new(
            t.clone(),
            CellAabb::new(CellPos::new(0, 0, 0), CellPos::new(31, 31, 31)),
        );
        // A ramp falling toward +x: the surface is at 20 - x.
        let mut chunk = Chunk::uniform(air);
        for x in 0..16 {
            for z in 0..32 {
                for y in 0..=(20 - x) {
                    chunk.storage.set(LocalPos::new(x, y, z), loam);
                }
            }
        }
        world.insert_chunk(ChunkPos::new(0, 0, 0), chunk).unwrap();

        let arrows = runoff_arrows(&world, (8, 8), 4, 2);
        assert!(!arrows.is_empty());
        for a in &arrows {
            assert_eq!(a.from.0.x % 2, 0, "on the spacing grid");
            assert_eq!(a.from.0.z % 2, 0);
            assert!((a.from.0.x - 8).abs() <= 4 && (a.from.0.z - 8).abs() <= 4);
            assert_eq!(a.to.0.x - a.from.0.x, 1, "down the ramp");
            assert_eq!(a.to.0.z, a.from.0.z);
            assert!(a.drop > 0 && a.to.0.y == a.from.0.y - i32::from(a.drop));
            assert_eq!(
                i32::from(world.surface_y(a.from.0.x, a.from.0.z)),
                a.from.0.y
            );
        }
        // Sorted x then z, so the same world gives the same arrows.
        let keys: Vec<(i32, i32)> = arrows.iter().map(|a| (a.from.0.x, a.from.0.z)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted);
    }
}
