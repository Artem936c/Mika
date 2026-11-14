//! The tracker: counts paid calls, prices them, watches the budget.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate};
use parking_lot::{Mutex, RwLock};
use rusqlite::Connection;
use tokio::sync::Notify;

use super::store::{self, Row};
use super::{
    currency_code, is_local, money_text, provider_host, units_text, BudgetAlert, BudgetPause,
    BudgetPeriod, BudgetState, CostKind, CostReport, CostsConfig, Money, PaidCall, PriceSource,
    Tokens, UnitPrices, UsageLine, KEEP_DAYS, PRICES_RETRY_S, PRICES_TTL_S,
};
use crate::storage::{Db, DbError};

/// Called when the spending crosses 80% or 100% of a limit.
pub type AlertFn = Arc<dyn Fn(BudgetAlert) + Send + Sync>;
/// Called once per period, when paid work of her own is first held back.
pub type PauseFn = Arc<dyn Fn(BudgetPause) + Send + Sync>;

/// The tracker of paid calls. Cloning gives another handle to the same one.
#[derive(Clone)]
pub struct Costs(Arc<Inner>);

struct Inner {
    db: Arc<Db>,
    /// Never held while the database is asked for; the other order is fine.
    state: Mutex<State>,
    alert: RwLock<Option<AlertFn>>,
    pause: RwLock<Option<PauseFn>>,
    /// Wakes `wanted()`.
    wake: Notify,
}

struct State {
    cfg: CostsConfig,
    /// The currency of the LLM provider's address; used while `cfg.currency` is empty.
    auto_currency: String,
    /// List prices by server name.
    book: HashMap<String, Book>,
    /// Addresses whose price list should be read.
    wanted: Vec<String>,
    /// The day old rows were last deleted on.
    pruned: Option<NaiveDate>,
}

/// What is known of one provider's price list.
#[derive(Default)]
struct Book {
    prices: HashMap<String, Listed>,
    /// Unix time of the last good read.
    read_at: Option<i64>,
    /// Unix time of the last failed read.
    failed_at: Option<i64>,
    /// A read was asked for and has not come back yet.
    asked_at: Option<i64>,
    /// `prices` holds the whole list, read while this program runs. A book
    /// restored from the base holds only the models used before: a model
    /// missing in it may still be in the provider's list.
    complete: bool,
}

struct Listed {
    prices: UnitPrices,
    /// The price has its row in `cost_prices`.
    stored: bool,
}

impl Book {
    /// Whether the list should be read now. `missing`: a call has just found
    /// no price; a book that is not the whole list is then read again, not
    /// after its day is over.
    fn needs_read(&self, now: i64, missing: bool) -> bool {
        if let Some(asked) = self.asked_at {
            return now - asked > PRICES_RETRY_S;
        }
        match (self.read_at, self.failed_at) {
            (None, None) => true,
            (read, Some(failed)) if read.is_none_or(|r| failed >= r) => {
                now - failed > PRICES_RETRY_S
            }
            (Some(read), _) => now - read > PRICES_TTL_S || (missing && !self.complete),
            (None, Some(failed)) => now - failed > PRICES_RETRY_S,
        }
    }
}

/// The money found for a call.
struct Found {
    cost: f64,
    currency: String,
    /// A list price that has no row in `cost_prices` yet.
    unsaved: Option<UnitPrices>,
}

impl State {
    fn currency(&self) -> String {
        let own = self.cfg.currency.trim();
        if own.is_empty() {
            self.auto_currency.clone()
        } else {
            own.to_string()
        }
    }

    /// Money whose currency is unknown is counted in the budget currency.
    /// Only while the main model's provider is an address that does not tell
    /// its currency and the user saved one: by saving it they said what that
    /// provider counts in. Otherwise such money stays apart, without a sign.
    fn folds_unknown(&self) -> bool {
        self.auto_currency.is_empty() && !self.cfg.currency.trim().is_empty()
    }

    fn listed(&self, provider: &str, model: &str) -> Option<&Listed> {
        self.book
            .get(provider)?
            .prices
            .get(model)
            .filter(|l| !l.prices.is_empty())
    }

    /// The price the user typed in for a model, with its currency.
    fn manual(&self, provider: &str, model: &str) -> Option<(UnitPrices, String)> {
        self.cfg
            .prices
            .iter()
            .filter(|m| provider_host(&m.provider) == provider && m.model.trim() == model)
            .map(|m| (UnitPrices::from_manual(m), m.currency.trim().to_string()))
            .find(|(prices, _)| !prices.is_empty())
    }

    /// Money of `requests` calls summed up in `call`, by the list price and
    /// then by the typed one.
    fn by_price(&self, call: &PaidCall, provider: &str, requests: u64) -> Option<Found> {
        let model = call.model.trim();
        if let Some(listed) = self.listed(provider, model) {
            if let Some(cost) = listed.prices.cost_n(call, requests) {
                return Some(Found {
                    cost,
                    currency: currency_code(provider).to_string(),
                    unsaved: (!listed.stored).then_some(listed.prices),
                });
            }
        }
        let (prices, currency) = self.manual(provider, model)?;
        prices.cost_n(call, requests).map(|cost| Found {
            cost,
            currency,
            unsaved: None,
        })
    }

    /// The money of one call, by the first source that knows it.
    fn price(&self, call: &PaidCall, provider: &str) -> Option<Found> {
        if let Some(cost) = call.cost.filter(|c| c.is_finite() && *c >= 0.0) {
            return Some(Found {
                cost,
                currency: currency_code(&call.base_url).to_string(),
                unsaved: None,
            });
        }
        if call.free || is_local(provider) {
            return Some(Found {
                cost: 0.0,
                currency: String::new(),
                unsaved: None,
            });
        }
        if call.blind() {
            return None;
        }
        self.by_price(call, provider, 1)
    }
}

fn day_text(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn month_text(d: NaiveDate) -> String {
    d.format("%Y-%m").to_string()
}

/// 0, 80 or 100: the threshold of the limit the spending has reached.
fn level(spent: f64, limit: f64) -> u8 {
    const EPS: f64 = 1e-9;
    if limit <= 0.0 {
        0
    } else if spent + EPS >= limit {
        100
    } else if spent + EPS >= 0.8 * limit {
        80
    } else {
        0
    }
}

/// One budget period of a day: its key in `cost_alerts` and its days.
struct Period {
    kind: BudgetPeriod,
    key: String,
    from: String,
    to: String,
    limit: f64,
}

fn periods(day: NaiveDate, cfg: &CostsConfig) -> [Period; 2] {
    let month = month_text(day);
    [
        Period {
            kind: BudgetPeriod::Day,
            key: format!("day:{}", day_text(day)),
            from: day_text(day),
            to: day_text(day),
            limit: cfg.day_limit,
        },
        Period {
            kind: BudgetPeriod::Month,
            key: format!("month:{month}"),
            from: format!("{month}-01"),
            to: format!("{month}-31"),
            limit: cfg.month_limit,
        },
    ]
}

/// The key in `cost_alerts` under which a period remembers that the user was
/// The period as the journal names it.
fn period_name(period: BudgetPeriod) -> &'static str {
    match period {
        BudgetPeriod::Day => "day",
        BudgetPeriod::Month => "month",
    }
}

/// told about its pause.
fn pause_key(period: &Period) -> String {
    format!("pause:{}", period.key)
}

/// Sums of the rows of one report line.
struct Sum {
    kind: CostKind,
    provider: String,
    model: String,
    requests: u64,
    blind: u64,
    unpriced: u64,
    priced_requests: u64,
    tokens_in: u64,
    tokens_cached: u64,
    tokens_out: u64,
    chars: u64,
    seconds: f64,
    images: u64,
    /// Money by currency.
    money: Vec<(String, f64)>,
    /// Money without a currency goes into this one (`State::folds_unknown`).
    fold: Option<String>,
}

impl Sum {
    fn new(kind: CostKind, provider: &str, model: &str, fold: Option<&str>) -> Self {
        Self {
            fold: fold.map(str::to_string),
            kind,
            provider: provider.to_string(),
            model: model.to_string(),
            requests: 0,
            blind: 0,
            unpriced: 0,
            priced_requests: 0,
            tokens_in: 0,
            tokens_cached: 0,
            tokens_out: 0,
            chars: 0,
            seconds: 0.0,
            images: 0,
            money: Vec::new(),
        }
    }

    fn add(&mut self, r: &Row) {
        self.requests += r.requests;
        self.blind += r.blind;
        if r.priced {
            self.priced_requests += r.requests;
            let currency = match &self.fold {
                Some(main) if r.currency.is_empty() => main,
                _ => &r.currency,
            };
            match self.money.iter_mut().find(|(c, _)| c == currency) {
                Some(bucket) => bucket.1 += r.cost,
                None => self.money.push((currency.clone(), r.cost)),
            }
        } else {
            self.unpriced += r.requests.saturating_sub(r.blind);
        }
        self.tokens_in += r.tokens_in;
        self.tokens_cached += r.tokens_cached;
        self.tokens_out += r.tokens_out;
        self.chars += r.chars;
        self.seconds += r.seconds;
        self.images += r.images;
    }

    fn in_currency(&self, currency: &str) -> f64 {
        self.money
            .iter()
            .find(|(c, _)| c == currency)
            .map_or(0.0, |(_, cost)| *cost)
    }

