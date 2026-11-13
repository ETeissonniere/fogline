use crate::calendar::{Agenda, Plan};
use crate::feeds::{Edition, Schedule, Weather};
use ab_glyph::{point, Font, FontRef, ScaleFont};
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::America::Los_Angeles;
use std::f32::consts::TAU;

const WIDTH: usize = 800;
const HEIGHT: usize = 480;

struct Canvas {
    pixels: Vec<u8>,
    body: FontRef<'static>,
    title: FontRef<'static>,
}

impl Canvas {
    fn new() -> Self {
        Self {
            pixels: vec![255; WIDTH / 8 * HEIGHT],
            body: FontRef::try_from_slice(include_bytes!("../fonts/DejaVuSans.ttf")).unwrap(),
            title: FontRef::try_from_slice(include_bytes!("../fonts/DMSerifText-Regular.ttf"))
                .unwrap(),
        }
    }

    fn ink(&mut self, x: i32, y: i32) {
        if (0..WIDTH as i32).contains(&x) && (0..HEIGHT as i32).contains(&y) {
            let (x, y) = (x as usize, y as usize);
            self.pixels[y * (WIDTH / 8) + x / 8] &= !(0x80 >> (x % 8));
        }
    }

    fn rect(&mut self, x: i32, y: i32, w: i32, h: i32) {
        for j in y..y + h {
            for i in x..x + w {
                self.ink(i, j);
            }
        }
    }

    fn line(&mut self, from: (i32, i32), to: (i32, i32), stroke: i32) {
        let (dx, dy) = ((to.0 - from.0) as f32, (to.1 - from.1) as f32);
        let steps = dx.abs().max(dy.abs()).max(1.) as i32;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            self.rect(
                from.0 + (t * dx).round() as i32 - stroke / 2,
                from.1 + (t * dy).round() as i32 - stroke / 2,
                stroke,
                stroke,
            );
        }
    }

    fn circle(&mut self, x: i32, y: i32, r: i32, thickness: i32) {
        for j in -r..=r {
            for i in -r..=r {
                let d = i * i + j * j;
                if d <= r * r && d >= (r - thickness).max(0).pow(2) {
                    self.ink(x + i, y + j)
                }
            }
        }
    }

    fn width(&self, s: &str, size: f32, serif: bool) -> f32 {
        let font = if serif { &self.title } else { &self.body }.as_scaled(size);
        s.chars().map(|c| font.h_advance(font.glyph_id(c))).sum()
    }

    fn text(&mut self, s: &str, x: i32, y: i32, size: f32, serif: bool, max: i32) {
        let f = if serif {
            self.title.clone()
        } else {
            self.body.clone()
        };
        let scaled = f.as_scaled(size);
        let ellipsis = "…";
        let mut clipped = String::new();
        let text = if self.width(s, size, serif) > max as f32 {
            let mut remaining = max as f32 - self.width(ellipsis, size, serif);
            for c in s.chars() {
                remaining -= scaled.h_advance(f.glyph_id(c));
                if remaining < 0. {
                    break;
                }
                clipped.push(c);
            }
            clipped.push('…');
            clipped.as_str()
        } else {
            s
        };
        let mut at = x as f32;
        for c in text.chars() {
            let id = f.glyph_id(c);
            if let Some(o) = f.outline_glyph(
                id.with_scale_and_position(size, point(at, y as f32 + scaled.ascent())),
            ) {
                let p = o.px_bounds().min;
                o.draw(|gx, gy, a| {
                    if a >= 0.43 {
                        self.ink(p.x as i32 + gx as i32, p.y as i32 + gy as i32)
                    }
                });
            }
            at += scaled.h_advance(id);
        }
    }

    fn centered(&mut self, s: &str, x: i32, y: i32, size: f32, serif: bool, max: i32) {
        let width = (self.width(s, size, serif).ceil() as i32).min(max);
        self.text(s, x - width / 2, y, size, serif, max)
    }

    fn title_lines(&self, s: &str, width: i32, size: f32) -> Vec<String> {
        let mut lines = Vec::new();
        let mut line = String::new();
        for w in s.split_whitespace() {
            let test = if line.is_empty() {
                w.to_owned()
            } else {
                format!("{line} {w}")
            };
            if !line.is_empty() && self.width(&test, size, true) > width as f32 {
                lines.push(std::mem::take(&mut line));
                line = w.to_owned();
            } else {
                line = test
            }
        }
        if !line.is_empty() {
            lines.push(line)
        }
        lines
    }

    fn bmp(&self) -> Vec<u8> {
        let mut b = b"BM".to_vec();
        for n in [48062u32, 0, 62, 40, 800, 480] {
            b.extend(n.to_le_bytes())
        }
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        for n in [0u32, 48000, 0, 0, 2, 2] {
            b.extend(n.to_le_bytes())
        }
        b.extend([0, 0, 0, 0, 255, 255, 255, 0]);
        for row in self.pixels.chunks_exact(WIDTH / 8).rev() {
            b.extend(row)
        }
        b
    }
}

