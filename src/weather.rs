use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::America::Los_Angeles;
use serde::Deserialize;

const NWS_HOURLY: &str = "https://api.weather.gov/gridpoints/MTR/86,104/forecast/hourly?units=si";
const OPEN_METEO: &str = "https://api.open-meteo.com/v1/forecast?latitude=37.758&longitude=-122.388&timezone=America%2FLos_Angeles&current=temperature_2m,wind_speed_10m,weather_code,is_day&daily=temperature_2m_min,temperature_2m_max,precipitation_probability_max,precipitation_sum,sunset&temperature_unit=celsius&wind_speed_unit=ms&precipitation_unit=mm&forecast_days=1&hourly=temperature_2m,precipitation_probability&forecast_hours=12";

#[derive(Clone, Copy, PartialEq)]
pub enum Condition {
    Clear,
    MostlyClear,
    PartlyCloudy,
    Overcast,
    Fog,
    Drizzle,
    Rain,
    Snow,
    Storm,
    Unknown,
}

impl Condition {
    fn from_code(code: Option<u8>) -> Self {
        // Open-Meteo's WMO weather interpretation codes.
        match code {
            Some(0) => Self::Clear,
            Some(1) => Self::MostlyClear,
            Some(2) => Self::PartlyCloudy,
            Some(3) => Self::Overcast,
            Some(45 | 48) => Self::Fog,
            Some(51 | 53 | 55 | 56 | 57) => Self::Drizzle,
            Some(61 | 63 | 65 | 66 | 67 | 80..=82) => Self::Rain,
            Some(71 | 73 | 75 | 77 | 85 | 86) => Self::Snow,
            Some(95..=97 | 99) => Self::Storm,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone)]
pub struct Hour {
    pub time: NaiveDateTime,
    pub temp: f64,
    pub rain: f64,
}

#[derive(Clone)]
pub struct Weather {
    pub fetched: DateTime<Utc>,
    pub condition: Condition,
    pub is_day: Option<bool>,
    pub temp: f64,
    pub wind: Option<f64>,
    pub hours: Vec<Hour>,
    pub details: ForecastDetails,
}

#[derive(Clone)]
pub enum ForecastDetails {
    Nws,
    OpenMeteo {
        low: f64,
        high: f64,
        rain: f64,
        mm: f64,
        sunset: NaiveTime,
    },
}

fn parse_open_meteo(body: &str, fetched: DateTime<Utc>) -> Result<Weather, String> {
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
    if d.time[0] != fetched.with_timezone(&Los_Angeles).date_naive() {
        return Err("forecast date differs from today".into());
    }
    let sunset = NaiveDateTime::parse_from_str(&d.sunset[0], "%Y-%m-%dT%H:%M")
        .map_err(|e| e.to_string())?
        .time();
    Ok(Weather {
        fetched,
        condition: Condition::from_code(f.current.weather_code),
        is_day: match f.current.is_day {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        },
        temp: f.current.temperature_2m,
        wind: Some(f.current.wind_speed_10m),
        details: ForecastDetails::OpenMeteo {
            low: d.temperature_2m_min[0],
            high: d.temperature_2m_max[0],
            rain: d.precipitation_probability_max[0],
            mm: d.precipitation_sum[0],
            sunset,
        },
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

pub fn forecast(
    fetched: DateTime<Utc>,
    fetch: impl Fn(&str) -> Result<String, String>,
) -> Result<Weather, String> {
    match fetch(OPEN_METEO).and_then(|body| parse_open_meteo(&body, fetched)) {
        Ok(weather) => Ok(weather),
        Err(err) => {
            eprintln!("Open-Meteo: {err}");
            fetch(NWS_HOURLY)
                .and_then(|body| parse_nws(&body, fetched))
                .map_err(|err| format!("NWS: {err}"))
        }
    }
}

impl Weather {
    pub fn usable_at(&self, now: DateTime<Utc>) -> bool {
        let age = now - self.fetched;
        (chrono::Duration::zero()..=chrono::Duration::hours(3)).contains(&age)
            && self.fetched.with_timezone(&Los_Angeles).date_naive()
                == now.with_timezone(&Los_Angeles).date_naive()
    }
}

fn parse_nws(body: &str, fetched: DateTime<Utc>) -> Result<Weather, String> {
    #[derive(Deserialize)]
    struct Response {
        properties: Properties,
    }
    #[derive(Deserialize)]
    struct Properties {
        periods: Vec<Period>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Period {
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        temperature: f64,
        temperature_unit: String,
        is_daytime: bool,
        wind_speed: String,
        icon: Option<String>,
        probability_of_precipitation: Option<Probability>,
    }
    #[derive(Deserialize)]
    struct Probability {
        value: Option<f64>,
    }
    let response: Response = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let periods = response.properties.periods;
    let current = periods
        .iter()
        .find(|p| p.start_time <= fetched && fetched < p.end_time && p.temperature_unit == "C")
        .ok_or("missing current metric forecast hour")?;
    let wind = current
        .wind_speed
        .strip_suffix(" km/h")
        .and_then(|v| v.parse::<f64>().ok())
        .map(|kmh| kmh / 3.6);
    let sky = current
        .icon
        .as_deref()
        .and_then(|url| url.rsplit('/').next())
        .and_then(|segment| segment.split([',', '?']).next())
        .unwrap_or("");
    let condition = match sky {
        "skc" | "wind_skc" => Condition::Clear,
        "few" | "wind_few" => Condition::MostlyClear,
        "sct" | "wind_sct" => Condition::PartlyCloudy,
        "bkn" | "ovc" | "wind_bkn" | "wind_ovc" => Condition::Overcast,
        "fog" => Condition::Fog,
        "rain" | "rain_showers" | "rain_showers_hi" => Condition::Rain,
        "snow" | "rain_snow" | "blizzard" | "sleet" => Condition::Snow,
        "tsra" | "tsra_sct" | "tsra_hi" => Condition::Storm,
        _ => Condition::Unknown,
    };
    Ok(Weather {
        fetched,
        temp: current.temperature,
        is_day: Some(current.is_daytime),
        wind,
        condition,
        details: ForecastDetails::Nws,
        hours: periods
            .iter()
            .filter(|p| p.temperature_unit == "C")
            .filter_map(|p| {
                Some(Hour {
                    time: p.start_time.with_timezone(&Los_Angeles).naive_local(),
                    temp: p.temperature,
                    rain: p.probability_of_precipitation.as_ref()?.value?,
                })
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::json;

    #[test]
    fn open_meteo_outage_uses_the_current_dogpatch_forecast_period() {
        let period = |hour, temp, rain| {
            json!({
                "startTime": format!("2026-11-13T{hour}:00:00-08:00"),
                "endTime": format!("2026-11-13T{}:00:00-08:00", hour + 1),
                "temperatureUnit": "C", "temperature": temp, "isDaytime": true,
                "windSpeed": "18 km/h", "icon": "https://api.weather.gov/icons/land/day/rain,70?size=small",
                "probabilityOfPrecipitation": {"value": rain}
            })
        };
        let periods = [
            period(10, 12, Some(25)),
            period(11, 15, Some(70)),
            period(12, 16, None),
        ];
        let now = "2026-11-13T19:10:00Z".parse().unwrap();
        let response = json!({"properties":{"periods": periods}}).to_string();
        let weather = forecast(now, |url| match url {
            OPEN_METEO => Err("timeout".into()),
            NWS_HOURLY => Ok(response.clone()),
            _ => unreachable!(),
        })
        .unwrap();
        assert!(matches!(weather.details, ForecastDetails::Nws));
        assert!(weather.condition == Condition::Rain);
        assert_eq!((weather.temp, weather.wind), (15., Some(5.)));
        assert_eq!(weather.hours.len(), 2);
        assert_eq!(weather.hours[1].rain, 70.);
        assert_eq!(
            weather.hours[1].time,
            now.with_timezone(&Los_Angeles)
                .date_naive()
                .and_hms_opt(11, 0, 0)
                .unwrap()
        );
        assert!(parse_nws(&response, now + Duration::days(1)).is_err());
    }

    #[test]
    fn repeated_outages_preserve_the_original_timestamp_until_expiry() {
        let mut edition = crate::sample("sample");
        let original = edition.fetched;
        for minutes in [15, 45, 180, 181] {
            let mut next = crate::sample("sample");
            next.fetched = original + Duration::minutes(minutes);
            next.weather = None;
            edition.replace_with(next);
            assert_eq!(
                edition.weather.as_ref().map(|w| w.fetched),
                (minutes <= 180).then_some(original)
            );
        }
        let mut recovery = crate::sample("sample");
        recovery.fetched = original + Duration::hours(4);
        recovery.weather.as_mut().unwrap().fetched = recovery.fetched;
        edition.replace_with(recovery);
        assert_eq!(edition.weather.unwrap().fetched, edition.fetched);
    }

    #[test]
    fn cached_daily_values_expire_at_pacific_midnight() {
        let mut w = crate::sample("sample").weather.unwrap();
        w.fetched = "2026-11-14T07:55:00Z".parse().unwrap();
        assert!(w.usable_at(w.fetched + Duration::minutes(4)));
        assert!(!w.usable_at(w.fetched + Duration::minutes(5)));
    }
}