    /// Non-zero money, the main currency first, the rest by name.
    fn money(&self, main: &str) -> Vec<Money> {
        let mut buckets: Vec<&(String, f64)> =
            self.money.iter().filter(|(_, cost)| *cost > 0.0).collect();
        buckets.sort_by(|a, b| (a.0 != main, &a.0).cmp(&(b.0 != main, &b.0)));
        buckets
            .into_iter()
            .map(|(currency, cost)| Money::new(*cost, currency))
            .collect()
    }

    /// Every request has its money and the money is zero. A request of
    /// unknown size made to a free provider (a stopped Ollama stream) is
    /// still free: only a request that waits for a price is not.
    fn free(&self) -> bool {
        self.requests > 0
            && self.priced_requests == self.requests
            && self.money.iter().all(|(_, cost)| *cost <= 0.0)
    }

    fn line(&self, main: &str, price: PriceSource) -> UsageLine {
        let cost = self.money(main);
        UsageLine {
            kind: self.kind,
            provider: self.provider.clone(),
            model: self.model.clone(),
            requests: self.requests,
            blind: self.blind,
            unpriced: self.unpriced,
            tokens_in: self.tokens_in,
            tokens_cached: self.tokens_cached,
            tokens_out: self.tokens_out,
            chars: self.chars,
            seconds: self.seconds,
            images: self.images,
            units: units_text(
                self.kind,
                self.tokens_in + self.tokens_cached + self.tokens_out,
                self.chars,
                self.seconds,
                self.images,
            ),
            free: self.free(),
            cost,
            price,
        }
    }
}

impl std::fmt::Debug for Costs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Costs")
    }
}

impl Costs {
    /// A tracker over `db` with the default settings. The list prices of the
    /// models used before are read from the base, so the first call after a
    /// start is priced at once.
    pub fn new(db: Arc<Db>) -> Costs {
        let mut book: HashMap<String, Book> = HashMap::new();
        match db.with(store::load_prices) {
            Ok(rows) => {
                for (provider, model, prices, at) in rows {
                    let b = book.entry(provider).or_default();
                    b.read_at = Some(b.read_at.map_or(at, |r| r.max(at)));
                    b.prices.insert(
                        model,
                        Listed {
                            prices,
                            stored: true,
                        },
                    );
                }
            }
            Err(e) => log::warn!("stored prices not read, calls wait for a price list: {e}"),
        }
        Costs(Arc::new(Inner {
            db,
            state: Mutex::new(State {
                cfg: CostsConfig::default(),
                auto_currency: String::new(),
                book,
                wanted: Vec::new(),
                pruned: None,
            }),
            alert: RwLock::new(None),
            pause: RwLock::new(None),
            wake: Notify::new(),
        }))
    }

    /// Takes the settings and the currency of the LLM provider's address.
    /// Calls that waited for a price get one if a typed price now covers them.
    /// The budget warnings are set to where the spending stands now, without
    /// a warning: a changed limit does not speak up by itself.
    pub fn set_settings(&self, cfg: &CostsConfig, auto_currency: &str) {
        {
            let mut s = self.0.state.lock();
            s.cfg = cfg.clone();
            s.auto_currency = auto_currency.trim().to_string();
        }
        if let Err(e) = self.reprice(None) {
            log::warn!("unpriced calls not repriced: {e}");
        }
        if let Err(e) = self.rearm(Local::now().date_naive()) {
            log::warn!("budget warnings not reset: {e}");
        }
    }

    pub fn settings(&self) -> CostsConfig {
        self.0.state.lock().cfg.clone()
    }

    pub fn enabled(&self) -> bool {
        self.0.state.lock().cfg.enabled
    }

    /// The currency of the budget: the saved one, else that of the LLM provider.
    pub fn currency(&self) -> String {
        self.0.state.lock().currency()
    }

    /// Money whose currency is unknown is counted in the budget currency:
    /// the main model's provider does not tell its currency, and the user
    /// saved one for the budget.
    pub fn assumes_currency(&self) -> bool {
        self.0.state.lock().folds_unknown()
    }

    /// Counts a paid call made today. Never fails: a database error is logged.
    pub fn record(&self, call: PaidCall) {
        self.record_at(Local::now().date_naive(), call);
    }

    /// Counts a paid call made on `day`.
    pub fn record_at(&self, day: NaiveDate, call: PaidCall) {
        match self.count(day, &call) {
            Ok(alerts) => self.fire(alerts),
            Err(e) => log::warn!(
                "paid call not counted ({}, {}, {}): {e}",
                call.kind.as_str(),
                provider_host(&call.base_url),
                call.model.trim()
            ),
        }
    }

    fn fire(&self, alerts: Vec<BudgetAlert>) {
        if alerts.is_empty() {
            return;
        }
        for alert in &alerts {
            log::warn!(
                "budget warning: period={} level={} spent={:.2} limit={:.2} currency={}",
                period_name(alert.period),
                alert.level,
                alert.spent,
                alert.limit,
                alert.currency
            );
        }
        let Some(tell) = self.0.alert.read().clone() else {
            return;
        };
        for alert in alerts {
            tell(alert);
        }
    }

    fn count(&self, day: NaiveDate, call: &PaidCall) -> Result<Vec<BudgetAlert>, DbError> {
        let provider = provider_host(&call.base_url);
        let model = call.model.trim().to_string();
        let now = Local::now().timestamp();
        let (found, listed_at, main, fold, cfg, prune, wake) = {
            let mut guard = self.0.state.lock();
            let s = &mut *guard;
            if !s.cfg.enabled {
                return Ok(Vec::new());
            }
            let found = s.price(call, &provider);
            // When the list this price came from was read.
            let listed_at = s.book.get(&provider).and_then(|b| b.read_at);
            // A paid provider whose price list is unread or old is asked for
            // it. So is one whose list is known only in part (after a
            // restart), when this call found no price in that part.
            let mut wake = false;
            let missing = found.is_none() && !call.blind();
            if !call.free && !provider.is_empty() && !is_local(&provider) {
                let book = s.book.entry(provider.clone()).or_default();
                if book.needs_read(now, missing) {
                    book.asked_at = Some(now);
                    let address = call.base_url.trim().trim_end_matches('/').to_string();
                    if !s.wanted.contains(&address) {
                        s.wanted.push(address);
                    }
                    wake = true;
                }
            }
            let prune = s.pruned != Some(day);
            s.pruned = Some(day);
            (
                found,
                listed_at,
                s.currency(),
                s.folds_unknown(),
                s.cfg.clone(),
                prune,
                wake,
            )
        };
        if wake {
            self.0.wake.notify_one();
        }

        let tokens = call.tokens.unwrap_or_default();
        let row = Row {
            day: day_text(day),
            kind: call.kind.as_str().to_string(),
            provider: provider.clone(),
            model: model.clone(),
            priced: found.is_some(),
            currency: found
                .as_ref()
                .map(|f| f.currency.clone())
                .unwrap_or_default(),
            requests: 1,
            blind: call.blind() as u64,
            tokens_in: tokens.input,
            tokens_cached: tokens.cached,
            tokens_out: tokens.output,
            chars: call.chars,
            seconds: call.seconds,
            images: call.images,
            cost: found.as_ref().map_or(0.0, |f| f.cost),
        };
        let unsaved = found.as_ref().and_then(|f| f.unsaved);
        let in_budget = row.priced && (row.currency == main || (fold && row.currency.is_empty()));
        self.0.db.with(|c| {
            store::add(c, &row)?;
            if let Some(prices) = unsaved {
                store::save_price(c, &provider, &model, &prices, listed_at.unwrap_or(now))?;
                let mut s = self.0.state.lock();
                if let Some(listed) = s
                    .book
                    .get_mut(&provider)
                    .and_then(|b| b.prices.get_mut(&model))
                {
                    listed.stored = true;
                }
            }
            if prune {
                store::prune(
                    c,
                    &day_text(day - Duration::days(KEEP_DAYS)),
                    &format!("day:{}", day_text(day)),
                    &format!("month:{}", month_text(day)),
                )?;
            }
            if in_budget {
                check_budget(c, day, &main, fold, &cfg)
            } else {
                Ok(Vec::new())
            }
        })
    }

    /// Whether a call like this would get money: by its own cost, as a free
    /// one, or by a known price.
    pub fn can_price(&self, call: &PaidCall) -> bool {
        self.0
            .state
            .lock()
            .price(call, &provider_host(&call.base_url))
            .is_some()
    }

    /// Where the price of a model comes from.
    pub fn price_source(&self, provider: &str, model: &str) -> PriceSource {
        let s = self.0.state.lock();
        if s.listed(provider, model.trim()).is_some() {
            PriceSource::List
        } else if s.manual(provider, model.trim()).is_some() {
            PriceSource::Manual
        } else {
            PriceSource::None
        }
    }

