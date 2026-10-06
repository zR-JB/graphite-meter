//! Local time for log lines from the system's zone: `TZ` (a POSIX rule, a zone name or `:path`), else
//! `/etc/localtime`; UTC without either, as in a scratch container.

use std::{
    path::Path,
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};

static ZONE: LazyLock<Option<Zone>> = LazyLock::new(|| Zone::system(std::env::var("TZ").ok().as_deref()));

/// `time` in RFC 3339 in the system's zone, such as `2026-10-06T16:43:56+02:00`; `Z` where the offset is zero.
pub fn local(time: SystemTime) -> String {
    let seconds = i64::try_from(time.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()).unwrap_or(i64::MAX);
    format(seconds, ZONE.as_ref().map_or(0, |zone| zone.offset(seconds)))
}

fn format(seconds: i64, offset: i32) -> String {
    let local = seconds.saturating_add(i64::from(offset)).max(0);
    let (year, month, day) = civil(local.div_euclid(86_400));
    let (hour, minute, second) = (local / 3600 % 24, local / 60 % 60, local % 60);
    let zone = match offset {
        0 => "Z".to_owned(),
        _ => {
            let (sign, minutes) = (if offset < 0 { '-' } else { '+' }, offset.unsigned_abs() / 60);
            format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
        }
    };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{zone}")
}

/// The proleptic Gregorian date of a day count since 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let (era, day_of_era) = (shifted.div_euclid(146_097), shifted.rem_euclid(146_097));
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

/// The day count since 1970-01-01 of a proleptic Gregorian date.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let (era, year_of_era) = (year.div_euclid(400), year.rem_euclid(400));
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// UTC offsets in seconds east: before the first transition, after each, and the rule past the last.
#[derive(Debug, PartialEq)]
struct Zone {
    transitions: Vec<i64>,
    offsets: Vec<i32>,
    before: i32,
    rule: Option<Rule>,
}

impl Zone {
    fn system(tz: Option<&str>) -> Option<Self> {
        let Some(tz) = tz.filter(|tz| !tz.is_empty()) else {
            return Self::file(Path::new("/etc/localtime"));
        };
        let name = tz.strip_prefix(':').unwrap_or(tz);
        if name.starts_with('/') {
            return Self::file(Path::new(name));
        }
        let named = (!name.contains("..")).then(|| Self::file(&Path::new("/usr/share/zoneinfo").join(name)));
        named.flatten().or_else(|| Some(Self::ruled(Rule::parse(tz)?)))
    }

    fn ruled(rule: Rule) -> Self {
        Self {
            transitions: Vec::new(),
            offsets: Vec::new(),
            before: rule.standard,
            rule: Some(rule),
        }
    }

    fn file(path: &Path) -> Option<Self> {
        Self::tzif(&std::fs::read(path).ok()?)
    }

    /// A TZif file (RFC 8536): its 64-bit data and footer rule from version 2, else its 32-bit data.
    fn tzif(data: &[u8]) -> Option<Self> {
        let mut input = Reader(data);
        let (version, mut counts) = input.header()?;
        let mut size = 4;
        if version >= b'2' {
            input.take(block(counts, 4))?;
            counts = input.header()?.1;
            size = 8;
        }
        let [utc, standard, leaps, count, types, characters] = counts;
        let transitions = (0..count).map(|_| input.int(size)).collect::<Option<Vec<_>>>()?;
        let indices = input.take(count)?.to_vec();
        let infos = (0..types)
            .map(|_| {
                let offset = i32::try_from(input.int(4)?).ok()?;
                let daylight = input.take(2)?[0] != 0;
                Some((offset, daylight))
            })
            .collect::<Option<Vec<_>>>()?;
        input.take(characters + leaps * (size + 4) + standard + utc)?;
        let offsets = indices
            .iter()
            .map(|&index| Some(infos.get(usize::from(index))?.0))
            .collect::<Option<_>>()?;
        let before = infos.iter().find(|(_, daylight)| !daylight).or(infos.first())?.0;
        let footer = (size == 8).then(|| std::str::from_utf8(input.0).ok()).flatten();
        let rule = footer.and_then(|footer| Rule::parse(footer.trim_matches('\n')));
        Some(Self { transitions, offsets, before, rule })
    }

    fn offset(&self, at: i64) -> i32 {
        let passed = self.transitions.partition_point(|&transition| transition <= at);
        match (passed, &self.rule) {
            (0, _) if !self.transitions.is_empty() => self.before,
            (0, None) => self.before,
            (passed, _) if passed < self.transitions.len() => self.offsets[passed - 1],
            (_, Some(rule)) => rule.offset(at),
            (passed, None) => self.offsets[passed - 1],
        }
    }
}

