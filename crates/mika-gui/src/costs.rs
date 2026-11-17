//! UI commands of the settings page of spending: what paid provider calls
//! cost, the budget and the user's own prices (decision 0028).

use std::sync::Arc;

use chrono::{Datelike, Local, NaiveDate};
use mika_core::costs::{
    provider_host, BudgetPeriod, BudgetState, CostReport, Costs, CostsConfig, ManualPrice,
};

use crate::app::Ctx;
use crate::strings::tr;

type CtxState<'a> = tauri::State<'a, Arc<Ctx>>;

/// Currencies a budget and a price can be in.
const CURRENCIES: [&str; 3] = ["RUB", "USD", "EUR"];
/// The largest limit a budget takes.
const MAX_LIMIT: f64 = 1e9;

/// What the page shows. Every command answers with it.
#[derive(serde::Serialize)]
pub struct CostsView {
    enabled: bool,
    /// The main currency: the saved one, else the one of the LLM address.
    currency: String,
    /// False while the currency follows the address of the LLM provider.
    currency_saved: bool,
    /// The main model's provider does not tell its currency: its money is
    /// counted in the saved budget currency.
    currency_assumed: bool,
    day_limit: f64,
    month_limit: f64,
    /// With a limit reached, paid work she starts by herself waits.
    pause_unattended: bool,
    /// The limit that holds such work right now: `day`, `month` or null.
    paused: Option<BudgetPeriod>,
    budget: BudgetState,
    today: CostReport,
    /// From the first day of this month to today.
    month: CostReport,
    manual: Vec<ManualPrice>,
    /// When a price list was last read, local `DD.MM HH:MM`.
    prices_at: Option<String>,
}

/// A limit of the budget: 0 for none, at most a billion. The error is a key
/// of the locale file.
pub fn check_limit(value: f64) -> Result<f64, &'static str> {
    if value.is_finite() && (0.0..=MAX_LIMIT).contains(&value) {
        Ok(value)
    } else {
        Err("error.costs_limit")
    }
}

/// `RUB`, `USD` or `EUR`, as it is saved.
pub fn check_currency(code: &str) -> Result<String, &'static str> {
    let code = code.trim();
    if CURRENCIES.contains(&code) {
        Ok(code.to_string())
    } else {
        Err("error.costs_currency")
    }
}

/// A price as it is saved: the server name of the provider, the model, a
/// known currency, sane numbers and at least one of them above zero.
pub fn check_price(mut price: ManualPrice) -> Result<ManualPrice, &'static str> {
    price.provider = provider_host(price.provider.trim());
    price.model = price.model.trim().to_string();
    let numbers = [
        price.input_mtok,
        price.output_mtok,
        price.per_kchar,
        price.per_minute,
        price.per_image,
    ];
    // Every number is a price, and a price of zero is "not set".
    let sane = numbers.iter().all(|n| n.is_finite() && *n >= 0.0);
    let set = numbers.iter().any(|n| *n > 0.0);
    if price.provider.is_empty() || price.model.is_empty() || !sane || !set {
        return Err("error.costs_price");
    }
    price.currency = check_currency(&price.currency)?;
    Ok(price)
}

/// Puts `price` in place of the one saved for the same provider and model,
/// or at the end of the list.
pub fn put_price(prices: &mut Vec<ManualPrice>, price: ManualPrice) {
    let same = |p: &ManualPrice| p.provider == price.provider && p.model == price.model;
    let at = prices.iter().position(same);
    // A file edited by hand may hold the pair twice: one entry stays.
    prices.retain(|p| !same(p));
    match at {
        Some(at) => prices.insert(at, price),
        None => prices.push(price),
    }
}

/// The page as of `today`, from the tracker and the saved settings.
fn view_of(costs: &Costs, cfg: CostsConfig, today: NaiveDate) -> Result<CostsView, String> {
    let first = today.with_day(1).unwrap_or(today);
    let failed = |e: mika_core::storage::DbError| e.to_string();
    Ok(CostsView {
        enabled: cfg.enabled,
        currency: costs.currency(),
        currency_saved: !cfg.currency.is_empty(),
        currency_assumed: costs.assumes_currency(),
        day_limit: cfg.day_limit,
        month_limit: cfg.month_limit,
        pause_unattended: cfg.pause_unattended,
        paused: costs.pause(today).map_err(failed)?.map(|p| p.period),
        budget: costs.budget(today).map_err(failed)?,
        today: costs.report(today, today).map_err(failed)?,
        month: costs.report(first, today).map_err(failed)?,
        manual: cfg.prices,
        prices_at: costs
            .prices_at()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .map(|t| t.with_timezone(&Local).format("%d.%m %H:%M").to_string()),
    })
}

