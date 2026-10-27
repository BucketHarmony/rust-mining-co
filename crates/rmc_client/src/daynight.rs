//! Day and night (`docs/CALENDAR.md`): the game calendar's clock on
//! screen.
//!
//! A host on the game calendar sends `Clock` after each hourly sub-step.
//! [`GameTime`] keeps the last one and how long ago it came, so the time
//! of day moves smoothly between two clocks at the app's [`ClockPace`] (the
//! real time an hour lasts); it stops while [`SeasonClockPaused`] is set.
//!
//! From the time of day and the season's sunrise and sunset, [`day_sky`]
//! puts the sun on its path: up at sunrise in the east (+X), highest at
//! midday ([`SUN_PEAK_DEG`], to the south), down at sunset in the west, and
//! under the ground at night while the moon, opposite it, is up. The
//! [`Sun`] light follows whichever of the two is up (warm near the horizon,
//! cool and dim by moonlight), the ambient light dims toward
//! [`NIGHT_AMBIENT`], and the sky, the fog and the clear color move from
//! the day's colors through a dusk glow to the night's. Rain clouds the
//! sky over and cut the sun ([`Weather::rain`]).
//!
//! The sky is a small cubemap ([`SKY_DAY_PX`]) made again whenever the time
//! has moved [`SKY_STEP_H`] or the clouds changed; the sun and the moon are
//! unlit discs far out along their directions, so they stay sharp.
//!
//! Without a clock (a host without the calendar, the editor's Season
//! button only, the tests) nothing here changes the fixed noon look of
//! [`crate::render`].

use std::f32::consts::PI;
use std::time::Duration;

use bevy::light::{NotShadowCaster, Skybox};
use bevy::pbr::DistanceFog;
use bevy::prelude::*;
use rmc_protocol::{GameClock, ServerMsg};

use crate::net::FromHost;
use crate::render::{
    AMBIENT_BRIGHTNESS, GameCamera, HORIZON_COLOR, SUN_DISC_COLOR, SUN_DISC_RADIUS_DEG,
    SUN_GLOW_COLOR, SUN_ILLUMINANCE, SkyParams, Sun, ZENITH_COLOR, sky_cubemap_with,
};
use crate::weather::{SeasonClockPaused, Weather, WeatherSystems};

/// How high the sun stands at midday, in degrees above the horizon.
pub const SUN_PEAK_DEG: f32 = 62.0;
/// The moon's light, in lux, when it is high.
pub const MOON_ILLUMINANCE: f32 = 700.0;
/// The ambient light at night, in cd/m² (the day's is
/// [`AMBIENT_BRIGHTNESS`]): dark enough to read as night, light enough to
/// see by.
pub const NIGHT_AMBIENT: f32 = 280.0;
/// The night sky at the horizon and overhead.
pub const NIGHT_HORIZON: Color = Color::srgb(0.10, 0.12, 0.20);
pub const NIGHT_ZENITH: Color = Color::srgb(0.02, 0.03, 0.08);
/// The horizon in the dusk glow, and the low sun's light.
pub const DUSK_HORIZON: Color = Color::srgb(0.95, 0.60, 0.38);
pub const DUSK_SUN: Color = Color::srgb(1.0, 0.66, 0.42);
/// Moonlight, and the night's ambient tint.
pub const MOON_COLOR: Color = Color::srgb(0.66, 0.74, 0.98);
/// The moon disc, linear HDR.
pub const MOON_DISC_COLOR: LinearRgba = LinearRgba::rgb(1.8, 1.9, 2.2);
/// The grey a fully clouded sky turns toward, and how far (0 to 1).
pub const CLOUD_COLOR: Color = Color::srgb(0.58, 0.61, 0.66);
pub const CLOUD_SKY: f32 = 0.65;
/// The share of the sun and moon light heavy rain clouds take away.
pub const CLOUD_LIGHT_CUT: f32 = 0.6;
/// Within this sine of the sun's elevation of the horizon the sky glows
/// with dusk.
pub const DUSK_BAND: f32 = 0.3;
/// Faces of the day sky's cubemap, in texels: the gradient and the glow
/// are smooth, so it can be small.
pub const SKY_DAY_PX: u32 = 32;
/// The sky is made again when the time has moved this many hours.
pub const SKY_STEP_H: f32 = 0.1;
/// How far out the sun and moon discs are drawn, in meters (inside the
/// camera's far plane, beyond the world).
pub const DISC_DISTANCE_M: f32 = 400.0;
/// The moon's angular radius, in degrees.
pub const MOON_DISC_RADIUS_DEG: f32 = 1.2;

