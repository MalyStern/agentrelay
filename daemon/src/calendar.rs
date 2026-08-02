//! Schedule a run around meetings and quiet hours (plan §ט: Calendar + quiet-hours,
//! Phase-4).
//!
//! This is the same decision `quota::decide_start` makes, against a different wall.
//! There the wall is the provider's quota window; here it is your day — the hours
//! you are in meetings, or simply do not want an agent burning quota and CPU on the
//! machine you are using. Given a set of forbidden windows, the question is: may a
//! unit of roughly this length start *now*, or would it collide with one? And if it
//! would, when does the next clear stretch open?
//!
//! Two kinds of forbidden window, both meaning "do not run":
//!
//! * **Quiet hours** — a weekly recurring pattern, e.g. 09:00–17:00 on weekdays.
//! * **Busy blocks** — one-off absolute time ranges, e.g. a meeting from 14:00 to
//!   15:00 next Tuesday. This is the shape a real calendar feed would drop into.
//!
//! Load-bearing choices, in the register of `quota`:
//!
//! * **`now` is a parameter, never read from the clock inside the logic.** That is
//!   what makes the whole thing DST-agnostic and testable: the caller resolves the
//!   wall-clock instant (the daemon passes `Local::now()`, which already reflects the
//!   machine's current offset), and the arithmetic here is pure.
//! * **Nothing configured means run, and it says so** (`StartNoSchedule`) — refusing
//!   to work because no schedule was given would be a worse failure than the risk it
//!   guards against, exactly as `decide_start` returns `StartUnknownWindow`.
//! * **A malformed schedule is a loud error, never a silently-dropped window.** A
//!   quiet block the user believes is protecting their 10am standup, but which was
//!   ignored because "9am" would not parse, is the fail-quiet shape this codebase
//!   keeps stamping out — so an unparseable day, time or timezone refuses.
//! * **A live Google/CalDAV feed is a documented seam, not a promise.** Fetching a
//!   calendar needs credentials and a network; this module implements the local
//!   scheduling fully and leaves the fetch as the clearly-marked next step, the same
//!   way `cloud` and `isolation` leave their outer edges visible.

use crate::error::{AgentRelayError, ErrorCode, Result};
use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, SecondsFormat, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

/// A weekly quiet window: on these weekdays, between these wall-clock times, do not
/// run. `days` empty means every day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuietHours {
    /// Weekday names ("mon"…"sun", or the full names). Empty = every day.
    #[serde(default)]
    pub days: Vec<String>,
    /// "HH:MM", 24-hour.
    pub start: String,
    /// "HH:MM", 24-hour. Must be after `start` — a window that wraps midnight is
    /// refused rather than guessed at (write it as two windows).
    pub end: String,
}

/// A one-off busy block, in absolute time. The shape a calendar event maps to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusyBlock {
    /// RFC3339 start, e.g. "2026-08-04T14:00:00+02:00".
    pub start: String,
    /// RFC3339 end.
    pub end: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// The schedule config. All fields optional; an empty schedule means "run whenever".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Schedule {
    #[serde(default)]
    pub quiet_hours: Vec<QuietHours>,
    #[serde(default)]
    pub busy: Vec<BusyBlock>,
    /// A fixed-offset timezone the quiet-hours wall-clock times are read in, e.g.
    /// "+02:00". Unset means the times are read in whatever offset `now` carries
    /// (the machine's local time, as the daemon passes it). An IANA name like
    /// "Asia/Jerusalem" is NOT accepted here — that needs a live timezone database
    /// and is part of the calendar-integration seam — and asking for one is a loud
    /// error rather than a silently-wrong offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// What to do with a unit given the schedule. Mirrors `quota::StartDecision`.
#[derive(Debug, Clone, PartialEq)]
pub enum ScheduleDecision {
    /// There is a clear stretch. Go.
    Start,
    /// Now falls in (or the unit would run into) a forbidden window. Waiting until
    /// it clears costs less than being interrupted mid-unit.
    WaitUntil { resume_at_utc: String, wait_ms: i64 },
    /// No schedule is configured, so there is nothing to reason about. Go — stated,
    /// not silent, so "no schedule" and "schedule says fine" never read the same.
    StartNoSchedule,
}

impl ScheduleDecision {
    pub fn may_start_now(&self) -> bool {
        !matches!(self, ScheduleDecision::WaitUntil { .. })
    }
}

