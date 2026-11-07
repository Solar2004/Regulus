//! Cron and Idle: a thread per node that sleeps until its next moment and wakes at once
//! when the node stops or, for Idle, when something arrives.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, TimeZone, Utc};
use croner::parser::{CronParser, Seconds};
use croner::Cron;

/// What a timer thread and the module share.
#[derive(Default)]
pub struct State {
    pub stopped: bool,
    /// Idle: when something last arrived, or the start.
    pub last: Option<Instant>,
}

pub type Shared = Arc<(Mutex<State>, Condvar)>;

pub struct Timer {
    pub shared: Shared,
    handle: Option<JoinHandle<()>>,
}

impl Timer {
    pub fn spawn(
        name: &str,
        shared: Shared,
        run: impl FnOnce(Shared) + Send + 'static,
    ) -> Result<Self, String> {
        let thread = shared.clone();
        let handle = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || run(thread))
            .map_err(|e| format!("cannot start a timer thread: {e}"))?;
        Ok(Self {
            shared,
            handle: Some(handle),
        })
    }

    /// Something arrived: Idle counts its silence from now.
    pub fn touch(&self) {
        let (state, wake) = &*self.shared;
        state.lock().unwrap_or_else(|e| e.into_inner()).last = Some(Instant::now());
        wake.notify_all();
    }

    /// Stops the thread and waits for it.
    pub fn stop(mut self) {
        {
            let (state, wake) = &*self.shared;
            state.lock().unwrap_or_else(|e| e.into_inner()).stopped = true;
            wake.notify_all();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Parses a schedule of five fields, or six with seconds first.
pub fn schedule(pattern: &str) -> Result<Cron, String> {
    CronParser::builder()
        .seconds(Seconds::Optional)
        .build()
        .parse(pattern)
        .map_err(|e| format!("schedule `{pattern}`: {e}"))
}

/// Waits until `deadline` or a stop. True if it stopped.
fn sleep_until(shared: &Shared, deadline: Instant) -> bool {
    let (state, wake) = &**shared;
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if guard.stopped {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        guard = wake
            .wait_timeout(guard, deadline - now)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
}

/// The next match after now, and how long until then.
fn next<Tz: TimeZone>(cron: &Cron, now: DateTime<Tz>) -> Result<(String, Duration), String>
where
    Tz::Offset: std::fmt::Display,
{
    let next = cron
        .find_next_occurrence(&now, false)
        .map_err(|e| e.to_string())?;
    let wait = (next.clone() - now).to_std().unwrap_or_default();
    Ok((next.to_rfc3339(), wait))
}

/// Calls `tick` with the time of each match until stopped. `utc` picks the zone the
/// schedule is read in.
pub fn run_cron(shared: Shared, cron: Cron, utc: bool, tick: impl Fn(String)) {
    loop {
        let upcoming = if utc {
            next(&cron, Utc::now())
        } else {
            next(&cron, Local::now())
        };
        let (at, wait) = match upcoming {
            Ok(next) => next,
            Err(error) => {
                eprintln!("regulus: triggers: cron stops: {error}");
                return;
            }
        };
        if sleep_until(&shared, Instant::now() + wait) {
            return;
        }
        tick(at);
    }
}

/// Calls `fire` each time nothing arrived for `after`; with `repeat`, again every `after`
/// while it stays quiet.
pub fn run_idle(shared: Shared, after: Duration, repeat: bool, fire: impl Fn()) {
    let (state, wake) = &*shared;
    let mut fired_for: Option<Instant> = None;
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if guard.stopped {
            return;
        }
        let last = *guard.last.get_or_insert_with(Instant::now);
        let quiet_since = if repeat {
            fired_for.unwrap_or(last).max(last)
        } else {
            last
        };
        let deadline = quiet_since + after;
        let now = Instant::now();
        if fired_for == Some(last) && !repeat {
            // Fired for this silence: wait for something to arrive.
            guard = wake.wait(guard).unwrap_or_else(|e| e.into_inner());
            continue;
        }
        if now < deadline {
            guard = wake
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
            continue;
        }
        drop(guard);
        fire();
        fired_for = Some(if repeat { Instant::now() } else { last });
        guard = state.lock().unwrap_or_else(|e| e.into_inner());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedules_take_five_or_six_fields() {
        assert!(schedule("*/2 * * * * *").is_ok());
        assert!(schedule("0 9 * * mon-fri").is_ok());
        assert!(schedule("@hourly").is_ok());
        assert!(schedule("every two seconds").is_err());
    }

    #[test]
    fn a_stopped_timer_wakes_at_once() {
        let shared = Shared::default();
        let timer = Timer::spawn("t", shared, |shared| {
            run_cron(shared, schedule("0 0 1 1 *").unwrap(), true, |_| {});
        })
        .unwrap();
        let started = Instant::now();
        timer.stop();
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}
