//! Customer-visible settled usage only. No provider identity, costs, or currency estimates.
use crate::ledger::{LedgerEntry, LedgerKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettledUsage {
    /// All totals cover [window_start, window_end), not lifetime consumption.
    pub total_tokens: u64,
    pub today_points: f64,
    pub today_tokens: u64,
    pub daily: Vec<DailyUsage>,
    pub models: Vec<ModelUsage>,
    pub window_start: u64,
    pub window_end: u64,
    pub timezone: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyUsage {
    pub date: String,
    pub points: f64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelUsage {
    pub name: String,
    pub tokens: u64,
    pub points: f64,
}

pub(crate) fn window(now_secs: u64) -> (u64, u64) {
    (
        (now_secs / 86_400).saturating_sub(29) * 86_400,
        now_secs.saturating_add(1),
    )
}

pub(crate) fn aggregate<'a>(
    entries: impl Iterator<Item = &'a LedgerEntry>,
    card_id: &str,
    now_secs: u64,
) -> SettledUsage {
    let (start, end) = window(now_secs);
    let today = now_secs / 86_400;
    let mut days = BTreeMap::<u64, (i64, u64)>::new();
    let mut models = BTreeMap::<String, (i64, u64)>::new();
    let mut seen = HashSet::new();
    let mut total_tokens = 0u64;
    for entry in entries.filter(|e| {
        e.card_id == card_id && e.kind == LedgerKind::Usage && e.ts_secs >= start && e.ts_secs < end
    }) {
        // Settled invocation is the unit, not provider attempts or repeated settlement reads.
        if !seen.insert((
            entry.invocation_id.is_some(),
            entry.invocation_id.as_deref().unwrap_or(&entry.id),
        )) {
            continue;
        }
        // Ledger input_tokens already includes cache-read and cache-creation tokens.
        let tokens = entry.input_tokens.saturating_add(entry.output_tokens);
        total_tokens = total_tokens.saturating_add(tokens);
        for value in [
            days.entry(entry.ts_secs / 86_400).or_default(),
            models.entry(entry.exposed_model.clone()).or_default(),
        ] {
            value.0 = value.0.saturating_add(entry.credits_charged);
            value.1 = value.1.saturating_add(tokens);
        }
    }
    let points = |micro: i64| micro as f64 / crate::MICRO_CREDITS_PER_CREDIT as f64;
    let (today_credits, today_tokens) = days.get(&today).copied().unwrap_or_default();
    let daily = if days.is_empty() {
        Vec::new()
    } else {
        (start / 86_400..=today)
            .map(|day| {
                let (micro, tokens) = days.get(&day).copied().unwrap_or_default();
                DailyUsage {
                    date: utc_date(day),
                    points: points(micro),
                    tokens,
                }
            })
            .collect()
    };
    SettledUsage {
        total_tokens,
        today_points: points(today_credits),
        today_tokens,
        daily,
        models: models
            .into_iter()
            .map(|(name, (micro, tokens))| ModelUsage {
                name,
                tokens,
                points: points(micro),
            })
            .collect(),
        window_start: start,
        window_end: end,
        timezone: "UTC".into(),
    }
}

// Gregorian civil date from Unix days (Howard Hinnant's civil_from_days).
fn utc_date(days: u64) -> String {
    let z = days as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utc_dates_include_leap_days() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(11016), "2000-02-29");
        assert_eq!(utc_date(19782), "2024-02-29");
    }
}