const DEFAULT_BUFFER_MS: i64 = 60_000;

fn to_iso(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn parse_day(s: &str) -> Result<Weekday> {
    Ok(match s.trim().to_ascii_lowercase().as_str() {
        "mon" | "monday" => Weekday::Mon,
        "tue" | "tues" | "tuesday" => Weekday::Tue,
        "wed" | "weds" | "wednesday" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thu,
        "fri" | "friday" => Weekday::Fri,
        "sat" | "saturday" => Weekday::Sat,
        "sun" | "sunday" => Weekday::Sun,
        other => {
            return Err(AgentRelayError::new(
                ErrorCode::Internal,
                format!(
                    "schedule: \"{other}\" is not a weekday. Use mon/tue/wed/thu/fri/sat/sun \
                     (or the full names)."
                ),
            ))
        }
    })
}

/// "HH:MM" -> minutes since midnight, refusing anything that is not a real time.
fn parse_hhmm(s: &str) -> Result<i64> {
    let bad = || {
        AgentRelayError::new(
            ErrorCode::Internal,
            format!("schedule: \"{s}\" is not a HH:MM time (00:00–23:59)."),
        )
    };
    let (h, m) = s.trim().split_once(':').ok_or_else(bad)?;
    let h: i64 = h.parse().map_err(|_| bad())?;
    let m: i64 = m.parse().map_err(|_| bad())?;
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
        return Err(bad());
    }
    Ok(h * 60 + m)
}

/// Parse a fixed-offset timezone like "+02:00", "-05:30" or "Z".
fn parse_offset(s: &str) -> Result<FixedOffset> {
    let bad = || {
        AgentRelayError::new(
            ErrorCode::Internal,
            format!(
                "schedule: timezone \"{s}\" is not a fixed offset. Use a form like \"+02:00\", \
                 \"-05:00\" or \"Z\". IANA names (e.g. \"Asia/Jerusalem\") need a live timezone \
                 database and are part of the calendar-integration seam that is not built yet — \
                 leave `timezone` unset to use this machine's local time."
            ),
        )
    };
    let t = s.trim();
    if t == "Z" || t == "z" {
        return Ok(FixedOffset::east_opt(0).unwrap());
    }
    let (sign, rest) = match t.strip_prefix('+') {
        Some(r) => (1, r),
        None => match t.strip_prefix('-') {
            Some(r) => (-1, r),
            None => return Err(bad()),
        },
    };
    let (h, m) = rest.split_once(':').ok_or_else(bad)?;
    let h: i32 = h.parse().map_err(|_| bad())?;
    let m: i32 = m.parse().map_err(|_| bad())?;
    if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
        return Err(bad());
    }
    FixedOffset::east_opt(sign * (h * 3600 + m * 60)).ok_or_else(bad)
}

struct CompiledQuiet {
    days: Vec<Weekday>,
    start_min: i64,
    end_min: i64,
}

/// The schedule reduced to machine-checkable form. Building it is where every
/// malformed entry is caught, once, loudly — rather than silently skipped at
/// decision time.
struct Compiled {
    quiet: Vec<CompiledQuiet>,
    /// Absolute [start_ms, end_ms) busy ranges.
    busy: Vec<(i64, i64)>,
    zone: Option<FixedOffset>,
}

impl Schedule {
    pub fn is_configured(&self) -> bool {
        !self.quiet_hours.is_empty() || !self.busy.is_empty()
    }

