//! Season and weather on screen: a HUD chip with the season, year, air
//! temperature and sky, and rain (or snow) falling around the camera while
//! the season's rain lands.
//!
//! [`ClientSeasons`] holds the season rows of `seasons.ron` (the app and the
//! test kit insert it from the eco tables). The current season follows
//! [`SeasonSummary`]: the running season, else the one after the last that
//! finished, else season 0. Season `n` uses row `n % len`, as the host does.
//!
//! [`Weather`] is what the HUD and the rain read. Its `rain` is the
//! season's rain as a share of the wettest row's, counting only what lands
//! (a row's rain of 1 or less lands nothing, SAD-P2 §7 rain). Below 0 °C
//! the precipitation is drawn as snow. While [`SeasonClockPaused`] is set
//! (the app's `P`), nothing falls.
//!
//! With the game calendar the host sends a `Clock` each hour
//! ([`GameTime`]): the weather is then that hour's ([`weather_at`]), its
//! rain a share of the calendar's wettest hour and its snow the host's,
//! and the chip shows the day and the time ("Spring · Day 3 · Year 1",
//! "14:15 · 24.5 C · Rain").
//!
//! Drops are thin unlit streaks spawned in a cylinder around the
//! [`GameCamera`], none within [`RAIN_NEAR_M`] of it. Each falls until it reaches the top of its cell column
//! in the replica (so it does not rain in caves or through roofs) or drops
//! below the cylinder, then respawns at the top. The drops are visual only:
//! the rain that lands is the host's season pass.

use bevy::light::NotShadowCaster;
use bevy::prelude::*;
use rmc_protocol::GameClock;
use rmc_world::coords::CELL_SIZE_M;

use crate::daynight::{ClockPace, GameTime, clock_label, follow_clock};
use crate::eco::SeasonSummary;
use crate::hud::{TemperatureUnit, shown_unit};
use crate::net::FromHost;
use crate::render::GameCamera;
use crate::replica::Replica;
use crate::ui::{ColorToken, HudAnchor, HudSlot, TextRole, UiFonts, UiTheme, text};

/// Most drops at full rain.
pub const MAX_DROPS: usize = 1500;
/// Radius of the cylinder of drops around the camera, in meters.
pub const RAIN_RADIUS_M: f32 = 8.0;
/// No drop falls closer to the camera than this, in meters: a streak
/// right in front of the eye would fill the view.
pub const RAIN_NEAR_M: f32 = 1.5;
/// Height of the cylinder above the camera, in meters; drops end this far
/// below it at the latest.
pub const RAIN_HEIGHT_M: f32 = 8.0;
/// Fall speed of a rain streak, m/s.
pub const RAIN_SPEED_MPS: f32 = 11.0;
/// Fall speed of a snowflake, m/s.
pub const SNOW_SPEED_MPS: f32 = 1.4;
/// Drops added per frame while the count grows, so rain sets in.
const SPAWN_PER_FRAME: usize = 60;
/// A rain streak's width and length, in meters.
const STREAK_M: (f32, f32) = (0.012, 0.5);
/// A snowflake's edge, in meters.
const FLAKE_M: f32 = 0.045;
/// Below this share of the wettest season's rain it is light rain; from
/// [`HEAVY_RAIN`] it is heavy.
const LIGHT_RAIN: f32 = 0.4;
const HEAVY_RAIN: f32 = 0.9;

/// One row of `seasons.ron`, as the client reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeasonView {
    pub key: String,
    /// 0.1 °C.
    pub air_temp: i16,
    /// Per-column rain per sub-step, before the seeded pattern (`/ 256`).
    pub rain: u8,
}

/// The season cycle (`seasons.ron` rows in order).
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientSeasons {
    pub rows: Vec<SeasonView>,
}

/// Set while the season clock is paused or off: no rain falls.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeasonClockPaused(pub bool);

/// What the sky is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sky {
    Dry,
    /// Dry and below 0 °C.
    Frost,
    Rain,
    Snow,
}