pub fn battery(value: Option<&str>) -> Option<f32> {
    value?
        .trim()
        .parse()
        .ok()
        .filter(|v| (2.5..=4.4).contains(v))
}
fn parking(schedule: Option<&Schedule>, day: NaiveDate, time: NaiveTime) -> Vec<String> {
    let Some(schedule) = schedule.filter(|s| s.first <= day && day <= s.last) else {
        return vec!["Schedule unavailable".into()];
    };
    let mut lines: Vec<_> = schedule
        .events
        .iter()
        .filter(|e| e.day == day)
        .take(2)
        .map(|e| {
            let status = if time < e.start {
                "Later today"
            } else if time < e.end {
                "Active now"
            } else {
                "Ended"
            };
            format!("{}   ·   {}   ·   {status}", e.venue, e.window())
        })
        .collect();
    let empty_today = lines.is_empty();
    if empty_today {
        lines.push("No special rates listed today".into());
    }
    if lines.len() == 1 {
        if let Some(e) = schedule
            .events
            .iter()
            .find(|e| e.day > day && (!empty_today || day.succ_opt() == Some(e.day)))
        {
            lines.push(format!(
                "Next   {}   ·   {}   ·   {}",
                e.day.format("%a %-d %b"),
                e.venue,
                e.window()
            ));
        }
    }
    lines
}