    fn compile(&self) -> Result<Compiled> {
        let mut quiet = Vec::new();
        for q in &self.quiet_hours {
            let days = q.days.iter().map(|d| parse_day(d)).collect::<Result<Vec<_>>>()?;
            let start_min = parse_hhmm(&q.start)?;
            let end_min = parse_hhmm(&q.end)?;
            if end_min <= start_min {
                return Err(AgentRelayError::new(
                    ErrorCode::Internal,
                    format!(
                        "schedule: quiet window {}–{} does not end after it starts. A window that \
                         crosses midnight must be written as two (…–23:59 and 00:00–…).",
                        q.start, q.end
                    ),
                ));
            }
            quiet.push(CompiledQuiet { days, start_min, end_min });
        }

        let mut busy = Vec::new();
        for b in &self.busy {
            let s = DateTime::parse_from_rfc3339(&b.start).map_err(|e| {
                AgentRelayError::new(
                    ErrorCode::Internal,
                    format!("schedule: busy block start \"{}\" is not an RFC3339 time: {e}", b.start),
                )
            })?;
            let e = DateTime::parse_from_rfc3339(&b.end).map_err(|e| {
                AgentRelayError::new(
                    ErrorCode::Internal,
                    format!("schedule: busy block end \"{}\" is not an RFC3339 time: {e}", b.end),
                )
            })?;
            let (s, e) = (s.timestamp_millis(), e.timestamp_millis());
            if e <= s {
                return Err(AgentRelayError::new(
                    ErrorCode::Internal,
                    format!("schedule: busy block \"{}\"–\"{}\" does not end after it starts.", b.start, b.end),
                ));
            }
            busy.push((s, e));
        }

        let zone = match &self.timezone {
            Some(tz) => Some(parse_offset(tz)?),
            None => None,
        };
        Ok(Compiled { quiet, busy, zone })
    }

    /// May a unit of `estimated_ms` start at `now_rfc3339`?
    ///
    /// `estimated_ms` of 0 asks only "is *now* inside a forbidden window"; a positive
    /// value additionally refuses to start work that could not finish before the next
    /// window opens — the same reasoning as `decide_start`, where being cut off
    /// mid-unit (unvalidated, uncheckpointed) is the expensive case.
    pub fn decide(&self, now_rfc3339: &str, estimated_ms: i64, buffer_ms: Option<i64>) -> Result<ScheduleDecision> {
        if !self.is_configured() {
            return Ok(ScheduleDecision::StartNoSchedule);
        }
        let buffer = buffer_ms.unwrap_or(DEFAULT_BUFFER_MS);
        let compiled = self.compile()?;

        let now = DateTime::parse_from_rfc3339(now_rfc3339).map_err(|e| {
            AgentRelayError::new(ErrorCode::Internal, format!("schedule: `now` \"{now_rfc3339}\" is not RFC3339: {e}"))
        })?;
        let now_ms = now.timestamp_millis();
        // Read wall-clock in the configured fixed offset, or in whatever offset `now`
        // already carries (the machine's local time).
        let zone: FixedOffset = compiled.zone.unwrap_or(*now.offset());
        let local = now.with_timezone(&zone);

        let intervals = materialize(&compiled, &zone, local.date_naive());

        // Inside a forbidden window right now?
        let mut inside_end: Option<i64> = None;
        for &(s, e) in &intervals {
            if s <= now_ms && now_ms < e {
                inside_end = Some(inside_end.map_or(e, |x| x.max(e)));
            }
        }
        if let Some(end) = inside_end {
            let end = chain_end(&intervals, end);
            let resume = end + buffer;
            return Ok(ScheduleDecision::WaitUntil { resume_at_utc: to_iso(resume), wait_ms: (resume - now_ms).max(0) });
        }

        // Free now — but would a unit of this length run into the next window?
        if estimated_ms > 0 {
            if let Some(&(s, e)) = intervals.iter().filter(|(s, _)| *s > now_ms).min_by_key(|(s, _)| *s) {
                if s - now_ms < estimated_ms {
                    // It could not finish before that window opens. Wait for the
                    // window (and anything contiguous with it) to clear instead of
                    // starting work that would be interrupted.
                    let end = chain_end(&intervals, e);
                    let resume = end + buffer;
                    return Ok(ScheduleDecision::WaitUntil { resume_at_utc: to_iso(resume), wait_ms: (resume - now_ms).max(0) });
                }
            }
        }

        Ok(ScheduleDecision::Start)
    }