    /// Takes a provider's price list read at the unix time `at`: it replaces
    /// the prices known for that provider. The prices of the models already
    /// used are kept in the base, and calls that waited for a price get one.
    /// Returns how many rows got a price. An empty list still counts as read.
    pub fn set_list_prices(
        &self,
        base_url: &str,
        prices: Vec<(String, UnitPrices)>,
        at: i64,
    ) -> Result<usize, DbError> {
        let provider = provider_host(base_url);
        {
            let mut s = self.0.state.lock();
            let book = s.book.entry(provider.clone()).or_default();
            book.prices = prices
                .into_iter()
                .filter(|(_, p)| !p.is_empty())
                .map(|(model, prices)| {
                    (
                        model.trim().to_string(),
                        Listed {
                            prices,
                            stored: false,
                        },
                    )
                })
                .collect();
            book.read_at = Some(at);
            book.asked_at = None;
            book.failed_at = None;
            book.complete = true;
        }
        self.0.db.with(|c| {
            let tx = c.unchecked_transaction()?;
            let used = store::used_models(&tx, &provider)?;
            store::delete_prices(&tx, &provider)?;
            let mut s = self.0.state.lock();
            if let Some(book) = s.book.get_mut(&provider) {
                for (model, listed) in book.prices.iter_mut() {
                    if used.contains(model) {
                        store::save_price(&tx, &provider, model, &listed.prices, at)?;
                        listed.stored = true;
                    }
                }
            }
            drop(s);
            tx.commit()
        })?;
        self.reprice(Some(&provider))
    }

    /// A read of a provider's price list failed at the unix time `at`.
    pub fn note_price_failure(&self, base_url: &str, at: i64) {
        let mut s = self.0.state.lock();
        let book = s.book.entry(provider_host(base_url)).or_default();
        book.failed_at = Some(at);
        book.asked_at = None;
    }

    /// Asks for the price lists of these addresses now, whatever their age.
    pub fn want(&self, base_urls: &[String]) {
        let now = Local::now().timestamp();
        let mut any = false;
        {
            let mut guard = self.0.state.lock();
            let s = &mut *guard;
            for url in base_urls {
                let address = url.trim().trim_end_matches('/').to_string();
                let provider = provider_host(&address);
                if provider.is_empty() || is_local(&provider) {
                    continue;
                }
                s.book.entry(provider).or_default().asked_at = Some(now);
                if !s.wanted.contains(&address) {
                    s.wanted.push(address);
                }
                any = true;
            }
        }
        if any {
            self.0.wake.notify_one();
        }
    }

    /// The addresses whose price list should be read; the list is emptied.
    pub fn take_wanted(&self) -> Vec<String> {
        std::mem::take(&mut self.0.state.lock().wanted)
    }

    /// Returns when `take_wanted` has something to give.
    pub async fn wanted(&self) {
        loop {
            if !self.0.state.lock().wanted.is_empty() {
                return;
            }
            self.0.wake.notified().await;
        }
    }

    /// Unix time of the newest good read of a price list.
    pub fn prices_at(&self) -> Option<i64> {
        let s = self.0.state.lock();
        s.book.values().filter_map(|b| b.read_at).max()
    }

    /// Spending from `from` to `to`, both days included.
    pub fn report(&self, from: NaiveDate, to: NaiveDate) -> Result<CostReport, DbError> {
        let rows = self
            .0
            .db
            .with(|c| store::rows(c, &day_text(from), &day_text(to)))?;
        let (main, fold) = {
            let s = self.0.state.lock();
            (s.currency(), s.folds_unknown())
        };
        let fold = fold.then_some(main.as_str());
        let mut total = Sum::new(CostKind::Chat, "", "", fold);
        let mut kinds: Vec<Sum> = Vec::new();
        let mut models: Vec<Sum> = Vec::new();
        for row in &rows {
            let Some(kind) = CostKind::parse(&row.kind) else {
                continue;
            };
            total.add(row);
            match kinds.iter_mut().find(|s| s.kind == kind) {
                Some(sum) => sum.add(row),
                None => {
                    let mut sum = Sum::new(kind, "", "", fold);
                    sum.add(row);
                    kinds.push(sum);
                }
            }
            match models
                .iter_mut()
                .find(|s| s.kind == kind && s.provider == row.provider && s.model == row.model)
            {
                Some(sum) => sum.add(row),
                None => {
                    let mut sum = Sum::new(kind, &row.provider, &row.model, fold);
                    sum.add(row);
                    models.push(sum);
                }
            }
        }
        let order = |kind: CostKind| CostKind::ALL.iter().position(|k| *k == kind);
        kinds.sort_by_key(|s| order(s.kind));
        // The most expensive first; then the busiest; then a stable order.
        models.sort_by(|a, b| {
            b.in_currency(&main)
                .total_cmp(&a.in_currency(&main))
                .then(b.requests.cmp(&a.requests))
                .then(order(a.kind).cmp(&order(b.kind)))
                .then_with(|| (&a.provider, &a.model).cmp(&(&b.provider, &b.model)))
        });
        Ok(CostReport {
            from: day_text(from),
            to: day_text(to),
            totals: total.money(&main),
            kinds: kinds
                .iter()
                .map(|s| s.line(&main, PriceSource::None))
                .collect(),
            models: models
                .iter()
                .map(|s| s.line(&main, self.price_source(&s.provider, &s.model)))
                .collect(),
            requests: total.requests,
            unpriced: total.unpriced,
            blind: total.blind,
            free: total.free(),
        })
    }

    /// The spending of `today` and of its month against the limits.
    pub fn budget(&self, today: NaiveDate) -> Result<BudgetState, DbError> {
        let (cfg, currency, fold) = {
            let s = self.0.state.lock();
            (s.cfg.clone(), s.currency(), s.folds_unknown())
        };
        let [day, month] = periods(today, &cfg);
        let (day_spent, month_spent) = self.0.db.with(|c| {
            Ok((
                store::spent(c, &day.from, &day.to, &currency, fold)?,
                store::spent(c, &month.from, &month.to, &currency, fold)?,
            ))
        })?;
        let share = |spent: f64, limit: f64| {
            if limit > 0.0 {
                // Whole percents, rounded down. Multiplied first and nudged:
                // 57 of 100 must not come out as 56.99… and read as 56%.
                (spent * 100.0 / limit + 1e-9).max(0.0) as u32
            } else {
                0
            }
        };
        let text = |spent: f64, limit: f64| {
            if limit > 0.0 {
                format!(
                    "{} из {}",
                    money_text(spent, &currency),
                    money_text(limit, &currency)
                )
            } else {
                money_text(spent, &currency)
            }
        };
        Ok(BudgetState {
            day_spent,
            day_limit: cfg.day_limit.max(0.0),
            day_share: share(day_spent, cfg.day_limit),
            month_spent,
            month_limit: cfg.month_limit.max(0.0),
            month_share: share(month_spent, cfg.month_limit),
            day_text: text(day_spent, cfg.day_limit),
            month_text: text(month_spent, cfg.month_limit),
            currency,
        })
    }

    /// Sets who hears of a crossed threshold. Called after every lock is released.
    pub fn on_alert(&self, f: AlertFn) {
        *self.0.alert.write() = Some(f);
    }

    /// Sets who hears that paid work of her own is held back. Called once
    /// per period, after every lock is released.
    pub fn on_pause(&self, f: PauseFn) {
        *self.0.pause.write() = Some(f);
    }

    /// The periods of `today` whose limit is reached, while that holds her
    /// own paid work: tracking on, `pause_unattended` on. The day first.
    fn reached(
        &self,
        c: &Connection,
        today: NaiveDate,
    ) -> rusqlite::Result<Vec<(Period, BudgetPause)>> {
        let (cfg, currency, fold) = {
            let s = self.0.state.lock();
            (s.cfg.clone(), s.currency(), s.folds_unknown())
        };
        let mut reached = Vec::new();
        if !cfg.enabled || !cfg.pause_unattended {
            return Ok(reached);
        }
        for p in periods(today, &cfg) {
            if p.limit <= 0.0 {
                continue;
            }
            let spent = store::spent(c, &p.from, &p.to, &currency, fold)?;
            if level(spent, p.limit) >= 100 {
                let pause = BudgetPause {
                    period: p.kind,
                    spent,
                    limit: p.limit,
                    currency: currency.clone(),
                };
                reached.push((p, pause));
            }
        }
        Ok(reached)
    }

    /// The limit that holds paid work of her own on `today`, or `None`:
    /// tracking is off, `pause_unattended` is off, no limit is set or none is
    /// reached (100%). The month goes before the day: it holds longer.
    /// Only reads; nobody is told.
    pub fn pause(&self, today: NaiveDate) -> Result<Option<BudgetPause>, DbError> {
        let reached = self.0.db.with(|c| self.reached(c, today))?;
        Ok(reached.into_iter().next_back().map(|(_, pause)| pause))
    }

