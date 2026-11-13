mod calendar;
mod display;
mod feeds;

use chrono::{DateTime, Utc};
use chrono_tz::America::Los_Angeles;
use feeds::{Edition, Event, Hour, Schedule, Weather};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::VecDeque,
    env,
    error::Error,
    fs,
    fs::OpenOptions,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, UdpSocket},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{Arc, RwLock},
    thread,
    time::Duration,
};
use tiny_http::{Header, Method, Request, Response, Server};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const POLL: u64 = 900;

fn sample(mode: &str) -> Edition {
    let now = DateTime::parse_from_rfc3339("2026-11-10T16:15:00-08:00")
        .unwrap()
        .to_utc();
    let day = now.with_timezone(&Los_Angeles).date_naive();
    let next = day + chrono::Days::new(11);
    let busy = mode == "sample-busy";
    let dry = matches!(mode, "sample-dry" | "sample-quiet");
    let event = Event {
        day,
        venue: "Chase Center".into(),
        start: if busy { "15:00" } else { "18:00" }.parse().unwrap(),
        end: "22:00".parse().unwrap(),
    };
    let fixture = if busy {
        include_str!("../examples/busy-day.ics")
    } else {
        include_str!("../examples/household.ics")
    };
    let plans = if mode == "sample-quiet" {
        calendar::CalendarPlans::default()
    } else {
        calendar::parse_calendar(fixture, now).expect("invalid built-in sample calendar")
    };
    let mut events = Vec::new();
    if busy {
        events.push(Event {
            day,
            venue: "Oracle Park".into(),
            start: "12:00".parse().unwrap(),
            end: "18:00".parse().unwrap(),
        });
    }
    if !dry {
        events.push(event.clone());
    }
    events.push(Event {
        day: next,
        start: "18:00".parse().unwrap(),
        ..event
    });
    let (code, rain, mm) = if dry {
        (0, 0., 0.)
    } else if busy {
        (61, 85., 4.8)
    } else {
        (2, 30., 0.7)
    };
    Edition {
        calendar: calendar::Agenda::Ready(plans),
        fetched: now,
        schedule: Some(Schedule {
            first: day,
            last: next,
            events,
        }),
        weather: Some(Weather {
            code: Some(code),
            is_day: Some(true),
            temp: 14.,
            low: 11.,
            high: 17.,
            wind: 3.6,
            rain,
            mm,
            sunset: "17:01".parse().unwrap(),
            hours: [
                (17, 14., 10., 85.),
                (18, 13., 15., 70.),
                (19, 13., 30., 45.),
                (20, 12., 25., 30.),
                (21, 12., 15., 15.),
                (22, 11., 5., 5.),
            ]
            .into_iter()
            .map(|(h, temp, rain, rainy_day)| Hour {
                time: day.and_hms_opt(h, 0, 0).unwrap(),
                temp,
                rain: if dry {
                    0.
                } else if busy {
                    rainy_day
                } else {
                    rain
                },
            })
            .collect(),
        }),
    }
}

