//! The game calendar (`assets/calendar.ron`, `docs/CALENDAR.md`): days,
//! hours and the weather of each hour.
//!
//! While the game runs the ecology on the wall clock, a season is
//! [`Calendar::days`] days of [`Calendar::hours`] sub-steps. Each sub-step
//! gets a [`SubstepWeather`] from [`Calendar::plan`]: the air temperature
//! clamp (SAD-P2 §7.1) for that hour and the rain (§7.3 D) that falls in
//! it. The season tick runs a plan with [`SeasonRun::start_planned`]; the
//! other passes do not know about the calendar.
//!
//! - **Temperature.** Each season has a daily `mean` at mid-season; day by
//!   day the mean moves on a straight line toward the neighbouring season's
//!   ([`Calendar::mean`]). Through the day it swings by `swing` times the
//!   `diurnal` curve (per cent).
//! - **Rain.** A day is wet with chance `wet` in 256, from a hash of the
//!   seed, season and day. A wet day has one spell of `spell` hours from a
//!   hashed start (wrapping past midnight within the day). Each hour of the
//!   spell rains `rain` on every column before the seeded column pattern,
//!   like a `seasons.ron` row.
//! - **Snow.** Precipitation in an hour whose air, or the next hour's, is at
//!   or below 0 °C is snow ([`SubstepWeather::snow`]): it soaks into a
//!   porous surface instead of standing on it, so it never leaves a film to
//!   freeze into a block of ice.
//! - **Plants.** Growth and spread (SAD-P2 §7.8) run once a day, in the
//!   hour `plant_hour` ([`SubstepWeather::plants`]). Plant stress then
//!   counts unmet days, as it counted sub-steps in a season of
//!   `seasons.ron` rows: a week of frost or drought is not
//!   `stress_death` hours, and a season grows, feeds and spreads its
//!   plants `days` times, about as often as the rows' `substeps`.
//!
//! The calendar is not part of the tables hash: it is the weather, not the
//! shape of the world.
//!
//! [`SeasonRun::start_planned`]: crate::tick::SeasonRun::start_planned

use std::path::Path;

use serde::{Deserialize, Serialize};
use xxhash_rust::xxh3::Xxh3;

use crate::tables::EcoTableError;

/// The calendar file in the assets directory.
pub const CALENDAR_FILE: &str = "calendar.ron";

/// `diurnal` entries are per cent of a season's `swing`.
const PERCENT: i32 = 100;

/// One season of the calendar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CalendarSeason {
    pub key: String,
    /// Daily mean air temperature at mid-season, 0.1 °C.
    pub mean: i16,
    /// Half the day's range, 0.1 °C.
    pub swing: i16,
    /// Chance in 256 that a day is wet.
    pub wet: u8,
    /// Rain of a wet hour, per column before the seeded pattern (`/ 256`).
    pub rain: u8,
    /// Shortest and longest spell of a wet day, in hours.
    pub spell: (u8, u8),
    /// Sunrise and sunset, in hours.
    pub daylight: (u8, u8),
}

/// `calendar.ron`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Calendar {
    /// Days in a season.
    pub days: u8,
    /// Sub-steps in a day: one an hour.
    pub hours: u8,
    /// Air temperature through the day, from midnight, in per cent of the
    /// season's `swing`; one entry per hour.
    pub diurnal: Vec<i16>,
    /// The hour of each day in which the plants grow and spread.
    pub plant_hour: u8,
    pub seasons: Vec<CalendarSeason>,
}

/// The weather of one sub-step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SubstepWeather {
    /// The clamp for surface and sky-open air, water and ice (SAD-P2
    /// §7.1), 0.1 °C.
    pub air_temp: i16,
    /// Rain per column before the seeded pattern (SAD-P2 §7.3 D).
    pub rain: u8,
    /// What keys the seeded column pattern ([`crate::water::rain_pattern`]):
    /// the season for a `seasons.ron` row, the day for the calendar.
    pub pattern: u32,
    /// The precipitation is snow (see the module docs).
    pub snow: bool,
    /// Plant growth and spread run in this sub-step (see the module docs).
    pub plants: bool,
}