    /// In plain words, for `status`. Never silently drops a malformed entry: if the
    /// schedule will not compile, that is what it says.
    pub fn describe(&self) -> String {
        if !self.is_configured() {
            return "No schedule is set. AgentRelay will start a run whenever it is asked to."
                .to_string();
        }
        if let Err(e) = self.compile() {
            return format!(
                "A schedule is configured but it will not load, so it is NOT protecting anything: \
                 {e}\nFix it in .agentrelay/config.json — AgentRelay will not quietly ignore a \
                 window you meant to set."
            );
        }
        let mut s = String::from("Schedule is set — the agent will wait rather than run inside these windows:");
        for q in &self.quiet_hours {
            let days = if q.days.is_empty() { "every day".to_string() } else { q.days.join(", ") };
            s.push_str(&format!("\n  quiet: {}–{} on {}", q.start, q.end, days));
        }
        for b in &self.busy {
            let label = b.label.as_deref().map(|l| format!(" ({l})")).unwrap_or_default();
            s.push_str(&format!("\n  busy:  {} → {}{}", b.start, b.end, label));
        }
        match &self.timezone {
            Some(tz) => s.push_str(&format!("\nQuiet-hour times are read in the fixed offset {tz}.")),
            None => s.push_str(
                "\nQuiet-hour times are read in this machine's local timezone. Set `timezone` to a \
                 fixed offset like \"+02:00\" to pin them.",
            ),
        }
        s.push_str(
            "\nNote: this is a local schedule. A live Google/CalDAV calendar feed is a documented \
             seam, not yet built — busy blocks are entered by hand for now.",
        );
        s
    }
}

/// Materialize the recurring quiet windows into absolute [start,end) ms ranges over
/// a window of days around `base_date`, and fold in the absolute busy blocks. The
/// horizon is generous so a wait computed near a week boundary still finds the next
/// clear stretch.
fn materialize(c: &Compiled, zone: &FixedOffset, base_date: NaiveDate) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    // -1 catches a window that began "yesterday" relative to the caller's date math;
    // +9 reaches past a full week so a Monday-only window is found from a Sunday.
    for d in -1..=9 {
        let date = base_date + Duration::days(d);
        let wd = date.weekday();
        for q in &c.quiet {
            if q.days.is_empty() || q.days.contains(&wd) {
                out.push((at(zone, date, q.start_min), at(zone, date, q.end_min)));
            }
        }
    }
    out.extend(c.busy.iter().copied());
    out.sort_unstable();
    out
}

/// Absolute ms for `minutes` past midnight on `date`, in `zone`.
fn at(zone: &FixedOffset, date: NaiveDate, minutes: i64) -> i64 {
    let (h, m) = ((minutes / 60) as u32, (minutes % 60) as u32);
    let naive = date.and_hms_opt(h, m, 0).expect("valid HH:MM checked at compile");
    // FixedOffset never has a DST gap, so a local time is always unambiguous.
    zone.from_local_datetime(&naive).single().expect("fixed offset is unambiguous").timestamp_millis()
}

