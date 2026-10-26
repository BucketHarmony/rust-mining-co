//! Wall-clock ecology (SAD-P2 §17 "Wall-clock seasons"): every
//! `interval` of real time the host thread runs one season sub-step
//! ([`Host::eco_substep`]), so the land changes while you play. A season is
//! `seasons.substeps` sub-steps. The host handles edits between sub-steps.
//!
//! `P` pauses and resumes the clock. A sub-step still running on the host
//! when the next is due is not doubled up: the clock waits for it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bevy::prelude::*;
use rmc_host::Host;

use crate::session::GameHost;

/// The ecology clock. `interval: None` turns it off.
#[derive(Resource, Debug)]
pub struct EcoClock {
    pub interval: Option<Duration>,
    pub paused: bool,
    /// Sub-steps handed to the host so far.
    pub sent: u64,
    since: Duration,
    busy: Arc<AtomicBool>,
}

impl EcoClock {
    pub fn new(interval: Option<Duration>) -> Self {
        Self {
            interval: interval.filter(|d| !d.is_zero()),
            paused: false,
            sent: 0,
            since: Duration::ZERO,
            busy: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Advances the clock by `dt`; true when a sub-step is due and none is
    /// still running.
    fn due(&mut self, dt: Duration) -> bool {
        let Some(interval) = self.interval else {
            return false;
        };
        if self.paused {
            return false;
        }
        self.since += dt;
        if self.since < interval || self.busy.load(Ordering::Acquire) {
            return false;
        }
        // Late ticks are not caught up: a slow host slows the clock.
        self.since = Duration::ZERO;
        true
    }
}

impl Default for EcoClock {
    fn default() -> Self {
        Self::new(None)
    }
}

/// Runs the [`EcoClock`]; off until a caller sets its interval.
pub struct EcoClockPlugin;

impl Plugin for EcoClockPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<EcoClock>()
            .add_systems(Update, (toggle_pause, tick).chain());
    }
}

fn toggle_pause(keys: Option<Res<ButtonInput<KeyCode>>>, mut clock: ResMut<EcoClock>) {
    if keys.is_some_and(|k| k.just_pressed(KeyCode::KeyP)) && clock.interval.is_some() {
        clock.paused = !clock.paused;
        info!(paused = clock.paused, "ecology clock");
    }
}

fn tick(time: Res<Time<Real>>, mut clock: ResMut<EcoClock>, host: Option<Res<GameHost>>) {
    if !clock.due(time.delta()) {
        return;
    }
    let Some(host) = host else {
        return;
    };
    let busy = clock.busy.clone();
    busy.store(true, Ordering::Release);
    let sent = host.submit(move |host: &mut Host| {
        host.eco_substep();
        busy.store(false, Ordering::Release);
    });
    if sent {
        clock.sent += 1;
    } else {
        clock.busy.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: Duration = Duration::from_secs(1);
    const HALF: Duration = Duration::from_millis(500);

    #[test]
    fn clock_fires_once_per_interval() {
        let mut c = EcoClock::new(Some(S));
        assert!(!c.due(HALF));
        assert!(c.due(HALF));
        assert!(!c.due(HALF));
        assert!(c.due(S));
    }

    #[test]
    fn clock_off_paused_or_busy_does_not_fire() {
        assert!(!EcoClock::new(None).due(S));
        assert!(!EcoClock::new(Some(Duration::ZERO)).due(S));
        let mut c = EcoClock::new(Some(S));
        c.paused = true;
        assert!(!c.due(S));
        c.paused = false;
        c.busy.store(true, Ordering::Release);
        assert!(!c.due(S));
        // It fires as soon as the host is free.
        c.busy.store(false, Ordering::Release);
        assert!(c.due(Duration::ZERO));
    }
}