/// The season and weather the HUD shows and the rain follows.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct Weather {
    /// Season number (0 is the first).
    pub season: u32,
    /// 1 for the first pass through the rows.
    pub year: u32,
    /// The row's key, or empty without rows.
    pub key: String,
    /// 0.1 °C.
    pub air_temp: i16,
    /// The rain that lands, as a share of the wettest row's: 0 to 1.
    pub rain: f32,
    /// Whether what falls is snow: below 0 °C, or the calendar hour's.
    pub snow: bool,
    pub paused: bool,
    /// The calendar's day and time; `None` without a clock.
    pub clock: Option<ClockView>,
}

/// The calendar day and time the chip shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClockView {
    /// Day of the season, from 0.
    pub day: u8,
    pub days: u8,
    /// Time of day in hours, from 0 to `hours`.
    pub time_h: f32,
    pub hours: u8,
}

impl Weather {
    pub fn sky(&self) -> Sky {
        match (self.rain > 0.0, self.snow, self.air_temp < 0) {
            (true, false, _) => Sky::Rain,
            (true, true, _) => Sky::Snow,
            (false, _, false) => Sky::Dry,
            (false, _, true) => Sky::Frost,
        }
    }

    /// The share of [`MAX_DROPS`] falling now: the rain, or 0 while paused.
    pub fn falling(&self) -> f32 {
        if self.paused { 0.0 } else { self.rain }
    }
}

/// The season after `summary`: the running one, else the one after the
/// last that finished, else 0.
pub fn current_season(summary: &SeasonSummary) -> u32 {
    summary
        .running
        .or(summary.last.as_ref().map(|d| d.season + 1))
        .unwrap_or(0)
}

/// The weather of season `season` (see the module docs).
pub fn weather(seasons: &ClientSeasons, season: u32, paused: bool) -> Weather {
    let len = seasons.rows.len() as u32;
    let Some(row) = (len > 0).then(|| &seasons.rows[(season % len) as usize]) else {
        return Weather {
            season,
            year: 1,
            paused,
            ..Weather::default()
        };
    };
    // A rain of 1 or less lands nothing (`rain · pattern / 256`).
    let lands = |rain: u8| f32::from(rain.saturating_sub(1));
    let wettest = seasons
        .rows
        .iter()
        .map(|r| lands(r.rain))
        .fold(0.0, f32::max);
    Weather {
        season,
        year: season / len + 1,
        key: row.key.clone(),
        air_temp: row.air_temp,
        rain: if wettest > 0.0 {
            lands(row.rain) / wettest
        } else {
            0.0
        },
        snow: row.air_temp < 0,
        paused,
        clock: None,
    }
}

/// The weather of the calendar hour `clock`, at `time_h` of its day: the
/// season's name from `seasons`, the hour's air and rain (as a share of
/// the calendar's wettest hour) and its snow.
pub fn weather_at(
    seasons: &ClientSeasons,
    clock: &GameClock,
    time_h: f32,
    paused: bool,
) -> Weather {
    let lands = |rain: u8| f32::from(rain.saturating_sub(1));
    let peak = lands(clock.peak_rain);
    Weather {
        air_temp: clock.air_temp,
        rain: if peak > 0.0 {
            (lands(clock.rain) / peak).min(1.0)
        } else {
            0.0
        },
        snow: clock.snow,
        clock: Some(ClockView {
            day: clock.day,
            days: clock.days,
            time_h,
            hours: clock.hours,
        }),
        ..weather(seasons, clock.season, paused)
    }
}

/// `key` with its first letter upper case.
fn title(key: &str) -> String {
    let mut chars = key.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_uppercase().chain(chars).collect::<String>()
    })
}

/// The sky in words: "Dry", "Frost", or the rain or snow with "Light" or
/// "Heavy" by its share.
pub fn sky_label(w: &Weather) -> String {
    let fall = match w.sky() {
        Sky::Dry => return "Dry".into(),
        Sky::Frost => return "Frost".into(),
        Sky::Rain => "rain",
        Sky::Snow => "snow",
    };
    if w.rain < LIGHT_RAIN {
        format!("Light {fall}")
    } else if w.rain >= HEAVY_RAIN {
        format!("Heavy {fall}")
    } else {
        title(fall)
    }
}

