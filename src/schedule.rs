//! Scheduled plans: a plan (or a playbook with its values) the agent runs on its own
//! at set times, e.g. a nightly `learn`, a weekday-morning review of what changed, or
//! an hourly end-to-end check through a peer. Every run is a normal evidence run.
//!
//! When to run, in the agent's local time:
//!
//! - `every 30m`, `every 2h`, `every 1d` (at most every 5 minutes);
//! - `daily 09:00` (or `every day at 09:00`);
//! - `weekdays 08:30` (Monday to Friday), `weekends 10:00`;
//! - `mon,wed,fri 17:00`.
//!
//! A schedule never overlaps itself: a run that is due while the previous one is
//! still going is skipped. A due run that can't start (e.g. its peer is offline) is
//! tried again every two minutes for an hour, then recorded as missed; runs missed
//! while the agent was off run once when it starts again, within that hour.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Datelike, Local, NaiveTime, TimeZone, Weekday};
use serde::{Deserialize, Serialize};

use crate::profile::validate_name;

/// The shortest interval between runs.
pub const MIN_INTERVAL_MINUTES: u64 = 5;

const HELP: &str =
    "use e.g. 'every 30m', 'every 2h', 'daily 09:00', 'weekdays 08:30' or 'mon,fri 17:00'";

/// When a schedule runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum When {
    /// Every so many minutes after the last run.
    Every(u64),
    /// At a time of day, on some weekdays (none listed: every day).
    At { days: Vec<Weekday>, time: NaiveTime },
}

fn interval(text: &str) -> Result<u64> {
    let digits = text.chars().take_while(char::is_ascii_digit).count();
    let (n, unit) = text.split_at(digits);
    let n: u64 = n
        .parse()
        .map_err(|_| anyhow!("{text:?} is not an interval; {HELP}"))?;
    let minutes = match unit {
        "m" | "min" | "mins" | "minute" | "minutes" => n,
        "h" | "hr" | "hrs" | "hour" | "hours" => n * 60,
        "d" | "day" | "days" => n * 24 * 60,
        _ => bail!("unknown unit in {text:?}; {HELP}"),
    };
    if minutes < MIN_INTERVAL_MINUTES {
        bail!("schedules run at most every {MIN_INTERVAL_MINUTES} minutes");
    }
    if minutes > 366 * 24 * 60 {
        bail!("schedules run at least once a year");
    }
    Ok(minutes)
}

fn days(text: &str) -> Result<Vec<Weekday>> {
    use Weekday::*;
    Ok(match text {
        "day" | "daily" => Vec::new(),
        "weekday" | "weekdays" => vec![Mon, Tue, Wed, Thu, Fri],
        "weekend" | "weekends" => vec![Sat, Sun],
        list => {
            let mut days = Vec::new();
            for day in list.split(',').filter(|d| !d.is_empty()) {
                let day: Weekday = day
                    .parse()
                    .map_err(|_| anyhow!("{day:?} is not a day of the week; {HELP}"))?;
                if !days.contains(&day) {
                    days.push(day);
                }
            }
            if days.is_empty() {
                bail!("no days in {list:?}; {HELP}");
            }
            days
        }
    })
}

impl When {
    pub fn parse(text: &str) -> Result<Self> {
        let lower = text.trim().to_ascii_lowercase();
        let words: Vec<&str> = lower
            .split_whitespace()
            .filter(|w| !matches!(*w, "every" | "at" | "on"))
            .collect();
        let time = |t: &str| {
            NaiveTime::parse_from_str(t, "%H:%M")
                .map_err(|_| anyhow!("{t:?} is not a time like 09:00; {HELP}"))
        };
        match words.as_slice() {
            [n, unit] if n.chars().all(|c| c.is_ascii_digit()) => {
                Ok(When::Every(interval(&format!("{n}{unit}"))?))
            }
            [one] if one.starts_with(|c: char| c.is_ascii_digit()) && !one.contains(':') => {
                Ok(When::Every(interval(one)?))
            }
            [t] => Ok(When::At {
                days: Vec::new(),
                time: time(t)?,
            }),
            [d, t] => Ok(When::At {
                days: days(d)?,
                time: time(t)?,
            }),
            _ => bail!("can't read {text:?}; {HELP}"),
        }
    }