/// Reads the base: call it off the main thread.
fn view(ctx: &Ctx) -> Result<CostsView, String> {
    let cfg = ctx.config.lock().costs.clone();
    view_of(&ctx.costs, cfg, Local::now().date_naive())
}

async fn fresh(ctx: Arc<Ctx>) -> Result<CostsView, String> {
    tauri::async_runtime::spawn_blocking(move || view(&ctx))
        .await
        .map_err(|e| e.to_string())?
}

/// Changes the `[costs]` settings, saves them and tells the tracker. Off the
/// main thread: a new price is applied to the calls that waited for one.
pub(crate) async fn change(
    ctx: Arc<Ctx>,
    edit: impl FnOnce(&mut CostsConfig) + Send + 'static,
) -> Result<CostsView, String> {
    tauri::async_runtime::spawn_blocking(move || {
        edit(&mut ctx.config.lock().costs);
        let saved = ctx.save_config();
        // The tracker follows the settings in memory, also when the file was
        // not written: the page and the counting must not disagree.
        ctx.apply_cost_settings();
        saved?;
        view(&ctx)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn get_costs(ctx: CtxState<'_>) -> Result<CostsView, String> {
    fresh(ctx.inner().clone()).await
}

/// Counting on or off. What was counted stays until it is wiped.
#[tauri::command]
pub async fn set_costs_enabled(ctx: CtxState<'_>, value: bool) -> Result<CostsView, String> {
    change(ctx.inner().clone(), move |c| c.enabled = value).await
}

/// Whether a reached limit holds back the paid work she starts by herself.
/// What the user asks for is never held.
#[tauri::command]
pub async fn set_cost_pause(ctx: CtxState<'_>, value: bool) -> Result<CostsView, String> {
    change(ctx.inner().clone(), move |c| c.pause_unattended = value).await
}

/// The limits for a day and a month (0 is no limit) and their currency. From
/// now on the currency no longer follows the address of the provider.
#[tauri::command]
pub async fn set_cost_budget(
    ctx: CtxState<'_>,
    day_limit: f64,
    month_limit: f64,
    currency: String,
) -> Result<CostsView, String> {
    let day_limit = check_limit(day_limit).map_err(tr)?;
    let month_limit = check_limit(month_limit).map_err(tr)?;
    let currency = check_currency(&currency).map_err(tr)?;
    change(ctx.inner().clone(), move |c| {
        c.day_limit = day_limit;
        c.month_limit = month_limit;
        c.currency = currency;
    })
    .await
}

/// The user's own price of a model, in place of the one saved for it.
#[tauri::command]
pub async fn set_cost_price(ctx: CtxState<'_>, price: ManualPrice) -> Result<CostsView, String> {
    let price = check_price(price).map_err(tr)?;
    change(ctx.inner().clone(), move |c| {
        put_price(&mut c.prices, price)
    })
    .await
}

#[tauri::command]
pub async fn remove_cost_price(
    ctx: CtxState<'_>,
    provider: String,
    model: String,
) -> Result<CostsView, String> {
    let (provider, model) = (provider.trim().to_string(), model.trim().to_string());
    change(ctx.inner().clone(), move |c| {
        c.prices
            .retain(|p| !(p.provider == provider && p.model == model));
    })
    .await
}

/// Reads the price lists of the providers in use now, not waiting for the
/// next paid call.
#[tauri::command]
pub async fn refresh_cost_prices(ctx: CtxState<'_>) -> Result<CostsView, String> {
    let ctx = ctx.inner().clone();
    ctx.refresh_cost_prices()
        .await
        .map_err(|e| tr("error.costs_prices").replace("{why}", &e))?;
    fresh(ctx).await
}

/// Wipes what was counted. The prices stay.
#[tauri::command]
pub async fn clear_costs(ctx: CtxState<'_>) -> Result<CostsView, String> {
    let ctx = ctx.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        ctx.costs.clear().map_err(|e| e.to_string())?;
        view(&ctx)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn price(provider: &str, model: &str) -> ManualPrice {
        ManualPrice {
            provider: provider.into(),
            model: model.into(),
            currency: "USD".into(),
            per_minute: 0.006,
            ..Default::default()
        }
    }

    #[test]
    fn a_limit_is_a_number_from_zero_to_a_billion() {
        assert_eq!(check_limit(0.0), Ok(0.0));
        assert_eq!(check_limit(99.5), Ok(99.5));
        assert_eq!(check_limit(1e9), Ok(1e9));
        for bad in [-0.01, 1e9 + 1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(check_limit(bad), Err("error.costs_limit"), "{bad}");
        }
    }

    #[test]
    fn a_currency_is_one_of_three() {
        assert_eq!(check_currency("RUB"), Ok("RUB".to_string()));
        assert_eq!(check_currency(" EUR "), Ok("EUR".to_string()));
        for bad in ["", "rub", "GBP", "₽", "USD;"] {
            assert_eq!(check_currency(bad), Err("error.costs_currency"), "{bad}");
        }
    }

    #[test]
    fn a_price_is_saved_under_the_server_name() {
        let typed = ManualPrice {
            provider: " https://key@API.OpenAI.com/v1?x=1 ".into(),
            model: " whisper-1 ".into(),
            ..price("", "")
        };
        let saved = check_price(typed).unwrap();
        assert_eq!(saved.provider, "api.openai.com");
        assert_eq!(saved.model, "whisper-1");
        assert_eq!(saved.currency, "USD");
        assert_eq!(saved.per_minute, 0.006);
    }

    #[test]
    fn a_price_needs_a_model_a_number_above_zero_and_a_currency() {
        let good = price("api.openai.com", "whisper-1");
        assert!(check_price(good.clone()).is_ok());
        let bad = |edit: fn(&mut ManualPrice)| {
            let mut p = good.clone();
            edit(&mut p);
            check_price(p)
        };
        assert_eq!(bad(|p| p.provider = "  ".into()), Err("error.costs_price"));
        assert_eq!(bad(|p| p.model = String::new()), Err("error.costs_price"));
        // Every price is zero: nothing is set.
        assert_eq!(bad(|p| p.per_minute = 0.0), Err("error.costs_price"));
        assert_eq!(bad(|p| p.input_mtok = -1.0), Err("error.costs_price"));
        assert_eq!(bad(|p| p.per_image = f64::NAN), Err("error.costs_price"));
        assert_eq!(
            bad(|p| p.output_mtok = f64::INFINITY),
            Err("error.costs_price")
        );
        assert_eq!(
            bad(|p| p.currency = "GBP".into()),
            Err("error.costs_currency")
        );
        assert_eq!(
            bad(|p| p.currency = String::new()),
            Err("error.costs_currency")
        );
    }

    #[test]
    fn a_new_price_takes_the_place_of_the_old_one() {
        let mut prices = vec![price("routerai.ru", "a"), price("api.openai.com", "a")];
        // The same model at another provider is another price.
        let mut next = price("api.openai.com", "a");
        next.per_minute = 0.01;
        put_price(&mut prices, next.clone());
        assert_eq!(prices, [price("routerai.ru", "a"), next.clone()]);
        put_price(&mut prices, price("api.openai.com", "b"));
        assert_eq!(
            prices,
            [
                price("routerai.ru", "a"),
                next,
                price("api.openai.com", "b")
            ]
        );
        // A pair written twice by hand becomes one entry, where the first stood.
        let mut twice = vec![price("x", "m"), price("y", "m"), price("x", "m")];
        put_price(&mut twice, price("x", "m"));
        assert_eq!(twice, [price("x", "m"), price("y", "m")]);
    }

    #[test]
    fn the_view_has_what_the_page_reads() {
        use mika_core::costs::{CostKind, PaidCall, Tokens};
        let db = Arc::new(mika_core::storage::Db::open_in_memory().unwrap());
        let costs = Costs::new(db);
        let cfg = CostsConfig {
            day_limit: 100.0,
            month_limit: 2000.0,
            prices: vec![price("api.openai.com", "whisper-1")],
            ..Default::default()
        };
        // The currency is not saved: it follows the address of the LLM provider.
        costs.set_settings(&cfg, "RUB");
        let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let call = PaidCall::new(CostKind::Chat, "https://routerai.ru/api/v1", "gemma")
            .tokens(Tokens {
                input: 1000,
                cached: 0,
                output: 200,
            })
            .cost(Some(12.4));
        costs.record_at(day("2026-11-14"), call.clone());
        costs.record_at(day("2026-11-03"), call.clone());
        costs.record_at(day("2026-10-31"), call);

        let view = view_of(&costs, cfg, day("2026-11-14")).unwrap();
        let v = serde_json::to_value(view).unwrap();
        assert_eq!(v["enabled"], true);
        assert_eq!(v["currency"], "RUB");
        assert_eq!(v["currency_saved"], false);
        assert_eq!(v["currency_assumed"], false);
        assert_eq!(v["today"]["free"], false);
        assert_eq!(v["day_limit"], 100.0);
        assert_eq!(v["month_limit"], 2000.0);
        // The pause is on by default and nothing is held at 12% of the day.
        assert_eq!(v["pause_unattended"], true);
        assert!(v["paused"].is_null());
        // Today, and this month from its first day: October is not in.
        assert_eq!(v["today"]["from"], "2026-11-14");
        assert_eq!(v["today"]["to"], "2026-11-14");
        assert_eq!(v["month"]["from"], "2026-11-01");
        assert_eq!(v["month"]["to"], "2026-11-14");
        assert_eq!(v["today"]["requests"], 1);
        assert_eq!(v["month"]["requests"], 2);
        assert_eq!(v["today"]["totals"][0]["currency"], "RUB");
        assert_eq!(v["today"]["totals"][0]["text"], "12,40\u{a0}₽");
        assert_eq!(v["budget"]["day_share"], 12);
        assert_eq!(v["budget"]["day_text"], "12,40\u{a0}₽ из 100\u{a0}₽");
        assert_eq!(v["budget"]["month_limit"], 2000.0);
        // A line as the page reads it.
        let line = &v["today"]["models"][0];
        assert_eq!(line["kind"], "chat");
        assert_eq!(line["provider"], "routerai.ru");
        assert_eq!(line["model"], "gemma");
        assert_eq!(line["requests"], 1);
        assert_eq!(line["unpriced"], 0);
        assert_eq!(line["blind"], 0);
        assert_eq!(line["free"], false);
        assert!(line["units"].is_string() && line["cost"].is_array());
        assert!(["list", "manual", "none"].contains(&line["price"].as_str().unwrap()));
        assert_eq!(v["today"]["kinds"][0]["kind"], "chat");
        // The user's own prices travel with their serde names.
        assert_eq!(v["manual"][0]["provider"], "api.openai.com");
        assert_eq!(v["manual"][0]["per_minute"], 0.006);
        assert!(v["manual"][0]["input_mtok"].is_number());
        assert!(v["prices_at"].is_null());
    }

    #[test]
    fn the_view_names_the_limit_that_holds_her_own_paid_work() {
        use mika_core::costs::{CostKind, PaidCall, Tokens};
        let db = Arc::new(mika_core::storage::Db::open_in_memory().unwrap());
        let costs = Costs::new(db);
        let mut cfg = CostsConfig {
            day_limit: 10.0,
            month_limit: 20.0,
            ..Default::default()
        };
        costs.set_settings(&cfg, "RUB");
        let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let call = PaidCall::new(CostKind::Chat, "https://routerai.ru/api/v1", "gemma")
            .tokens(Tokens::default())
            .cost(Some(10.0));
        costs.record_at(day("2026-11-13"), call.clone());
        let paused = |cfg: &CostsConfig, at: &str| {
            let view = view_of(&costs, cfg.clone(), day(at)).unwrap();
            serde_json::to_value(view).unwrap()["paused"].clone()
        };
        assert_eq!(paused(&cfg, "2026-11-13"), "day");
        assert!(paused(&cfg, "2026-11-14").is_null());
        costs.record_at(day("2026-11-14"), call);
        assert_eq!(paused(&cfg, "2026-11-14"), "month");
        // Only looking at the page tells nobody and marks nothing as told.
        let told = Arc::new(parking_lot::Mutex::new(0));
        let seen = told.clone();
        costs.on_pause(Arc::new(move |_| *seen.lock() += 1));
        assert_eq!(paused(&cfg, "2026-11-14"), "month");
        assert_eq!(*told.lock(), 0);
        assert!(!costs.unattended_allowed_at(day("2026-11-14")));
        assert_eq!(*told.lock(), 1);
        // Switched off by the user: the limits only warn.
        cfg.pause_unattended = false;
        costs.set_settings(&cfg, "RUB");
        let view = view_of(&costs, cfg, day("2026-11-14")).unwrap();
        let v = serde_json::to_value(view).unwrap();
        assert_eq!(v["pause_unattended"], false);
        assert!(v["paused"].is_null());
    }

    #[test]
    fn the_texts_of_the_errors_exist() {
        for key in [
            "error.costs_limit",
            "error.costs_currency",
            "error.costs_price",
            "error.costs_prices",
            "settings.page.costs",
        ] {
            assert_ne!(tr(key), key, "{key} is missing in ru.json");
        }
        assert!(tr("error.costs_prices").contains("{why}"));
    }
}