/// Real time an hour of the calendar lasts (the app's ecology interval);
/// without it the time of day steps from clock to clock.
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockPace(pub Duration);

/// The last clock the host sent and the real time since.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct GameTime {
    pub clock: Option<GameClock>,
    /// Seconds since `clock` came, not counting paused time.
    pub since_s: f32,
}

impl GameTime {
    /// The time of day in hours, from 0 to the clock's `hours`: the end
    /// of the hour last run, plus the share of the next one `pace` says
    /// has passed. `None` without a clock.
    pub fn time_h(&self, pace: Option<Duration>) -> Option<f32> {
        let c = self.clock?;
        let frac = match pace {
            Some(p) if !p.is_zero() => (self.since_s / p.as_secs_f32()).min(1.0),
            _ => 0.0,
        };
        Some((f32::from(c.hour) + 1.0 + frac) % f32::from(c.hours.max(1)))
    }
}

/// The time of day as "HH:MM" on a 24-hour clock, for a day of `hours`.
pub fn clock_label(time_h: f32, hours: u8) -> String {
    let minutes = (time_h / f32::from(hours.max(1)) * 24.0 * 60.0).floor() as u32 % (24 * 60);
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// The sun's angle on its circle at `time_h`: 0 at sunrise, π/2 at
/// midday, π at sunset, and on through the night back to 2π at the next
/// sunrise.
pub fn sun_angle(time_h: f32, hours: u8, daylight: (u8, u8)) -> f32 {
    let hours = f32::from(hours.max(1));
    let (rise, set) = (f32::from(daylight.0), f32::from(daylight.1));
    let t = time_h.rem_euclid(hours);
    let day = (set - rise).max(f32::EPSILON);
    let night = (hours - day).max(f32::EPSILON);
    if (rise..set).contains(&t) {
        PI * (t - rise) / day
    } else {
        PI + PI * (t - set).rem_euclid(hours) / night
    }
}

/// The unit vector toward the sun at `angle` ([`sun_angle`]): rising at
/// +X, through the south (+Z) at [`SUN_PEAK_DEG`], setting at -X.
pub fn sun_toward(angle: f32) -> Vec3 {
    let peak = SUN_PEAK_DEG.to_radians();
    Vec3::new(
        angle.cos(),
        angle.sin() * peak.sin(),
        angle.sin() * peak.cos(),
    )
}

/// The sky and light at one time of day.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DaySky {
    /// Toward the sun; the moon is opposite.
    pub sun: Vec3,
    /// Toward the light that shines: the sun by day, the moon by night.
    pub light_toward: Vec3,
    /// That light's illuminance, in lux, and color.
    pub illuminance: f32,
    pub light_color: Color,
    /// The ambient light, cd/m², and its tint.
    pub ambient: f32,
    pub ambient_color: Color,
    /// 0 at night to 1 by day, through twilight.
    pub daylight: f32,
    /// The sky's colors and glow (no disc: the discs are drawn apart).
    pub sky: SkyParams,
}

