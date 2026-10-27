# Game calendar: days, hours and weather

Owner request, 2026-10-27: the ecology ticked per season, with no day or night. The owner asked for a day/night cycle with temperature changes through the day, 7 days to a season, and weather that varies: a wet spring, a hot summer, a dry autumn and a snowy winter.

## What the ecology did before

- A season was `seasons.ron` `substeps: 8` sub-steps. The game ran one sub-step a second (`--eco-tick-ms 1000`), so a season took 8 s and a year 32 s.
- Every sub-step of a season had the same air temperature (`air_temp`: spring 10 °C, summer 22 °C, autumn 8 °C, winter -8 °C) and the same rain (`rain` 8, 6, 10 and 1, on every column, through the seeded pattern). Every sub-step that was not winter rained.
- The client's sky was fixed: one sun, always at the same angle, and no night.
- Plant stress (SAD-P2 §7.8) counted sub-steps: `stress_death: 16` meant two seasons of unmet needs.

## The calendar (`assets/calendar.ron`)

- **Days and hours.** `days: 7`, `hours: 24`. While the game runs the ecology on the wall clock, a season is 168 sub-steps, one per game hour.
- **Temperature.** Each season has a daily `mean` at mid-season. Day by day, the mean moves on a straight line toward the next season's, so the seasons turn gradually. Through the day the air swings by `swing` times the `diurnal` curve: coldest just before dawn, warmest mid-afternoon. Each hour's value is the SAD-P2 §7.1 air clamp for that hour.
- **Rain.**
  - A day is wet with chance `wet`/256, from a hash of the seed, season and day.
  - A wet day has one spell of `spell` hours. Each hour of the spell rains `rain` through the seeded column pattern, which changes each day.
  - Spring: wet 168/256, long gentle spells. Summer: wet 56, short heavy storms. Autumn: wet 36, light. Winter: wet 128, mostly snow.
- **Snow.** Precipitation in an hour whose air, or the next hour's, is at or below 0 °C is snow. Snow soaks into porous ground or joins open water. It never stands as a film on other ground, where it would freeze into a block of ice.
- **Plants.** Growth and spread run once a day, in the hour `plant_hour` (12). Plant stress therefore counts unmet days, so a 7-day winter or a dry week does not kill. A season feeds and spreads its plants 7 times, close to the 8 of a `seasons.ron` season, so the plant balance tuned on the rows holds.
- **Daylight.** `daylight: (sunrise, sunset)` per season, used by the client only.

The calendar is not in the tables hash: it is weather, not the shape of the world, so existing saves load. The editor's Season button, `--headless --seasons N` and the tests still run `seasons.ron` seasons.

## Pacing

`--day-seconds` (default 120) sets the length of a game day: 5 s an hour, 14 minutes a season, 56 minutes a year. `--eco-tick-ms N` sets the hour directly; 0 turns the clock off. `P` pauses the ecology and the time of day together.

## Host and protocol

- `Host::set_calendar` makes the host run `SeasonRun::start_planned` seasons.
- After each batch of sub-steps the host sends `ServerMsg::Clock(GameClock)`. `GameClock` holds the season, day, hour, the air temperature, the rain and whether it is snow, and the daylight hours.
- The message is additive, so `PROTOCOL_VERSION` stays 2. Hosts without a calendar never send it.

## Client

- `GameTime` follows the clock and moves smoothly between hours, paced by `ClockPace`. It stops while the clock is paused.
- `DayNightPlugin`:
  - **Sun path.** The sun rises in the east (+X), passes south, and sets in the west, with a 62° peak.
  - **Moon.** The moon is opposite the sun and lights the night at 700 lx.
  - **Light.** Ambient light runs from 1200 at day to 280 at night.
  - **Sky.** A dusk glow; clouds follow the hour's rain. Fog and the clear colour follow the horizon.
  - **Discs.** Sun and moon discs are hidden under overcast.
- The HUD's season chip reads, for example, "Summer · Day 3 · Year 2" and "14:15 · 24.5 C · Rain".
- With no clock (headless, tests without a calendar) the sky stays exactly as before.
- Screenshots: `docs/screens/day_night_renders-{morning,noon,sunset,night}.png`.

## Departure from SAD-P2

SAD-P2 §17 lists wall-clock seasons as an extension point. The owner chose hourly sub-steps with an hourly clamp and rain. The pass order, the passes and the per-sub-step rules are unchanged. A calendar run changes only the clamp, the rain, and whether the two plant passes run in a given hour.