    /// The check of a place where she starts paid work by herself, on
    /// `today`. False while a limit holds it (`pause`). The first refusal of
    /// a period tells the user through `on_pause`; the later ones of the same
    /// period are silent, however many runs are skipped. A database that
    /// cannot be read holds nothing: counting must not stop her work.
    pub fn unattended_allowed_at(&self, today: NaiveDate) -> bool {
        let checked = self.0.db.with(|c| {
            let reached = self.reached(c, today)?;
            let mut first = false;
            for (period, _) in &reached {
                let key = pause_key(period);
                if store::alert_level(c, &key)? == 0 {
                    store::set_alert_level(c, &key, 100)?;
                    first = true;
                }
            }
            let pause = reached.into_iter().next_back().map(|(_, pause)| pause);
            Ok((pause, first))
        });
        let (pause, first) = match checked {
            Ok(found) => found,
            Err(e) => {
                log::warn!("budget pause not checked, paid work of her own goes on: {e}");
                return true;
            }
        };
        let Some(pause) = pause else {
            return true;
        };
        // Later refusals leave no record here: this is asked every half a
        // minute while a summary waits. Who was held says so itself, once.
        if first {
            log::warn!(
                "budget pause: period={} spent={:.2} limit={:.2} currency={}: paid work of her own waits for the end of the period",
                period_name(pause.period),
                pause.spent,
                pause.limit,
                pause.currency
            );
        }
        if first {
            let tell = self.0.pause.read().clone();
            if let Some(tell) = tell {
                tell(pause);
            }
        }
        false
    }

    /// Forgets the spending and the shown warnings. Prices stay.
    pub fn clear(&self) -> Result<(), DbError> {
        self.0.db.with(store::clear)?;
        log::info!("spending and shown budget warnings are cleared");
        Ok(())
    }

    /// Gives money to the rows that waited for a price, of one provider or of
    /// all, in one transaction. Rows that have money are never recomputed.
    /// Returns how many rows got a price.
    fn reprice(&self, provider: Option<&str>) -> Result<usize, DbError> {
        self.0.db.with(|c| {
            let tx = c.unchecked_transaction()?;
            let mut priced = 0;
            for row in store::unpriced(&tx, provider)? {
                let Some(kind) = CostKind::parse(&row.kind) else {
                    continue;
                };
                let tokens = Tokens {
                    input: row.tokens_in,
                    cached: row.tokens_cached,
                    output: row.tokens_out,
                };
                let mut call = PaidCall::new(kind, &row.provider, &row.model)
                    .chars(row.chars)
                    .seconds(row.seconds)
                    .images(row.images);
                if kind.by_tokens() || tokens.total() > 0 {
                    call = call.tokens(tokens);
                }
                let counted = row.requests.saturating_sub(row.blind);
                let found = self.0.state.lock().by_price(&call, &row.provider, counted);
                let Some(found) = found else {
                    continue;
                };
                store::delete(&tx, &row)?;
                store::add(
                    &tx,
                    &Row {
                        priced: true,
                        currency: found.currency,
                        cost: found.cost,
                        ..row
                    },
                )?;
                priced += 1;
            }
            tx.commit()?;
            Ok(priced)
        })
    }

    /// Sets the shown warnings of today and of this month to the level the
    /// spending has now. Nobody is told.
    fn rearm(&self, today: NaiveDate) -> Result<(), DbError> {
        let (cfg, currency, fold) = {
            let s = self.0.state.lock();
            (s.cfg.clone(), s.currency(), s.folds_unknown())
        };
        self.0.db.with(|c| {
            for p in periods(today, &cfg) {
                let spent = store::spent(c, &p.from, &p.to, &currency, fold)?;
                let now = level(spent, p.limit);
                store::set_alert_level(c, &p.key, now)?;
                // A period that is no longer held forgets that its pause was
                // told: reached again, it is told again.
                if now < 100 || !cfg.enabled || !cfg.pause_unattended {
                    let key = pause_key(&p);
                    if store::alert_level(c, &key)? > 0 {
                        log::info!("budget pause lifted: period={}", period_name(p.kind));
                    }
                    store::set_alert_level(c, &key, 0)?;
                }
            }
            Ok(())
        })
    }
}