impl DaySky {
    /// The horizon color: the fog's and the clear color.
    pub fn horizon(&self) -> Color {
        Color::LinearRgba(self.sky.horizon)
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn lin(c: Color) -> LinearRgba {
    LinearRgba::from(c)
}

fn mix(a: LinearRgba, b: LinearRgba, t: f32) -> LinearRgba {
    a * (1.0 - t) + b * t
}

/// The sky and light at `time_h` of a day of `hours` with `daylight`
/// (sunrise, sunset), under clouds of `cloud` (0 clear to 1 overcast).
pub fn day_sky(time_h: f32, hours: u8, daylight: (u8, u8), cloud: f32) -> DaySky {
    let cloud = cloud.clamp(0.0, 1.0);
    let sun = sun_toward(sun_angle(time_h, hours, daylight));
    let e = sun.y;
    let day = smoothstep(-0.12, 0.2, e);
    let dusk = smoothstep(DUSK_BAND, 0.0, e.abs()) * smoothstep(-DUSK_BAND, 0.0, e);
    let grey = lin(CLOUD_COLOR) * (0.15 + 0.85 * day);
    let clouded = |c: LinearRgba| mix(c, grey, cloud * CLOUD_SKY);
    let horizon = mix(lin(NIGHT_HORIZON), lin(HORIZON_COLOR), day);
    let horizon = clouded(mix(horizon, lin(DUSK_HORIZON), dusk * 0.75));
    let zenith = clouded(mix(lin(NIGHT_ZENITH), lin(ZENITH_COLOR), day));
    let glow = SUN_GLOW_COLOR * ((1.0 + 2.0 * dusk) * day * (1.0 - cloud));
    let cut = 1.0 - cloud * CLOUD_LIGHT_CUT;
    let (light_toward, illuminance, light_color) = if e >= 0.0 {
        let warm = smoothstep(0.0, 0.4, e);
        (
            sun,
            SUN_ILLUMINANCE * smoothstep(0.0, 0.25, e) * cut,
            Color::LinearRgba(mix(lin(DUSK_SUN), lin(Color::WHITE), warm)),
        )
    } else {
        let moon = -sun;
        (
            moon,
            MOON_ILLUMINANCE * smoothstep(0.0, 0.2, moon.y) * cut,
            MOON_COLOR,
        )
    };
    DaySky {
        sun,
        light_toward,
        illuminance,
        light_color,
        ambient: NIGHT_AMBIENT + (AMBIENT_BRIGHTNESS - NIGHT_AMBIENT) * day,
        ambient_color: Color::LinearRgba(mix(lin(MOON_COLOR), lin(Color::WHITE), day)),
        daylight: day,
        sky: SkyParams {
            horizon: LinearRgba {
                alpha: 1.0,
                ..horizon
            },
            zenith: LinearRgba {
                alpha: 1.0,
                ..zenith
            },
            sun,
            glow,
            disc: false,
        },
    }
}

/// How clouded the sky is for `weather`: clear when dry, from 0.4 in light
/// rain to 1 in the heaviest.
pub fn cloud_cover(weather: &Weather) -> f32 {
    if weather.rain > 0.0 {
        0.4 + 0.6 * weather.rain.min(1.0)
    } else {
        0.0
    }
}

/// The sky and light now; `None` without a clock.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct DayNight(pub Option<DaySky>);

/// The sun or the moon disc.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkyDisc {
    Sun,
    Moon,
}

/// When the sky cubemap was last made: the time and the clouds.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
struct SkyMade(Option<(f32, f32)>);

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct DayNightSystems;

/// Follows the host's clock and lights the day (see the module docs). Add
/// with [`WeatherPlugin`](crate::WeatherPlugin), which keeps [`GameTime`].
pub struct DayNightPlugin;

impl Plugin for DayNightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<GameTime>()
            .init_resource::<DayNight>()
            .init_resource::<SkyMade>()
            .add_systems(
                Update,
                (update_day_sky, light_the_day, paint_the_sky, place_discs)
                    .chain()
                    .after(WeatherSystems)
                    .in_set(DayNightSystems),
            );
    }
}

/// Keeps [`GameTime`] on the host's `Clock` messages (run by the weather).
pub(crate) fn follow_clock(
    time: Res<Time<Real>>,
    mut inbox: MessageReader<FromHost>,
    paused: Option<Res<SeasonClockPaused>>,
    mut game_time: ResMut<GameTime>,
) {
    let mut latest = None;
    for FromHost(msg) in inbox.read() {
        if let ServerMsg::Clock(c) = msg {
            latest = Some(*c);
        }
    }
    if let Some(c) = latest {
        *game_time = GameTime {
            clock: Some(c),
            since_s: 0.0,
        };
    } else if game_time.clock.is_some() && !paused.is_some_and(|p| p.0) {
        game_time.since_s += time.delta_secs();
    }
}

fn update_day_sky(
    game_time: Res<GameTime>,
    pace: Option<Res<ClockPace>>,
    weather: Option<Res<Weather>>,
    mut out: ResMut<DayNight>,
) {
    let sky = game_time
        .clock
        .zip(game_time.time_h(pace.map(|p| p.0)))
        .map(|(c, t)| {
            let cloud = weather.as_deref().map_or(0.0, cloud_cover);
            day_sky(t, c.hours, c.daylight, cloud)
        });
    if out.0 != sky {
        out.0 = sky;
    }
}

