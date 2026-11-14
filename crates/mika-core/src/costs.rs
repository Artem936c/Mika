//! What paid provider calls cost (decision 0028).
//!
//! An adapter reports every paid call with `record`. The tracker counts the
//! units (tokens, characters, seconds, pictures) and, where a price is known,
//! the money: from the provider's own answer, then from its model list, then
//! from a price the user typed in. Without a price only the units are kept.
//!
//! Only numbers, the model name and the server name are stored. A `PaidCall`
//! carries no text of a request or an answer, no key and no file name.

mod prices;
mod store;
mod tracker;

use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

use crate::storage::Db;

pub use prices::{PriceSource, UnitPrices};
pub use tracker::{AlertFn, Costs, PauseFn};

/// Days of spending kept.
pub const KEEP_DAYS: i64 = 400;
/// A provider's price list is read again after this many seconds.
pub const PRICES_TTL_S: i64 = 86_400;
/// After a failed read of a price list the next try waits this many seconds.
pub const PRICES_RETRY_S: i64 = 3_600;

/// What a paid call was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostKind {
    Chat,
    Vision,
    Speech,
    Transcription,
    Image,
    Embedding,
}

impl CostKind {
    pub const ALL: [CostKind; 6] = [
        CostKind::Chat,
        CostKind::Vision,
        CostKind::Speech,
        CostKind::Transcription,
        CostKind::Image,
        CostKind::Embedding,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CostKind::Chat => "chat",
            CostKind::Vision => "vision",
            CostKind::Speech => "speech",
            CostKind::Transcription => "transcription",
            CostKind::Image => "image",
            CostKind::Embedding => "embedding",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// The kind in words, for tool texts.
    pub fn title(self) -> &'static str {
        match self {
            CostKind::Chat => "ответы модели",
            CostKind::Vision => "зрение",
            CostKind::Speech => "озвучка",
            CostKind::Transcription => "распознавание речи",
            CostKind::Image => "картинки",
            CostKind::Embedding => "эмбеддинги",
        }
    }

    /// Kinds whose unit is the token: without `usage` nothing is known of them.
    fn by_tokens(self) -> bool {
        matches!(
            self,
            CostKind::Chat | CostKind::Vision | CostKind::Embedding
        )
    }
}

/// Tokens of one request. `input` excludes `cached`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Tokens {
    pub input: u64,
    pub cached: u64,
    pub output: u64,
}

impl Tokens {
    pub fn total(self) -> u64 {
        self.input + self.cached + self.output
    }
}

/// One metered call to a paid endpoint.
#[derive(Debug, Clone, PartialEq)]
pub struct PaidCall {
    pub kind: CostKind,
    pub base_url: String,
    pub model: String,
    pub tokens: Option<Tokens>,
    pub chars: u64,
    pub seconds: f64,
    pub images: u64,
    /// Money the provider itself reported for this request, in its currency.
    pub cost: Option<f64>,
    /// The provider takes no money: Ollama on this computer or in the home network.
    pub free: bool,
}

impl PaidCall {
    pub fn new(kind: CostKind, base_url: &str, model: &str) -> Self {
        Self {
            kind,
            base_url: base_url.to_string(),
            model: model.to_string(),
            tokens: None,
            chars: 0,
            seconds: 0.0,
            images: 0,
            cost: None,
            free: false,
        }
    }

    pub fn tokens(mut self, tokens: Tokens) -> Self {
        self.tokens = Some(tokens);
        self
    }

    pub fn chars(mut self, chars: u64) -> Self {
        self.chars = chars;
        self
    }

    pub fn seconds(mut self, seconds: f64) -> Self {
        self.seconds = if seconds.is_finite() {
            seconds.max(0.0)
        } else {
            0.0
        };
        self
    }

    pub fn images(mut self, images: u64) -> Self {
        self.images = images;
        self
    }

    /// Negative and non-finite values are dropped.
    pub fn cost(mut self, cost: Option<f64>) -> Self {
        self.cost = cost.filter(|c| c.is_finite() && *c >= 0.0);
        self
    }

    pub fn free(mut self, free: bool) -> Self {
        self.free = free;
        self
    }