/// Compares the spending of the day and of the month with the limits. A
/// threshold crossed for the first time is stored and returned; a jump from
/// nothing to the whole limit gives only the 100 warning.
fn check_budget(
    c: &Connection,
    day: NaiveDate,
    currency: &str,
    fold: bool,
    cfg: &CostsConfig,
) -> rusqlite::Result<Vec<BudgetAlert>> {
    let mut alerts = Vec::new();
    for p in periods(day, cfg) {
        if p.limit <= 0.0 {
            continue;
        }
        let spent = store::spent(c, &p.from, &p.to, currency, fold)?;
        let now = level(spent, p.limit);
        if now > store::alert_level(c, &p.key)? {
            store::set_alert_level(c, &p.key, now)?;
            alerts.push(BudgetAlert {
                period: p.kind,
                level: now,
                spent,
                limit: p.limit,
                currency: currency.to_string(),
            });
        }
    }
    Ok(alerts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::costs::ManualPrice;

    const ROUTER: &str = "https://routerai.ru/api/v1";
    const OPENAI: &str = "https://api.openai.com/v1";

    /// A tracker over an empty base, counting in roubles.
    fn costs() -> (Costs, Arc<Db>) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let c = Costs::new(db.clone());
        c.set_settings(&CostsConfig::default(), "RUB");
        (c, db)
    }

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn now() -> i64 {
        Local::now().timestamp()
    }

    fn chat(base: &str, model: &str, input: u64, output: u64) -> PaidCall {
        PaidCall::new(CostKind::Chat, base, model).tokens(Tokens {
            input,
            cached: 0,
            output,
        })
    }

    /// A RouterAI chat call whose money the provider reported.
    fn paid(cost: f64) -> PaidCall {
        chat(ROUTER, "m", 100, 50).cost(Some(cost))
    }

    fn token_prices(input: f64, output: f64) -> UnitPrices {
        UnitPrices {
            input: Some(input),
            output: Some(output),
            ..Default::default()
        }
    }

    fn limits(day_limit: f64, month_limit: f64) -> CostsConfig {
        CostsConfig {
            day_limit,
            month_limit,
            ..Default::default()
        }
    }

    /// Collects every alert the tracker gives.
    fn alerts(c: &Costs) -> Arc<Mutex<Vec<(BudgetPeriod, u8)>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        c.on_alert(Arc::new(move |a: BudgetAlert| {
            sink.lock().push((a.period, a.level))
        }));
        seen
    }

    fn take(seen: &Mutex<Vec<(BudgetPeriod, u8)>>) -> Vec<(BudgetPeriod, u8)> {
        std::mem::take(&mut *seen.lock())
    }

    fn count(db: &Db, sql: &str) -> i64 {
        db.with(|c| c.query_row(sql, [], |r| r.get(0))).unwrap()
    }

    fn texts(money: &[Money]) -> Vec<&str> {
        money.iter().map(|m| m.text.as_str()).collect()
    }

    #[test]
    fn the_reported_cost_wins_then_the_list_then_the_typed_price() {
        let (c, _) = costs();
        let d = day("2026-11-14");
        c.set_list_prices(
            ROUTER,
            vec![("m".into(), token_prices(0.001, 0.002))],
            now(),
        )
        .unwrap();
        // The provider said what the request cost: the list is not asked.
        c.record_at(d, chat(ROUTER, "m", 1000, 500).cost(Some(0.42)));
        assert_eq!(texts(&c.report(d, d).unwrap().totals), ["0,42\u{a0}₽"]);
        // No cost in the answer: 1000 × 0.001 + 500 × 0.002.
        c.record_at(d, chat(ROUTER, "m", 1000, 500));
        assert_eq!(texts(&c.report(d, d).unwrap().totals), ["2,42\u{a0}₽"]);

        // A typed price for the same model does not beat the list.
        let typed = |model: &str| ManualPrice {
            provider: "routerai.ru".into(),
            model: model.into(),
            currency: "USD".into(),
            input_mtok: 2.0,
            output_mtok: 4.0,
            ..Default::default()
        };
        c.set_settings(
            &CostsConfig {
                prices: vec![typed("m"), typed("n")],
                ..Default::default()
            },
            "RUB",
        );
        c.record_at(d, chat(ROUTER, "m", 1000, 0));
        assert_eq!(texts(&c.report(d, d).unwrap().totals), ["3,42\u{a0}₽"]);
        assert_eq!(c.price_source("routerai.ru", "m"), PriceSource::List);
        // The list has no price for this one: the typed price and its currency.
        c.record_at(d, chat(ROUTER, "n", 1_000_000, 500_000));
        assert_eq!(
            texts(&c.report(d, d).unwrap().totals),
            ["3,42\u{a0}₽", "4\u{a0}$"]
        );
        assert_eq!(c.price_source("routerai.ru", "n"), PriceSource::Manual);
        assert_eq!(c.price_source("routerai.ru", "other"), PriceSource::None);
        assert_eq!(c.price_source("api.openai.com", "n"), PriceSource::None);

        assert!(c.can_price(&chat(ROUTER, "m", 1, 1)));
        assert!(c.can_price(&chat(ROUTER, "other", 1, 1).cost(Some(0.0))));
        assert!(!c.can_price(&chat(ROUTER, "other", 1, 1)));
        assert!(!c.can_price(&PaidCall::new(CostKind::Chat, ROUTER, "m")));
        assert!(c.can_price(&PaidCall::new(
            CostKind::Chat,
            "http://localhost:11434/v1",
            "x"
        )));
    }

    #[test]
    fn free_and_local_calls_cost_nothing_and_say_so() {
        let (c, _) = costs();
        let d = day("2026-11-14");
        c.record_at(d, chat("http://localhost:11434/v1", "qwen3", 800, 40));
        c.record_at(d, chat("http://127.0.0.1:8080/v1", "llama", 10, 10));
        c.record_at(d, chat("https://example.test/v1", "own", 10, 10).free(true));
        let r = c.report(d, d).unwrap();
        assert!(r.totals.is_empty());
        assert_eq!((r.requests, r.unpriced, r.blind), (3, 0, 0));
        assert_eq!(r.models.len(), 3);
        for line in &r.models {
            assert!(line.free && line.cost.is_empty(), "{line:?}");
        }
        assert!(r.kinds[0].free && r.free);
        let ollama = r
            .models
            .iter()
            .find(|l| l.provider == "localhost:11434")
            .unwrap();
        assert_eq!(ollama.units, "840 ток.");
        // Nobody is asked for a price list.
        assert!(c.take_wanted().is_empty());

        // A stopped stream of a local model reports no tokens: it is free all
        // the same, not «the price is unknown».
        c.record_at(
            d,
            PaidCall::new(CostKind::Chat, "http://localhost:11434/v1", "qwen3"),
        );
        let r = c.report(d, d).unwrap();
        assert_eq!((r.requests, r.unpriced, r.blind), (4, 0, 1));
        assert!(r.free && r.totals.is_empty());
        let ollama = r
            .models
            .iter()
            .find(|l| l.provider == "localhost:11434")
            .unwrap();
        assert!(ollama.free && ollama.blind == 1);
        // One call that waits for a price, and the day is no longer free.
        c.record_at(d, chat(OPENAI, "gpt", 10, 10));
        let r = c.report(d, d).unwrap();
        assert!(!r.free && r.totals.is_empty());
        assert!(!r.kinds[0].free);
        // Nothing counted is not «free» either.
        assert!(!c.report(day("2026-01-01"), day("2026-01-01")).unwrap().free);
    }

    #[test]
    fn a_call_without_usage_is_counted_blind_and_gets_no_money() {
        let (c, _) = costs();
        let d = day("2026-11-14");
        c.set_list_prices(
            ROUTER,
            vec![("m".into(), token_prices(0.001, 0.002))],
            now(),
        )
        .unwrap();
        c.record_at(d, PaidCall::new(CostKind::Chat, ROUTER, "m"));
        let r = c.report(d, d).unwrap();
        assert_eq!((r.requests, r.blind, r.unpriced), (1, 1, 0));
        assert!(r.totals.is_empty());
        let line = &r.models[0];
        assert_eq!((line.blind, line.unpriced), (1, 0));
        assert_eq!(line.units, "токены не сообщены");
        assert!(!line.free && line.cost.is_empty() && !r.free);
        assert_eq!(
            (line.tokens_in, line.tokens_cached, line.tokens_out),
            (0, 0, 0)
        );
    }

    #[test]
    fn nothing_is_written_while_counting_is_off() {
        let (c, db) = costs();
        let d = day("2026-11-14");
        c.set_settings(
            &CostsConfig {
                enabled: false,
                ..Default::default()
            },
            "RUB",
        );
        assert!(!c.enabled());
        c.record_at(d, paid(5.0));
        assert_eq!(c.report(d, d).unwrap().requests, 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_usage"), 0);
        assert!(c.take_wanted().is_empty());
        c.set_settings(&CostsConfig::default(), "RUB");
        c.record_at(d, paid(5.0));
        assert_eq!(c.report(d, d).unwrap().requests, 1);
    }

    #[test]
    fn a_recognition_of_unknown_length_is_unpriced_not_free() {
        let (c, db) = costs();
        let d = day("2026-11-14");
        let per_second = UnitPrices {
            second: Some(0.00036),
            ..Default::default()
        };
        c.set_list_prices(ROUTER, vec![("asr".into(), per_second)], now())
            .unwrap();
        let asr =
            |seconds: f64| PaidCall::new(CostKind::Transcription, ROUTER, "asr").seconds(seconds);
        // An OGG voice message and no `usage.seconds`: the length is unknown.
        // The request is counted as one of unknown size, without money.
        c.record_at(d, asr(0.0));
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM cost_usage WHERE priced = 0 AND blind = 1"
            ),
            1
        );
        let r = c.report(d, d).unwrap();
        assert_eq!((r.requests, r.unpriced, r.blind), (1, 0, 1));
        assert!(r.totals.is_empty() && !r.models[0].free && !r.free);
        assert_eq!(r.models[0].units, "длительность не сообщена");
        c.record_at(d, asr(100.0));
        let r = c.report(d, d).unwrap();
        assert_eq!((r.requests, r.unpriced, r.blind), (2, 0, 1));
        assert_eq!(texts(&r.totals), ["0,036\u{a0}₽"]);
        assert!(!r.models[0].free && !r.free);
    }

    #[test]
    fn a_price_that_comes_later_does_not_make_unknown_lengths_free() {
        let (c, db) = costs();
        let d = day("2026-11-14");
        let asr = |seconds: f64| {
            PaidCall::new(CostKind::Transcription, OPENAI, "whisper-1").seconds(seconds)
        };
        // Three recordings of known length and two voice messages of unknown
        // length, all before any price is known: one row.
        for seconds in [60.0, 60.0, 60.0, 0.0, 0.0] {
            c.record_at(d, asr(seconds));
        }
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_usage"), 1);
        let r = c.report(d, d).unwrap();
        assert_eq!((r.requests, r.unpriced, r.blind), (5, 3, 2));
        c.set_settings(
            &CostsConfig {
                prices: vec![ManualPrice {
                    provider: "api.openai.com".into(),
                    model: "whisper-1".into(),
                    currency: "USD".into(),
                    per_minute: 0.006,
                    ..Default::default()
                }],
                ..Default::default()
            },
            "RUB",
        );
        // The price covers the three minutes that are known. The two of
        // unknown length stay named as such: they are not paid at zero.
        let r = c.report(d, d).unwrap();
        assert_eq!(texts(&r.totals), ["0,018\u{a0}$"]);
        assert_eq!((r.requests, r.unpriced, r.blind), (5, 0, 2));
        let line = &r.models[0];
        assert_eq!((line.requests, line.blind), (5, 2));
        assert!(!line.free && !r.free);
        // A row of unknown lengths only has nothing to price.
        let next = day("2026-11-15");
        c.record_at(next, asr(0.0));
        c.set_settings(&c.settings(), "RUB");
        let r = c.report(next, next).unwrap();
        assert_eq!((r.requests, r.unpriced, r.blind), (1, 0, 1));
        assert!(r.totals.is_empty() && !r.free);
    }

    #[test]
    fn calls_that_waited_get_money_when_a_list_price_arrives() {
        let (c, _) = costs();
        let d = day("2026-11-14");
        c.record_at(d, chat(ROUTER, "m", 1000, 500));
        c.record_at(d, PaidCall::new(CostKind::Chat, ROUTER, "m"));
        c.record_at(d, PaidCall::new(CostKind::Chat, ROUTER, "blind-only"));
        let r = c.report(d, d).unwrap();
        assert_eq!((r.requests, r.unpriced, r.blind), (3, 1, 2));
        assert!(r.totals.is_empty());

        let list = |k: f64| {
            vec![
                ("m".to_string(), token_prices(0.001 * k, 0.002 * k)),
                ("blind-only".to_string(), token_prices(0.001 * k, 0.002 * k)),
            ]
        };
        assert_eq!(c.set_list_prices(ROUTER, list(1.0), now()).unwrap(), 1);
        let r = c.report(d, d).unwrap();
        assert_eq!(texts(&r.totals), ["2\u{a0}₽"]);
        assert_eq!((r.requests, r.unpriced, r.blind), (3, 0, 2));
        let m = r.models.iter().find(|l| l.model == "m").unwrap();
        assert_eq!((m.requests, m.blind, m.unpriced), (2, 1, 0));
        assert_eq!(texts(&m.cost), ["2\u{a0}₽"]);
        assert_eq!(m.price, PriceSource::List);
        // Requests without usage have nothing to price.
        let b = r.models.iter().find(|l| l.model == "blind-only").unwrap();
        assert_eq!((b.requests, b.blind, b.unpriced), (1, 1, 0));
        assert!(b.cost.is_empty());

        // The price doubles: what was counted keeps its money.
        assert_eq!(c.set_list_prices(ROUTER, list(2.0), now()).unwrap(), 0);
        assert_eq!(texts(&c.report(d, d).unwrap().totals), ["2\u{a0}₽"]);
        c.record_at(d, chat(ROUTER, "m", 1000, 500));
        assert_eq!(texts(&c.report(d, d).unwrap().totals), ["6\u{a0}₽"]);
    }

    #[test]
    fn calls_that_waited_get_money_when_a_price_is_typed_in() {
        let (c, _) = costs();
        let d = day("2026-11-14");
        c.record_at(
            d,
            PaidCall::new(CostKind::Transcription, OPENAI, "whisper-1").seconds(120.0),
        );
        c.record_at(d, chat(OPENAI, "gpt-4o-mini", 2_000_000, 1_000_000));
        assert_eq!(c.report(d, d).unwrap().unpriced, 2);
        let cfg = CostsConfig {
            prices: vec![ManualPrice {
                provider: "api.openai.com".into(),
                model: "whisper-1".into(),
                currency: "USD".into(),
                per_minute: 0.006,
                ..Default::default()
            }],
            ..Default::default()
        };
        c.set_settings(&cfg, "RUB");
        let r = c.report(d, d).unwrap();
        assert_eq!(r.unpriced, 1);
        assert_eq!(texts(&r.totals), ["0,012\u{a0}$"]);
        let whisper = r.models.iter().find(|l| l.model == "whisper-1").unwrap();
        assert_eq!(whisper.price, PriceSource::Manual);
        assert_eq!(whisper.units, "2 мин 00 с");
        // A model nobody gave a price for stays as it was.
        let gpt = r.models.iter().find(|l| l.model == "gpt-4o-mini").unwrap();
        assert_eq!((gpt.unpriced, gpt.price), (1, PriceSource::None));
        // Taking the price away leaves the money that was counted.
        c.set_settings(&CostsConfig::default(), "RUB");
        assert_eq!(texts(&c.report(d, d).unwrap().totals), ["0,012\u{a0}$"]);
    }

    #[test]
    fn the_report_groups_by_kind_and_model_and_keeps_currencies_apart() {
        let (c, _) = costs();
        let (d1, d2) = (day("2026-11-13"), day("2026-11-14"));
        let speech = PaidCall::new(CostKind::Speech, ROUTER, "tts")
            .chars(2900)
            .cost(Some(3.1));
        let vision = PaidCall::new(CostKind::Vision, OPENAI, "gpt")
            .tokens(Tokens {
                input: 900,
                cached: 100,
                output: 200,
            })
            .cost(Some(0.42));
        c.record_at(d2, chat(ROUTER, "big", 200_000, 10_000).cost(Some(9.1)));
        c.record_at(d2, chat(ROUTER, "big", 90_000, 10_000).cost(Some(1.0)));
        c.record_at(d2, chat(ROUTER, "small", 500, 100).cost(Some(0.5)));
        c.record_at(d2, speech);
        c.record_at(d2, vision);
        c.record_at(
            d2,
            PaidCall::new(CostKind::Transcription, OPENAI, "whisper-1").seconds(185.0),
        );
        c.record_at(d2, PaidCall::new(CostKind::Chat, ROUTER, "big"));
        c.record_at(d2, chat("http://localhost:11434/v1", "local", 10, 10));
        c.record_at(d1, chat(ROUTER, "big", 1000, 1000).cost(Some(5.0)));
        c.record_at(day("2026-11-12"), paid(100.0));

        let r = c.report(d1, d2).unwrap();
        assert_eq!(
            (r.from.as_str(), r.to.as_str()),
            ("2026-11-13", "2026-11-14")
        );
        // The free call adds no money: its zero bucket is dropped.
        assert_eq!(texts(&r.totals), ["18,70\u{a0}₽", "0,42\u{a0}$"]);
        assert_eq!(r.totals[0].currency, "RUB");
        assert_eq!((r.requests, r.unpriced, r.blind), (9, 1, 1));

        let kinds: Vec<CostKind> = r.kinds.iter().map(|l| l.kind).collect();
        assert_eq!(
            kinds,
            [
                CostKind::Chat,
                CostKind::Vision,
                CostKind::Speech,
                CostKind::Transcription
            ]
        );
        let chat_line = &r.kinds[0];
        assert!(chat_line.provider.is_empty() && chat_line.model.is_empty());
        assert_eq!((chat_line.requests, chat_line.blind), (6, 1));
        assert_eq!(texts(&chat_line.cost), ["15,60\u{a0}₽"]);
        assert_eq!(chat_line.price, PriceSource::None);
        assert_eq!(chat_line.units, "313 тыс. токенов");
        assert!(!chat_line.free);

        // The most expensive in the main currency first, then the busiest.
        let models: Vec<&str> = r.models.iter().map(|l| l.model.as_str()).collect();
        // Lines without money in the main currency keep the order of the kinds.
        assert_eq!(models, ["big", "tts", "small", "local", "gpt", "whisper-1"]);
        let big = &r.models[0];
        assert_eq!((big.provider.as_str(), big.requests), ("routerai.ru", 4));
        assert_eq!((big.tokens_in, big.tokens_out), (291_000, 21_000));
        assert_eq!(r.models[1].units, "2,9 тыс. знаков");
        assert!(r.models[3].free);
        assert_eq!(texts(&r.models[4].cost), ["0,42\u{a0}$"]);
        assert_eq!(r.models[4].tokens_cached, 100);
        let whisper = &r.models[5];
        assert_eq!(
            (whisper.unpriced, whisper.units.as_str()),
            (1, "3 мин 05 с")
        );
        assert!(whisper.cost.is_empty() && !whisper.free);

        // Both ends of the range are included.
        assert_eq!(texts(&c.report(d2, d2).unwrap().totals)[0], "13,70\u{a0}₽");
        assert_eq!(texts(&c.report(d1, d1).unwrap().totals), ["5\u{a0}₽"]);
        assert_eq!(c.report(day("2026-11-01"), d2).unwrap().requests, 10);

        // Another main currency comes first.
        c.set_settings(
            &CostsConfig {
                currency: "USD".into(),
                ..Default::default()
            },
            "RUB",
        );
        assert_eq!(c.currency(), "USD");
        let r = c.report(d1, d2).unwrap();
        assert_eq!(texts(&r.totals), ["0,42\u{a0}$", "18,70\u{a0}₽"]);
        assert_eq!(r.models[0].model, "gpt");
    }

    #[test]
    fn the_budget_warns_at_80_and_at_100_once_each() {
        let (c, _) = costs();
        let seen = alerts(&c);
        c.set_settings(&limits(100.0, 200.0), "RUB");
        let d = day("2026-11-14");
        c.record_at(d, paid(79.0));
        assert!(take(&seen).is_empty());
        c.record_at(d, paid(1.0));
        assert_eq!(take(&seen), [(BudgetPeriod::Day, 80)]);
        c.record_at(d, paid(5.0));
        assert!(take(&seen).is_empty());
        c.record_at(d, paid(15.0));
        assert_eq!(take(&seen), [(BudgetPeriod::Day, 100)]);
        c.record_at(d, paid(50.0));
        assert!(take(&seen).is_empty(), "the limit only warns, once");
        c.record_at(d, paid(10.0));
        assert_eq!(take(&seen), [(BudgetPeriod::Month, 80)]);

        let b = c.budget(d).unwrap();
        assert_eq!((b.day_spent, b.day_share), (160.0, 160));
        assert_eq!((b.month_spent, b.month_share), (160.0, 80));
        assert_eq!(b.day_text, "160\u{a0}₽ из 100\u{a0}₽");
        assert_eq!(b.month_text, "160\u{a0}₽ из 200\u{a0}₽");
        assert_eq!(b.currency, "RUB");

        // A new day warns again; the month has already said 80.
        let next = day("2026-11-15");
        c.record_at(next, paid(10.0));
        assert!(take(&seen).is_empty());
        c.record_at(next, paid(75.0));
        assert_eq!(
            take(&seen),
            [(BudgetPeriod::Day, 80), (BudgetPeriod::Month, 100)]
        );
        let b = c.budget(next).unwrap();
        assert_eq!((b.day_spent, b.month_spent), (85.0, 245.0));
        // A new month starts from nothing.
        let b = c.budget(day("2026-12-01")).unwrap();
        assert_eq!((b.day_spent, b.month_spent, b.month_share), (0.0, 0.0, 0));
    }

    #[test]
    fn a_jump_over_the_whole_limit_gives_one_warning() {
        let (c, _) = costs();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        c.on_alert(Arc::new(move |a: BudgetAlert| sink.lock().push(a)));
        c.set_settings(&limits(100.0, 0.0), "RUB");
        c.record_at(day("2026-11-14"), paid(150.0));
        assert_eq!(
            *seen.lock(),
            [BudgetAlert {
                period: BudgetPeriod::Day,
                level: 100,
                spent: 150.0,
                limit: 100.0,
                currency: "RUB".into(),
            }]
        );
    }

    #[test]
    fn no_limit_no_warning_and_other_currencies_stay_out_of_the_budget() {
        let (c, _) = costs();
        let seen = alerts(&c);
        let d = day("2026-11-14");
        c.record_at(d, paid(1_000_000.0));
        assert!(take(&seen).is_empty());
        let b = c.budget(d).unwrap();
        assert_eq!((b.day_limit, b.day_share, b.month_share), (0.0, 0, 0));
        assert_eq!(b.day_text, "1\u{a0}000\u{a0}000\u{a0}₽");

        let (c, _) = costs();
        let seen = alerts(&c);
        c.set_settings(&limits(100.0, 100.0), "RUB");
        // Dollars are not roubles: no rate, no budget.
        c.record_at(d, chat(OPENAI, "gpt", 10, 10).cost(Some(500.0)));
        assert!(take(&seen).is_empty());
        let b = c.budget(d).unwrap();
        assert_eq!((b.day_spent, b.month_spent), (0.0, 0.0));
        assert_eq!(b.day_text, "0\u{a0}₽ из 100\u{a0}₽");

        // The share is whole percents rounded down, and exact on round numbers.
        for (spent, share) in [(57.0, 57), (29.0, 29), (0.29, 0), (12.4, 12), (104.2, 104)] {
            let (c, _) = costs();
            c.set_settings(&limits(100.0, 100.0), "RUB");
            c.record_at(d, paid(spent));
            assert_eq!(c.budget(d).unwrap().day_share, share, "{spent}");
        }
    }

    #[test]
    fn a_restart_does_not_repeat_a_warning() {
        let (c, db) = costs();
        let seen = alerts(&c);
        let cfg = limits(100.0, 0.0);
        c.set_settings(&cfg, "RUB");
        let today = Local::now().date_naive();
        c.record_at(today, paid(85.0));
        assert_eq!(take(&seen), [(BudgetPeriod::Day, 80)]);

        let again = Costs::new(db);
        let seen = alerts(&again);
        again.set_settings(&cfg, "RUB");
        again.record_at(today, paid(1.0));
        assert!(take(&seen).is_empty());
        again.record_at(today, paid(20.0));
        assert_eq!(take(&seen), [(BudgetPeriod::Day, 100)]);
    }

    #[test]
    fn a_changed_limit_does_not_speak_up_by_itself() {
        let (c, _) = costs();
        let seen = alerts(&c);
        let today = Local::now().date_naive();
        c.set_settings(&limits(100.0, 0.0), "RUB");
        c.record_at(today, paid(50.0));
        assert!(take(&seen).is_empty());
        // The limit drops under what is spent: stored as said, nobody is told.
        c.set_settings(&limits(60.0, 0.0), "RUB");
        assert!(take(&seen).is_empty());
        c.record_at(today, paid(1.0));
        assert!(take(&seen).is_empty(), "80 was passed silently");
        c.record_at(today, paid(10.0));
        assert_eq!(take(&seen), [(BudgetPeriod::Day, 100)]);
        // A higher limit arms the warnings again.
        c.set_settings(&limits(70.0, 0.0), "RUB");
        c.record_at(today, paid(1.0));
        assert!(take(&seen).is_empty(), "62 of 70 is 88%: said silently");
        c.set_settings(&limits(1000.0, 0.0), "RUB");
        c.record_at(today, paid(800.0));
        assert_eq!(take(&seen), [(BudgetPeriod::Day, 80)]);
    }

    #[test]
    fn a_price_list_is_asked_for_once_and_again_when_it_is_old() {
        let (c, _) = costs();
        let d = day("2026-11-14");
        assert!(c.take_wanted().is_empty() && c.prices_at().is_none());
        c.record_at(d, chat(" https://routerai.ru/api/v1/ ", "m", 1, 1));
        assert_eq!(c.take_wanted(), [ROUTER]);
        // Asked and not answered yet: no second request.
        c.record_at(d, chat(ROUTER, "m", 1, 1));
        assert!(c.take_wanted().is_empty());

        let t = now();
        c.set_list_prices(ROUTER, Vec::new(), t).unwrap();
        assert_eq!(c.prices_at(), Some(t));
        c.record_at(d, chat(ROUTER, "m", 1, 1));
        assert!(c.take_wanted().is_empty(), "read a moment ago");

        c.set_list_prices(ROUTER, Vec::new(), t - PRICES_TTL_S - 5)
            .unwrap();
        c.record_at(d, chat(ROUTER, "m", 1, 1));
        assert_eq!(c.take_wanted(), [ROUTER], "a day has passed");

        // A failed read waits an hour.
        c.note_price_failure(ROUTER, t);
        c.record_at(d, chat(ROUTER, "m", 1, 1));
        assert!(c.take_wanted().is_empty());
        c.note_price_failure(ROUTER, t - PRICES_RETRY_S - 5);
        c.record_at(d, chat(ROUTER, "m", 1, 1));
        assert_eq!(c.take_wanted(), [ROUTER]);

        // Local and free providers have no list to read.
        c.record_at(d, chat("http://localhost:11434/v1", "q", 1, 1));
        c.record_at(d, chat("https://own.test/v1", "q", 1, 1).free(true));
        assert!(c.take_wanted().is_empty());

        // Another provider is asked on its own.
        c.record_at(d, chat(OPENAI, "gpt", 1, 1));
        assert_eq!(c.take_wanted(), [OPENAI]);

        // A refresh by hand does not look at the age.
        c.set_list_prices(ROUTER, Vec::new(), t).unwrap();
        c.want(&[
            format!("{ROUTER}/"),
            "http://localhost:11434/v1".to_string(),
            String::new(),
        ]);
        assert_eq!(c.take_wanted(), [ROUTER]);
    }

    #[tokio::test]
    async fn the_refresher_is_woken_by_a_paid_call() {
        let (c, _) = costs();
        let waiter = c.clone();
        let job = tokio::spawn(async move {
            waiter.wanted().await;
            waiter.take_wanted()
        });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!job.is_finished());
        c.record_at(day("2026-11-14"), chat(ROUTER, "m", 1, 1));
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), job)
            .await
            .expect("the waiter wakes")
            .unwrap();
        assert_eq!(got, [ROUTER]);
    }

    #[test]
    fn list_prices_of_used_models_survive_a_restart() {
        let (c, db) = costs();
        let d = day("2026-11-14");
        c.record_at(d, chat(ROUTER, "used", 1000, 0));
        let list = vec![
            ("used".to_string(), token_prices(0.001, 0.002)),
            ("unused".to_string(), token_prices(0.001, 0.002)),
            ("empty".to_string(), UnitPrices::default()),
        ];
        assert_eq!(c.set_list_prices(ROUTER, list, 1000).unwrap(), 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_prices"), 1);
        assert_eq!(c.price_source("routerai.ru", "unused"), PriceSource::List);
        assert_eq!(c.price_source("routerai.ru", "empty"), PriceSource::None);

        let again = Costs::new(db.clone());
        again.set_settings(&CostsConfig::default(), "RUB");
        assert_eq!(again.price_source("routerai.ru", "used"), PriceSource::List);
        assert_eq!(
            again.price_source("routerai.ru", "unused"),
            PriceSource::None
        );
        assert_eq!(again.prices_at(), Some(1000));
        // The first call after the start is priced at once.
        again.record_at(d, chat(ROUTER, "used", 1000, 0));
        assert_eq!(texts(&again.report(d, d).unwrap().totals), ["2\u{a0}₽"]);

        // A model used for the first time brings its price into the base.
        c.record_at(d, chat(ROUTER, "unused", 1000, 0));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_prices"), 2);
        assert_eq!(
            count(
                &db,
                "SELECT fetched_at FROM cost_prices WHERE model = 'unused'"
            ),
            1000
        );
        assert_eq!(
            Costs::new(db).price_source("routerai.ru", "unused"),
            PriceSource::List
        );
    }

    #[test]
    fn after_a_restart_a_model_not_used_before_asks_for_the_list_at_once() {
        let (c, db) = costs();
        let d = day("2026-11-14");
        c.record_at(d, chat(ROUTER, "used", 1000, 0));
        c.take_wanted();
        let list = vec![
            ("used".to_string(), token_prices(0.001, 0.002)),
            ("other".to_string(), token_prices(0.01, 0.02)),
        ];
        // Read an hour ago: well inside its day.
        let read = now() - 3600;
        c.set_list_prices(ROUTER, list.clone(), read).unwrap();
        // The whole list is in memory: a model it does not name has no price
        // there, and asking again would not find one.
        c.record_at(d, chat(ROUTER, "nowhere", 10, 10));
        assert!(c.take_wanted().is_empty());

        // After a restart only the prices of used models are known.
        let again = Costs::new(db.clone());
        again.set_settings(&CostsConfig::default(), "RUB");
        assert_eq!(again.prices_at(), Some(read));
        assert_eq!(
            again.price_source("routerai.ru", "other"),
            PriceSource::None
        );
        // A call that needs no list price does not ask for the list.
        again.record_at(d, chat(ROUTER, "used", 10, 10));
        again.record_at(d, chat(ROUTER, "other", 10, 10).cost(Some(0.5)));
        again.record_at(d, PaidCall::new(CostKind::Chat, ROUTER, "other"));
        assert!(again.take_wanted().is_empty());
        // The first priceable call to a model not used before does, though
        // the list was read an hour ago; and once, not with every call.
        again.record_at(d, chat(ROUTER, "other", 1000, 0));
        assert_eq!(again.take_wanted(), [ROUTER]);
        again.record_at(d, chat(ROUTER, "other", 1000, 0));
        assert!(again.take_wanted().is_empty());
        assert_eq!(again.report(d, d).unwrap().unpriced, 3);
        // The list comes: both calls get their money, the model its price.
        assert_eq!(again.set_list_prices(ROUTER, list, now()).unwrap(), 1);
        let r = again.report(d, d).unwrap();
        assert_eq!(r.unpriced, 1, "only the model the list does not name");
        assert_eq!(
            again.price_source("routerai.ru", "other"),
            PriceSource::List
        );
        // Now the list is whole again: a model it does not name stays quiet.
        again.record_at(d, chat(ROUTER, "nowhere", 10, 10));
        assert!(again.take_wanted().is_empty());

        // A failed read waits its hour, also for a missing model.
        let third = Costs::new(db);
        third.set_settings(&CostsConfig::default(), "RUB");
        third.record_at(d, chat(ROUTER, "third", 10, 10));
        assert_eq!(third.take_wanted(), [ROUTER]);
        third.note_price_failure(ROUTER, now());
        third.record_at(d, chat(ROUTER, "third", 10, 10));
        assert!(third.take_wanted().is_empty());
    }

    #[test]
    fn money_without_a_currency_is_counted_in_the_saved_budget_currency() {
        const OTHER: &str = "https://llm.example.test/v1";
        let d = day("2026-11-14");
        let spend = |c: &Costs| c.record_at(d, chat(OTHER, "m", 10, 10).cost(Some(90.0)));

        // The main provider does not tell its currency and none is saved:
        // the money is shown without a sign and there is no budget to set.
        let (c, _) = costs();
        let seen = alerts(&c);
        c.set_settings(&CostsConfig::default(), "");
        assert!(!c.assumes_currency());
        spend(&c);
        let r = c.report(d, d).unwrap();
        assert_eq!(texts(&r.totals), ["90"]);
        assert_eq!(r.totals[0].currency, "");
        assert!(take(&seen).is_empty());

        // The user saves the budget in roubles: by that they said what this
        // provider counts in. What was spent before is counted too.
        let saved = CostsConfig {
            currency: "RUB".into(),
            ..limits(100.0, 0.0)
        };
        c.set_settings(&saved, "");
        assert!(c.assumes_currency());
        let b = c.budget(d).unwrap();
        assert_eq!((b.day_spent, b.day_share), (90.0, 90));
        assert_eq!(b.day_text, "90\u{a0}₽ из 100\u{a0}₽");
        let r = c.report(d, d).unwrap();
        assert_eq!(texts(&r.totals), ["90\u{a0}₽"]);
        assert_eq!(texts(&r.models[0].cost), ["90\u{a0}₽"]);
        assert!(
            take(&seen).is_empty(),
            "a changed setting does not speak up"
        );
        spend(&c);
        assert_eq!(take(&seen), [(BudgetPeriod::Day, 100)]);
        assert_eq!(c.budget(d).unwrap().day_spent, 180.0);
        // Money in a known other currency stays apart as before.
        c.record_at(d, chat(OPENAI, "gpt", 10, 10).cost(Some(1.0)));
        assert_eq!(c.budget(d).unwrap().day_spent, 180.0);
        assert_eq!(
            texts(&c.report(d, d).unwrap().totals),
            ["180\u{a0}₽", "1\u{a0}$"]
        );

        // The main provider tells its currency: money of an unknown one is
        // not guessed at. It stays apart, and outside the budget.
        let (c, _) = costs();
        let seen = alerts(&c);
        c.set_settings(&saved, "RUB");
        assert!(!c.assumes_currency());
        spend(&c);
        spend(&c);
        assert!(take(&seen).is_empty());
        assert_eq!(c.budget(d).unwrap().day_spent, 0.0);
        let r = c.report(d, d).unwrap();
        assert_eq!(texts(&r.totals), ["180"]);
        assert_eq!(r.totals[0].currency, "");
    }

    #[test]
    fn old_days_go_and_clear_wipes_spending_but_keeps_prices() {
        let (c, db) = costs();
        c.set_settings(&limits(10.0, 0.0), "RUB");
        let (old, kept, today) = (day("2025-01-01"), day("2025-10-11"), day("2026-11-14"));
        c.record_at(old, paid(20.0));
        c.record_at(kept, paid(20.0));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_usage"), 2);
        // The warning of a day that is over goes when the next day begins.
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM cost_alerts WHERE period = 'day:2025-10-11'"
            ),
            1
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_alerts"), 1);
        // 400 days before 14 November 2026 is 10 October 2025.
        c.record_at(today, paid(20.0));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_usage"), 2);
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM cost_usage WHERE day = '2025-01-01'"
            ),
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM cost_alerts WHERE period = 'day:2026-11-14'"
            ),
            1
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_alerts"), 1);

        c.set_list_prices(ROUTER, vec![("m".into(), token_prices(1.0, 1.0))], 5)
            .unwrap();
        c.clear().unwrap();
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_usage"), 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_alerts"), 0);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM cost_prices"), 1);
        assert_eq!(c.report(old, today).unwrap().requests, 0);
    }

    /// Collects every pause the tracker tells about.
    fn pauses(c: &Costs) -> Arc<Mutex<Vec<(BudgetPeriod, f64, f64)>>> {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        c.on_pause(Arc::new(move |p: BudgetPause| {
            sink.lock().push((p.period, p.spent, p.limit))
        }));
        seen
    }

    #[test]
    fn a_reached_limit_holds_her_own_paid_work_and_says_so_once() {
        let (c, _) = costs();
        let told = pauses(&c);
        let d = day("2026-11-14");
        // No limit: nothing is held, however much is spent.
        c.record_at(d, paid(500.0));
        assert!(c.unattended_allowed_at(d));
        assert_eq!(c.pause(d).unwrap(), None);

        c.set_settings(&limits(1000.0, 0.0), "RUB");
        // 99%: a warning has come, nothing is held yet.
        c.record_at(d, paid(490.0));
        assert!(c.unattended_allowed_at(d));
        assert!(told.lock().is_empty());
        // 100%: held, and told at the first refusal only.
        c.record_at(d, paid(10.0));
        assert_eq!(
            c.pause(d).unwrap(),
            Some(BudgetPause {
                period: BudgetPeriod::Day,
                spent: 1000.0,
                limit: 1000.0,
                currency: "RUB".into(),
            })
        );
        // Asking what holds tells nobody.
        assert!(told.lock().is_empty());
        for _ in 0..5 {
            assert!(!c.unattended_allowed_at(d));
        }
        assert_eq!(*told.lock(), [(BudgetPeriod::Day, 1000.0, 1000.0)]);
        // The next day is free again, and its own pause is told again.
        let next = day("2026-11-15");
        assert!(c.unattended_allowed_at(next));
        c.record_at(next, paid(1000.0));
        assert!(!c.unattended_allowed_at(next));
        assert!(!c.unattended_allowed_at(next));
        assert_eq!(told.lock().len(), 2);
    }

    #[test]
    fn the_month_holds_longer_than_the_day_and_is_told_once_a_month() {
        let (c, _) = costs();
        let told = pauses(&c);
        c.set_settings(&limits(10.0, 30.0), "RUB");
        let d = day("2026-11-14");
        c.record_at(d, paid(30.0));
        // Both limits are reached: the month is the one named.
        assert_eq!(c.pause(d).unwrap().unwrap().period, BudgetPeriod::Month);
        assert!(!c.unattended_allowed_at(d));
        assert_eq!(*told.lock(), [(BudgetPeriod::Month, 30.0, 30.0)]);
        // The days after it: still held by the month, and nothing new is said.
        for next in ["2026-11-15", "2026-11-30"] {
            assert!(!c.unattended_allowed_at(day(next)));
        }
        assert_eq!(told.lock().len(), 1);
        // A new month is free.
        assert!(c.unattended_allowed_at(day("2026-12-01")));
    }

    #[test]
    fn the_pause_follows_its_switch_the_counting_and_the_limit() {
        let (c, db) = costs();
        let told = pauses(&c);
        let d = day("2026-11-14");
        c.set_settings(&limits(10.0, 0.0), "RUB");
        c.record_at(d, paid(10.0));
        assert!(!c.unattended_allowed_at(d));
        assert_eq!(told.lock().len(), 1);

        // The user switched the pause off: the limit only warns again.
        let mut cfg = limits(10.0, 0.0);
        cfg.pause_unattended = false;
        c.set_settings(&cfg, "RUB");
        assert!(c.unattended_allowed_at(d));
        assert_eq!(c.pause(d).unwrap(), None);
        // Counting off: nothing is known of the spending, nothing is held.
        let mut cfg = limits(10.0, 0.0);
        cfg.enabled = false;
        c.set_settings(&cfg, "RUB");
        assert!(c.unattended_allowed_at(d));

        // The limit was raised above the spending: free, and the mark of the
        // told pause goes, so the same day tells again when it is reached.
        c.set_settings(&limits(50.0, 0.0), "RUB");
        c.rearm(d).unwrap();
        assert!(c.unattended_allowed_at(d));
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM cost_alerts WHERE period LIKE 'pause:%'"
            ),
            0
        );
        c.record_at(d, paid(40.0));
        assert!(!c.unattended_allowed_at(d));
        assert_eq!(told.lock().len(), 2);
        // A restart does not tell it again: the mark is in the database.
        let again = Costs::new(db.clone());
        again.set_settings(&limits(50.0, 0.0), "RUB");
        let told_again = pauses(&again);
        assert!(!again.unattended_allowed_at(d));
        assert!(told_again.lock().is_empty());
    }

    #[test]
    fn money_in_another_currency_holds_nothing_and_old_marks_go() {
        let (c, db) = costs();
        c.set_settings(&limits(10.0, 0.0), "RUB");
        let d = day("2026-11-14");
        // Dollars are not counted in a rouble budget.
        c.record_at(d, chat(OPENAI, "gpt", 100, 50).cost(Some(500.0)));
        assert!(c.unattended_allowed_at(d));
        c.record_at(d, paid(10.0));
        assert!(!c.unattended_allowed_at(d));
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM cost_alerts WHERE period = 'pause:day:2026-11-14'"
            ),
            1
        );
        // The mark of a day that is over goes with its warning.
        c.record_at(day("2026-11-15"), paid(1.0));
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM cost_alerts WHERE period LIKE 'pause:%'"
            ),
            0
        );
    }
}
