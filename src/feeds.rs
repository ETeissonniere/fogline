use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::America::Los_Angeles;
use scraper::{ElementRef, Html, Selector};
use serde::Deserialize;
use std::process::Command;

pub const PARKING: &str =
    "https://www.sfmta.com/notices/current-special-event-parking-regulations-schedule";
const WEATHER: &str = "https://api.open-meteo.com/v1/forecast?latitude=37.758&longitude=-122.388&timezone=America%2FLos_Angeles&current=temperature_2m,wind_speed_10m,weather_code,is_day&daily=temperature_2m_min,temperature_2m_max,precipitation_probability_max,precipitation_sum,sunset&temperature_unit=celsius&wind_speed_unit=ms&precipitation_unit=mm&forecast_days=1&hourly=temperature_2m,precipitation_probability&forecast_hours=8";

fn fetch(url: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "12", "--proto", "=https", url])
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

#[derive(Clone)]
pub struct Hour {
    pub time: NaiveDateTime,
    pub temp: f64,
    pub rain: f64,
}

#[derive(Clone)]
pub struct Weather {
    pub code: Option<u8>,
    pub is_day: Option<bool>,
    pub temp: f64,
    pub wind: f64,
    pub low: f64,
    pub high: f64,
    pub rain: f64,
    pub mm: f64,
    pub sunset: NaiveTime,
    pub hours: Vec<Hour>,
}

fn parse_weather(body: &str, day: NaiveDate) -> Result<Weather, String> {
    #[derive(Deserialize)]
    struct Current {
        weather_code: Option<u8>,
        is_day: Option<u8>,
        temperature_2m: f64,
        wind_speed_10m: f64,
    }
    #[derive(Deserialize)]
    struct Daily {
        time: [NaiveDate; 1],
        temperature_2m_min: [f64; 1],
        temperature_2m_max: [f64; 1],
        precipitation_probability_max: [f64; 1],
        precipitation_sum: [f64; 1],
        sunset: [String; 1],
    }
    #[derive(Deserialize)]
    struct Hourly {
        time: Vec<String>,
        temperature_2m: Vec<Option<f64>>,
        precipitation_probability: Vec<Option<f64>>,
    }
    #[derive(Deserialize)]
    struct Forecast {
        hourly: Option<Hourly>,
        current: Current,
        daily: Daily,
    }
    let f: Forecast = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let d = f.daily;
    if d.time[0] != day {
        return Err("forecast date differs from today".into());
    }
    let sunset = NaiveDateTime::parse_from_str(&d.sunset[0], "%Y-%m-%dT%H:%M")
        .map_err(|e| e.to_string())?
        .time();
    Ok(Weather {
        code: f.current.weather_code,
        is_day: match f.current.is_day {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        temp: f.current.temperature_2m,
        wind: f.current.wind_speed_10m,
        low: d.temperature_2m_min[0],
        high: d.temperature_2m_max[0],
        rain: d.precipitation_probability_max[0],
        mm: d.precipitation_sum[0],
        sunset,
        hours: f
            .hourly
            .into_iter()
            .flat_map(|h| {
                h.time
                    .into_iter()
                    .zip(h.temperature_2m)
                    .zip(h.precipitation_probability)
                    .filter_map(|((stamp, temp), rain)| {
                        Some(Hour {
                            time: NaiveDateTime::parse_from_str(&stamp, "%Y-%m-%dT%H:%M").ok()?,
                            temp: temp?,
                            rain: rain?,
                        })
                    })
            })
            .collect(),
    })
}

pub struct Edition {
    pub calendar: crate::calendar::Agenda,
    pub fetched: DateTime<Utc>,
    pub schedule: Option<Schedule>,
    pub weather: Option<Weather>,
}
pub fn refresh() -> Edition {
    fn known<T>(name: &str, result: Result<T, String>) -> Option<T> {
        result.map_err(|e| eprintln!("{name}: {e}")).ok()
    }
    let fetched = Utc::now();
    let calendar = std::thread::spawn(move || crate::calendar::refresh(fetched));
    let weather = std::thread::spawn(move || {
        known(
            "Open-Meteo",
            fetch(WEATHER)
                .and_then(|s| parse_weather(&s, fetched.with_timezone(&Los_Angeles).date_naive())),
        )
    });
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