type SunLight<'a> = (&'a mut Transform, &'a mut DirectionalLight);

fn light_the_day(
    day: Res<DayNight>,
    mut suns: Query<SunLight<'_>, (With<Sun>, Without<GameCamera>)>,
    mut fogs: Query<&mut DistanceFog, With<GameCamera>>,
    ambient: Option<ResMut<GlobalAmbientLight>>,
    clear: Option<ResMut<ClearColor>>,
) {
    let Some(sky) = day.0 else {
        return;
    };
    if !day.is_changed() {
        return;
    }
    for (mut transform, mut light) in &mut suns {
        *transform = Transform::default().looking_to(-sky.light_toward, Vec3::Y);
        light.illuminance = sky.illuminance;
        light.color = sky.light_color;
    }
    for mut fog in &mut fogs {
        fog.color = sky.horizon();
    }
    if let Some(mut ambient) = ambient {
        ambient.brightness = sky.ambient;
        ambient.color = sky.ambient_color;
    }
    if let Some(mut clear) = clear {
        clear.0 = sky.horizon();
    }
}

fn paint_the_sky(
    day: Res<DayNight>,
    game_time: Res<GameTime>,
    pace: Option<Res<ClockPace>>,
    weather: Option<Res<Weather>>,
    mut made: ResMut<SkyMade>,
    images: Option<ResMut<Assets<Image>>>,
    mut skyboxes: Query<&mut Skybox, With<GameCamera>>,
) {
    let (Some(sky), Some(mut images)) = (day.0, images) else {
        return;
    };
    let Some(t) = game_time.time_h(pace.map(|p| p.0)) else {
        return;
    };
    let cloud = weather.as_deref().map_or(0.0, cloud_cover);
    let fresh = made.0.is_some_and(|(at, was)| {
        let hours = f32::from(game_time.clock.map_or(1, |c| c.hours.max(1)));
        let moved = (t - at).rem_euclid(hours);
        moved.min(hours - moved) < SKY_STEP_H && was == cloud
    });
    if fresh {
        return;
    }
    made.0 = Some((t, cloud));
    let image = images.add(sky_cubemap_with(&sky.sky, SKY_DAY_PX));
    for mut skybox in &mut skyboxes {
        skybox.image = Some(image.clone());
    }
}

/// An unlit, unfogged disc material, HDR so it blooms.
fn disc_material(color: LinearRgba) -> StandardMaterial {
    StandardMaterial {
        base_color: Color::LinearRgba(color),
        unlit: true,
        fog_enabled: false,
        ..default()
    }
}

