//! The tables of the tracker: `cost_usage`, `cost_prices`, `cost_alerts`.

use std::collections::HashSet;

use rusqlite::{params, Connection, OptionalExtension};

use super::UnitPrices;

/// One row of `cost_usage`: a day, a kind, a model, priced or not, one currency.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Row {
    pub day: String,
    pub kind: String,
    pub provider: String,
    pub model: String,
    /// The money of these calls is known and stands in `cost`.
    pub priced: bool,
    pub currency: String,
    pub requests: u64,
    pub blind: u64,
    pub tokens_in: u64,
    pub tokens_cached: u64,
    pub tokens_out: u64,
    pub chars: u64,
    pub seconds: f64,
    pub images: u64,
    pub cost: f64,
}

const COLUMNS: &str = "day, kind, provider, model, priced, currency, requests, blind, \
tokens_in, tokens_cached, tokens_out, chars, seconds, images, cost";

fn read(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let count = |i: usize| r.get::<_, i64>(i).map(|v| v.max(0) as u64);
    Ok(Row {
        day: r.get(0)?,
        kind: r.get(1)?,
        provider: r.get(2)?,
        model: r.get(3)?,
        priced: r.get::<_, i64>(4)? != 0,
        currency: r.get(5)?,
        requests: count(6)?,
        blind: count(7)?,
        tokens_in: count(8)?,
        tokens_cached: count(9)?,
        tokens_out: count(10)?,
        chars: count(11)?,
        seconds: r.get(12)?,
        images: count(13)?,
        cost: r.get(14)?,
    })
}

/// Adds the numbers of `row` to the stored row with the same key.
pub(super) fn add(c: &Connection, row: &Row) -> rusqlite::Result<()> {
    c.execute(
        &format!(
            "INSERT INTO cost_usage ({COLUMNS})
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(day, kind, provider, model, priced, currency) DO UPDATE SET
                requests = requests + excluded.requests,
                blind = blind + excluded.blind,
                tokens_in = tokens_in + excluded.tokens_in,
                tokens_cached = tokens_cached + excluded.tokens_cached,
                tokens_out = tokens_out + excluded.tokens_out,
                chars = chars + excluded.chars,
                seconds = seconds + excluded.seconds,
                images = images + excluded.images,
                cost = cost + excluded.cost"
        ),
        params![
            row.day,
            row.kind,
            row.provider,
            row.model,
            row.priced as i64,
            row.currency,
            row.requests as i64,
            row.blind as i64,
            row.tokens_in as i64,
            row.tokens_cached as i64,
            row.tokens_out as i64,
            row.chars as i64,
            row.seconds,
            row.images as i64,
            row.cost,
        ],
    )
    .map(|_| ())
}

pub(super) fn delete(c: &Connection, row: &Row) -> rusqlite::Result<()> {
    c.execute(
        "DELETE FROM cost_usage WHERE day = ?1 AND kind = ?2 AND provider = ?3 AND model = ?4
         AND priced = ?5 AND currency = ?6",
        params![
            row.day,
            row.kind,
            row.provider,
            row.model,
            row.priced as i64,
            row.currency
        ],
    )
    .map(|_| ())
}

/// Rows of the days `from..=to`, as `YYYY-MM-DD`.
pub(super) fn rows(c: &Connection, from: &str, to: &str) -> rusqlite::Result<Vec<Row>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLUMNS} FROM cost_usage WHERE day >= ?1 AND day <= ?2
         ORDER BY day, kind, provider, model, priced, currency"
    ))?;
    let rows = st.query_map(params![from, to], read)?;
    rows.collect()
}

/// Rows without money that have units to price, of one provider or of all.
pub(super) fn unpriced(c: &Connection, provider: Option<&str>) -> rusqlite::Result<Vec<Row>> {
    let mut st = c.prepare(&format!(
        "SELECT {COLUMNS} FROM cost_usage
         WHERE priced = 0 AND requests > blind AND (?1 IS NULL OR provider = ?1)
         ORDER BY day, kind, provider, model"
    ))?;
    let rows = st.query_map(params![provider], read)?;
    rows.collect()
}