/// The byte length of a TZif data block with `size`-byte times.
fn block([utc, standard, leaps, count, types, characters]: [usize; 6], size: usize) -> usize {
    count * size + count + types * 6 + characters + leaps * (size + 4) + standard + utc
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, length: usize) -> Option<&'a [u8]> {
        let (taken, rest) = self.0.split_at_checked(length)?;
        self.0 = rest;
        Some(taken)
    }

    /// A big-endian signed integer of 4 or 8 bytes.
    fn int(&mut self, size: usize) -> Option<i64> {
        let bytes = self.take(size)?;
        Some(match size {
            4 => i64::from(i32::from_be_bytes(bytes.try_into().ok()?)),
            _ => i64::from_be_bytes(bytes.try_into().ok()?),
        })
    }

    /// The version and the six counts of a TZif header.
    fn header(&mut self) -> Option<(u8, [usize; 6])> {
        let header = self.take(44)?;
        if &header[..4] != b"TZif" {
            return None;
        }
        let count = |index: usize| {
            let bytes = header[20 + 4 * index..24 + 4 * index].try_into().ok()?;
            usize::try_from(u32::from_be_bytes(bytes)).ok()
        };
        Some((header[4], [count(0)?, count(1)?, count(2)?, count(3)?, count(4)?, count(5)?]))
    }
}

/// A POSIX `TZ` rule, such as `CET-1CEST,M3.5.0,M10.5.0/3`; offsets in seconds east of UTC.
#[derive(Debug, PartialEq)]
struct Rule {
    standard: i32,
    daylight: Option<(i32, Change, Change)>,
}

/// When daylight time starts or ends: a day and a local time of day in seconds.
#[derive(Debug, PartialEq)]
struct Change {
    day: Day,
    time: i32,
}

#[derive(Debug, PartialEq)]
enum Day {
    /// `Mm.w.d`: weekday `d` (0 is Sunday) of week `w` of month `m`; week 5 is the last.
    Month(i64, i64, i64),
    /// `Jn`: day 1 to 365, never counting February 29.
    Julian(i64),
    /// `n`: day 0 to 365, counting February 29.
    Ordinal(i64),
}

impl Rule {
    fn parse(text: &str) -> Option<Self> {
        let mut text = text;
        name(&mut text)?;
        let standard = -clock(&mut text)?;
        if text.is_empty() {
            return Some(Self { standard, daylight: None });
        }
        name(&mut text)?;
        let daylight = match text.chars().next() {
            Some(',') | None => standard + 3600,
            Some(_) => -clock(&mut text)?,
        };
        let rules = text.strip_prefix(',').unwrap_or("M3.2.0,M11.1.0");
        let (start, end) = rules.split_once(',')?;
        Some(Self {
            standard,
            daylight: Some((daylight, Change::parse(start)?, Change::parse(end)?)),
        })
    }

    fn offset(&self, at: i64) -> i32 {
        let Some((daylight, start, end)) = &self.daylight else {
            return self.standard;
        };
        let year = civil((at + i64::from(self.standard)).div_euclid(86_400)).0;
        let start = start.at(year) - i64::from(self.standard);
        let end = end.at(year) - i64::from(*daylight);
        let summer = if start < end {
            (start..end).contains(&at)
        } else {
            !(end..start).contains(&at)
        };
        if summer { *daylight } else { self.standard }
    }
}

impl Change {
    fn parse(text: &str) -> Option<Self> {
        let (day, time) = text
            .split_once('/')
            .map_or((text, None), |(day, time)| (day, Some(time)));
        let time = match time {
            Some(mut time) => clock(&mut time).filter(|_| time.is_empty())?,
            None => 7200,
        };
        let number = |text: &str| text.parse::<i64>().ok();
        let day = if let Some(month) = day.strip_prefix('M') {
            let mut fields = month.split('.').map(number);
            let (month, week, weekday) = (fields.next()??, fields.next()??, fields.next()??);
            ((1..=12).contains(&month) && (1..=5).contains(&week) && (0..=6).contains(&weekday))
                .then_some(Day::Month(month, week, weekday))?
        } else if let Some(julian) = day.strip_prefix('J') {
            Day::Julian(number(julian).filter(|day| (1..=365).contains(day))?)
        } else {
            Day::Ordinal(number(day).filter(|day| (0..=365).contains(day))?)
        };
        Some(Self { day, time })
    }

