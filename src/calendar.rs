use chrono::{DateTime, Days, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::{America::Los_Angeles, Tz};
use icalendar::{
    rrule, Calendar, CalendarDateTime, Component, DatePerhapsTime, Event, EventStatus, Property,
    UnvalidatedRRule,
};
use std::{
    collections::HashMap,
    env, fs,
    io::Write,
    process::{Command, Stdio},
};

#[derive(Clone, Debug)]
pub enum Agenda {
    NotConfigured,
    Unavailable,
    Ready(Vec<Plan>),
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
}

type CalendarResult<T> = Result<T, &'static str>;

pub fn refresh(now: DateTime<Utc>) -> Agenda {
    let input = match env::var("FOGLINE_ICS") {
        Ok(source) if source.starts_with("https://") || source.starts_with("webcal://") => {
            fetch_url(&source)
        }
        Ok(path) => fs::read_to_string(path).map_err(|_| "could not read configured calendar"),
        Err(env::VarError::NotPresent) => match fs::read_to_string("calendar.ics") {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Agenda::NotConfigured,
            other => other.map_err(|_| "could not read calendar.ics"),
        },
        Err(_) => Err("invalid calendar source"),
    };
    match input.and_then(|body| parse_calendar(&body, now)) {
        Ok(plans) => Agenda::Ready(plans),
        Err(reason) => {
            eprintln!("Shared calendar: {reason}");
            Agenda::Unavailable
        }
    }
}

fn fetch_url(source: &str) -> CalendarResult<String> {
    // curl's config comes from stdin so the subscription token never appears in argv.
    if source
        .chars()
        .any(|c| c.is_control() || matches!(c, '"' | '\\'))
    {
        return Err("invalid subscription URL");
    }
    let url = source
        .strip_prefix("webcal://")
        .map(|s| format!("https://{s}"));
    let mut child = Command::new("curl")
        .args([
            "-q",
            "-fsSL",
            "--max-time",
            "12",
            "--max-filesize",
            "16777216",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "-K",
            "-",
        ])
        .env_remove("FOGLINE_ICS")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "could not launch curl")?;
    let mut input = child.stdin.take().ok_or("missing curl input")?;
    let written = writeln!(input, "url = \"{}\"", url.as_deref().unwrap_or(source));
    drop(input);
    let output = child
        .wait_with_output()
        .map_err(|_| "subscription fetch failed")?;
    if written.is_err() || !output.status.success() {
        return Err("subscription fetch failed");
    }
    String::from_utf8(output.stdout).map_err(|_| "calendar is not UTF-8")
}

fn midnight(day: NaiveDate) -> CalendarResult<DateTime<Utc>> {
    Los_Angeles
        .from_local_datetime(&day.and_time(NaiveTime::MIN))
        .earliest()
        .map(|t| t.to_utc())
        .ok_or("invalid calendar day")
}

fn date_time(value: DatePerhapsTime) -> CalendarResult<DateTime<Tz>> {
    let (tz, wall) = match value {
        DatePerhapsTime::Date(day) => (Los_Angeles, day.and_time(NaiveTime::MIN)),
        DatePerhapsTime::DateTime(CalendarDateTime::Floating(wall)) => (Los_Angeles, wall),
        DatePerhapsTime::DateTime(CalendarDateTime::Utc(t)) => return Ok(t.with_timezone(&Tz::UTC)),
        DatePerhapsTime::DateTime(CalendarDateTime::WithTimezone { date_time, tzid }) => {
            let tz = tzid
                .trim_matches('"')
                .parse()
                .map_err(|_| "unsupported calendar time zone")?;
            (tz, date_time)
        }
    };
    tz.from_local_datetime(&wall)
        .earliest()
        .ok_or("nonexistent local event time")
}

fn extra_dates(event: &Event, key: &str) -> CalendarResult<Vec<DateTime<rrule::Tz>>> {
    let mut dates = Vec::new();
    for p in event
        .multi_properties()
        .get(key)
        .into_iter()
        .flatten()
        .chain(event.properties().get(key))
    {
        for raw in p.value().split(',') {
            let mut value = Property::new(key, raw);
            for parameter in p.params().values() {
                value.append_parameter(parameter.clone());
            }
            let t = date_time(
                DatePerhapsTime::from_property(&value).ok_or("unsupported recurrence date")?,
            )?;
            dates.push(t.with_timezone(&t.timezone().into()));
        }
    }
    Ok(dates)
}

