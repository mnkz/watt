use chrono::{Datelike, Days, NaiveDate};
use serde::{Deserialize, Serialize};

pub const RATE_REV: u32 = 1;
pub const RATE_LABEL: &str = "29 oct. 2026";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
    Grok,
    Agy,
    Cursor,
}

impl Provider {
    pub fn all() -> [Provider; 5] {
        [
            Provider::Claude,
            Provider::Codex,
            Provider::Grok,
            Provider::Agy,
            Provider::Cursor,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude",
            Provider::Codex => "Codex",
            Provider::Grok => "Grok",
            Provider::Agy => "AGY",
            Provider::Cursor => "Cursor",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
            Provider::Grok => "grok",
            Provider::Agy => "agy",
            Provider::Cursor => "cursor",
        }
    }

    /// Plans courants, en dollars par mois. Le premier est le défaut.
    pub fn presets(self) -> &'static [(&'static str, f64)] {
        match self {
            Provider::Claude => &[("Pro 20", 20.0), ("Max 100", 100.0), ("Max 200", 200.0)],
            Provider::Codex => &[("Plus 20", 20.0), ("Pro 100", 100.0), ("Pro 200", 200.0)],
            Provider::Grok => &[("Plus 8", 8.0), ("Premium 30", 30.0), ("Super 100", 100.0)],
            Provider::Agy => &[("Offert", 0.0), ("AI Pro 20", 20.0), ("Ultra 100", 100.0)],
            Provider::Cursor => &[("Pro 20", 20.0), ("Pro+ 60", 60.0), ("Ultra 200", 200.0)],
        }
    }

    pub fn default_abo(self) -> f64 {
        self.presets()[0].1
    }

    pub fn detail_note(self) -> Option<&'static str> {
        match self {
            Provider::Cursor => {
                Some("Date = ouverture de la session. Le modèle n’est pas écrit : tarif indicatif.")
            }
            Provider::Agy => Some("Antigravity ne publie pas les jetons dans ses fichiers locaux."),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Today,
    Yesterday,
    Week,
    Month,
    Custom,
}

impl Period {
    pub fn all() -> [Period; 5] {
        [
            Period::Today,
            Period::Yesterday,
            Period::Week,
            Period::Month,
            Period::Custom,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            Period::Today => "Aujourd’hui",
            Period::Yesterday => "Hier",
            Period::Week => "7 jours",
            Period::Month => "30 jours",
            Period::Custom => "Dates",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Period::Today => "today",
            Period::Yesterday => "yesterday",
            Period::Week => "week",
            Period::Month => "month",
            Period::Custom => "custom",
        }
    }

    pub fn from_key(key: &str) -> Option<Period> {
        Period::all().into_iter().find(|p| p.key() == key)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bucket {
    pub day: String,
    pub provider: Provider,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub cost_micros: u64,
}

impl Bucket {
    pub fn tokens(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write)
    }

    pub fn date(&self) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(&self.day, "%Y-%m-%d").ok()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub buckets: Vec<Bucket>,
    pub files: usize,
    pub calls: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Parts {
    pub fresh: u64,
    pub cache_read: u64,
    pub write_5m: u64,
    pub write_1h: u64,
    pub output: u64,
    /// Taille du prompt, pour le palier long contexte.
    pub prompt: u64,
}

impl Parts {
    pub fn is_empty(self) -> bool {
        self.fresh + self.cache_read + self.write_5m + self.write_1h + self.output == 0
    }
}

pub fn period_bounds(
    period: Period,
    today: NaiveDate,
    custom_start: NaiveDate,
    custom_end: NaiveDate,
) -> (NaiveDate, NaiveDate) {
    match period {
        Period::Today => (today, today),
        Period::Yesterday => {
            let day = today.checked_sub_days(Days::new(1)).unwrap_or(today);
            (day, day)
        }
        Period::Week => (today.checked_sub_days(Days::new(6)).unwrap_or(today), today),
        Period::Month => (
            today.checked_sub_days(Days::new(29)).unwrap_or(today),
            today,
        ),
        Period::Custom => {
            if custom_start <= custom_end {
                (custom_start, custom_end)
            } else {
                (custom_end, custom_start)
            }
        }
    }
}

pub fn day_span(start: NaiveDate, end: NaiveDate) -> i64 {
    (end - start).num_days() + 1
}

pub fn month_start(today: NaiveDate) -> NaiveDate {
    today.with_day(1).unwrap_or(today)
}

pub fn fmt_day(day: NaiveDate) -> String {
    const MONTHS: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    format!("{} {}", day.day(), MONTHS[day.month0() as usize])
}

pub fn fmt_span(start: NaiveDate, end: NaiveDate) -> String {
    if start == end {
        fmt_day(start)
    } else {
        format!("{} – {}", fmt_day(start), fmt_day(end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn week_includes_today() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let (a, b) = period_bounds(Period::Week, today, today, today);
        assert_eq!(a, NaiveDate::from_ymd_opt(2026, 10, 23).unwrap());
        assert_eq!(b, today);
        assert_eq!(day_span(a, b), 7);
    }

    #[test]
    fn month_is_thirty_days() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 29).unwrap();
        let (a, b) = period_bounds(Period::Month, today, today, today);
        assert_eq!(day_span(a, b), 30);
    }
}
