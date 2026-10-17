//! Timeline: stacks grouped by shooting day (and month), with per-day
//! counts for the heat strip. Plain calendar arithmetic on the EXIF capture
//! date (camera local time), no time zones.

use std::collections::BTreeMap;

use crate::catalog::Catalog;

/// A calendar date.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    pub y: i32,
    pub m: u8,
    pub d: u8,
}

pub const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
pub const WEEKDAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

impl Date {
    /// From EXIF `YYYY:MM:DD HH:MM:SS` (or `YYYY-MM-DD…`).
    pub fn from_exif(t: &str) -> Option<Self> {
        let s = t.trim();
        let (y, m, d) = (s.get(0..4)?, s.get(5..7)?, s.get(8..10)?);
        let date = Date {
            y: y.parse().ok()?,
            m: m.parse().ok()?,
            d: d.parse().ok()?,
        };
        (date.y > 0
            && (1..=12).contains(&date.m)
            && date.d >= 1
            && date.d <= days_in_month(date.y, date.m))
        .then_some(date)
    }

    /// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
    pub fn days(self) -> i64 {
        let y = i64::from(self.y) - i64::from(self.m <= 2);
        let era = y.div_euclid(400);
        let yoe = y - era * 400;
        let m = i64::from(self.m);
        let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(self.d) - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    }

    /// The date `n` days after 1970-01-01 (`civil_from_days`).
    pub fn from_days(n: i64) -> Self {
        let z = n + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let m = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let y = (yoe + era * 400 + i64::from(m <= 2)) as i32;
        Date { y, m, d }
    }

    /// 0 = Monday … 6 = Sunday.
    pub fn weekday(self) -> usize {
        // 1970-01-01 was a Thursday (3).
        (self.days() + 3).rem_euclid(7) as usize
    }

    /// "Fri 1 May 2026".
    pub fn long(self) -> String {
        format!(
            "{} {} {} {}",
            WEEKDAYS[self.weekday()],
            self.d,
            &MONTHS[usize::from(self.m) - 1][..3],
            self.y
        )
    }
}

pub fn days_in_month(y: i32, m: u8) -> u8 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// One shooting day: its stacks in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Day {
    pub date: Date,
    pub stacks: Vec<usize>,
}

/// Stacks (in display order) grouped into days, days in the order they
/// first appear (so the timeline follows the grid's sort), and the undated
/// stacks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Timeline {
    pub days: Vec<Day>,
    pub undated: Vec<usize>,
}

impl Timeline {
    pub fn build(c: &Catalog, stacks: &[usize]) -> Self {
        let mut out = Timeline::default();
        let mut at: std::collections::HashMap<Date, usize> = Default::default();
        for &s in stacks {
            let p = &c.photos[c.stacks[s].primary];
            match p
                .meta
                .exif
                .capture_time
                .as_deref()
                .and_then(Date::from_exif)
            {
                Some(date) => match at.get(&date) {
                    Some(&i) => out.days[i].stacks.push(s),
                    None => {
                        at.insert(date, out.days.len());
                        out.days.push(Day {
                            date,
                            stacks: vec![s],
                        });
                    }
                },
                None => out.undated.push(s),
            }
        }
        out
    }

    /// Stacks per date (for the heat strip).
    pub fn counts(&self) -> BTreeMap<Date, usize> {
        self.days.iter().map(|d| (d.date, d.stacks.len())).collect()
    }

    /// Years with photos, ascending.
    pub fn years(&self) -> Vec<i32> {
        let mut v: Vec<i32> = self.days.iter().map(|d| d.date.y).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Shade levels for the heat strip: 0 = no photos, 1–3 by quartile of the
/// shooting days' counts (so one huge day doesn't wash out the rest), 4 for
/// the top quarter and the busiest day.
pub fn heat_levels(counts: &BTreeMap<Date, usize>) -> impl Fn(usize) -> u8 {
    let mut v: Vec<usize> = counts.values().copied().filter(|&n| n > 0).collect();
    v.sort_unstable();
    let q = |f: f64| -> usize {
        if v.is_empty() {
            0
        } else {
            v[((v.len() - 1) as f64 * f).round() as usize]
        }
    };
    let (a, b, c) = (q(0.25), q(0.5), q(0.75));
    let max = v.last().copied().unwrap_or(0);
    move |n| match n {
        0 => 0,
        // The busiest day always stands out (with few days the quartiles
        // collapse onto it).
        n if n >= max => 4,
        n if n <= a => 1,
        n if n <= b => 2,
        n if n <= c => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        let d = Date::from_exif("2026:05:01 12:34:56").unwrap();
        assert_eq!(
            d,
            Date {
                y: 2026,
                m: 5,
                d: 1
            }
        );
        assert_eq!(d.weekday(), 4, "Friday");
        assert_eq!(d.long(), "Fri 1 May 2026");
        assert_eq!(Date::from_days(d.days()), d);
        assert_eq!(
            Date {
                y: 1970,
                m: 1,
                d: 1
            }
            .days(),
            0
        );
        assert_eq!(
            Date {
                y: 2000,
                m: 3,
                d: 1
            }
            .days()
                - Date {
                    y: 2000,
                    m: 2,
                    d: 28
                }
                .days(),
            2
        );
        for bad in ["0000:00:00 00:00:00", "2026:02:30 00:00:00", "garbage", ""] {
            assert!(Date::from_exif(bad).is_none(), "{bad}");
        }
        // Every day round-trips over a long span.
        for n in -1000..30_000 {
            assert_eq!(Date::from_days(n).days(), n);
        }
    }

    #[test]
    fn levels_by_quartile() {
        let counts: BTreeMap<Date, usize> = [1usize, 2, 3, 4, 100]
            .iter()
            .enumerate()
            .map(|(i, &n)| (Date::from_days(i as i64), n))
            .collect();
        let level = heat_levels(&counts);
        assert_eq!([0, 1, 2, 3, 4, 100].map(&level), [0, 1, 1, 2, 3, 4],);
    }
}