fn event_duration(event: &Event, start: DateTime<Tz>, all_day: bool) -> CalendarResult<Duration> {
    let duration = if let Some(end) = event.get_end() {
        let end = date_time(end)?;
        if all_day {
            Duration::days((end.date_naive() - start.date_naive()).num_days())
        } else {
            end.to_utc() - start.to_utc()
        }
    } else if let Some(duration) = event.property_value("DURATION") {
        Duration::from_std(
            iso8601::duration(duration)
                .map_err(|_| "invalid event duration")?
                .into(),
        )
        .map_err(|_| "invalid event duration")?
    } else if all_day {
        Duration::days(1)
    } else {
        Duration::zero()
    };
    if duration < Duration::zero() {
        return Err("negative event duration");
    }

    Ok(duration)
}

fn occurrence_set(event: &Event, start: DateTime<Tz>) -> CalendarResult<rrule::RRuleSet> {
    let tz: rrule::Tz = start.timezone().into();
    let first = start.with_timezone(&tz);
    let mut recurrence = rrule::RRuleSet::new(first).rdate(first);
    if let Some(raw) = event.property_value("RRULE") {
        let rule = raw
            .parse::<UnvalidatedRRule>()
            .and_then(|r| r.validate(first))
            .map_err(|_| "unsupported recurrence rule")?;
        recurrence = recurrence.rrule(rule);
    }
    for date in extra_dates(event, "RDATE")? {
        recurrence = recurrence.rdate(date);
    }
    recurrence = recurrence.set_exdates(extra_dates(event, "EXDATE")?);
    Ok(recurrence)
}