/// The HUD chip's lines in Celsius (see [`weather_lines_in`]).
pub fn weather_lines(w: &Weather) -> [String; 3] {
    weather_lines_in(w, TemperatureUnit::Celsius)
}

/// The HUD chip's lines: the season and year, the air temperature (in
/// `unit`) and sky, and a note while the clock is paused (empty otherwise).
pub fn weather_lines_in(w: &Weather, unit: TemperatureUnit) -> [String; 3] {
    let season = if w.key.is_empty() {
        format!("Season {}", w.season)
    } else {
        title(&w.key)
    };
    let (day, time) = match w.clock {
        Some(c) => (
            format!(" · Day {}", c.day + 1),
            format!("{} · ", clock_label(c.time_h, c.hours)),
        ),
        None => (String::new(), String::new()),
    };
    [
        format!("{season}{day} · Year {}", w.year),
        format!("{time}{} · {}", unit.format(w.air_temp), sky_label(w)),
        if w.paused {
            "Seasons paused (P)".into()
        } else {
            String::new()
        },
    ]
}

/// The HUD chip.
#[derive(Component, Debug)]
pub struct WeatherHud;

/// Line `n` of the HUD chip (see [`weather_lines`]).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct WeatherLine(pub usize);

/// A falling drop; `snow` says which mesh it has.
#[derive(Component, Debug, Clone, Copy)]
pub struct RainDrop {
    pub snow: bool,
    /// The top of its cell column, in meters: it ends there.
    pub ground: f32,
}

/// The drop meshes and materials, made on first use.
#[derive(Resource, Clone)]
struct RainAssets {
    rain: (Handle<Mesh>, Handle<StandardMaterial>),
    snow: (Handle<Mesh>, Handle<StandardMaterial>),
}

/// The drops' random positions (xorshift64*; looks only).
#[derive(Resource)]
struct RainRng(u64);

impl RainRng {
    /// Uniform in `[0, 1)`.
    fn next(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct WeatherSystems;

/// The weather, its HUD chip and the rain. Insert [`ClientSeasons`]; the
/// HUD chip needs the UI kit (`UiTheme`), the drops need mesh and material
/// assets, and the drop floors need the [`Replica`].
pub struct WeatherPlugin;

impl Plugin for WeatherPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ClientSeasons>()
            .add_message::<FromHost>()
            .init_resource::<GameTime>()
            .init_resource::<SeasonSummary>()
            .init_resource::<SeasonClockPaused>()
            .init_resource::<Weather>()
            .insert_resource(RainRng(0x9e37_79b9_7f4a_7c15))
            .add_systems(
                Update,
                (
                    follow_clock,
                    update_weather,
                    spawn_weather_hud,
                    update_weather_hud,
                    run_rain,
                )
                    .chain()
                    .in_set(WeatherSystems),
            );
    }
}

fn update_weather(
    seasons: Res<ClientSeasons>,
    summary: Res<SeasonSummary>,
    paused: Res<SeasonClockPaused>,
    game_time: Res<GameTime>,
    pace: Option<Res<ClockPace>>,
    mut out: ResMut<Weather>,
) {
    let now = game_time.clock.zip(game_time.time_h(pace.map(|p| p.0)));
    let w = match now {
        Some((clock, t)) => weather_at(&seasons, &clock, t, paused.0),
        None => weather(&seasons, current_season(&summary), paused.0),
    };
    if *out != w {
        *out = w;
    }
}

fn spawn_weather_hud(
    mut commands: Commands,
    theme: Option<Res<UiTheme>>,
    fonts: Option<Res<UiFonts>>,
    existing: Query<(), With<WeatherHud>>,
) {
    let (Some(theme), Some(fonts)) = (theme, fonts) else {
        return;
    };
    if !existing.is_empty() {
        return;
    }
    let s = &theme.space;
    let line = |n: usize, role: TextRole, color: ColorToken| {
        (WeatherLine(n), text(&theme, &fonts, role, color, ""))
    };
    commands.spawn((
        WeatherHud,
        HudSlot(HudAnchor::BottomLeft),
        Node {
            flex_direction: FlexDirection::Column,
            padding: UiRect::axes(Val::Px(s.md), Val::Px(s.sm)),
            row_gap: Val::Px(s.xs),
            border: UiRect::all(Val::Px(theme.border)),
            border_radius: BorderRadius::all(Val::Px(theme.radius.md)),
            ..default()
        },
        BackgroundColor(theme.colors.surface),
        BorderColor::all(theme.colors.border),
        Pickable::IGNORE,
        children![
            line(0, TextRole::Label, ColorToken::Text),
            line(1, TextRole::Body, ColorToken::Text),
            line(2, TextRole::Caption, ColorToken::Warning),
        ],
    ));
}