fn clock_face(c: &mut Canvas, day: NaiveDate, now: DateTime<Utc>, agenda: &Agenda) {
    let cx = 126;
    let cy = 158;
    let polar = |angle: f32, r: i32| {
        (
            cx + (angle.sin() * r as f32).round() as i32,
            cy - (angle.cos() * r as f32).round() as i32,
        )
    };
    let day_start = Los_Angeles
        .from_local_datetime(&day.and_time(NaiveTime::MIN))
        .single()
        .unwrap()
        .to_utc();
    let day_end = Los_Angeles
        .from_local_datetime(&day.succ_opt().unwrap().and_time(NaiveTime::MIN))
        .single()
        .unwrap()
        .to_utc();
    // Local clock labels stay in civil time even on daylight-saving transitions.
    let angle = |t: DateTime<Utc>| {
        if t >= day_end {
            TAU
        } else if t <= day_start {
            0.
        } else {
            t.with_timezone(&Los_Angeles)
                .time()
                .num_seconds_from_midnight() as f32
                / 86400.
                * TAU
        }
    };
    c.circle(cx, cy, 72, 2);
    for i in 0..24 {
        let a = TAU * (i as f32) / 24.;
        let major = i % 6 == 0;
        c.line(
            polar(a, 72),
            polar(a, if major { 81 } else { 76 }),
            if major { 3 } else { 2 },
        );
    }
    if let Agenda::Ready(plans) = agenda {
        let mut booked_until = day_start;
        for p in plans
            .events
            .iter()
            .filter(|p| !p.all_day && p.start < day_end && p.end.max(p.start) >= day_start)
        {
            let start = angle(p.start);
            let end = angle(p.end).max(start + 0.025).min(TAU);
            let overlap_end = angle(booked_until.min(p.end));
            let steps = ((end - start) * 100.).ceil() as i32;
            for k in 0..=steps {
                let a = start + (end - start) * (k as f32 / steps.max(1) as f32);
                c.line(polar(a, 63), polar(a, 72), 2);
                if a < overlap_end {
                    c.line(polar(a, 55), polar(a, 59), 2);
                }
            }
            booked_until = booked_until.max(p.end);
        }
    }
    c.centered("12a", cx, cy - 100, 17., false, 48);
    c.centered("6a", cx + 95, cy - 9, 17., false, 30);
    c.centered("noon", cx, cy + 81, 17., false, 64);
    c.centered("6p", cx - 95, cy - 9, 17., false, 30);
    let a = angle(now);
    c.line(polar(a, 52), polar(a, 78), 3);
    let tip = polar(a, 81);
    for i in -5..=5 {
        c.rect(
            tip.0 - i32::abs(i) / 2 - 2,
            tip.1 + i,
            5 - i32::abs(i) * 3 / 5,
            1,
        );
    }
    c.centered(
        &day.format("%b").to_string().to_uppercase(),
        cx,
        cy - 36,
        17.,
        false,
        80,
    );
    c.centered(&day.day().to_string(), cx, cy - 21, 52., true, 85);
    c.centered(
        &now.with_timezone(&Los_Angeles)
            .format("%-I:%M %p")
            .to_string(),
        cx,
        cy + 29,
        16.,
        false,
        92,
    );
}

fn time_label(p: &Plan, today: NaiveDate) -> String {
    let start = p.start.with_timezone(&Los_Angeles);
    let prefix = if start.date_naive() == today || (p.all_day && start.date_naive() < today) {
        String::new()
    } else if Some(start.date_naive()) == today.succ_opt() {
        "Tomorrow · ".into()
    } else {
        format!("{} · ", start.format("%a %-d"))
    };
    if p.all_day {
        format!("{prefix}All day")
    } else {
        format!("{prefix}{}", start.format("%-I:%M %p"))
    }
}

struct AgendaFocus<'a> {
    primary: &'a Plan,
    following: Vec<&'a Plan>,
    more_today: usize,
}

fn agenda_focus(events: &[Plan], now: DateTime<Utc>) -> Option<AgendaFocus<'_>> {
    // Calendar occurrences arrive chronologically. Give today's appointments first billing.
    let day = now.with_timezone(&Los_Angeles).date_naive();
    let mut remaining = events.iter().filter(|p| p.end > now || p.start >= now);
    let today: Vec<_> = remaining
        .clone()
        .take_while(|p| p.start.with_timezone(&Los_Angeles).date_naive() <= day)
        .collect();
    let primary = today
        .iter()
        .copied()
        .find(|p| !p.all_day)
        .or_else(|| remaining.next())?;
    let mut following: Vec<_> = today
        .iter()
        .copied()
        .filter(|p| !std::ptr::eq(*p, primary))
        .collect();
    // Explicitly name simultaneous commitments before all-day items and later plans.
    following.sort_by_key(|p| {
        if !p.all_day && p.start <= now {
            0
        } else if p.all_day {
            1
        } else {
            2
        }
    });
    let more_today = following.len().saturating_sub(2);
    following.truncate(2);
    if following.is_empty() {
        if let Some(next) = remaining.find(|p| !std::ptr::eq(*p, primary)) {
            following.push(next);
        }
    }
    Some(AgendaFocus {
        primary,
        following,
        more_today,
    })
}

