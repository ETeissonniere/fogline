use chrono::{DateTime, Days, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::{America::Los_Angeles, Tz};
use icalendar::{
    rrule, Calendar, CalendarDateTime, Component, DatePerhapsTime, Event, EventStatus, Property,
    UnvalidatedRRule,
};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    process::{Command, Stdio},
};

#[derive(Clone, Debug)]
pub enum Agenda {
    NotConfigured,
    Unavailable,
    Ready(CalendarPlans),
}

#[derive(Clone, Debug, Default)]
pub struct CalendarPlans {
    pub events: Vec<Plan>,
    unsupported_rules: usize,
    unsupported_zones: usize,
}

impl CalendarPlans {
    pub fn is_partial(&self) -> bool {
        self.unsupported_rules > 0 || self.unsupported_zones > 0
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub title: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub all_day: bool,
}

type CalendarResult<T> = Result<T, &'static str>;
const UNKNOWN_ZONE: &str = "unsupported calendar time zone";

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
        Ok(plans) => {
            if plans.unsupported_rules > 0 {
                eprintln!(
                    "Shared calendar: could not expand {} recurrence rule(s)",
                    plans.unsupported_rules
                );
            }
            if plans.unsupported_zones > 0 {
                eprintln!(
                    "Shared calendar: unsupported calendar time zone in {} component(s)",
                    plans.unsupported_zones
                );
            }
            Agenda::Ready(plans)
        }
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
            let tz = tzid.trim_matches('"').parse().map_err(|_| UNKNOWN_ZONE)?;
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

fn recurrence_rule(
    raw: &str,
    start: &DatePerhapsTime,
    first: DateTime<rrule::Tz>,
) -> Option<rrule::RRule> {
    // rrule parses an unqualified UNTIL in the server's timezone. Resolve it in
    // the same household timezone as a DATE or floating DTSTART before validation.
    let mut fields = Vec::new();
    for field in raw.split(';') {
        if let Some((key, value)) = field.split_once('=') {
            if key.eq_ignore_ascii_case("UNTIL") && !value.ends_with('Z') {
                let wall = match start {
                    DatePerhapsTime::Date(_) => Some(
                        NaiveDate::parse_from_str(value, "%Y%m%d")
                            .ok()?
                            .and_time(NaiveTime::MIN),
                    ),
                    DatePerhapsTime::DateTime(CalendarDateTime::Floating(_)) => {
                        Some(NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?)
                    }
                    _ => None,
                };
                if let Some(wall) = wall {
                    let until = Los_Angeles.from_local_datetime(&wall).earliest()?.to_utc();
                    fields.push(format!("UNTIL={}", until.format("%Y%m%dT%H%M%SZ")));
                    continue;
                }
            }
        }
        fields.push(field.to_owned());
    }
    fields
        .join(";")
        .parse::<UnvalidatedRRule>()
        .ok()?
        .validate(first)
        .ok()
}

fn event_schedule(
    event: &Event,
    unsupported_rules: &mut usize,
) -> CalendarResult<(bool, Duration, rrule::RRuleSet)> {
    let value = event.get_start().ok_or("missing event start")?;
    let all_day = matches!(value, DatePerhapsTime::Date(_));
    let start = date_time(value.clone())?;
    let duration = event_duration(event, start, all_day)?;
    let tz: rrule::Tz = start.timezone().into();
    let first = start.with_timezone(&tz);
    let mut recurrence = rrule::RRuleSet::new(first).rdate(first);
    if let Some(raw) = event.property_value("RRULE") {
        if let Some(rule) = recurrence_rule(raw, &value, first) {
            recurrence = recurrence.rrule(rule);
        } else {
            *unsupported_rules += 1;
        }
    }
    for date in extra_dates(event, "RDATE")? {
        recurrence = recurrence.rdate(date);
    }
    recurrence = recurrence.set_exdates(extra_dates(event, "EXDATE")?);
    Ok((all_day, duration, recurrence))
}

pub(crate) fn parse_calendar(body: &str, now: DateTime<Utc>) -> CalendarResult<CalendarPlans> {
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
    let mut uncertain_series = HashSet::new();
    let mut plans = CalendarPlans::default();
    for event in calendar.events() {
        let uid = event.get_uid().ok_or("missing event UID")?;
        let original = match event.get_recurrence_id().map(date_time).transpose() {
            Ok(value) => value.map(|t| t.to_utc()),
            Err(UNKNOWN_ZONE) => {
                // Without the original instant we cannot tell which master occurrence was replaced.
                uncertain_series.insert(uid);
                plans.unsupported_zones += 1;
                continue;
            }
            Err(reason) => return Err(reason),
        };
        if event
            .properties()
            .get("RECURRENCE-ID")
            .and_then(|p| p.params().get("RANGE"))
            .is_some()
        {
            return Err("unsupported range of modified recurrences");
        }
        let current: &mut &Event = components.entry((uid, original)).or_insert(event);
        if (event.get_sequence(), event.get_timestamp())
            >= (current.get_sequence(), current.get_timestamp())
        {
            *current = event;
        }
    }

    for (&(uid, original), &event) in &components {
        if (original.is_none() && uncertain_series.contains(uid))
            || event.get_status() == Some(EventStatus::Cancelled)
            || components
                .get(&(uid, None))
                .is_some_and(|base| base.get_status() == Some(EventStatus::Cancelled))
        {
            continue;
        }
        let (all_day, duration, recurrence) =
            match event_schedule(event, &mut plans.unsupported_rules) {
                Ok(schedule) => schedule,
                Err(UNKNOWN_ZONE) => {
                    plans.unsupported_zones += 1;
                    continue;
                }
                Err(reason) => return Err(reason),
            };
        let tz = recurrence.get_dt_start().timezone();
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
                plans.events.push(Plan {
                    title,
                    start,
                    end,
                    all_day,
                });
            }
        }
    }
    plans
        .events
        .sort_by(|a, b| (a.start, a.end, &a.title).cmp(&(b.start, b.end, &b.title)));
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().to_utc()
    }
    fn calendar(data: &str, now: &str) -> CalendarPlans {
        parse_calendar(
            &format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{data}\r\nEND:VCALENDAR\r\n"),
            day(now),
        )
        .unwrap()
    }
    fn plans(data: &str, now: &str) -> Vec<Plan> {
        let calendar = calendar(data, now);
        assert_eq!(calendar.unsupported_rules, 0);
        assert!(!calendar.is_partial());
        calendar.events
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
    fn last_weekday_with_an_inclusive_until_keeps_the_final_occurrence() {
        for (start, until, expected) in [
            (";VALUE=DATE:20261030", "20261130", "2026-11-30T08:00:00Z"),
            (
                ":20261030T190000",
                "20261130T190000",
                "2026-12-01T03:00:00Z",
            ),
            (
                ";TZID=America/Los_Angeles:20261030T190000",
                "20261201T030000Z",
                "2026-12-01T03:00:00Z",
            ),
        ] {
            let event = format!("BEGIN:VEVENT\nUID:last-weekday\nDTSTART{start}\nRRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;UNTIL={until}\nEND:VEVENT");
            let result = plans(&event, "2026-11-29T12:00:00Z");
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].start, day(expected));
            assert!(plans(&event, "2026-12-28T12:00:00Z").is_empty());
        }
    }

    #[test]
    fn an_unsupported_rule_keeps_known_dates_and_unrelated_events() {
        let events = "BEGIN:VEVENT\nUID:series\nDTSTART:20261110T180000Z\nRRULE:FREQ=DAILY;BYSETPOS=0\nRDATE:20261111T180000Z,20261112T180000Z\nEXDATE:20261111T180000Z\nEND:VEVENT\nBEGIN:VEVENT\nUID:series\nRECURRENCE-ID:20261112T180000Z\nDTSTART:20261112T190000Z\nEND:VEVENT\nBEGIN:VEVENT\nUID:independent\nDTSTART:20261113T180000Z\nEND:VEVENT";
        let result = calendar(events, "2026-11-10T12:00:00Z");
        assert_eq!(result.unsupported_rules, 1);
        assert_eq!(
            result.events.iter().map(|p| p.start).collect::<Vec<_>>(),
            [
                day("2026-11-10T18:00:00Z"),
                day("2026-11-12T19:00:00Z"),
                day("2026-11-13T18:00:00Z")
            ]
        );
    }

    #[test]
    fn unknown_time_zones_leave_independent_events_visible() {
        let independent = "BEGIN:VEVENT\nUID:other\nDTSTART:20261110T170000Z\nEND:VEVENT";
        for dates in [
            "DTSTART;TZID=Custom/Unknown:20261110T090000",
            "DTSTART:20261110T180000Z\nDTEND;TZID=Custom/Unknown:20261110T120000",
            "DTSTART:20261110T180000Z\nEXDATE;TZID=Custom/Unknown:20261110T090000",
            "DTSTART:20261110T180000Z\nRDATE;TZID=Custom/Unknown:20261111T090000",
        ] {
            let bad = format!("BEGIN:VEVENT\nUID:bad\n{dates}\nEND:VEVENT");
            let result = calendar(&format!("{bad}\n{independent}"), "2026-11-10T12:00:00Z");
            assert!(result.is_partial());
            assert_eq!(result.unsupported_zones, 1);
            assert_eq!(
                result.events.iter().map(|p| p.start).collect::<Vec<_>>(),
                [day("2026-11-10T17:00:00Z")]
            );
        }
    }

    #[test]
    fn unknown_override_zones_never_resurrect_superseded_occurrences() {
        let base = "BEGIN:VEVENT\nUID:series\nDTSTART:20261110T180000Z\nRRULE:FREQ=DAILY;COUNT=3\nEND:VEVENT";
        let moved = "BEGIN:VEVENT\nUID:series\nRECURRENCE-ID:20261111T180000Z\nDTSTART:20261111T190000Z\nEND:VEVENT";
        let independent = "BEGIN:VEVENT\nUID:other\nDTSTART:20261110T170000Z\nEND:VEVENT";
        for (change, final_base_is_known) in [
            (
                "RECURRENCE-ID:20261110T180000Z\nDTSTART;TZID=Custom/Unknown:20261110T120000",
                true,
            ),
            (
                "RECURRENCE-ID;TZID=Custom/Unknown:20261110T090000\nSTATUS:CANCELLED",
                false,
            ),
        ] {
            for before_master in [true, false] {
                let change = format!("BEGIN:VEVENT\nUID:series\n{change}\nEND:VEVENT");
                let feed = if before_master {
                    format!("{change}\n{base}")
                } else {
                    format!("{base}\n{change}")
                };
                let result = calendar(
                    &format!("{feed}\n{moved}\n{independent}"),
                    "2026-11-10T12:00:00Z",
                );
                assert!(result.is_partial());
                assert_eq!(result.unsupported_zones, 1);
                let mut expected = vec![day("2026-11-10T17:00:00Z"), day("2026-11-11T19:00:00Z")];
                if final_base_is_known {
                    expected.push(day("2026-11-12T18:00:00Z"));
                }
                assert_eq!(
                    result.events.iter().map(|p| p.start).collect::<Vec<_>>(),
                    expected
                );
            }
        }
    }

    #[test]
    fn empty_calendar_and_failed_subscription_are_distinct() {
        let now = day("2026-11-10T00:00:00Z");
        assert!(parse_calendar("<html>Sign in</html>", now).is_err());
        assert!(
            parse_calendar("BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR", now)
                .unwrap()
                .events
                .is_empty()
        );
    }
}