fn update_weather_hud(
    weather: Res<Weather>,
    mut lines: Query<(&WeatherLine, &mut Text, &mut Node)>,
    added: Query<(), Added<WeatherLine>>,
    unit: Option<Res<TemperatureUnit>>,
) {
    let unit_changed = unit.as_ref().is_some_and(|u| u.is_changed());
    if !weather.is_changed() && added.is_empty() && !unit_changed {
        return;
    }
    let values = weather_lines_in(&weather, shown_unit(unit.as_deref()));
    for (line, mut text, mut node) in &mut lines {
        let value = values.get(line.0).cloned().unwrap_or_default();
        node.display = if value.is_empty() {
            Display::None
        } else {
            Display::Flex
        };
        if text.0 != value {
            text.0 = value;
        }
    }
}

/// The top of cell column `(x, z)` in meters (in the replica), or `None`
/// with no surface or no replica.
fn column_top(replica: Option<&Replica>, x: f32, z: f32) -> Option<f32> {
    let world = replica?.world();
    let cell = |m: f32| (m / CELL_SIZE_M).floor() as i32;
    let y = world.surface_y(cell(x), cell(z));
    (y != rmc_world::columns::NO_SURFACE).then(|| f32::from(y + 1) * CELL_SIZE_M)
}

fn rain_assets(meshes: &mut Assets<Mesh>, materials: &mut Assets<StandardMaterial>) -> RainAssets {
    let mut unlit = |color: Color| {
        materials.add(StandardMaterial {
            base_color: color,
            unlit: true,
            alpha_mode: AlphaMode::Blend,
            ..default()
        })
    };
    let rain_material = unlit(Color::srgba(0.70, 0.78, 0.92, 0.6));
    let snow_material = unlit(Color::srgba(0.97, 0.98, 1.0, 0.9));
    RainAssets {
        rain: (
            meshes.add(Cuboid::new(STREAK_M.0, STREAK_M.1, STREAK_M.0)),
            rain_material,
        ),
        snow: (
            meshes.add(Cuboid::new(FLAKE_M, FLAKE_M, FLAKE_M)),
            snow_material,
        ),
    }
}

/// Places `drop` at a random spot in the cylinder over `eye`: anywhere in
/// its height when `fill`, else in its top half. Hidden when the column's
/// top is above that spot.
fn place_drop(
    rng: &mut RainRng,
    replica: Option<&Replica>,
    eye: Vec3,
    fill: bool,
) -> (Vec3, f32, Visibility) {
    let angle = rng.next() * std::f32::consts::TAU;
    // Uniform over the ring between the near radius and the edge.
    let (near, far) = (RAIN_NEAR_M * RAIN_NEAR_M, RAIN_RADIUS_M * RAIN_RADIUS_M);
    let r = (near + rng.next() * (far - near)).sqrt();
    let x = eye.x + r * angle.cos();
    let z = eye.z + r * angle.sin();
    let low = if fill { -RAIN_HEIGHT_M } else { 0.0 };
    let y = eye.y + low + rng.next() * (RAIN_HEIGHT_M - low);
    let ground = column_top(replica, x, z).unwrap_or(f32::NEG_INFINITY);
    let visible = if ground < y {
        Visibility::Inherited
    } else {
        Visibility::Hidden
    };
    (Vec3::new(x, y, z), ground, visible)
}