impl Calendar {
    /// Reads and checks `calendar.ron` in `dir`.
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, EcoTableError> {
        let path = dir.as_ref().join(CALENDAR_FILE);
        let text = std::fs::read_to_string(&path).map_err(|source| EcoTableError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&text)
    }

    /// Parses and checks the text of `calendar.ron`.
    pub fn parse(text: &str) -> Result<Self, EcoTableError> {
        let c: Self = ron::from_str(text).map_err(|source| EcoTableError::Parse {
            file: CALENDAR_FILE,
            source,
        })?;
        c.check()?;
        Ok(c)
    }

    fn check(&self) -> Result<(), EcoTableError> {
        let bad = |msg: String| {
            Err(EcoTableError::Invalid {
                file: CALENDAR_FILE,
                msg,
            })
        };
        if self.days == 0 || self.hours == 0 {
            return bad("days and hours must be at least 1".into());
        }
        if u32::from(self.days) * u32::from(self.hours) > u32::from(u8::MAX) {
            return bad(format!(
                "a season of {} days of {} hours has more than {} sub-steps",
                self.days,
                self.hours,
                u8::MAX
            ));
        }
        if self.diurnal.len() != usize::from(self.hours) {
            return bad(format!(
                "diurnal has {} entries for {} hours",
                self.diurnal.len(),
                self.hours
            ));
        }
        if self.plant_hour >= self.hours {
            return bad(format!(
                "plant_hour {} of {} hours",
                self.plant_hour, self.hours
            ));
        }
        if self.seasons.is_empty() {
            return bad("no seasons".into());
        }
        for s in &self.seasons {
            let (lo, hi) = s.spell;
            if lo == 0 || lo > hi || hi > self.hours {
                return bad(format!("{}: spell {lo}..{hi}", s.key));
            }
            let (rise, set) = s.daylight;
            if rise >= set || set > self.hours {
                return bad(format!("{}: daylight {rise}..{set}", s.key));
            }
            if s.swing < 0 {
                return bad(format!("{}: negative swing", s.key));
            }
        }
        Ok(())
    }

    /// Sub-steps in a season.
    pub fn substeps(&self) -> u8 {
        self.days * self.hours
    }

    /// The row of season `season`.
    pub fn season(&self, season: u32) -> &CalendarSeason {
        &self.seasons[season as usize % self.seasons.len()]
    }

    /// The day of the season and the hour of the day of sub-step `substep`.
    pub fn day_hour(&self, substep: u8) -> (u8, u8) {
        (substep / self.hours, substep % self.hours)
    }

    /// The daily mean air temperature of day `day` of season `season`:
    /// the season's `mean` at mid-season, on a straight line to the
    /// neighbouring season's mean, meeting it halfway at the boundary.
    pub fn mean(&self, season: u32, day: u8) -> i16 {
        let len = self.seasons.len() as u32;
        let here = i32::from(self.season(season).mean);
        // Distance from mid-season in half days: -(days - 1)..=days - 1.
        let days = i32::from(self.days);
        let p = 2 * i32::from(day) + 1 - days;
        let other = if p < 0 {
            self.season(season + len - 1).mean
        } else {
            self.season(season + 1).mean
        };
        let t = here + (i32::from(other) - here) * p.abs() / (2 * days);
        i16::try_from(t).expect("between two i16 means")
    }

    /// The air temperature of sub-step `substep` of season `season`; a
    /// sub-step past the season's end is in the next season.
    pub fn air_temp(&self, season: u32, substep: u32) -> i16 {
        let per = u32::from(self.substeps());
        let (season, substep) = (season + substep / per, substep % per);
        let substep = u8::try_from(substep).expect("below a season's sub-steps");
        let (day, hour) = self.day_hour(substep);
        let swing = i32::from(self.season(season).swing);
        let shape = i32::from(self.diurnal[usize::from(hour)]);
        let t = i32::from(self.mean(season, day)) + swing * shape / PERCENT;
        i16::try_from(t.clamp(i32::from(i16::MIN), i32::from(i16::MAX))).expect("clamped")
    }