    /// The first time to run strictly after `after`.
    pub fn next_after<Tz: TimeZone>(&self, after: &DateTime<Tz>) -> DateTime<Tz> {
        match self {
            When::Every(minutes) => after.clone() + chrono::Duration::minutes(*minutes as i64),
            When::At { days, time } => {
                let tz = after.timezone();
                let today = after.date_naive();
                for offset in 0..=8 {
                    let date = today + chrono::Duration::days(offset);
                    if !days.is_empty() && !days.contains(&date.weekday()) {
                        continue;
                    }
                    // A time skipped by a clock change has no run that day.
                    if let Some(at) = tz.from_local_datetime(&date.and_time(*time)).earliest()
                        && at > *after
                    {
                        return at;
                    }
                }
                after.clone() + chrono::Duration::days(1)
            }
        }
    }
}

fn yes() -> bool {
    true
}

/// How long after a failed start (e.g. a peer still reconnecting) a due run is tried again.
pub const RETRY_SECS: i64 = 120;
/// How long a due run that can't start is retried before it is counted as missed.
pub const GRACE_SECS: i64 = 3600;

/// A plan the agent runs on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    pub name: String,
    /// When to run (see the module docs).
    pub when: String,
    /// The plan text to run.
    pub plan: String,
    /// Where the plan came from (a file or a playbook), for listings.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub created_unix: i64,
    /// When its time counts from, if later than its last run: when it was resumed,
    /// changed, or a run was missed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_unix: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_unix: Option<i64>,
    /// The last run's verdict, or why it didn't run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
    /// The last run's evidence run id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_id: Option<String>,
    /// A due run couldn't start: try again at this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_unix: Option<i64>,
}

impl Schedule {
    /// A new, enabled schedule; fails unless the name, `when` and the plan are valid.
    pub fn new(name: &str, when: &str, plan: &str, source: &str) -> Result<Self> {
        validate_name(name).context("invalid schedule name")?;
        When::parse(when)?;
        crate::plan::Plan::parse(plan).context("the scheduled plan is invalid")?;
        Ok(Self {
            name: name.to_owned(),
            when: when.trim().to_owned(),
            plan: plan.to_owned(),
            source: source.to_owned(),
            enabled: true,
            created_unix: Local::now().timestamp(),
            anchor_unix: None,
            last_run_unix: None,
            last_result: None,
            last_run_id: None,
            retry_unix: None,
        })
    }

    /// The time its next run counts from.
    fn since(&self) -> i64 {
        self.created_unix
            .max(self.last_run_unix.unwrap_or(i64::MIN))
            .max(self.anchor_unix.unwrap_or(i64::MIN))
    }

    /// When it runs next (`None` when disabled or `when` is invalid).
    pub fn next_run(&self) -> Option<DateTime<Local>> {
        if !self.enabled {
            return None;
        }
        let since = Local.timestamp_opt(self.since(), 0).single()?;
        Some(When::parse(&self.when).ok()?.next_after(&since))
    }

    /// Its next `n` run times.
    pub fn next_runs(&self, n: usize) -> Vec<DateTime<Local>> {
        let (Some(mut at), Ok(when)) = (self.next_run(), When::parse(&self.when)) else {
            return Vec::new();
        };
        let mut runs = vec![at];
        while runs.len() < n {
            at = when.next_after(&at);
            runs.push(at);
        }
        runs
    }

    pub fn due(&self, now: &DateTime<Local>) -> bool {
        self.next_run().is_some_and(|next| next <= *now)
            && self.retry_unix.is_none_or(|retry| retry <= now.timestamp())
    }

    /// Sets when it runs, counting from now.
    pub fn set_when(&mut self, when: &str) -> Result<()> {
        When::parse(when)?;
        self.when = when.trim().to_owned();
        self.anchor_unix = Some(Local::now().timestamp());
        self.retry_unix = None;
        Ok(())
    }