/// Extend an end time through any window that overlaps or touches it, so a run of
/// back-to-back meetings is treated as one wait rather than a series of restarts.
fn chain_end(intervals: &[(i64, i64)], mut end: i64) -> i64 {
    loop {
        let mut extended = end;
        for &(s, e) in intervals {
            if s <= end && e > extended {
                extended = e;
            }
        }
        if extended == end {
            return end;
        }
        end = extended;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quiet(days: &[&str], start: &str, end: &str) -> QuietHours {
        QuietHours { days: days.iter().map(|s| s.to_string()).collect(), start: start.into(), end: end.into() }
    }

    const HOUR_MS: i64 = 3_600_000;

    #[test]
    fn nothing_configured_starts_and_says_it_is_not_scheduling() {
        let s = Schedule::default();
        assert!(!s.is_configured());
        assert_eq!(s.decide("2026-08-03T10:00:00+00:00", HOUR_MS, None).unwrap(), ScheduleDecision::StartNoSchedule);
        assert!(s.describe().contains("No schedule is set"));
    }

    #[test]
    fn a_unit_inside_quiet_hours_waits_until_the_window_closes() {
        // 2026-08-03 is a Monday. Quiet 09:00–17:00 on weekdays; it is 10:00.
        let s = Schedule { quiet_hours: vec![quiet(&["mon", "tue", "wed", "thu", "fri"], "09:00", "17:00")], ..Default::default() };
        match s.decide("2026-08-03T10:00:00+00:00", 0, None).unwrap() {
            ScheduleDecision::WaitUntil { resume_at_utc, wait_ms } => {
                // Reopens at 17:00 + 1 min buffer = 17:01, which is 7h01m away.
                assert!(resume_at_utc.starts_with("2026-08-03T17:01"), "{resume_at_utc}");
                assert_eq!(wait_ms, 7 * HOUR_MS + 60_000);
            }
            other => panic!("expected a wait, got {other:?}"),
        }
    }

    #[test]
    fn a_window_that_already_closed_today_does_not_hold_the_run() {
        // Same Monday quiet 09–17, but it is 18:00 — the window is behind us.
        let s = Schedule { quiet_hours: vec![quiet(&["mon"], "09:00", "17:00")], ..Default::default() };
        assert_eq!(s.decide("2026-08-03T18:00:00+00:00", 0, None).unwrap(), ScheduleDecision::Start);
    }

    #[test]
    fn a_unit_that_would_run_into_a_window_waits_even_though_now_is_free() {
        // It is 08:30, free — but a 1h unit would still be running when quiet hours
        // begin at 09:00. Starting it means being interrupted, so wait for the far
        // side of the window instead. This is decide_start's reasoning exactly.
        let s = Schedule { quiet_hours: vec![quiet(&["mon"], "09:00", "17:00")], ..Default::default() };
        match s.decide("2026-08-03T08:30:00+00:00", HOUR_MS, None).unwrap() {
            ScheduleDecision::WaitUntil { resume_at_utc, .. } => {
                assert!(resume_at_utc.starts_with("2026-08-03T17:01"), "{resume_at_utc}");
            }
            other => panic!("expected a wait, got {other:?}"),
        }
        // A short unit that DOES fit before 09:00 is allowed to go now.
        assert_eq!(s.decide("2026-08-03T08:30:00+00:00", 10 * 60_000, None).unwrap(), ScheduleDecision::Start);
    }

    #[test]
    fn overlapping_windows_wait_for_the_later_end() {
        // Two overlapping quiet windows; inside both, the run waits for the furthest
        // end, not the nearer one — otherwise it would wake straight into the second.
        let s = Schedule {
            quiet_hours: vec![quiet(&["mon"], "09:00", "12:00"), quiet(&["mon"], "11:00", "15:00")],
            ..Default::default()
        };
        match s.decide("2026-08-03T10:00:00+00:00", 0, None).unwrap() {
            ScheduleDecision::WaitUntil { resume_at_utc, .. } => {
                assert!(resume_at_utc.starts_with("2026-08-03T15:01"), "{resume_at_utc}");
            }
            other => panic!("expected a wait to the later end, got {other:?}"),
        }
    }

    #[test]
    fn a_one_off_busy_block_blocks_the_run_and_then_clears() {
        let s = Schedule {
            busy: vec![BusyBlock {
                start: "2026-08-03T14:00:00+00:00".into(),
                end: "2026-08-03T15:00:00+00:00".into(),
                label: Some("all-hands".into()),
            }],
            ..Default::default()
        };
        // During the meeting: wait until it ends (+buffer).
        match s.decide("2026-08-03T14:30:00+00:00", 0, None).unwrap() {
            ScheduleDecision::WaitUntil { resume_at_utc, .. } => {
                assert!(resume_at_utc.starts_with("2026-08-03T15:01"), "{resume_at_utc}");
            }
            other => panic!("expected a wait, got {other:?}"),
        }
        // After it: free.
        assert_eq!(s.decide("2026-08-03T15:30:00+00:00", 0, None).unwrap(), ScheduleDecision::Start);
    }

    #[test]
    fn a_monday_only_window_is_found_from_the_sunday_before_it() {
        // Week-boundary case: it is Sunday night, and the only quiet window is Monday
        // 09–17. A big unit that would still be running Monday morning waits.
        let s = Schedule { quiet_hours: vec![quiet(&["mon"], "09:00", "17:00")], ..Default::default() };
        // Sunday 2026-08-02 23:00, unit ~ 11h — runs into Monday 09:00.
        match s.decide("2026-08-02T23:00:00+00:00", 11 * HOUR_MS, None).unwrap() {
            ScheduleDecision::WaitUntil { resume_at_utc, .. } => {
                assert!(resume_at_utc.starts_with("2026-08-03T17:01"), "{resume_at_utc}");
            }
            other => panic!("expected a wait into Monday's window, got {other:?}"),
        }
        // A short unit on Sunday night is fine — Monday is far away.
        assert_eq!(s.decide("2026-08-02T23:00:00+00:00", 30 * 60_000, None).unwrap(), ScheduleDecision::Start);
    }

    #[test]
    fn the_wall_clock_is_read_in_the_offset_now_carries_not_utc() {
        // DST-agnostic property: the same instant, expressed in a +02:00 offset,
        // reads as 10:00 local and lands inside a 09–17 window; the caller's offset
        // is what decides, not a hidden UTC assumption.
        let s = Schedule { quiet_hours: vec![quiet(&["mon"], "09:00", "17:00")], ..Default::default() };
        // 08:00Z == 10:00+02:00, a Monday.
        assert!(!s.decide("2026-08-03T10:00:00+02:00", 0, None).unwrap().may_start_now());
        // The very same instant labelled as UTC is 08:00 — before the window — so it
        // is free. Proves the offset in `now` is doing the work.
        assert_eq!(s.decide("2026-08-03T08:00:00+00:00", 0, None).unwrap(), ScheduleDecision::Start);
    }

    #[test]
    fn a_fixed_offset_timezone_override_is_applied_not_ignored() {
        // now is given in UTC, but the config pins quiet hours to +02:00. 08:00Z is
        // 10:00 in +02:00, which is inside 09–17: the override must be honoured.
        let s = Schedule {
            quiet_hours: vec![quiet(&["mon"], "09:00", "17:00")],
            timezone: Some("+02:00".into()),
            ..Default::default()
        };
        assert!(!s.decide("2026-08-03T08:00:00+00:00", 0, None).unwrap().may_start_now());
    }

    #[test]
    fn an_iana_timezone_is_a_loud_error_not_a_silently_wrong_offset() {
        let s = Schedule {
            quiet_hours: vec![quiet(&["mon"], "09:00", "17:00")],
            timezone: Some("Asia/Jerusalem".into()),
            ..Default::default()
        };
        let err = s.decide("2026-08-03T10:00:00+00:00", 0, None).unwrap_err();
        assert!(err.message.contains("Asia/Jerusalem"), "{}", err.message);
        assert!(err.message.contains("not built yet"), "{}", err.message);
        // …and status shows the problem rather than a schedule that looks fine.
        assert!(s.describe().contains("will not load"), "{}", s.describe());
    }

    #[test]
    fn a_malformed_day_or_time_refuses_rather_than_dropping_the_window() {
        // The fail-quiet shape this guards: a window the user believes protects a
        // meeting, silently ignored because it did not parse.
        let bad_day = Schedule { quiet_hours: vec![quiet(&["funday"], "09:00", "17:00")], ..Default::default() };
        assert!(bad_day.decide("2026-08-03T10:00:00+00:00", 0, None).is_err());

        let bad_time = Schedule { quiet_hours: vec![quiet(&["mon"], "9am", "17:00")], ..Default::default() };
        assert!(bad_time.decide("2026-08-03T10:00:00+00:00", 0, None).is_err());

        let wraps = Schedule { quiet_hours: vec![quiet(&["mon"], "22:00", "06:00")], ..Default::default() };
        let err = wraps.decide("2026-08-03T23:00:00+00:00", 0, None).unwrap_err();
        assert!(err.message.contains("crosses midnight"), "{}", err.message);
    }

    #[test]
    fn an_empty_days_list_means_every_day() {
        let s = Schedule { quiet_hours: vec![quiet(&[], "09:00", "17:00")], ..Default::default() };
        // Saturday 2026-08-01, 10:00 — a weekend, but the window has no day filter.
        assert!(!s.decide("2026-08-01T10:00:00+00:00", 0, None).unwrap().may_start_now());
    }

    #[test]
    fn config_round_trips_and_an_absent_schedule_is_off() {
        let s = Schedule {
            quiet_hours: vec![quiet(&["mon", "fri"], "09:00", "17:00")],
            busy: vec![BusyBlock { start: "2026-08-03T14:00:00+00:00".into(), end: "2026-08-03T15:00:00+00:00".into(), label: None }],
            timezone: Some("+02:00".into()),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Schedule>(&json).unwrap(), s);
        assert_eq!(serde_json::from_str::<Schedule>("{}").unwrap(), Schedule::default());
        assert!(!serde_json::from_str::<Schedule>("{}").unwrap().is_configured());
    }

    #[test]
    fn describe_lists_the_windows_and_names_the_calendar_seam() {
        let s = Schedule { quiet_hours: vec![quiet(&["mon"], "09:00", "17:00")], ..Default::default() };
        let d = s.describe();
        assert!(d.contains("09:00–17:00"), "{d}");
        assert!(d.contains("mon"), "{d}");
        assert!(d.contains("live Google/CalDAV"), "{d}");
    }
}