    /// The change in `year` as seconds since 1970-01-01 in the local time it reads.
    fn at(&self, year: i64) -> i64 {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let day = match self.day {
            Day::Month(month, week, weekday) => {
                let first = days(year, month, 1);
                let length = days(year + month / 12, month % 12 + 1, 1) - first;
                let mut day = first + (weekday - (first + 4).rem_euclid(7)).rem_euclid(7) + (week - 1) * 7;
                while day - first >= length {
                    day -= 7;
                }
                day
            }
            Day::Julian(day) => days(year, 1, 1) + day - 1 + i64::from(leap && day >= 60),
            Day::Ordinal(day) => days(year, 1, 1) + day,
        };
        day * 86_400 + i64::from(self.time)
    }
}

/// A zone abbreviation: letters, or anything within `<` and `>`.
fn name(text: &mut &str) -> Option<()> {
    let length = match text.strip_prefix('<') {
        Some(quoted) => quoted.find('>')? + 2,
        None => text.find(|c: char| !c.is_ascii_alphabetic()).unwrap_or(text.len()),
    };
    (length >= 3).then(|| *text = &text[length..])
}

/// `[+-]hh[:mm[:ss]]` in seconds, as a POSIX offset west of UTC or a time of day.
fn clock(text: &mut &str) -> Option<i32> {
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let rest = text.trim_start_matches(['+', '-']);
    let length = rest
        .find(|c: char| !c.is_ascii_digit() && c != ':')
        .unwrap_or(rest.len());
    let mut parts = rest[..length].split(':').map(|part| part.parse::<i32>().ok());
    let hours = parts.next()??;
    let minutes = parts.next().unwrap_or(Some(0))?;
    let seconds = parts.next().unwrap_or(Some(0))?;
    *text = &rest[length..];
    Some(sign * (hours * 3600 + minutes * 60 + seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-01-15T12:00:00Z, 2026-03-29T00:59:59Z, 2026-03-29T01:00:00Z and 2026-10-06T12:00:00Z.
    const WINTER: i64 = 1_768_478_400;
    const BEFORE_SPRING: i64 = 1_774_745_999;
    const SPRING: i64 = 1_774_746_000;
    const AUTUMN: i64 = 1_791_288_000;

    #[test]
    fn posix_rules_switch_at_their_changes_in_either_hemisphere() {
        let berlin = Rule::parse("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        let offsets = [WINTER, BEFORE_SPRING, SPRING, AUTUMN].map(|at| berlin.offset(at));
        assert_eq!(offsets, [3600, 3600, 7200, 7200]);
        let sydney = Rule::parse("AEST-10AEDT,M10.1.0,M4.1.0/3").unwrap();
        assert_eq!([WINTER, AUTUMN].map(|at| sydney.offset(at)), [39_600, 39_600]);
        assert_eq!(sydney.offset(WINTER + 150 * 86_400), 36_000, "mid-June is standard time");
        let fixed = Rule::parse("<-03>3").unwrap();
        assert_eq!((fixed.offset(AUTUMN), fixed.daylight), (-10_800, None));
        for bad in ["", "C1", "CET", "CET-1CEST,M13.1.0,M10.5.0", "CET-1CEST,M3.5.0"] {
            assert_eq!(Rule::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn tzif_files_use_their_transitions_then_their_footer() {
        let mut file = Vec::new();
        let header = |file: &mut Vec<u8>, counts: [u32; 6]| {
            file.extend_from_slice(b"TZif2");
            file.extend_from_slice(&[0; 15]);
            counts
                .iter()
                .for_each(|count| file.extend_from_slice(&count.to_be_bytes()));
        };
        header(&mut file, [0; 6]);
        header(&mut file, [0, 0, 0, 1, 2, 8]);
        file.extend_from_slice(&WINTER.to_be_bytes());
        file.push(1);
        for (offset, daylight) in [(0_i32, 0_u8), (3600, 0)] {
            file.extend_from_slice(&offset.to_be_bytes());
            file.extend_from_slice(&[daylight, 0]);
        }
        file.extend_from_slice(b"UTC\0CET\0\nCET-1CEST,M3.5.0,M10.5.0/3\n");
        let zone = Zone::tzif(&file).unwrap();
        assert_eq!([WINTER - 1, WINTER, AUTUMN].map(|at| zone.offset(at)), [0, 3600, 7200]);
        assert_eq!(Zone::tzif(b"TZif2"), None);
    }

    #[test]
    fn times_print_with_their_offset_or_z() {
        assert_eq!(format(AUTUMN, 7200), "2026-10-06T14:00:00+02:00");
        assert_eq!(format(AUTUMN, -12_600), "2026-10-06T08:30:00-03:30");
        assert_eq!(format(AUTUMN, 0), "2026-10-06T12:00:00Z");
        let zone = Zone::system(Some("CET-1CEST,M3.5.0,M10.5.0/3")).unwrap();
        assert_eq!(zone.offset(AUTUMN), 7200);
    }
}