fn agenda(c: &mut Canvas, state: &Agenda, now: DateTime<Utc>) {
    let day = now.with_timezone(&Los_Angeles).date_naive();
    clock_face(c, day, now, state);
    let mut next_lines = Vec::new();
    let mut more_today = 0;
    let mut time = None;
    let mut quiet = false;
    let (eyebrow, title) = match state {
        Agenda::NotConfigured => ("SHARED CALENDAR", "Connect your calendar"),
        Agenda::Unavailable => ("SHARED CALENDAR", "Calendar unavailable"),
        Agenda::Ready(plans) => {
            let events = &plans.events;
            if let Some(focus) = agenda_focus(events, now) {
                let p = focus.primary;
                more_today = focus.more_today;
                for other in focus.following {
                    let label = if !other.all_day && other.start <= now && now < other.end {
                        "Also now".into()
                    } else {
                        time_label(other, day)
                    };
                    next_lines.push(format!("{label}  ·  {}", other.title));
                }
                let ongoing = !p.all_day && p.start <= now && p.end > now;
                let status = if ongoing {
                    "NOW"
                } else if p.all_day {
                    "ON THE CALENDAR"
                } else {
                    "NEXT UP"
                };
                time = Some(if ongoing {
                    format!(
                        "Until {}",
                        p.end.with_timezone(&Los_Angeles).format("%-I:%M %p")
                    )
                } else {
                    time_label(p, day)
                });
                (status, p.title.as_str())
            } else if plans.is_partial() {
                ("SHARED CALENDAR", "Schedule incomplete")
            } else {
                quiet = true;
                let ended_today = events.iter().any(|p| {
                    p.start.with_timezone(&Los_Angeles).date_naive() == day
                        || (p.end.with_timezone(&Los_Angeles).date_naive() == day
                            && p.end.with_timezone(&Los_Angeles).time() != NaiveTime::MIN)
                });
                (
                    "SHARED CALENDAR",
                    if ended_today {
                        "Nothing else today"
                    } else {
                        "An open day"
                    },
                )
            }
        }
    };
    c.text(eyebrow, 258, 68, 17., false, 516);
    if time.is_some() && matches!(state, Agenda::Ready(p) if p.is_partial()) {
        c.text("Partial calendar", 634, 68, 17., false, 140);
    }
    if let Some(t) = time.as_deref() {
        c.text(t, 256, 88, 38., false, 516);
    }
    if quiet {
        waterfront(c);
    }

    let mut size = 61.;
    let mut lines = c.title_lines(title, 510, size);
    if lines.len() > 2 || (lines.len() > 1 && next_lines.len() > 1) {
        size = if next_lines.len() > 1 { 43. } else { 48. };
        lines = c.title_lines(title, 510, size);
    }
    let lines = if lines.len() > 2 {
        vec![lines[0].clone(), format!("{}…", lines[1])]
    } else {
        lines
    };
    for (i, line) in lines.iter().enumerate() {
        c.text(
            line,
            255,
            (if time.is_some() { 127 } else { 93 }) + i as i32 * (size as i32 - 12),
            size,
            true,
            516,
        );
    }
    let row_y = if next_lines.len() == 2 { 207 } else { 232 };
    for (index, line) in next_lines.iter().enumerate() {
        let y = row_y + 25 * index as i32;
        let last = index + 1 == next_lines.len() && more_today > 0;
        let mut width = 516;
        if last {
            let count = format!("+{more_today} today");
            let badge_width = c.width(&count, 18., false).ceil() as i32;
            width -= badge_width + 22;
            c.text(
                &count,
                774 - badge_width,
                y + 2,
                18.,
                false,
                badge_width + 1,
            );
        }
        c.text(line, 258, y, 22., false, width);
    }
}

