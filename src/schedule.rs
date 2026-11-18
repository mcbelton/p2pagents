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
//! still going is skipped. Runs missed while the agent was off run once when it
//! starts again.

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_unix: Option<i64>,
    /// The last run's verdict, or why it didn't run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<String>,
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
            last_run_unix: None,
            last_result: None,
        })
    }

    /// When it runs next (`None` when disabled or `when` is invalid).
    pub fn next_run(&self) -> Option<DateTime<Local>> {
        if !self.enabled {
            return None;
        }
        let since = self.last_run_unix.unwrap_or(self.created_unix);
        let since = Local.timestamp_opt(since, 0).single()?;
        Some(When::parse(&self.when).ok()?.next_after(&since))
    }

    pub fn due(&self, now: &DateTime<Local>) -> bool {
        self.next_run().is_some_and(|next| next <= *now)
    }
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
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", path.display()))
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

/// Runs one schedule's plan; returns the run's verdict line.
pub type Launch =
    Arc<dyn Fn(Schedule) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

/// Starts schedules when they are due (or on request), one run per schedule at a
/// time, and records each run's start and verdict in the store.
#[derive(Clone)]
pub struct Scheduler {
    path: PathBuf,
    launch: Launch,
    running: Arc<Mutex<HashSet<String>>>,
    /// Serializes this agent's updates to the store.
    write: Arc<Mutex<()>>,
}

impl Scheduler {
    pub fn new(path: PathBuf, launch: Launch) -> Self {
        Self {
            path,
            launch,
            running: Arc::default(),
            write: Arc::default(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn is_running(&self, name: &str) -> bool {
        self.running.lock().unwrap().contains(name)
    }

    /// Checks for due schedules every `tick`. The store is read each time, so
    /// schedules added or changed while the agent runs are picked up.
    pub fn spawn(&self, tick: Duration) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let mut ticks = tokio::time::interval(tick);
            loop {
                ticks.tick().await;
                this.run_due(&Local::now());
            }
        })
    }

    /// Starts every enabled schedule due at `now` that isn't running; returns their names.
    pub fn run_due(&self, now: &DateTime<Local>) -> Vec<String> {
        let Ok(store) = ScheduleStore::load(&self.path) else {
            return Vec::new();
        };
        store
            .schedules
            .into_iter()
            .filter(|s| s.due(now))
            .filter_map(|s| {
                let name = s.name.clone();
                self.start(s, now).then_some(name)
            })
            .collect()
    }

    /// Runs schedule `name` now (enabled or not), unless it is running.
    pub fn trigger(&self, name: &str) -> Result<()> {
        let store = ScheduleStore::load(&self.path)?;
        let schedule = store
            .get(name)
            .cloned()
            .with_context(|| format!("no schedule named '{name}'"))?;
        if !self.start(schedule, &Local::now()) {
            bail!("schedule '{name}' is already running");
        }
        Ok(())
    }

    fn start(&self, schedule: Schedule, now: &DateTime<Local>) -> bool {
        if !self.running.lock().unwrap().insert(schedule.name.clone()) {
            return false;
        }
        let name = schedule.name.clone();
        let started = now.timestamp();
        self.record(&name, |s| {
            s.last_run_unix = Some(started);
            s.last_result = Some("running".into());
        });
        let this = self.clone();
        tokio::spawn(async move {
            let result = match (this.launch)(schedule).await {
                Ok(verdict) => verdict,
                Err(e) => format!("not run: {e:#}"),
            };
            this.record(&name, |s| s.last_result = Some(result));
            this.running.lock().unwrap().remove(&name);
        });
        true
    }

    fn record(&self, name: &str, update: impl FnOnce(&mut Schedule)) {
        let _guard = self.write.lock().unwrap();
        let Ok(mut store) = ScheduleStore::load(&self.path) else {
            return;
        };
        if let Some(s) = store.get_mut(name) {
            update(s);
            if let Err(e) = store.save(&self.path) {
                eprintln!("could not record schedule '{name}': {e:#}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

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
        let nightly = Schedule::new("nightly", "daily 03:00", plan, "").unwrap();
        store.add(hourly);
        store.add(off);
        store.add(nightly);
        store.save(&path).unwrap();
        assert!(Schedule::new("bad", "every 1h", "not a plan", "").is_err());
        assert!(Schedule::new("../x", "every 1h", plan, "").is_err());

        let (release, wait) = tokio::sync::watch::channel(false);
        let launch: Launch = Arc::new(move |s: Schedule| {
            let mut wait = wait.clone();
            Box::pin(async move {
                wait.wait_for(|go| *go).await.unwrap();
                if s.name == "off" {
                    bail!("peer gone");
                }
                Ok(format!("PASS: ran {}", s.name))
            })
        });
        let scheduler = Scheduler::new(path.clone(), launch);
        let now = Local::now();
        assert_eq!(
            scheduler.run_due(&now),
            ["hourly"],
            "only due, enabled ones"
        );
        let saved = ScheduleStore::load(&path).unwrap();
        assert_eq!(
            saved.get("hourly").unwrap().last_result.as_deref(),
            Some("running")
        );
        assert!(
            !saved.get("hourly").unwrap().due(&now),
            "next run is an hour later"
        );
        assert!(
            scheduler
                .run_due(&(now + chrono::Duration::hours(2)))
                .is_empty(),
            "no overlap"
        );
        assert!(scheduler.trigger("hourly").is_err(), "already running");
        scheduler.trigger("off").unwrap();
        assert!(scheduler.trigger("missing").is_err());

        release.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while scheduler.is_running("hourly") || scheduler.is_running("off") {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let saved = ScheduleStore::load(&path).unwrap();
        assert_eq!(
            saved.get("hourly").unwrap().last_result.as_deref(),
            Some("PASS: ran hourly")
        );
        assert_eq!(
            saved.get("off").unwrap().last_result.as_deref(),
            Some("not run: peer gone")
        );
        assert!(saved.get("nightly").unwrap().last_run_unix.is_none());
    }
}