#[allow(clippy::type_complexity)]
fn place_discs(
    mut commands: Commands,
    day: Res<DayNight>,
    weather: Option<Res<Weather>>,
    camera: Query<&Transform, (With<GameCamera>, Without<SkyDisc>)>,
    mut discs: Query<(&SkyDisc, &mut Transform, &mut Visibility)>,
    meshes: Option<ResMut<Assets<Mesh>>>,
    materials: Option<ResMut<Assets<StandardMaterial>>>,
) {
    let Some(sky) = day.0 else {
        return;
    };
    let Ok(eye) = camera.single().map(|t| t.translation) else {
        return;
    };
    if discs.is_empty() {
        let (Some(mut meshes), Some(mut materials)) = (meshes, materials) else {
            return;
        };
        let radius = |deg: f32| DISC_DISTANCE_M * deg.to_radians().tan();
        for (disc, deg, color) in [
            (SkyDisc::Sun, SUN_DISC_RADIUS_DEG, SUN_DISC_COLOR),
            (SkyDisc::Moon, MOON_DISC_RADIUS_DEG, MOON_DISC_COLOR),
        ] {
            commands.spawn((
                disc,
                Mesh3d(meshes.add(Sphere::new(radius(deg)))),
                MeshMaterial3d(materials.add(disc_material(color))),
                Transform::from_translation(eye),
                Visibility::Hidden,
                NotShadowCaster,
            ));
        }
        return;
    }
    let clear = weather.as_deref().map_or(0.0, cloud_cover) < 0.5;
    for (disc, mut transform, mut visibility) in &mut discs {
        let toward = match disc {
            SkyDisc::Sun => sky.sun,
            SkyDisc::Moon => -sky.sun,
        };
        transform.translation = eye + toward * DISC_DISTANCE_M;
        let shown = if clear && toward.y > -0.05 {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        if *visibility != shown {
            *visibility = shown;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAYLIGHT: (u8, u8) = (6, 19);

    fn clock(hour: u8) -> GameClock {
        GameClock {
            season: 0,
            day: 2,
            days: 7,
            hour,
            hours: 24,
            air_temp: 150,
            rain: 0,
            peak_rain: 48,
            snow: false,
            daylight: DAYLIGHT,
        }
    }

    #[test]
    fn time_of_day_moves_between_clocks_and_wraps() {
        let pace = Some(Duration::from_secs(4));
        let none = GameTime::default();
        assert_eq!(none.time_h(pace), None);
        let mut t = GameTime {
            clock: Some(clock(13)),
            since_s: 0.0,
        };
        assert_eq!(t.time_h(pace), Some(14.0), "the end of the hour run");
        t.since_s = 1.0;
        assert_eq!(t.time_h(pace), Some(14.25));
        t.since_s = 9.0;
        assert_eq!(t.time_h(pace), Some(15.0), "never past the next clock");
        assert_eq!(t.time_h(None), Some(14.0), "no pace: steps");
        t.clock = Some(clock(23));
        t.since_s = 2.0;
        assert_eq!(t.time_h(pace), Some(0.5), "past midnight");
        assert_eq!(clock_label(14.25, 24), "14:15");
        assert_eq!(clock_label(0.5, 24), "00:30");
        assert_eq!(clock_label(6.0, 12), "12:00", "a short day on a 24 h face");
    }

    #[test]
    fn sun_rises_east_peaks_south_sets_west_and_is_down_at_night() {
        let at = |t: f32| sun_toward(sun_angle(t, 24, DAYLIGHT));
        let rise = at(6.0);
        assert!(rise.x > 0.99 && rise.y.abs() < 1e-5, "{rise:?}");
        let noon = at(12.5);
        assert!((noon.y - SUN_PEAK_DEG.to_radians().sin()).abs() < 1e-4);
        assert!(noon.z > 0.0 && noon.x.abs() < 1e-4, "{noon:?}");
        let set = at(19.0);
        assert!(set.x < -0.99 && set.y.abs() < 1e-5, "{set:?}");
        for t in [0.0, 3.0, 5.5, 19.5, 23.0] {
            assert!(at(t).y < 0.0, "{t}: the sun is down");
        }
        for t in [7.0, 10.0, 15.0, 18.5] {
            assert!(at(t).y > 0.0, "{t}: the sun is up");
        }
        // The path is continuous through sunset and midnight.
        for t in 0..240 {
            let t = t as f32 / 10.0;
            let step = at(t).distance(at(t + 0.1));
            assert!(step < 0.05, "{t}: {step}");
        }
    }

    #[test]
    fn noon_is_bright_dusk_glows_and_night_is_moonlit() {
        let noon = day_sky(12.5, 24, DAYLIGHT, 0.0);
        let dusk = day_sky(19.0, 24, DAYLIGHT, 0.0);
        let night = day_sky(1.0, 24, DAYLIGHT, 0.0);
        assert_eq!(noon.daylight, 1.0);
        assert_eq!(night.daylight, 0.0);
        assert!((noon.illuminance - SUN_ILLUMINANCE).abs() < 1.0);
        assert_eq!(noon.light_toward, noon.sun);
        assert!((noon.ambient - AMBIENT_BRIGHTNESS).abs() < 1e-3);
        // By night the moon, opposite the sun, lights the land, dimly.
        assert_eq!(night.light_toward, -night.sun);
        assert!(night.light_toward.y > 0.0);
        assert!(0.0 < night.illuminance && night.illuminance <= MOON_ILLUMINANCE);
        assert_eq!(night.light_color, MOON_COLOR);
        assert!((night.ambient - NIGHT_AMBIENT).abs() < 1e-3);
        // The noon sky is the fixed sky's colors; night is darker; dusk
        // is redder at the horizon than noon.
        assert_eq!(noon.sky.horizon, lin(HORIZON_COLOR));
        assert_eq!(noon.sky.zenith, lin(ZENITH_COLOR));
        assert!(night.sky.horizon.luminance() < noon.sky.horizon.luminance());
        assert!(night.sky.zenith.luminance() < noon.sky.zenith.luminance());
        let red = |c: LinearRgba| c.red / c.blue.max(1e-6);
        assert!(red(dusk.sky.horizon) > red(noon.sky.horizon));
        assert!(
            !noon.sky.disc && !night.sky.disc,
            "the discs are drawn apart"
        );
        assert_eq!(noon.horizon(), Color::LinearRgba(noon.sky.horizon));
    }

    #[test]
    fn clouds_grey_the_sky_and_cut_the_sun() {
        let clear = day_sky(12.5, 24, DAYLIGHT, 0.0);
        let rain = day_sky(12.5, 24, DAYLIGHT, 1.0);
        assert!(rain.illuminance < clear.illuminance);
        assert!(
            (rain.illuminance - SUN_ILLUMINANCE * (1.0 - CLOUD_LIGHT_CUT)).abs() < 1.0,
            "{}",
            rain.illuminance
        );
        assert_ne!(rain.sky.zenith, clear.sky.zenith);
        assert_eq!(rain.sky.glow.luminance(), 0.0, "no glow through the clouds");
        let dry = Weather::default();
        assert_eq!(cloud_cover(&dry), 0.0);
        let wet = Weather {
            rain: 1.0,
            ..Weather::default()
        };
        assert_eq!(cloud_cover(&wet), 1.0);
    }

    fn app() -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default()))
            .init_asset::<Image>()
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_message::<FromHost>()
            .init_resource::<SeasonClockPaused>()
            .init_resource::<Weather>()
            .insert_resource(GlobalAmbientLight::default())
            .insert_resource(ClearColor(HORIZON_COLOR))
            .add_systems(Update, follow_clock.in_set(WeatherSystems))
            .add_plugins(DayNightPlugin);
        app.world_mut().spawn((
            Sun,
            DirectionalLight::default(),
            crate::render::sun_transform(),
        ));
        app.world_mut().spawn((
            GameCamera,
            Transform::default(),
            crate::render::distance_fog(),
            Skybox::default(),
        ));
        app
    }

    #[test]
    fn a_clock_moves_the_sun_ambient_fog_and_sky() {
        let mut app = app();
        app.update();
        let fixed = crate::render::sun_transform();
        let sun = |app: &mut App| {
            let w = app.world_mut();
            w.query_filtered::<(&Transform, &DirectionalLight), With<Sun>>()
                .single(w)
                .map(|(t, l)| (*t, l.illuminance))
                .unwrap()
        };
        assert_eq!(sun(&mut app).0, fixed, "no clock: the fixed sun");
        assert_eq!(app.world().resource::<DayNight>().0, None);

        app.world_mut()
            .write_message(FromHost(ServerMsg::Clock(clock(11))));
        app.update();
        app.update();
        let t = app.world().resource::<GameTime>().time_h(None);
        assert_eq!(t, Some(12.0));
        let want = day_sky(12.0, 24, DAYLIGHT, 0.0);
        let (transform, lux) = sun(&mut app);
        assert!(transform.forward().as_vec3().distance(-want.light_toward) < 1e-5);
        assert_eq!(lux, want.illuminance);
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().brightness,
            want.ambient
        );
        assert_eq!(app.world().resource::<ClearColor>().0, want.horizon());
        let w = app.world_mut();
        let (fog, skybox) = w
            .query_filtered::<(&DistanceFog, &Skybox), With<GameCamera>>()
            .single(w)
            .unwrap();
        assert_eq!(fog.color, want.horizon());
        assert!(skybox.image.is_some(), "the day's sky is painted");
        let discs: Vec<SkyDisc> = w.query::<&SkyDisc>().iter(w).copied().collect();
        assert_eq!(discs.len(), 2, "{discs:?}");

        // Midnight: the moon lights the land, the ambient dims.
        app.world_mut()
            .write_message(FromHost(ServerMsg::Clock(clock(23))));
        app.update();
        let night = app.world().resource::<DayNight>().0.unwrap();
        assert_eq!(night.light_toward, -night.sun);
        assert_eq!(
            app.world().resource::<GlobalAmbientLight>().brightness,
            night.ambient
        );
        assert!(night.ambient < want.ambient);
    }
}