/// Money of the priced rows of the days `from..=to` in one currency. With
/// `fold` the money without a currency is counted in too.
pub(super) fn spent(
    c: &Connection,
    from: &str,
    to: &str,
    currency: &str,
    fold: bool,
) -> rusqlite::Result<f64> {
    c.query_row(
        "SELECT COALESCE(SUM(cost), 0.0) FROM cost_usage
         WHERE priced = 1 AND (currency = ?3 OR (?4 AND currency = ''))
           AND day >= ?1 AND day <= ?2",
        params![from, to, currency, fold],
        |r| r.get(0),
    )
}

/// Models of a provider that were ever called.
pub(super) fn used_models(c: &Connection, provider: &str) -> rusqlite::Result<HashSet<String>> {
    let mut st = c.prepare("SELECT DISTINCT model FROM cost_usage WHERE provider = ?1")?;
    let rows = st.query_map([provider], |r| r.get::<_, String>(0))?;
    rows.collect()
}

/// Deletes spending older than `edge` and the alerts of periods before
/// `day_period` and `month_period`.
pub(super) fn prune(
    c: &Connection,
    edge: &str,
    day_period: &str,
    month_period: &str,
) -> rusqlite::Result<()> {
    c.execute("DELETE FROM cost_usage WHERE day < ?1", [edge])?;
    c.execute(
        "DELETE FROM cost_alerts WHERE (period LIKE 'day:%' AND period < ?1)
         OR (period LIKE 'month:%' AND period < ?2)
         OR (period LIKE 'pause:day:%' AND period < 'pause:' || ?1)
         OR (period LIKE 'pause:month:%' AND period < 'pause:' || ?2)",
        params![day_period, month_period],
    )?;
    Ok(())
}

/// The highest warning already shown for a period: 0, 80 or 100.
pub(super) fn alert_level(c: &Connection, period: &str) -> rusqlite::Result<u8> {
    let level: Option<i64> = c
        .query_row(
            "SELECT level FROM cost_alerts WHERE period = ?1",
            [period],
            |r| r.get(0),
        )
        .optional()?;
    Ok(level.unwrap_or(0).clamp(0, 100) as u8)
}

pub(super) fn set_alert_level(c: &Connection, period: &str, level: u8) -> rusqlite::Result<()> {
    if level == 0 {
        return c
            .execute("DELETE FROM cost_alerts WHERE period = ?1", [period])
            .map(|_| ());
    }
    c.execute(
        "INSERT INTO cost_alerts (period, level) VALUES (?1, ?2)
         ON CONFLICT(period) DO UPDATE SET level = excluded.level",
        params![period, level as i64],
    )
    .map(|_| ())
}

/// Stored list prices: (provider, model, prices, unix time of the read).
pub(super) fn load_prices(
    c: &Connection,
) -> rusqlite::Result<Vec<(String, String, UnitPrices, i64)>> {
    let mut st = c.prepare("SELECT provider, model, prices, fetched_at FROM cost_prices")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (provider, model, json, at) = row?;
        // A row that no longer parses is as good as no price.
        if let Ok(prices) = serde_json::from_str::<UnitPrices>(&json) {
            out.push((provider, model, prices, at));
        }
    }
    Ok(out)
}

pub(super) fn save_price(
    c: &Connection,
    provider: &str,
    model: &str,
    prices: &UnitPrices,
    at: i64,
) -> rusqlite::Result<()> {
    let json = serde_json::to_string(prices).unwrap_or_else(|_| "{}".into());
    c.execute(
        "INSERT INTO cost_prices (provider, model, prices, fetched_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(provider, model) DO UPDATE SET
            prices = excluded.prices, fetched_at = excluded.fetched_at",
        params![provider, model, json, at],
    )
    .map(|_| ())
}

pub(super) fn delete_prices(c: &Connection, provider: &str) -> rusqlite::Result<()> {
    c.execute("DELETE FROM cost_prices WHERE provider = ?1", [provider])
        .map(|_| ())
}

/// Empties the spending and the shown warnings. Prices stay.
pub(super) fn clear(c: &Connection) -> rusqlite::Result<()> {
    c.execute("DELETE FROM cost_usage", [])?;
    c.execute("DELETE FROM cost_alerts", [])?;
    Ok(())
}