// A spare drawing of the industrial shoreline fills the calendar only when the day is open.
fn waterfront(c: &mut Canvas) {
    let baseline = 225;
    c.line((260, baseline), (769, baseline), 2);
    for (x, w, h) in [
        (287, 38, 9),
        (326, 33, 9),
        (360, 39, 9),
        (399, 32, 9),
        (431, 35, 9),
    ] {
        c.line((x, baseline - h), (x + w, baseline - h - 8), 2);
        c.line((x + w, baseline - h - 8), (x + w, baseline), 2);
        c.rect(x + 7, baseline - 7, 4, 5);
        c.rect(x + 16, baseline - 7, 4, 5);
    }
    c.rect(287, baseline - 9, 2, 9);
    for (x, h) in [(514, 17), (555, 13), (592, 11)] {
        c.rect(x, baseline - h, 18, h);
    }
    let x = 678;
    // Mast, lattice boom, suspension cable and hook.
    c.line((x - 11, baseline), (x + 2, baseline - 36), 3);
    c.line((x + 12, baseline), (x + 2, baseline - 36), 3);
    c.line((x - 7, baseline - 9), (x + 8, baseline - 14), 2);
    c.line((x - 6, baseline - 21), (x + 6, baseline - 18), 2);
    c.line((x - 13, baseline - 32), (x + 56, baseline - 49), 3);
    c.line((x - 13, baseline - 37), (x + 56, baseline - 49), 2);
    c.line((x, baseline - 35), (x + 7, baseline - 62), 2);
    c.line((x + 7, baseline - 62), (x + 53, baseline - 48), 1);
    c.line((x + 51, baseline - 48), (x + 51, baseline - 26), 1);
    c.rect(x + 49, baseline - 26, 5, 3);
    for (left, length, offset) in [
        (273, 41, 6),
        (328, 18, 10),
        (384, 57, 6),
        (473, 77, 8),
        (581, 31, 4),
        (660, 58, 7),
        (739, 16, 4),
    ] {
        c.rect(left, baseline + offset, length, 1);
    }
}