    /// Nothing is known of the size of the request: no usage for a kind
    /// counted in tokens; no characters, seconds or pictures for a kind
    /// counted in them (a sound that is not WAV, with no length in the
    /// answer). Such a request is counted, and no price is put on it.
    pub fn blind(&self) -> bool {
        self.tokens.is_none()
            && match self.kind {
                CostKind::Chat | CostKind::Vision | CostKind::Embedding => true,
                CostKind::Speech => self.chars == 0,
                CostKind::Transcription => self.seconds <= 0.0,
                CostKind::Image => self.images == 0,
            }
    }
}

/// `[costs]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CostsConfig {
    /// Count paid calls. Nothing is sent anywhere; only numbers are kept.
    pub enabled: bool,
    /// Budget for a day in `currency`; 0 means no limit.
    pub day_limit: f64,
    /// Budget for a calendar month; 0 means no limit.
    pub month_limit: f64,
    /// `RUB`, `USD` or `EUR`. Empty: the currency of the LLM provider's address.
    pub currency: String,
    /// Prices the user typed in for models whose provider lists none.
    pub prices: Vec<ManualPrice>,
    /// With a limit reached, paid work she starts by herself waits: scenario
    /// runs, scheduled tasks, the summaries. What the user asks for goes on.
    pub pause_unattended: bool,
}

impl Default for CostsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            day_limit: 0.0,
            month_limit: 0.0,
            currency: String::new(),
            prices: Vec::new(),
            pause_unattended: true,
        }
    }
}

impl CostsConfig {
    /// A budget holds the paid work she starts by herself, or will once a
    /// limit is reached: counting is on, the pause is on and a limit is set.
    pub fn holds_unattended(&self) -> bool {
        self.enabled && self.pause_unattended && (self.day_limit > 0.0 || self.month_limit > 0.0)
    }
}

/// A price the user typed in. A price of 0 means "not set".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ManualPrice {
    /// Server name, as `provider_host` returns it.
    pub provider: String,
    pub model: String,
    /// `RUB`, `USD` or `EUR`.
    pub currency: String,
    /// Per million input tokens.
    pub input_mtok: f64,
    /// Per million output tokens.
    pub output_mtok: f64,
    /// Per thousand characters of spoken text.
    pub per_kchar: f64,
    /// Per minute of recognised sound.
    pub per_minute: f64,
    pub per_image: f64,
}

/// The server name of an address: no scheme, user, path, query or fragment;
/// lower case; the port is kept.
pub fn provider_host(base_url: &str) -> String {
    let s = base_url.trim();
    let s = s.split_once("://").map_or(s, |(_, rest)| rest);
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit_once('@').map_or(s, |(_, host)| host);
    s.to_lowercase()
}

/// A server on this computer: its calls cost nothing.
pub fn is_local(host: &str) -> bool {
    let h = host.trim().to_lowercase();
    if h == "::1" || h.starts_with("[::1]") {
        return true;
    }
    let name = h.split(':').next().unwrap_or("");
    name == "localhost"
        || name
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// A server on this computer or in the home network: `localhost`, a private
/// or link-local IPv4 address, a name without a dot, a name ending in
/// `.local`. An Ollama server there takes no money; one at a public address
/// may be a paid service.
pub fn is_private(host: &str) -> bool {
    if is_local(host) {
        return true;
    }
    let h = host.trim().to_lowercase();
    if h.starts_with('[') {
        return false;
    }
    let name = h.split(':').next().unwrap_or("");
    if let Ok(ip) = name.parse::<std::net::Ipv4Addr>() {
        return ip.is_private() || ip.is_link_local();
    }
    !name.is_empty() && (!name.contains('.') || name.ends_with(".local"))
}

/// The currency a provider counts in, when its address tells: `RUB`, `USD` or empty.
pub fn currency_code(base_url: &str) -> &'static str {
    let host = provider_host(base_url);
    if host.contains("routerai.ru") {
        "RUB"
    } else if ["openrouter.ai", "api.openai.com", "anthropic.com"]
        .iter()
        .any(|h| host.contains(h))
    {
        "USD"
    } else {
        ""
    }
}

pub fn currency_sign(code: &str) -> &'static str {
    match code {
        "RUB" => "₽",
        "USD" => "$",
        "EUR" => "€",
        _ => "",
    }
}

/// Digits in groups of three, separated by a no-break space: `2 000`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{a0}');
        }
        out.push(c);
    }
    out
}

