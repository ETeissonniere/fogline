use crate::weather::{self, Weather};
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc};
use chrono_tz::America::Los_Angeles;
use scraper::{ElementRef, Html, Selector};
use std::process::Command;

pub const PARKING: &str =
    "https://www.sfmta.com/notices/current-special-event-parking-regulations-schedule";

fn fetch(url: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--user-agent",
            "Fogline (https://github.com/ETeissonniere/fogline)",
            "--max-time",
            "12",
            "--proto",
            "=https",
            url,
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned());
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

#[derive(Clone, Debug)]
pub struct Event {
    pub day: NaiveDate,
    pub venue: String,
    pub start: NaiveTime,
    pub end: NaiveTime,
}
impl Event {
    pub fn window(&self) -> String {
        format!(
            "{}–{}",
            self.start.format("%-I:%M %p"),
            self.end.format("%-I:%M %p")
        )
    }
}

#[derive(Clone)]
pub struct Schedule {
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub events: Vec<Event>,
}

fn text(node: ElementRef<'_>) -> String {
    node.text()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}
fn hour(time: &str) -> Option<NaiveTime> {
    let mut s = time.to_lowercase().replace(['.', ' '], "");
    if s == "noon" {
        s = "12pm".into();
    }
    if s == "midnight" {
        s = "12am".into();
    }
    if !s.contains(':') {
        let suffix = s.find(['a', 'p'])?;
        s.insert_str(suffix, ":00");
    }
    NaiveTime::parse_from_str(&s, "%I:%M%P").ok()
}

pub fn parse_schedule(body: &str) -> Result<Schedule, String> {
    let doc = Html::parse_document(body);
    let select = |s| Selector::parse(s).unwrap();
    let row_sel = select("tr");
    let table = doc
        .select(&select("table"))
        .find(|t| {
            t.select(&row_sel).next().is_some_and(|row| {
                text(row)
                    .to_lowercase()
                    .contains("special event rate hours")
            })
        })
        .ok_or("missing SFMTA event table")?;
    let issued = doc
        .select(&select("time[datetime]"))
        .next()
        .and_then(|e| DateTime::parse_from_rfc3339(e.value().attr("datetime")?).ok())
        .ok_or("missing SFMTA effective date")?
        .with_timezone(&Los_Angeles)
        .date_naive();
    let mut events = Vec::new();
    for row in table.select(&row_sel).skip(1) {
        let fields: Vec<_> = row.select(&select("td")).map(text).collect();
        let [date, venue, window] = fields.as_slice() else {
            return Err("incomplete SFMTA row".into());
        };
        let day = (issued.year() - 1..=issued.year() + 1)
            .filter_map(|y| NaiveDate::parse_from_str(&format!("{date} {y}"), "%B %e %Y").ok())
            .min_by_key(|d| (*d - issued).num_days().abs())
            .filter(|d| (*d - issued).num_days().abs() < 150)
            .ok_or("unknown event date")?;
        let (start, end) = window
            .split_once(" to ")
            .or_else(|| window.split_once('–'))
            .ok_or("unknown meter window")?;
        let start = hour(start).ok_or("unknown start hour")?;
        let end = hour(end).ok_or("unknown end hour")?;
        if end <= start {
            return Err("unexpected overnight meter window".into());
        }
        let venue = match venue.as_str() {
            "Chase" => "Chase Center",
            "Oracle" => "Oracle Park",
            "" => return Err("missing venue".into()),
            other => other,
        };
        events.push(Event {
            day,
            venue: venue.into(),
            start,
            end,
        });
    }
    events.sort_by_key(|e| (e.day, e.start));
    let first = issued.min(events.first().ok_or("empty event table")?.day);
    let last = events.last().unwrap().day;
    Ok(Schedule {
        first,
        last,
        events,
    })
}

pub struct Edition {
    pub calendar: crate::calendar::Agenda,
    pub fetched: DateTime<Utc>,
    pub schedule: Option<Schedule>,
    pub weather: Option<Weather>,
}
impl Edition {
    pub fn replace_with(&mut self, mut next: Edition) {
        if next.weather.is_none() {
            next.weather = self.weather.take().filter(|w| w.usable_at(next.fetched));
        }
        *self = next;
    }
}

pub fn refresh() -> Edition {
    fn known<T>(name: &str, result: Result<T, String>) -> Option<T> {
        result.map_err(|e| eprintln!("{name}: {e}")).ok()
    }
    let fetched = Utc::now();
    let calendar = std::thread::spawn(move || crate::calendar::refresh(fetched));
    let weather = std::thread::spawn(move || known("Weather", weather::forecast(fetched, fetch)));
    let schedule = known("SFMTA", fetch(PARKING).and_then(|s| parse_schedule(&s)));
    Edition {
        calendar: calendar
            .join()
            .unwrap_or(crate::calendar::Agenda::Unavailable),
        fetched,
        schedule,
        weather: weather.join().unwrap_or_default(),
    }
}