    /// Sets the plan it runs; fails unless the plan is valid.
    pub fn set_plan(&mut self, plan: &str) -> Result<()> {
        crate::plan::Plan::parse(plan).context("the scheduled plan is invalid")?;
        self.plan = plan.to_owned();
        Ok(())
    }

    /// Pauses it, or resumes it counting from now (a paused time is not caught up).
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled {
            self.anchor_unix = Some(Local::now().timestamp());
        }
        self.enabled = enabled;
        self.retry_unix = None;
    }

    /// The last run's state: `running`, `passed`, `failed`, `no-claims`,
    /// `not-run` (it couldn't start), `missed` or `cancelled`.
    pub fn last_status(&self) -> Option<&'static str> {
        let r = self.last_result.as_deref()?;
        Some(if r == "running" {
            "running"
        } else if r.starts_with("PASS") {
            "passed"
        } else if r.starts_with("FAIL") {
            "failed"
        } else if r.starts_with("NO CLAIMS") {
            "no-claims"
        } else if r.starts_with("missed") {
            "missed"
        } else if r.starts_with("cancelled") {
            "cancelled"
        } else {
            "not-run"
        })
    }
}

/// When a schedule runs, for display: `{"every_minutes": 30}` or
/// `{"days": ["mon", ...], "time": "09:00"}` (no days: every day).
pub fn when_parts(when: &str) -> Option<serde_json::Value> {
    Some(match When::parse(when).ok()? {
        When::Every(minutes) => serde_json::json!({ "every_minutes": minutes }),
        When::At { days, time } => serde_json::json!({
            "days": days.iter().map(|d| d.to_string().to_ascii_lowercase()).collect::<Vec<_>>(),
            "time": time.format("%H:%M").to_string(),
        }),
    })
}

/// The first `n` times a new schedule running `when` would run after `from`.
pub fn upcoming(when: &str, from: &DateTime<Local>, n: usize) -> Result<Vec<DateTime<Local>>> {
    let when = When::parse(when)?;
    let mut at = *from;
    Ok((0..n)
        .map(|_| {
            at = when.next_after(&at);
            at
        })
        .collect())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleStore {
    #[serde(default)]
    pub schedules: Vec<Schedule>,
}

impl ScheduleStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::store::write_atomic(path, &serde_json::to_vec_pretty(self)?)
    }

    /// Loads the store at `path`, changes it with `change` and saves it, holding its
    /// lock, so changes from the agent, the web UI and the CLI never overwrite each other.
    pub fn update<T>(
        path: &Path,
        change: impl FnOnce(&mut ScheduleStore) -> Result<T>,
    ) -> Result<T> {
        let _lock = crate::store::FileLock::acquire(path)?;
        let mut store = Self::load(path)?;
        let out = change(&mut store)?;
        store.save(path)?;
        Ok(out)
    }

    /// Adds `schedule`, replacing one of the same name.
    pub fn add(&mut self, schedule: Schedule) {
        self.schedules.retain(|s| s.name != schedule.name);
        self.schedules.push(schedule);
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.schedules.len();
        self.schedules.retain(|s| s.name != name);
        self.schedules.len() != before
    }

    pub fn get(&self, name: &str) -> Option<&Schedule> {
        self.schedules.iter().find(|s| s.name == name)
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Schedule> {
        self.schedules.iter_mut().find(|s| s.name == name)
    }
}

/// How a run ended: its verdict line and its evidence run id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub verdict: String,
    pub run_id: Option<String>,
}

/// Starts one schedule's plan: checks it (peers, folders, checks...) and returns
/// the run to await, or why it can't start now.
pub type Launch =
    Arc<dyn Fn(Schedule) -> Result<Pin<Box<dyn Future<Output = Outcome> + Send>>> + Send + Sync>;

/// Removes a schedule from the running set however its run ends (even a panic).
struct Running {
    set: Arc<Mutex<HashSet<String>>>,
    name: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.set.lock().unwrap().remove(&self.name);
    }
}