    /// The spell of day `day` of season `season`: its first hour and its
    /// length, or `None` on a dry day.
    pub fn spell(&self, seed: u64, season: u32, day: u8) -> Option<(u8, u8)> {
        let row = self.season(season);
        let mut h = Xxh3::with_seed(seed);
        h.update(b"calendar-spell");
        h.update(&season.to_le_bytes());
        h.update(&[day]);
        let b = h.digest().to_le_bytes();
        if b[0] >= row.wet {
            return None;
        }
        let (lo, hi) = row.spell;
        let len = lo + b[1] % (hi - lo + 1);
        let start = b[2] % self.hours;
        Some((start, len))
    }

    /// Whether hour `hour` is in the spell `spell` (which wraps past
    /// midnight within its day).
    fn in_spell(&self, spell: Option<(u8, u8)>, hour: u8) -> bool {
        spell.is_some_and(|(start, len)| {
            let since = (u16::from(hour) + u16::from(self.hours) - u16::from(start))
                % u16::from(self.hours);
            since < u16::from(len)
        })
    }

    /// The weather of every sub-step of season `season` in a world of seed
    /// `seed` (see the module docs).
    pub fn plan(&self, seed: u64, season: u32) -> Vec<SubstepWeather> {
        let row = self.season(season);
        let per = self.substeps();
        let days = u32::from(self.days);
        (0..per)
            .map(|i| {
                let (day, hour) = self.day_hour(i);
                let air_temp = self.air_temp(season, u32::from(i));
                let next = self.air_temp(season, u32::from(i) + 1);
                let wet = self.in_spell(self.spell(seed, season, day), hour);
                SubstepWeather {
                    air_temp,
                    rain: if wet { row.rain } else { 0 },
                    pattern: season.wrapping_mul(days).wrapping_add(u32::from(day)),
                    snow: wet && (air_temp <= 0 || next <= 0),
                    plants: hour == self.plant_hour,
                }
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const ASSETS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets");

    pub(crate) fn shipped() -> Calendar {
        Calendar::load(ASSETS).expect("the shipped calendar loads")
    }

    #[test]
    fn shipped_calendar_is_a_week_of_hours() {
        let c = shipped();
        assert_eq!((c.days, c.hours, c.substeps()), (7, 24, 168));
        let keys: Vec<&str> = c.seasons.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["spring", "summer", "autumn", "winter"]);
        assert_eq!(c.day_hour(0), (0, 0));
        assert_eq!(c.day_hour(25), (1, 1));
        assert_eq!(c.day_hour(167), (6, 23));
    }

    #[test]
    fn bad_calendars_are_refused() {
        let ok = std::fs::read_to_string(format!("{ASSETS}/{CALENDAR_FILE}")).unwrap();
        for (from, to) in [
            ("days: 7", "days: 0"),
            ("days: 7", "days: 11"),
            ("hours: 24", "hours: 23"),
            ("spell: (4, 12)", "spell: (0, 12)"),
            ("spell: (1, 3)", "spell: (3, 1)"),
            ("daylight: (6, 19)", "daylight: (19, 6)"),
            ("swing: 55", "swing: -55"),
            ("plant_hour: 12", "plant_hour: 24"),
        ] {
            assert!(ok.contains(from), "{from}");
            let text = ok.replacen(from, to, 1);
            assert!(
                matches!(Calendar::parse(&text), Err(EcoTableError::Invalid { .. })),
                "{to} is refused"
            );
        }
    }

    /// Nights are colder than days, by the season's swing, and the mean
    /// turns smoothly from season to season.
    #[test]
    fn days_are_warmer_than_nights_and_seasons_turn_gradually() {
        let c = shipped();
        for season in 0..4u32 {
            let row = c.season(season);
            let mid = c.days / 2;
            let at = |h: u8| c.air_temp(season, u32::from(mid * c.hours + h));
            let (coldest, warmest) = (at(4), at(15));
            assert_eq!(c.mean(season, mid), row.mean, "{}", row.key);
            assert_eq!(coldest, row.mean - row.swing, "{}", row.key);
            assert_eq!(warmest, row.mean + row.swing, "{}", row.key);
            // No jump at a season boundary: the last day's mean and the
            // next season's first are a step of the line apart.
            let last = i32::from(c.mean(season, c.days - 1));
            let first = i32::from(c.mean(season + 1, 0));
            let step = (i32::from(c.season(season + 1).mean) - i32::from(row.mean)).abs()
                / i32::from(c.days);
            // Each side truncates by less than 1: at most 2 over the step.
            assert!(
                (last - first).abs() <= step + 2,
                "{}: {last} -> {first}",
                row.key
            );
        }
        let summer = c.season(1);
        assert!(summer.mean + summer.swing >= 300, "a hot summer");
        let winter = c.season(3);
        assert!(winter.mean + winter.swing <= 0, "winter days freeze");
    }

    /// Over many years, spring has the most wet days, autumn the fewest,
    /// summer the heaviest rain, and winter's precipitation is mostly snow.
    #[test]
    fn spring_is_wet_summer_stormy_autumn_dry_winter_snowy() {
        let c = shipped();
        let mut wet_hours = [0u32; 4];
        let mut wet_days = [0u32; 4];
        let mut snow = [0u32; 4];
        let mut rain = [0u32; 4];
        for seed in 0..8u64 {
            for year in 0..8u32 {
                for s in 0..4u32 {
                    let season = year * 4 + s;
                    let plan = c.plan(seed, season);
                    assert_eq!(plan.len(), usize::from(c.substeps()));
                    for day in 0..c.days {
                        wet_days[s as usize] += u32::from(c.spell(seed, season, day).is_some());
                    }
                    for w in &plan {
                        if w.rain > 0 {
                            wet_hours[s as usize] += 1;
                            rain[s as usize] += u32::from(w.rain);
                            snow[s as usize] += u32::from(w.snow);
                        } else {
                            assert!(!w.snow, "no snow without precipitation");
                        }
                    }
                }
            }
        }
        let [spring, summer, autumn, winter] = wet_days;
        assert!(
            spring > summer && spring > autumn && spring > winter,
            "{wet_days:?}"
        );
        assert!(autumn < summer && autumn < winter, "{wet_days:?}");
        let per_hour = |s: usize| rain[s] / wet_hours[s].max(1);
        assert!(
            (0..4).all(|s| s == 1 || per_hour(1) > per_hour(s)),
            "summer storms"
        );
        assert!(
            snow[3] * 2 > wet_hours[3],
            "winter snows: {snow:?} of {wet_hours:?}"
        );
        assert_eq!(snow[1], 0, "no summer snow");
    }

    /// The plan is a function of the seed, the season and the calendar,
    /// and each day keys its own rain pattern.
    #[test]
    fn plan_is_deterministic_and_patterns_by_day() {
        let c = shipped();
        assert_eq!(c.plan(7, 5), c.plan(7, 5));
        assert_ne!(c.plan(7, 5), c.plan(8, 5), "the seed moves the spells");
        let plan = c.plan(7, 5);
        let days = u32::from(c.days);
        for (i, w) in plan.iter().enumerate() {
            let (day, _) = c.day_hour(u8::try_from(i).unwrap());
            assert_eq!(w.pattern, 5 * days + u32::from(day));
        }
        // A spell is contiguous within its day (wrapping past midnight).
        for day in 0..c.days {
            let wet: Vec<bool> = (0..c.hours)
                .map(|h| plan[usize::from(day * c.hours + h)].rain > 0)
                .collect();
            let edges = (0..wet.len())
                .filter(|&h| wet[h] != wet[(h + 1) % wet.len()])
                .count();
            assert!(edges == 0 || edges == 2, "day {day}: {wet:?}");
        }
    }

    #[test]
    fn plants_grow_once_a_day_at_the_plant_hour() {
        let c = shipped();
        let plan = c.plan(7, 2);
        let hours: Vec<(u8, u8)> = (0..c.substeps())
            .filter(|&i| plan[usize::from(i)].plants)
            .map(|i| c.day_hour(i))
            .collect();
        let expected: Vec<(u8, u8)> = (0..c.days).map(|d| (d, c.plant_hour)).collect();
        assert_eq!(hours, expected);
    }
}
