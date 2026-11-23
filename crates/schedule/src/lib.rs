//! Deterministic weekly-window scheduling rules. This crate performs no I/O.
use chrono::{DateTime, Datelike, Duration, NaiveTime, TimeZone, Utc, Weekday};
use chrono_tz::Tz;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Unique to one schedule and weekly local start occurrence.
    pub id: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct WeeklySchedule {
    pub id: i64,
    /// ISO weekday: 1=Monday ... 7=Sunday.
    pub weekday: u32,
    pub start_time: String,
    pub end_time: String,
    pub timezone: String,
    pub interval_seconds: i64,
    pub completed_window: Option<String>,
    pub last_started_at: Option<DateTime<Utc>>,
}

/// Reject unsupported overnight windows and nonpositive intervals in V1.
///
/// # Errors
/// Returns a readable validation error.
pub fn validate(schedule: &WeeklySchedule) -> Result<(), String> {
    let start = NaiveTime::parse_from_str(&schedule.start_time, "%H:%M")
        .map_err(|_| "start time must be HH:MM".to_owned())?;
    let end = NaiveTime::parse_from_str(&schedule.end_time, "%H:%M")
        .map_err(|_| "end time must be HH:MM".to_owned())?;
    if start >= end {
        return Err("V1 requires the window to start and end on the same day".to_owned());
    }
    if !(1..=7).contains(&schedule.weekday) {
        return Err("weekday must be between 1 and 7".to_owned());
    }
    if !(5..=86_400).contains(&schedule.interval_seconds) {
        return Err("polling interval must be between 5 and 86400 seconds".to_owned());
    }
    let _: Tz = schedule
        .timezone
        .parse()
        .map_err(|_| "unknown IANA timezone".to_owned())?;
    Ok(())
}

/// Returns the currently active weekly occurrence. Ambiguous start/end times
/// use the earlier instant; missing DST-local times skip that occurrence.
#[must_use]
pub fn active_window(schedule: &WeeklySchedule, now: DateTime<Utc>) -> Option<Window> {
    let timezone: Tz = schedule.timezone.parse().ok()?;
    let local_now = now.with_timezone(&timezone);
    let weekday_number = match local_now.weekday() {
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
        Weekday::Sun => 7,
    };
    if weekday_number != schedule.weekday {
        return None;
    }
    let start_time = NaiveTime::parse_from_str(&schedule.start_time, "%H:%M").ok()?;
    let end_time = NaiveTime::parse_from_str(&schedule.end_time, "%H:%M").ok()?;
    let date = local_now.date_naive();
    let start = timezone
        .from_local_datetime(&date.and_time(start_time))
        .earliest()?
        .with_timezone(&Utc);
    let end = timezone
        .from_local_datetime(&date.and_time(end_time))
        .latest()?
        .with_timezone(&Utc);
    if now < start || now >= end {
        return None;
    }
    Some(Window {
        id: format!("{}:{}:{}", schedule.id, date, schedule.start_time),
        start,
        end,
    })
}

/// Is a flow due when capacity becomes available? An old week's last poll
/// never delays the first poll of this week's window.
#[must_use]
pub fn is_due(schedule: &WeeklySchedule, now: DateTime<Utc>) -> Option<Window> {
    let window = active_window(schedule, now)?;
    if schedule.completed_window.as_deref() == Some(window.id.as_str()) {
        return None;
    }
    if let Some(last) = schedule.last_started_at
        && last >= window.start
    {
        let next = last.checked_add_signed(Duration::seconds(schedule.interval_seconds))?;
        if now < next {
            return None;
        }
    }
    Some(window)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn schedule() -> WeeklySchedule {
        WeeklySchedule {
            id: 1,
            weekday: 6,
            start_time: "13:00".to_owned(),
            end_time: "19:00".to_owned(),
            timezone: "Europe/Amsterdam".to_owned(),
            interval_seconds: 300,
            completed_window: None,
            last_started_at: None,
        }
    }
    #[test]
    fn saturday_only() {
        let saturday = Utc.with_ymd_and_hms(2026, 11, 21, 13, 0, 0).single();
        let sunday = Utc.with_ymd_and_hms(2026, 11, 22, 13, 0, 0).single();
        assert!(saturday.is_some_and(|t| is_due(&schedule(), t).is_some()));
        assert!(sunday.is_some_and(|t| is_due(&schedule(), t).is_none()));
    }
    #[test]
    fn completion_and_new_week() {
        if let Some(t) = Utc.with_ymd_and_hms(2026, 11, 21, 13, 0, 0).single()
            && let Some(w) = is_due(&schedule(), t)
        {
            let mut s = schedule();
            s.completed_window = Some(w.id);
            assert!(is_due(&s, t).is_none());
            if let Some(next_week) = Utc.with_ymd_and_hms(2026, 11, 28, 13, 0, 0).single() {
                assert!(is_due(&s, next_week).is_some());
            }
        }
    }
    #[test]
    fn prior_poll_delays_current_window_only() {
        if let Some(t) = Utc.with_ymd_and_hms(2026, 11, 21, 13, 10, 0).single() {
            let mut s = schedule();
            s.last_started_at = Some(t);
            assert!(is_due(&s, t).is_none());
            if let Some(later) = t.checked_add_signed(Duration::minutes(5)) {
                assert!(is_due(&s, later).is_some());
            }
        }
    }
    #[test]
    fn rejects_overnight_window() {
        let mut s = schedule();
        s.end_time = "07:00".to_owned();
        assert!(validate(&s).is_err());
    }
}