#[allow(clippy::too_many_arguments)]
fn run_rain(
    mut commands: Commands,
    time: Res<Time>,
    weather: Res<Weather>,
    replica: Option<Res<Replica>>,
    camera: Query<&Transform, (With<GameCamera>, Without<RainDrop>)>,
    mut drops: Query<(Entity, &mut RainDrop, &mut Transform, &mut Visibility)>,
    mut rng: ResMut<RainRng>,
    assets: Option<Res<RainAssets>>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
) {
    let snow = weather.sky() == Sky::Snow;
    let eye = camera.single().ok().map(|t| t.translation);
    let target = match eye {
        Some(_) => (weather.falling() * MAX_DROPS as f32).round() as usize,
        None => 0,
    };
    // Drop the extra drops and the drops of the other kind.
    let mut count = 0;
    for (e, drop, ..) in &drops {
        if drop.snow != snow || count >= target {
            commands.entity(e).despawn();
        } else {
            count += 1;
        }
    }
    let Some(eye) = eye else {
        return;
    };
    let replica = replica.as_deref();
    let speed = if snow { SNOW_SPEED_MPS } else { RAIN_SPEED_MPS };
    let fall = speed * time.delta_secs();
    for (_, mut drop, mut transform, mut visibility) in &mut drops {
        if drop.snow != snow {
            continue;
        }
        transform.translation.y -= fall;
        let p = transform.translation;
        let off = Vec2::new(p.x - eye.x, p.z - eye.z).length() > RAIN_RADIUS_M;
        if p.y <= drop.ground || p.y < eye.y - RAIN_HEIGHT_M || off {
            let (at, ground, visible) = place_drop(&mut rng, replica, eye, false);
            transform.translation = at;
            *visibility = visible;
            drop.ground = ground;
        }
    }
    let add = target.saturating_sub(count).min(SPAWN_PER_FRAME);
    if add == 0 {
        return;
    }
    let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
        return;
    };
    let handles = match assets.as_deref() {
        Some(a) => a.clone(),
        None => {
            let a = rain_assets(&mut meshes, &mut materials);
            commands.insert_resource(a.clone());
            a
        }
    };
    let (mesh, material) = if snow { handles.snow } else { handles.rain };
    for _ in 0..add {
        let (at, ground, visible) = place_drop(&mut rng, replica, eye, true);
        commands.spawn((
            RainDrop { snow, ground },
            Mesh3d(mesh.clone()),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(at),
            visible,
            NotShadowCaster,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco::SeasonDone;
    use crate::ui::UiKitPlugin;
    use crate::ui::theme::tests::THEME_PATH;
    use rmc_protocol::ServerMsg;

    fn row(key: &str, air_temp: i16, rain: u8) -> SeasonView {
        SeasonView {
            key: key.into(),
            air_temp,
            rain,
        }
    }

    /// The shipped cycle's shape: spring, summer, autumn, a dry winter.
    fn seasons() -> ClientSeasons {
        ClientSeasons {
            rows: vec![
                row("spring", 100, 8),
                row("summer", 220, 6),
                row("autumn", 80, 10),
                row("winter", -80, 1),
            ],
        }
    }

    fn done(season: u32) -> SeasonDone {
        SeasonDone {
            season,
            changed_chunks: 0,
            report: Default::default(),
        }
    }

    #[test]
    fn weather_follows_the_season_and_its_rain() {
        let s = seasons();
        let none = SeasonSummary::default();
        assert_eq!(current_season(&none), 0, "before any season: the first");
        let running = SeasonSummary {
            running: Some(6),
            last: Some(done(5)),
        };
        assert_eq!(current_season(&running), 6, "the running season");
        let between = SeasonSummary {
            running: None,
            last: Some(done(3)),
        };
        assert_eq!(current_season(&between), 4, "the one after the last");

        let spring = weather(&s, 0, false);
        assert_eq!((spring.key.as_str(), spring.year), ("spring", 1));
        assert!((spring.rain - 7.0 / 9.0).abs() < 1e-6, "{}", spring.rain);
        assert_eq!(spring.sky(), Sky::Rain);
        assert_eq!(
            weather_lines(&spring),
            [
                "Spring · Year 1".to_string(),
                "10.0 C · Rain".into(),
                String::new()
            ]
        );

        let autumn = weather(&s, 6, false);
        assert_eq!(
            (autumn.key.as_str(), autumn.year, autumn.rain),
            ("autumn", 2, 1.0)
        );
        assert_eq!(sky_label(&autumn), "Heavy rain");

        // Winter's rain of 1 lands nothing: dry, and below 0 °C, frost.
        let winter = weather(&s, 3, false);
        assert_eq!((winter.rain, winter.sky()), (0.0, Sky::Frost));
        assert_eq!(weather_lines(&winter)[1], "-8.0 C · Frost");

        // Paused: the same season, but nothing falls, and the HUD says so.
        let paused = weather(&s, 2, true);
        assert_eq!(paused.rain, 1.0);
        assert_eq!(paused.falling(), 0.0);
        assert_eq!(weather_lines(&paused)[2], "Seasons paused (P)");

        // Rain below 0 °C is snow; a little of it is light.
        let cold = ClientSeasons {
            rows: vec![row("thaw", 50, 21), row("blizzard", -30, 5)],
        };
        let w = weather(&cold, 1, false);
        assert_eq!(w.sky(), Sky::Snow);
        assert_eq!(sky_label(&w), "Light snow");

        // No rows: a numbered season, dry.
        let w = weather(&ClientSeasons::default(), 5, false);
        assert_eq!(weather_lines(&w)[0], "Season 5 · Year 1");
        assert_eq!(w.sky(), Sky::Dry);
    }

    #[test]
    fn calendar_hours_give_the_day_the_time_and_the_hours_weather() {
        let s = seasons();
        let mut clock = GameClock {
            season: 5,
            day: 2,
            days: 7,
            hour: 13,
            hours: 24,
            air_temp: 245,
            rain: 25,
            peak_rain: 49,
            snow: false,
            daylight: (5, 21),
        };
        let w = weather_at(&s, &clock, 14.25, false);
        assert_eq!((w.key.as_str(), w.year, w.season), ("summer", 2, 5));
        assert_eq!(w.rain, 0.5);
        assert_eq!(w.sky(), Sky::Rain);
        assert_eq!(
            weather_lines(&w),
            [
                "Summer · Day 3 · Year 2".to_string(),
                "14:15 · 24.5 C · Rain".into(),
                String::new()
            ]
        );
        // A dry hour; snow is the host's word, not the temperature's.
        clock.rain = 0;
        assert_eq!(weather_at(&s, &clock, 14.25, false).sky(), Sky::Dry);
        clock.rain = 49;
        clock.snow = true;
        clock.air_temp = 5;
        let w = weather_at(&s, &clock, 3.0, false);
        assert_eq!((w.sky(), sky_label(&w).as_str()), (Sky::Snow, "Heavy snow"));
        clock.snow = false;
        clock.rain = 0;
        clock.air_temp = -40;
        assert_eq!(weather_at(&s, &clock, 3.0, false).sky(), Sky::Frost);
    }

    #[test]
    fn a_clock_from_the_host_drives_the_weather() {
        let mut app = rain_app(seasons());
        set_season(&mut app, 3);
        app.update();
        assert_eq!(app.world().resource::<Weather>().clock, None);
        let clock = GameClock {
            season: 0,
            day: 0,
            days: 7,
            hour: 7,
            hours: 24,
            air_temp: 120,
            rain: 48,
            peak_rain: 48,
            snow: false,
            daylight: (6, 19),
        };
        app.world_mut()
            .write_message(FromHost(ServerMsg::Clock(clock)));
        app.update();
        let w = app.world().resource::<Weather>().clone();
        assert_eq!((w.key.as_str(), w.air_temp, w.rain), ("spring", 120, 1.0));
        assert_eq!(w.clock.map(|c| c.time_h), Some(8.0));
        app.update();
        assert!(!drops(&mut app).is_empty(), "the wet hour rains");
    }

    fn rain_app(seasons: ClientSeasons) -> App {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .insert_resource(Assets::<Mesh>::default())
            .insert_resource(Assets::<StandardMaterial>::default())
            .insert_resource(seasons)
            .add_plugins(WeatherPlugin);
        app.world_mut()
            .spawn((GameCamera, Transform::from_xyz(10.0, 50.0, -4.0)));
        app
    }

    fn set_season(app: &mut App, season: u32) {
        app.world_mut().resource_mut::<SeasonSummary>().running = Some(season);
    }

    fn drops(app: &mut App) -> Vec<(RainDrop, Vec3, bool)> {
        let mut q = app
            .world_mut()
            .query::<(&RainDrop, &Transform, Option<&Mesh3d>)>();
        q.iter(app.world())
            .map(|(d, t, m)| (*d, t.translation, m.is_some()))
            .collect()
    }

    #[test]
    fn rain_falls_around_the_camera_only_while_it_rains() {
        let mut app = rain_app(seasons());
        set_season(&mut app, 2);
        for _ in 0..MAX_DROPS / SPAWN_PER_FRAME + 2 {
            app.update();
        }
        let eye = Vec3::new(10.0, 50.0, -4.0);
        let all = drops(&mut app);
        assert_eq!(all.len(), MAX_DROPS, "full rain in autumn");
        for (drop, at, mesh) in &all {
            assert!(!drop.snow && *mesh, "a rain streak with a mesh");
            let r = Vec2::new(at.x - eye.x, at.z - eye.z).length();
            assert!(r <= RAIN_RADIUS_M + 1e-3, "{at} within the radius");
            assert!(r >= RAIN_NEAR_M - 1e-3, "{at} not at the eye");
            assert!(at.y <= eye.y + RAIN_HEIGHT_M && at.y >= eye.y - RAIN_HEIGHT_M);
        }

        // Summer's lighter rain: fewer drops.
        set_season(&mut app, 1);
        app.update();
        app.update();
        let summer = drops(&mut app).len();
        assert!(summer > 0 && summer < MAX_DROPS, "{summer} drops in summer");

        // A dry winter, or a paused clock: none.
        set_season(&mut app, 3);
        app.update();
        app.update();
        assert!(drops(&mut app).is_empty(), "no rain in a dry winter");
        set_season(&mut app, 2);
        app.insert_resource(SeasonClockPaused(true));
        app.update();
        app.update();
        assert!(drops(&mut app).is_empty(), "no rain while paused");

        // Rain below 0 °C falls as snow.
        let mut app = rain_app(ClientSeasons {
            rows: vec![row("blizzard", -30, 9)],
        });
        app.update();
        app.update();
        let flakes = drops(&mut app);
        assert!(!flakes.is_empty() && flakes.iter().all(|(d, ..)| d.snow));
    }

    #[test]
    fn hud_chip_shows_the_season_in_the_bottom_left() {
        let mut app = App::new();
        app.add_plugins(MinimalPlugins)
            .init_resource::<ButtonInput<KeyCode>>()
            .add_plugins(UiKitPlugin {
                theme: UiTheme::load(THEME_PATH).unwrap(),
                assets_dir: None,
            })
            .insert_resource(seasons())
            .add_plugins(WeatherPlugin);
        // The lines below are Celsius; the kit's default is Fahrenheit.
        app.insert_resource(TemperatureUnit::Celsius);
        set_season(&mut app, 6);
        app.update();
        app.update();
        let mut q = app
            .world_mut()
            .query_filtered::<&HudSlot, With<WeatherHud>>();
        let slots: Vec<HudSlot> = q.iter(app.world()).copied().collect();
        assert_eq!(slots, [HudSlot(HudAnchor::BottomLeft)], "one chip");
        let lines = |app: &mut App| {
            let mut q = app.world_mut().query::<(&WeatherLine, &Text, &Node)>();
            let mut v: Vec<(usize, String, bool)> = q
                .iter(app.world())
                .map(|(l, t, n)| (l.0, t.0.clone(), n.display != Display::None))
                .collect();
            v.sort();
            v
        };
        assert_eq!(
            lines(&mut app),
            [
                (0, "Autumn · Year 2".to_string(), true),
                (1, "8.0 C · Heavy rain".into(), true),
                (2, String::new(), false),
            ]
        );
        app.insert_resource(SeasonClockPaused(true));
        set_season(&mut app, 7);
        app.update();
        assert_eq!(
            lines(&mut app),
            [
                (0, "Winter · Year 2".to_string(), true),
                (1, "-8.0 C · Frost".into(), true),
                (2, "Seasons paused (P)".into(), true),
            ]
        );
    }
}