fn sunset_icon(c: &mut Canvas, x: i32, y: i32) {
    // Half sun below a flat horizon.
    for dx in -14..=14 {
        let dy = (196. - (dx * dx) as f32).max(0.).sqrt().round() as i32;
        c.rect(x + dx, y - dy, 2, 3);
    }
    c.rect(x - 23, y + 1, 48, 3);
    c.rect(x - 15, y + 8, 31, 2);
    for k in 0..=4 {
        let a = std::f32::consts::PI * k as f32 / 4.;
        let ray = |r: f32| {
            (
                x + (a.cos() * r).round() as i32,
                y - (a.sin() * r).round() as i32,
            )
        };
        c.line(ray(20.), ray(25.), 2);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Condition {
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
    fn label(self) -> &'static str {
        match self {
            Self::Clear => "Clear sky",
            Self::MostlyClear => "Mostly clear",
            Self::PartlyCloudy => "Partly cloudy",
            Self::Overcast => "Overcast",
            Self::Fog => "Fog",
            Self::Drizzle => "Drizzle",
            Self::Rain => "Rain",
            Self::Snow => "Snow",
            Self::Storm => "Thunderstorms",
            Self::Unknown => "Outside now",
        }
    }
}

fn cloud_contains(x: i32, y: i32) -> bool {
    let disk = |cx: i32, cy: i32, r: i32| (x - cx).pow(2) + (y - cy).pow(2) <= r * r;
    disk(-17, 1, 11)
        || disk(-5, -6, 15)
        || disk(9, -2, 12)
        || disk(18, 2, 10)
        || ((-17..=18).contains(&x) && (-1..=12).contains(&y))
}

fn condition_icon(c: &mut Canvas, x: i32, y: i32, condition: Condition, is_day: Option<bool>) {
    use Condition::*;
    let cloudy = !matches!(condition, Clear | MostlyClear | Unknown);
    let has_sky = matches!(condition, Clear | MostlyClear | PartlyCloudy);
    let (sx, sy) = if cloudy { (-11, -14) } else { (0, 0) };
    let radius = if cloudy { 12 } else { 16 };
    for dy in -37..=34 {
        for dx in -37..=37 {
            if cloudy && cloud_contains(dx, dy) {
                if [(-3, 0), (3, 0), (0, -3), (0, 3)]
                    .iter()
                    .any(|(a, b)| !cloud_contains(dx + a, dy + b))
                {
                    c.ink(x + dx, y + dy);
                }
            } else if has_sky {
                let (u, v) = (dx - sx, dy - sy);
                let d = u * u + v * v;
                let sky = match is_day {
                    Some(true) => {
                        let outer = radius + 9;
                        let on_ray = u.abs() <= 1 || v.abs() <= 1 || (u.abs() - v.abs()).abs() <= 1;
                        (d <= radius * radius && d >= (radius - 3).pow(2))
                            || (on_ray && d > (radius + 3).pow(2) && d <= outer * outer)
                    }
                    Some(false) => {
                        d <= radius * radius
                            && (u - 7).pow(2) + (v + 4).pow(2) > (radius - 1).pow(2)
                    }
                    None => false,
                };
                if sky {
                    c.ink(x + dx, y + dy);
                }
            }
        }
    }
    match condition {
        Fog => {
            for (lx, ly, w) in [(-19, 17, 43), (-26, 23, 38), (-16, 29, 32)] {
                c.rect(x + lx, y + ly, w, 2);
            }
        }
        Drizzle | Rain => {
            for dx in [-14, 0, 14] {
                let h = if condition == Rain { 10 } else { 5 };
                c.line((x + dx + 1, y + 17), (x + dx - 4, y + 17 + h), 3);
            }
        }
        Snow => {
            for dx in [-15, 0, 15] {
                c.line((x + dx - 3, y + 20), (x + dx + 3, y + 26), 2);
                c.line((x + dx + 3, y + 20), (x + dx - 3, y + 26), 2);
            }
        }
        Storm => {
            for (from, to) in [
                ((4, 14), (-4, 23)),
                ((-4, 23), (3, 23)),
                ((3, 23), (-6, 33)),
            ] {
                c.line((x + from.0, y + from.1), (x + to.0, y + to.1), 3);
            }
        }
        _ if condition == Unknown || (has_sky && !cloudy && is_day.is_none()) => {
            c.circle(x, y + 11, 8, 3);
            c.rect(x - 4, y - 19, 3, 24);
            c.rect(x + 2, y - 19, 3, 24);
            c.rect(x - 4, y - 20, 9, 3);
        }
        _ => {}
    }
}

fn hourly_chart(c: &mut Canvas, w: &Weather, local: chrono::NaiveDateTime) {
    let first = local.date().and_hms_opt(local.hour(), 0, 0).unwrap() + chrono::Duration::hours(1);
    let points: Vec<_> = (0..6)
        .map(|i| {
            let time = first + chrono::Duration::hours(i);
            (time, w.hours.iter().find(|h| h.time == time))
        })
        .collect();
    if points
        .iter()
        .all(|(_, hour)| hour.is_some_and(|h| h.rain == 0.))
    {
        c.text("Temperature", 260, 269, 22., true, 153);
        c.text("Dry for 6 hours", 432, 271, 17., false, 175);
        let hours = points.iter().filter_map(|(_, h)| *h);
        let low = hours.clone().map(|h| h.temp).fold(f64::INFINITY, f64::min);
        let high = hours
            .clone()
            .map(|h| h.temp)
            .fold(f64::NEG_INFINITY, f64::max);
        let mut previous = None;
        for (i, h) in hours.enumerate() {
            let x = 281 + 56 * i as i32;
            let y =
                323 - ((h.temp - (low + high) / 2.) * 40. / (high - low).max(8.)).round() as i32;
            if let Some(p) = previous {
                c.line(p, (x, y), 2);
            }
            c.circle(x, y, 4, 4);
            c.centered(&h.time.format("%-I%P").to_string(), x, 351, 17., false, 49);
            c.centered(&format!("{:.0}°", h.temp), x, 373, 17., false, 44);
            previous = Some((x, y));
        }
        return;
    }
    c.text("Rain chance", 260, 269, 22., true, 137);
    c.text(
        &format!("Today {:.0}% · {:.1} mm", w.rain, w.mm),
        432,
        271,
        17.,
        false,
        175,
    );
    let peak = points
        .iter()
        .filter_map(|(_, h)| h.map(|h| h.rain))
        .reduce(f64::max);
    let Some(peak) = peak else {
        c.text("Hourly forecast unavailable", 261, 321, 22., false, 335);
        return;
    };
    let mut peak_labelled = false;
    // Every bar uses the same 0–100% scale. Gaps in the feed leave gaps on the hourly axis.
    for (i, (time, h)) in points.iter().enumerate() {
        let x = 281 + 56 * i as i32;
        for y in (310..350).step_by(5) {
            c.rect(x, y, 1, 2);
        }
        if let Some(h) = h {
            let height = (h.rain.clamp(0., 100.) * 0.40).round() as i32;
            c.rect(x - 13, 350 - height, 27, height.max(2));
            if h.rain == peak && !peak_labelled {
                c.centered(
                    &format!("{:.0}%", h.rain),
                    x,
                    350 - height - 17,
                    16.,
                    false,
                    46,
                );
                peak_labelled = true;
            }
            c.centered(&format!("{:.0}°", h.temp), x, 373, 17., false, 44);
        } else {
            c.centered("—", x, 326, 17., false, 30);
        }
        c.centered(&time.format("%-I%P").to_string(), x, 351, 17., false, 49);
    }
}

fn weather(c: &mut Canvas, w: Option<&Weather>, local: chrono::NaiveDateTime) {
    let Some(w) = w else {
        c.text("Forecast unavailable", 26, 305, 32., true, 650);
        return;
    };
    let condition = Condition::from_code(w.code);
    c.text(condition.label(), 26, 269, 23., true, 214);
    condition_icon(c, 61, 322, condition, w.is_day);
    c.text(&format!("{:.0} °C", w.temp), 98, 291, 50., true, 145);
    c.text(
        &format!("{:.0}–{:.0} °C   ·   {:.1} m/s", w.low, w.high, w.wind),
        26,
        362,
        19.,
        false,
        218,
    );
    c.rect(240, 278, 1, 101);
    hourly_chart(c, w, local);
    c.rect(611, 278, 1, 101);
    sunset_icon(c, 693, 321);
    c.centered("Sunset", 693, 335, 20., false, 128);
    c.centered(
        &w.sunset.format("%-I:%M %p").to_string(),
        693,
        356,
        27.,
        true,
        152,
    );
}

pub fn render(edition: &Edition, now: DateTime<Utc>, volts: Option<f32>, sample: bool) -> Vec<u8> {
    let mut c = Canvas::new();
    let local = now.with_timezone(&Los_Angeles);
    let fetched = edition.fetched.with_timezone(&Los_Angeles);
    let day = local.date_naive();
    c.text("Dogpatch", 23, 4, 40., true, 203);
    c.text(&local.format("%A").to_string(), 259, 15, 25., false, 259);
    let caption = volts.map_or_else(|| "—".to_string(), |v| format!("{v:.2} V"));
    c.rect(681, 22, 27, 2);
    c.rect(681, 33, 27, 2);
    c.rect(681, 22, 2, 11);
    c.rect(706, 22, 2, 11);
    c.rect(709, 25, 2, 5);
    if let Some(v) = volts {
        for i in 0..[3.30, 3.60, 3.78, 4.02].iter().filter(|p| v >= **p).count() as i32 {
            c.rect(685 + i * 5, 25, 4, 7)
        }
    }
    c.text(&caption, 718, 15, 19., false, 60);
    c.rect(24, 52, 752, 3);
    agenda(&mut c, &edition.calendar, now);
    c.rect(24, 260, 752, 2);
    weather(
        &mut c,
        edition
            .weather
            .as_ref()
            .filter(|_| fetched.date_naive() == day),
        local.naive_local(),
    );
    c.rect(24, 395, 752, 1);
    let p = parking(edition.schedule.as_ref(), day, local.time());
    c.rect(25, 411, 24, 2);
    c.rect(25, 431, 24, 2);
    c.rect(25, 411, 2, 22);
    c.rect(47, 411, 2, 22);
    c.centered("P", 37, 408, 23., true, 20);
    c.text("Event meters", 61, 409, 21., true, 145);
    for (i, line) in p.iter().enumerate() {
        let size = if line.starts_with("Next   ") {
            16.
        } else {
            21.
        };
        c.text(line, 200, 402 + i as i32 * 24, size, false, 570);
    }
    c.rect(24, 452, 752, 1);
    c.text(
        &format!(
            "{}Updated {}",
            if sample { "Sample   ·   " } else { "" },
            fetched.format("%-I:%M %p PT")
        ),
        25,
        458,
        16.,
        false,
        470,
    );
    c.text("SFMTA   /   Open-Meteo", 601, 458, 16., false, 182);
    c.bmp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::parse_schedule;
    #[test]
    fn year_rollover_and_end_of_meter_window() {
        let body = r#"<time datetime="2026-12-30T08:00:00Z"></time><table><tr><th>Special Event Rate Hours</th></tr><tr><td>January 1</td><td>Chase</td><td>Noon to 10 P.M.</td></tr></table>"#;
        let s = parse_schedule(body).unwrap();
        let day = NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        for (h, want) in [(11, "Later today"), (12, "Active now"), (22, "Ended")] {
            assert_eq!(
                parking(Some(&s), day, NaiveTime::from_hms_opt(h, 0, 0).unwrap())[0]
                    .split("   ·   ")
                    .last()
                    .unwrap(),
                want
            )
        }
        assert_eq!(
            parking(Some(&s), day.succ_opt().unwrap(), NaiveTime::MIN)[0],
            "Schedule unavailable"
        );
    }
    #[test]
    fn busy_day_advances_after_each_appointment_and_counts_only_today() {
        let edition = crate::sample("sample-busy");
        let Agenda::Ready(plans) = edition.calendar else {
            panic!("expected sample calendar");
        };
        let focus = agenda_focus(&plans.events, edition.fetched).unwrap();
        assert_eq!(focus.primary.title, "Pottery studio");
        assert_eq!(
            focus
                .following
                .iter()
                .map(|p| p.title.as_str())
                .collect::<Vec<_>>(),
            ["Pick up the bread", "Make pasta together"]
        );
        assert_eq!(focus.more_today, 1);
        let after_studio = edition.fetched + chrono::Duration::minutes(45);
        let focus = agenda_focus(&plans.events, after_studio).unwrap();
        assert_eq!(focus.primary.title, "Make pasta together");
        assert_eq!(focus.following[0].title, "Movie night");
        assert_eq!(focus.more_today, 0);
    }
    #[test]
    fn one_venue_ending_does_not_end_the_other_meter_window() {
        let edition = crate::sample("sample-busy");
        let day = edition.fetched.with_timezone(&Los_Angeles).date_naive();
        let lines = parking(
            edition.schedule.as_ref(),
            day,
            NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
        );
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("Oracle Park") && lines[0].ends_with("Ended"));
        assert!(lines[1].contains("Chase Center") && lines[1].ends_with("Active now"));
    }
    #[test]
    fn missing_and_implausible_battery_values_stay_unknown() {
        for v in [None, Some("0"), Some("NaN"), Some("3900"), Some("5.2")] {
            assert_eq!(battery(v), None)
        }
        assert_eq!(battery(Some("3.91")), Some(3.91));
    }
}