#[derive(Deserialize, Serialize)]
struct Device {
    mac: String,
    key: String,
    image: String,
}
fn secret() -> Result<String> {
    let mut bytes = [0; 16];
    fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn register(mac: String, path: &Path) -> Result<Device> {
    let d = Device {
        mac,
        key: secret()?,
        image: secret()?,
    };
    let tmp = path.with_extension("tmp");
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    serde_json::to_writer(&mut out, &d)?;
    out.flush()?;
    fs::rename(tmp, path)?;
    Ok(d)
}
fn normalized_mac(s: &str) -> Option<String> {
    let mac = s.trim().to_ascii_uppercase().replace('-', ":");
    let valid = mac.split(':').count() == 6
        && mac
            .split(':')
            .all(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    valid.then_some(mac)
}
fn header<'a>(r: &'a Request, name: &'static str) -> Option<&'a str> {
    r.headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str())
}

#[derive(Default)]
struct Images {
    recent: VecDeque<(i64, Vec<u8>)>,
}
impl Images {
    fn store(&mut self, ts: i64, bmp: Vec<u8>) -> String {
        if let Some((key, last)) = self.recent.back() {
            if last == &bmp {
                return format!("screen-foglin-{key}");
            }
        }
        let id = self.recent.back().map_or(ts, |(n, _)| ts.max(n + 1));
        self.recent.push_back((id, bmp));
        if self.recent.len() > 2 {
            self.recent.pop_front();
        }
        format!("screen-foglin-{id}")
    }
}
fn address(arg: Option<String>) -> Result<SocketAddrV4> {
    let addr = if let Some(s) = arg {
        s.parse()?
    } else {
        let sock = UdpSocket::bind("0.0.0.0:0")?;
        sock.connect("192.0.2.1:9")?;
        let std::net::IpAddr::V4(ip) = sock.local_addr()?.ip() else {
            return Err("Specify the Pi's Wi-Fi IPv4 address".into());
        };
        SocketAddrV4::new(ip, 8088)
    };
    let ip: Ipv4Addr = *addr.ip();
    if !(ip.is_private() || ip.is_loopback()) {
        return Err("Specify a private LAN IPv4 address and port".into());
    }
    Ok(addr)
}

fn serve(addr: SocketAddrV4) -> Result<()> {
    let server = Server::http(addr)?;
    let path = Path::new("pair.json");
    let mut device: Option<Device> = match fs::read(path) {
        Ok(b) => Some(serde_json::from_slice(&b)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let edition = Arc::new(RwLock::new(feeds::refresh()));
    let shared = Arc::clone(&edition);
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(POLL));
        let next = feeds::refresh();
        *shared.write().unwrap() = next;
    });
    let mut images = Images::default();
    println!("Enter http://{addr} under Advanced → Server in the TRMNL setup portal.");
    for r in server.incoming_requests() {
        let route = r
            .url()
            .split('?')
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        let setup = r.method() == &Method::Get && route == "/api/setup";
        let display = r.method() == &Method::Get && route == "/api/display";
        let token_ok = device
            .as_ref()
            .is_some_and(|d| Some(d.key.as_str()) == header(&r, "Access-Token"));
        let response = if setup || display {
            let allowed = if setup {
                if let Some(mac) = header(&r, "ID").and_then(normalized_mac) {
                    if device.is_none() {
                        device = Some(register(mac.clone(), path)?);
                    }
                    device.as_ref().is_some_and(|d| d.mac == mac)
                } else {
                    false
                }
            } else {
                token_ok
            };
            if allowed {
                let d = device.as_ref().unwrap();
                let now = Utc::now();
                let volts = display::battery(header(&r, "Battery-Voltage"));
                let frame = display::render(&edition.read().unwrap(), now, volts, false);
                let name = images.store(now.timestamp(), frame);
                let image = format!("http://{addr}/screens/{}/{name}.bmp", d.image);
                let payload = if setup {
                    json!({"status":200,"api_key":d.key,"friendly_id":"FOG001","image_url":image,"message":"Dogpatch is ready"})
                } else {
                    // Firmware 1.6.10 otherwise requests a partial refresh for each BMP.
                    json!({"status":0,"filename":name,"image_url":image,"refresh_rate":POLL,"maximum_compatibility":true,"reset_firmware":false,"update_firmware":false,"firmware_url":"","special_function":"none"})
                };
                (200, "application/json", serde_json::to_vec(&payload)?)
            } else {
                (403, "application/json", br#"{"status":403}"#.to_vec())
            }
        } else if r.method() == &Method::Post && route == "/api/log" && token_ok {
            (204, "text/plain", Vec::new())
        } else {
            let image = device
                .as_ref()
                .filter(|_| r.method() == &Method::Get)
                .and_then(|d| {
                    let id = route
                        .strip_prefix(&format!("/screens/{}/screen-foglin-", d.image))?
                        .strip_suffix(".bmp")?
                        .parse::<i64>()
                        .ok()?;
                    images
                        .recent
                        .iter()
                        .find(|(k, _)| *k == id)
                        .map(|(_, bmp)| bmp)
                });
            if let Some(bmp) = image {
                (200, "image/bmp", bmp.clone())
            } else {
                (404, "text/plain", Vec::new())
            }
        };
        let reply = Response::from_data(response.2)
            .with_status_code(response.0)
            .with_header(Header::from_bytes("Content-Type", response.1).unwrap())
            .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
        if let Err(e) = r.respond(reply) {
            eprintln!("Display disconnected: {e}")
        }
    }
    Ok(())
}
fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("serve") => serve(address(args.next())?),
        Some(mode @ ("sample" | "sample-busy" | "sample-quiet" | "sample-dry" | "preview")) => {
            let sample_mode = mode != "preview";
            let data = if sample_mode {
                sample(mode)
            } else {
                feeds::refresh()
            };
            let bmp = display::render(
                &data,
                data.fetched,
                if sample_mode { Some(3.86) } else { None },
                sample_mode,
            );
            fs::write(args.next().unwrap_or_else(|| "dogpatch.bmp".into()), bmp)?;
            Ok(())
        }
        _ => {
            println!(
                "Fogline · Dogpatch household dashboard

fogline serve [PI_LAN_IP:8088]     Serve the display on your home Wi-Fi
fogline preview [FILE.bmp]        Render live feeds
fogline sample [FILE.bmp]         Preview a fictional household calendar
fogline sample-busy [FILE.bmp]    Preview overlapping plans and both event venues
fogline sample-dry [FILE.bmp]     Preview clear weather and no event meters today
fogline sample-quiet [FILE.bmp]   Preview a day without plans

To connect Apple Calendar, set FOGLINE_ICS to a webcal/HTTPS subscription
link or a local .ics path. The default file is calendar.ics in the working
directory. On NixOS, set services.fogline.environmentFile to the runtime path
of the file containing FOGLINE_ICS. Pairing is saved
in pair.json in the working directory."
            );
            Ok(())
        }
    }
}