pub(crate) fn parse_calendar(body: &str, now: DateTime<Utc>) -> CalendarResult<Vec<Plan>> {
    let body = body.trim().trim_start_matches('\u{feff}');
    if !body.starts_with("BEGIN:VCALENDAR") || !body.ends_with("END:VCALENDAR") {
        return Err("expected a complete iCalendar file");
    }
    let calendar: Calendar = body
        .replace("\r\n", "\n")
        .parse()
        .map_err(|_| "could not parse calendar")?;
    let today = now.with_timezone(&Los_Angeles).date_naive();
    let lower = midnight(today)?;
    let upper = midnight(today + Days::new(7))?;

    // RECURRENCE-ID refers to the original start, even when this instance was moved.
    let mut components = HashMap::new();
    for event in calendar.events() {
        let uid = event.get_uid().ok_or("missing event UID")?;
        if event
            .properties()
            .get("RECURRENCE-ID")
            .and_then(|p| p.params().get("RANGE"))
            .is_some()
        {
            return Err("unsupported range of modified recurrences");
        }
        let original = event
            .get_recurrence_id()
            .map(date_time)
            .transpose()?
            .map(|t| t.to_utc());
        let current: &mut &Event = components.entry((uid, original)).or_insert(event);
        if (event.get_sequence(), event.get_timestamp())
            >= (current.get_sequence(), current.get_timestamp())
        {
            *current = event;
        }
    }

    let mut plans = Vec::new();
    for (&(uid, original), &event) in &components {
        if event.get_status() == Some(EventStatus::Cancelled)
            || components
                .get(&(uid, None))
                .is_some_and(|base| base.get_status() == Some(EventStatus::Cancelled))
        {
            continue;
        }
        let value = event.get_start().ok_or("missing event start")?;
        let all_day = matches!(value, DatePerhapsTime::Date(_));
        let start = date_time(value)?;
        let duration = event_duration(event, start, all_day)?;
        let recurrence = occurrence_set(event, start)?;
        let tz = start.timezone().into();
        // All-day spans use calendar days: their absolute duration can differ by an hour at DST.
        let lookback = duration
            + if all_day {
                Duration::days(1)
            } else {
                Duration::zero()
            };
        let mut occurrences = recurrence
            .after((lower - lookback).with_timezone(&tz))
            .before(upper.with_timezone(&tz))
            .all(2048);
        if occurrences.limited {
            return Err("too many calendar occurrences");
        }
        occurrences.dates.dedup();
        for occurrence in occurrences.dates {
            let start = occurrence.to_utc();
            if original.is_none() && components.contains_key(&(uid, Some(start))) {
                continue;
            }
            let end = if all_day {
                midnight(start.with_timezone(&Los_Angeles).date_naive() + duration)?
            } else {
                start + duration
            };
            if start < upper && (end > lower || (start == end && start >= lower)) {
                let title = event
                    .get_summary()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or("Busy")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                plans.push(Plan {
                    title,
                    start,
                    end,
                    all_day,
                });
            }
        }
    }
    plans.sort_by(|a, b| (a.start, a.end, &a.title).cmp(&(b.start, b.end, &b.title)));
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }
    fn plans(data: &str, now: &str) -> Vec<Plan> {
        parse_calendar(
            &format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{data}\r\nEND:VCALENDAR\r\n"),
            day(now),
        )
        .unwrap()
    }
    #[test]
    fn calendar_exceptions_replace_cancel_and_exclude_original_instances() {
        let feed = "BEGIN:VEVENT
UID:r
DTSTART;TZID=America/New_York:20261102T180000
DTEND;TZID=America/New_York:20261102T190000
RRULE:FREQ=DAILY;COUNT=12
EXDATE;TZID=America/New_York:20261110T180000,20261111T180000
SUMMARY:Original
END:VEVENT
BEGIN:VEVENT
UID:r
RECURRENCE-ID;TZID=America/New_York:20261109T180000
DTSTART;TZID=America/New_York:20261109T190000
DTEND;TZID=America/New_York:20261109T200000
SUMMARY:Changed\\, with a
  folded title
END:VEVENT
BEGIN:VEVENT
UID:r
RECURRENCE-ID;TZID=America/New_York:20261112T180000
STATUS:CANCELLED
END:VEVENT";
        let plans = plans(feed, "2026-11-09T23:59:00-08:00");
        assert_eq!(plans.len(), 2);
        assert_eq!(plans[0].title, "Changed, with a folded title");
        assert_eq!(plans[0].start, day("2026-11-09T16:00:00-08:00"));
        assert_eq!(plans[0].end, day("2026-11-09T17:00:00-08:00"));
        assert_eq!(plans[1].start, day("2026-11-13T15:00:00-08:00"));
    }

    #[test]
    fn all_day_events_remain_local_days_across_spring_clock_change() {
        let feed = "BEGIN:VEVENT
UID:trip
DTSTART;VALUE=DATE:20260307
DTEND;VALUE=DATE:20260310
SUMMARY:Trip
END:VEVENT
BEGIN:VEVENT
UID:morning
DTSTART:20260308T091500
DURATION:PT45M
SUMMARY:Brunch
END:VEVENT";
        let events = plans(feed, "2026-03-08T20:00:00-07:00");
        assert_eq!(events.len(), 2);
        assert!(events[0].all_day);
        assert_eq!(events[0].start, day("2026-03-07T00:00:00-08:00"));
        assert_eq!(events[0].end, day("2026-03-10T00:00:00-07:00"));
        assert_eq!(events[1].start, day("2026-03-08T09:15:00-07:00"));
        assert_eq!(events[1].end, day("2026-03-08T10:00:00-07:00"));
    }

    #[test]
    fn first_occurrence_is_included_once_even_when_declared_by_multiple_rules() {
        let feed="BEGIN:VEVENT\nUID:daily\nDTSTART:20261110T190000Z\nRRULE:FREQ=DAILY;COUNT=2\nRDATE:20261110T190000Z\nSUMMARY:Lunch\nEND:VEVENT";
        assert_eq!(plans(feed, "2026-11-10T20:00:00Z").len(), 2);
    }

    #[test]
    fn empty_calendar_and_failed_subscription_are_distinct() {
        let now = day("2026-11-10T00:00:00Z");
        assert!(parse_calendar("<html>Sign in</html>", now).is_err());
        assert!(
            parse_calendar("BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR", now)
                .unwrap()
                .is_empty()
        );
    }
}