/// Money for people: `12,40 ₽`, `100 ₽`, `2 000 $`, `0,0031 ₽`, `меньше 0,0001 ₽`.
/// An unknown currency gets no sign.
pub fn money_text(cost: f64, currency: &str) -> String {
    let sign = currency_sign(currency);
    let signed = |number: String| {
        if sign.is_empty() {
            number
        } else {
            format!("{number}\u{a0}{sign}")
        }
    };
    let cost = if cost.is_finite() { cost.max(0.0) } else { 0.0 };
    if cost == 0.0 {
        return signed("0".into());
    }
    if cost < 0.99995 {
        // Below one: up to four decimals, at least two.
        let mut number = format!("{cost:.4}");
        if number == "0.0000" {
            return format!("меньше {}", signed("0,0001".into()));
        }
        while number.ends_with('0') && number.len() > "0.00".len() {
            number.pop();
        }
        return signed(number.replace('.', ","));
    }
    let cents = (cost * 100.0).round() as u64;
    let (whole, part) = (cents / 100, cents % 100);
    if part == 0 {
        signed(grouped(whole))
    } else {
        signed(format!("{},{part:02}", grouped(whole)))
    }
}

/// `840 ток.`, `3,1 тыс. токенов`, `310 тыс. токенов`, `1,2 млн токенов`.
fn count_text(n: u64, few: &str, many: &str) -> String {
    if n < 1000 {
        return format!("{n} {few}");
    }
    let scaled = |unit: u128| {
        let n = n as u128;
        let tenths = (n * 10 + unit / 2) / unit;
        if tenths >= 100 {
            ((n + unit / 2) / unit).to_string()
        } else if tenths.is_multiple_of(10) {
            (tenths / 10).to_string()
        } else {
            format!("{},{}", tenths / 10, tenths % 10)
        }
    };
    if n < 999_500 {
        format!("{} тыс. {many}", scaled(1000))
    } else {
        format!("{} млн {many}", scaled(1_000_000))
    }
}

/// The units of a line for people. `tokens` is input, cached and output together.
pub fn units_text(kind: CostKind, tokens: u64, chars: u64, seconds: f64, images: u64) -> String {
    match kind {
        CostKind::Chat | CostKind::Vision | CostKind::Embedding => {
            if tokens == 0 {
                "токены не сообщены".into()
            } else {
                count_text(tokens, "ток.", "токенов")
            }
        }
        CostKind::Speech => count_text(chars, "зн.", "знаков"),
        CostKind::Transcription => {
            let s = if seconds.is_finite() {
                seconds.max(0.0).round() as u64
            } else {
                0
            };
            match (s / 3600, s % 3600 / 60, s % 60) {
                (0, 0, 0) => "длительность не сообщена".into(),
                (0, 0, s) => format!("{s} с"),
                (0, m, s) => format!("{m} мин {s:02} с"),
                (h, m, _) => format!("{h} ч {m:02} мин"),
            }
        }
        CostKind::Image => format!("{images} шт."),
    }
}

/// Money in one currency.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Money {
    /// `RUB`, `USD`, `EUR`, or empty when the currency is unknown.
    pub currency: String,
    pub cost: f64,
    /// `money_text(cost, currency)`.
    pub text: String,
}

impl Money {
    pub fn new(cost: f64, currency: &str) -> Self {
        Self {
            currency: currency.to_string(),
            cost,
            text: money_text(cost, currency),
        }
    }
}

/// One line of a report: a kind, or a model of a kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageLine {
    pub kind: CostKind,
    /// Empty in a line of `CostReport::kinds`.
    pub provider: String,
    /// Empty in a line of `CostReport::kinds`.
    pub model: String,
    pub requests: u64,
    /// Requests of unknown size: the provider reported no usage, or the
    /// length of the sound is not known.
    pub blind: u64,
    /// Requests with units and no price.
    pub unpriced: u64,
    pub tokens_in: u64,
    pub tokens_cached: u64,
    pub tokens_out: u64,
    pub chars: u64,
    pub seconds: f64,
    pub images: u64,
    /// `units_text` of the sums.
    pub units: String,
    /// Non-zero money, the main currency first.
    pub cost: Vec<Money>,
    /// Every request has its money, and the money is zero.
    pub free: bool,
    pub price: PriceSource,
}