/// Starts schedules when they are due (or on request), one run per schedule at a
/// time, and records each run's start and verdict in the store.
#[derive(Clone)]
pub struct Scheduler {
    path: PathBuf,
    launch: Launch,
    running: Arc<Mutex<HashSet<String>>>,
    changed: Option<Arc<dyn Fn() + Send + Sync>>,
    /// Runs that fell due before this were missed while the agent was off: their
    /// grace period counts from here, so peers have time to reconnect.
    started_unix: i64,
}

impl Scheduler {
    pub fn new(path: PathBuf, launch: Launch) -> Self {
        Self {
            path,
            launch,
            running: Arc::default(),
            changed: None,
            started_unix: Local::now().timestamp(),
        }
    }

    /// Calls `changed` whenever the scheduler records a start, a verdict, a retry or a miss.
    pub fn on_change(mut self, changed: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.changed = Some(changed);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.running.lock().unwrap().contains(name)
    }

    /// Checks for due schedules every `tick`, the first time after one `tick` (so
    /// peers have reconnected). The store is read each time, so schedules added or
    /// changed while the agent runs are picked up.
    pub fn spawn(&self, tick: Duration) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + tick, tick);
            loop {
                ticks.tick().await;
                this.run_due(&Local::now());
            }
        })
    }

    /// Starts every enabled schedule due at `now` that isn't running; returns the
    /// names of those that started. A due run that can't start (e.g. its peer is
    /// offline) is tried again every [`RETRY_SECS`] for [`GRACE_SECS`], then
    /// counted as missed.
    pub fn run_due(&self, now: &DateTime<Local>) -> Vec<String> {
        let Ok(store) = ScheduleStore::load(&self.path) else {
            return Vec::new();
        };
        let mut started = Vec::new();
        for s in store.schedules.into_iter().filter(|s| s.due(now)) {
            if self.is_running(&s.name) {
                continue;
            }
            let name = s.name.clone();
            let due_unix = s
                .next_run()
                .map_or(now.timestamp(), |t| t.timestamp())
                .max(self.started_unix);
            match self.start(s, now) {
                Ok(()) => started.push(name),
                Err(e) => {
                    let late = now.timestamp() - due_unix > GRACE_SECS;
                    let at = now.timestamp();
                    self.record(&name, |s| {
                        if late {
                            s.last_result = Some(format!("missed: {e:#}"));
                            s.anchor_unix = Some(at);
                            s.retry_unix = None;
                        } else {
                            s.last_result = Some(format!("not run yet (trying again): {e:#}"));
                            s.retry_unix = Some(at + RETRY_SECS);
                        }
                    });
                }
            }
        }
        started
    }

    /// Runs schedule `name` now (enabled or not); fails if it is running or can't start.
    pub fn trigger(&self, name: &str) -> Result<()> {
        let store = ScheduleStore::load(&self.path)?;
        let schedule = store
            .get(name)
            .cloned()
            .with_context(|| format!("no schedule named '{name}'"))?;
        if self.is_running(name) {
            bail!("schedule '{name}' is already running");
        }
        self.start(schedule, &Local::now())
    }

    fn start(&self, schedule: Schedule, now: &DateTime<Local>) -> Result<()> {
        let name = schedule.name.clone();
        if !self.running.lock().unwrap().insert(name.clone()) {
            bail!("schedule '{name}' is already running");
        }
        let guard = Running {
            set: self.running.clone(),
            name: name.clone(),
        };
        let run = (self.launch)(schedule)?;
        let at = now.timestamp();
        self.record(&name, |s| {
            s.last_run_unix = Some(at);
            s.last_result = Some("running".into());
            s.last_run_id = None;
            s.retry_unix = None;
        });
        let this = self.clone();
        tokio::spawn(async move {
            let _guard = guard;
            use futures_util::FutureExt;
            let outcome = std::panic::AssertUnwindSafe(run)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Outcome {
                    verdict: "cancelled: the run stopped unexpectedly".into(),
                    run_id: None,
                });
            this.record(&name, |s| {
                s.last_result = Some(outcome.verdict);
                s.last_run_id = outcome.run_id;
            });
        });
        Ok(())
    }

    fn record(&self, name: &str, update: impl FnOnce(&mut Schedule)) {
        let result = ScheduleStore::update(&self.path, |store| {
            if let Some(s) = store.get_mut(name) {
                update(s);
            }
            Ok(())
        });
        match result {
            Ok(()) => {
                if let Some(changed) = &self.changed {
                    changed();
                }
            }
            Err(e) => eprintln!("could not record schedule '{name}': {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Timelike, Utc};

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn when_reads_intervals_times_and_days() {
        use Weekday::*;
        let nine = NaiveTime::from_hms_opt(9, 0, 0).unwrap();
        assert_eq!(When::parse("every 30m").unwrap(), When::Every(30));
        assert_eq!(When::parse("Every 2 hours").unwrap(), When::Every(120));
        assert_eq!(When::parse("1d").unwrap(), When::Every(1440));
        let daily = When::At {
            days: vec![],
            time: nine,
        };
        assert_eq!(When::parse("daily 09:00").unwrap(), daily);
        assert_eq!(When::parse("every day at 09:00").unwrap(), daily);
        assert_eq!(When::parse("09:00").unwrap(), daily);
        assert_eq!(
            When::parse("weekdays 09:00").unwrap(),
            When::At {
                days: vec![Mon, Tue, Wed, Thu, Fri],
                time: nine
            }
        );
        assert_eq!(
            When::parse("every mon,fri at 09:00").unwrap(),
            When::At {
                days: vec![Mon, Fri],
                time: nine
            }
        );
        for bad in [
            "every 2m",
            "every 30s",
            "daily 25:00",
            "funday 09:00",
            "",
            "now",
        ] {
            assert!(When::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn next_run_follows_the_last_run() {
        // 2026-11-20 is a Friday.
        let friday_noon = at("2026-11-20T12:00:00Z");
        assert_eq!(
            When::parse("every 30m").unwrap().next_after(&friday_noon),
            at("2026-11-20T12:30:00Z")
        );
        let daily = When::parse("daily 09:00").unwrap();
        assert_eq!(daily.next_after(&friday_noon), at("2026-11-21T09:00:00Z"));
        assert_eq!(
            daily.next_after(&at("2026-11-20T08:59:00Z")),
            at("2026-11-20T09:00:00Z")
        );
        assert_eq!(
            When::parse("weekdays 09:00")
                .unwrap()
                .next_after(&friday_noon),
            at("2026-11-23T09:00:00Z"),
            "skips the weekend"
        );
        assert_eq!(
            When::parse("sun 18:30").unwrap().next_after(&friday_noon),
            at("2026-11-22T18:30:00Z")
        );
    }

    #[tokio::test]
    async fn the_scheduler_runs_due_schedules_once_at_a_time_and_records_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedules.json");
        let plan = "check @worker Report the health of the service";
        let mut store = ScheduleStore::default();
        let mut hourly = Schedule::new("hourly", "every 1h", plan, "plan.txt").unwrap();
        hourly.created_unix -= 2 * 3600;
        let mut off = hourly.clone();
        off.name = "off".into();
        off.enabled = false;
        let mut offline = hourly.clone();
        offline.name = "offline".into();
        offline.created_unix += 30 * 60; // due half an hour ago: well inside the grace period
        let mut downtime = offline.clone();
        downtime.name = "offline-long".into();
        downtime.created_unix -= 2 * 86400; // fell due while the agent was off
        let mut boom = hourly.clone();
        boom.name = "boom".into();
        boom.enabled = false;
        let nightly_hour = (Local::now().hour() + 12) % 24;
        let nightly =
            Schedule::new("nightly", &format!("daily {nightly_hour:02}:00"), plan, "").unwrap();
        for s in [hourly, off, offline, downtime, boom, nightly] {
            store.add(s);
        }
        store.save(&path).unwrap();
        assert!(Schedule::new("bad", "every 1h", "not a plan", "").is_err());
        assert!(Schedule::new("../x", "every 1h", plan, "").is_err());

        let (release, wait) = tokio::sync::watch::channel(false);
        let launch: Launch = Arc::new(move |s: Schedule| {
            if s.name.starts_with("offline") {
                bail!("worker is not connected");
            }
            let mut wait = wait.clone();
            Ok(Box::pin(async move {
                wait.wait_for(|go| *go).await.unwrap();
                if s.name == "boom" {
                    panic!("the run panics");
                }
                Outcome {
                    verdict: format!("PASS: ran {}", s.name),
                    run_id: Some(format!("run-{}", s.name)),
                }
            }))
        });
        let scheduler = Scheduler::new(path.clone(), launch);
        let now = Local::now();
        assert_eq!(
            scheduler.run_due(&now),
            ["hourly"],
            "only due, enabled ones that can start"
        );
        let saved = ScheduleStore::load(&path).unwrap();
        let hourly = saved.get("hourly").unwrap();
        assert_eq!(hourly.last_status(), Some("running"));
        assert!(!hourly.due(&now), "next run is an hour later");
        assert!(scheduler.trigger("hourly").is_err(), "already running");
        assert!(scheduler.trigger("missing").is_err());

        // A due run that can't start is tried again later, then counted as missed.
        let offline = saved.get("offline").unwrap().clone();
        assert!(
            offline
                .last_result
                .as_deref()
                .unwrap()
                .starts_with("not run yet (trying again): worker is not connected")
        );
        assert!(offline.last_run_unix.is_none(), "it didn't run");
        assert!(!offline.due(&(now + chrono::Duration::seconds(60))));
        assert!(offline.due(&(now + chrono::Duration::seconds(RETRY_SECS + 1))));
        let downtime = saved.get("offline-long").unwrap();
        assert_eq!(
            downtime.last_status(),
            Some("not-run"),
            "its peers get time to reconnect after a restart"
        );
        let later = now + chrono::Duration::hours(2);
        assert!(
            scheduler.run_due(&later).is_empty(),
            "hourly is still running (no overlap); offline can't start"
        );
        let offline = ScheduleStore::load(&path)
            .unwrap()
            .get("offline")
            .unwrap()
            .clone();
        assert_eq!(offline.last_status(), Some("missed"), "over an hour late");
        assert_eq!(
            offline.next_run().unwrap().timestamp(),
            later.timestamp() + 3600,
            "counts from the miss"
        );
        let err = scheduler.trigger("offline").unwrap_err().to_string();
        assert!(err.contains("not connected"), "Run now says why: {err}");

        // A run that panics still frees its schedule.
        scheduler.trigger("boom").unwrap();
        release.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while scheduler.is_running("hourly") || scheduler.is_running("boom") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let saved = ScheduleStore::load(&path).unwrap();
        let hourly = saved.get("hourly").unwrap();
        assert_eq!(hourly.last_result.as_deref(), Some("PASS: ran hourly"));
        assert_eq!(
            (hourly.last_status(), hourly.last_run_id.as_deref()),
            (Some("passed"), Some("run-hourly"))
        );
        assert_eq!(
            saved.get("boom").unwrap().last_status(),
            Some("cancelled"),
            "not stuck at running"
        );
        assert!(saved.get("nightly").unwrap().last_run_unix.is_none());

        // Pausing and resuming counts from the resume, without a made-up last run.
        ScheduleStore::update(&path, |store| {
            let s = store.get_mut("off").unwrap();
            s.set_enabled(true);
            Ok(())
        })
        .unwrap();
        let off = ScheduleStore::load(&path)
            .unwrap()
            .get("off")
            .unwrap()
            .clone();
        assert!(off.last_run_unix.is_none() && !off.due(&Local::now()));
        assert_eq!(off.next_runs(3).len(), 3);
        assert_eq!(when_parts("weekdays 08:30").unwrap()["days"][0], "mon");
        assert_eq!(when_parts("every 2h").unwrap()["every_minutes"], 120);
        let from = Local::now();
        let next = upcoming("every 2h", &from, 3).unwrap();
        assert_eq!(
            next.iter()
                .map(|t| t.timestamp() - from.timestamp())
                .collect::<Vec<_>>(),
            [7200, 14400, 21600]
        );
        assert!(upcoming("every 1m", &from, 3).is_err());
    }
}