/// Spending from `from` to `to`, both days included.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostReport {
    pub from: String,
    pub to: String,
    /// Non-zero money, the main currency first.
    pub totals: Vec<Money>,
    /// One line per kind that has requests, in `CostKind::ALL` order.
    pub kinds: Vec<UsageLine>,
    /// One line per kind, provider and model; the most expensive first.
    pub models: Vec<UsageLine>,
    pub requests: u64,
    pub unpriced: u64,
    pub blind: u64,
    /// There are requests, every one has its money, and the money is zero:
    /// a local model. Not the same as «the price is unknown».
    pub free: bool,
}

/// Where the budget stands today, in the main currency.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BudgetState {
    pub currency: String,
    pub day_spent: f64,
    /// 0 means no limit.
    pub day_limit: f64,
    /// Percent of the limit; 0 without one; may exceed 100.
    pub day_share: u32,
    pub month_spent: f64,
    pub month_limit: f64,
    pub month_share: u32,
    /// `12,40 ₽ из 100 ₽`, or `12,40 ₽` without a limit.
    pub day_text: String,
    pub month_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetPeriod {
    Day,
    Month,
}

/// The spending crossed a threshold of a limit. What the user asks for is
/// never blocked by it; see `BudgetPause` for what a reached limit holds.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetAlert {
    pub period: BudgetPeriod,
    /// 80 or 100.
    pub level: u8,
    pub spent: f64,
    pub limit: f64,
    pub currency: String,
}

/// A limit is reached and paid work she starts by herself waits for the end
/// of its period: scenario runs, scheduled tasks, the summaries by schedule.
/// Everything the user starts goes on.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetPause {
    /// The period that holds longest: the month when both limits are reached.
    pub period: BudgetPeriod,
    pub spent: f64,
    pub limit: f64,
    pub currency: String,
}

/// The tracker every adapter reports to. One per program: so the signatures of
/// the adapters stay as they are.
static SINK: RwLock<Option<Costs>> = RwLock::new(None);

/// Makes `costs` the tracker of this program, in place of the previous one.
pub fn install(costs: Costs) {
    *SINK.write() = Some(costs);
}

pub fn installed() -> Option<Costs> {
    SINK.read().clone()
}

/// The installed tracker, or a new one over `db` when none is installed.
pub fn installed_or(db: Arc<Db>) -> Costs {
    installed().unwrap_or_else(|| Costs::new(db))
}

/// Reports a paid call. Does nothing until a tracker is installed.
pub fn record(call: PaidCall) {
    if let Some(costs) = installed() {
        costs.record(call);
    }
}

/// The one check of every place where she starts paid work by herself: a
/// scenario run, a scheduled task, a summary by schedule. True when no
/// tracker is installed, tracking is off, `pause_unattended` is off, no limit
/// is set or none is reached. At the first refusal of a period the tracker
/// tells the user (`Costs::on_pause`); the later ones are silent.
///
/// Never asked for what the user starts: the chat, the voice, a hotkey, a
/// message of the owner in Telegram.
pub fn unattended_allowed() -> bool {
    installed().is_none_or(|costs| costs.unattended_allowed_at(today()))
}

/// The local day a call made now is counted on.
pub fn today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_have_names_and_parse_back() {
        for k in CostKind::ALL {
            assert_eq!(CostKind::parse(k.as_str()), Some(k));
            assert!(!k.title().is_empty());
        }
        assert_eq!(CostKind::parse("video"), None);
        assert_eq!(
            serde_json::to_value(CostKind::Transcription).unwrap(),
            "transcription"
        );
    }

    #[test]
    fn a_call_keeps_only_sane_numbers() {
        let c = PaidCall::new(CostKind::Chat, "https://routerai.ru/api/v1", "m");
        assert!(c.blind() && c.cost.is_none() && !c.free);
        assert!(!c.clone().tokens(Tokens::default()).blind());
        assert_eq!(c.clone().cost(Some(-1.0)).cost, None);
        assert_eq!(c.clone().cost(Some(f64::NAN)).cost, None);
        assert_eq!(c.clone().cost(Some(0.5)).cost, Some(0.5));
        assert_eq!(c.clone().seconds(f64::INFINITY).seconds, 0.0);
        // Speech, recognition and pictures have units of their own; without
        // them and without tokens nothing is known of the request either.
        let unit = |kind| PaidCall::new(kind, "", "m");
        for kind in [CostKind::Speech, CostKind::Transcription, CostKind::Image] {
            assert!(unit(kind).blind());
            assert!(!unit(kind).tokens(Tokens::default()).blind());
        }
        assert!(!unit(CostKind::Speech).chars(1).blind());
        assert!(!unit(CostKind::Transcription).seconds(0.5).blind());
        assert!(!unit(CostKind::Image).images(1).blind());
    }

    #[test]
    fn the_server_name_drops_everything_else_of_the_address() {
        assert_eq!(
            provider_host("https://u:p@RouterAI.ru/api/v1?x=1"),
            "routerai.ru"
        );
        assert_eq!(
            provider_host("http://localhost:11434/v1"),
            "localhost:11434"
        );
        assert_eq!(
            provider_host(" https://api.openai.com/v1/ "),
            "api.openai.com"
        );
        assert_eq!(provider_host("Example.test#frag"), "example.test");
        assert_eq!(provider_host(""), "");
    }

    #[test]
    fn local_servers_are_told_from_the_rest() {
        for h in [
            "localhost",
            "localhost:11434",
            "127.0.0.1",
            "127.0.0.1:8080",
            "127.1.2.3",
            "[::1]",
            "[::1]:11434",
            "::1",
        ] {
            assert!(is_local(h), "{h}");
        }
        for h in [
            "routerai.ru",
            "127.example.com",
            "10.0.0.1",
            "",
            "mylocalhost",
        ] {
            assert!(!is_local(h), "{h}");
        }
    }

    #[test]
    fn the_home_network_is_told_from_the_internet() {
        for h in [
            "localhost:11434",
            "127.0.0.1",
            "[::1]:11434",
            "192.168.1.20:11434",
            "10.0.0.5",
            "172.16.4.1:8080",
            "169.254.10.10",
            "nas:11434",
            "desktop.local",
        ] {
            assert!(is_private(h), "{h}");
        }
        for h in [
            "",
            "ollama.example.com",
            "ollama.example.com:11434",
            "8.8.8.8",
            "172.32.0.1",
            "routerai.ru",
            "[2001:db8::1]:11434",
        ] {
            assert!(!is_private(h), "{h}");
        }
    }

    #[test]
    fn the_currency_follows_the_address() {
        assert_eq!(currency_code("https://routerai.ru/api/v1"), "RUB");
        assert_eq!(currency_code("https://openrouter.ai/api/v1"), "USD");
        assert_eq!(currency_code("https://api.openai.com/v1"), "USD");
        assert_eq!(currency_code("https://api.anthropic.com"), "USD");
        assert_eq!(currency_code("http://localhost:11434/v1"), "");
        // The server name decides, not the path.
        assert_eq!(currency_code("https://example.test/routerai.ru"), "");
        assert_eq!(currency_code("routerai.ru"), "RUB");
        assert_eq!(currency_sign("RUB"), "₽");
        assert_eq!(currency_sign("USD"), "$");
        assert_eq!(currency_sign("EUR"), "€");
        assert_eq!(currency_sign(""), "");
        assert_eq!(currency_sign("GBP"), "");
    }

    #[test]
    fn money_reads_as_people_write_it() {
        assert_eq!(money_text(0.0, "RUB"), "0\u{a0}₽");
        assert_eq!(money_text(12.4, "RUB"), "12,40\u{a0}₽");
        assert_eq!(money_text(100.0, "RUB"), "100\u{a0}₽");
        assert_eq!(money_text(2000.0, "USD"), "2\u{a0}000\u{a0}$");
        assert_eq!(
            money_text(1234567.5, "EUR"),
            "1\u{a0}234\u{a0}567,50\u{a0}€"
        );
        assert_eq!(money_text(0.5, "RUB"), "0,50\u{a0}₽");
        assert_eq!(money_text(0.0031, "RUB"), "0,0031\u{a0}₽");
        assert_eq!(money_text(0.00004, "RUB"), "меньше 0,0001\u{a0}₽");
        assert_eq!(money_text(3.2, ""), "3,20");
        assert_eq!(money_text(0.99999, "RUB"), "1\u{a0}₽");
        assert_eq!(money_text(-5.0, "RUB"), "0\u{a0}₽");
    }

    #[test]
    fn units_read_as_people_write_them() {
        use CostKind::*;
        assert_eq!(units_text(Chat, 840, 0, 0.0, 0), "840 ток.");
        assert_eq!(units_text(Chat, 3100, 0, 0.0, 0), "3,1 тыс. токенов");
        assert_eq!(units_text(Vision, 310_000, 0, 0.0, 0), "310 тыс. токенов");
        assert_eq!(
            units_text(Embedding, 1_200_000, 0, 0.0, 0),
            "1,2 млн токенов"
        );
        assert_eq!(units_text(Chat, 3000, 0, 0.0, 0), "3 тыс. токенов");
        assert_eq!(units_text(Chat, 999_900, 0, 0.0, 0), "1 млн токенов");
        assert_eq!(units_text(Chat, 0, 0, 0.0, 0), "токены не сообщены");
        assert_eq!(units_text(Speech, 0, 840, 0.0, 0), "840 зн.");
        assert_eq!(units_text(Speech, 0, 2900, 0.0, 0), "2,9 тыс. знаков");
        assert_eq!(units_text(Transcription, 0, 0, 45.2, 0), "45 с");
        assert_eq!(
            units_text(Transcription, 0, 0, 0.0, 0),
            "длительность не сообщена"
        );
        assert_eq!(units_text(Transcription, 0, 0, 250.0, 0), "4 мин 10 с");
        assert_eq!(units_text(Transcription, 0, 0, 3900.0, 0), "1 ч 05 мин");
        assert_eq!(units_text(Image, 0, 0, 0.0, 3), "3 шт.");
    }

    #[test]
    fn settings_start_on_without_limits() {
        let c = CostsConfig::default();
        assert!(c.enabled && c.day_limit == 0.0 && c.month_limit == 0.0);
        assert!(c.currency.is_empty() && c.prices.is_empty());
        // A reached limit holds her own paid work unless the user says otherwise.
        assert!(c.pause_unattended);
        // A settings file written before the switch existed reads with it on.
        let old: CostsConfig = toml::from_str("day_limit = 5.0").unwrap();
        assert!(old.pause_unattended && old.day_limit == 5.0);
    }

    #[test]
    fn the_sink_takes_calls_only_once_a_tracker_is_installed() {
        // The only test of this crate that installs a tracker: the sink is one
        // per process.
        let call = || {
            PaidCall::new(CostKind::Chat, "http://localhost:1/v1", "sink-test").tokens(Tokens {
                input: 10,
                cached: 0,
                output: 5,
            })
        };
        assert!(installed().is_none());
        // Without a tracker nothing holds her own paid work.
        assert!(unattended_allowed());
        record(call());
        let db = Arc::new(Db::open_in_memory().unwrap());
        let fresh = installed_or(db.clone());
        assert!(installed().is_none(), "installed_or installs nothing");
        install(fresh.clone());
        record(call());
        let today = chrono::Local::now().date_naive();
        let report = installed().unwrap().report(today, today).unwrap();
        assert_eq!(report.requests, 1);
        assert_eq!(installed_or(db).report(today, today).unwrap().requests, 1);
        // The installed tracker is the one that is asked: no limit, no pause;
        // a limit of the day that the spending has reached holds, and says
        // so once.
        assert!(unattended_allowed());
        let told = Arc::new(parking_lot::Mutex::new(0));
        let seen = told.clone();
        fresh.on_pause(Arc::new(move |_| *seen.lock() += 1));
        fresh.set_settings(
            &CostsConfig {
                day_limit: 1.0,
                ..Default::default()
            },
            "RUB",
        );
        fresh.record(
            PaidCall::new(CostKind::Chat, "https://routerai.ru/api/v1", "sink-test")
                .tokens(Tokens::default())
                .cost(Some(2.0)),
        );
        assert!(!unattended_allowed());
        assert!(!unattended_allowed());
        assert_eq!(*told.lock(), 1);
        // Left as it was found: other tests of this process are not held.
        fresh.set_settings(&CostsConfig::default(), "RUB");
        assert!(unattended_allowed());
    }
}
