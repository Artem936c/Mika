//! What paid providers cost (decision 0028): the tool that answers «сколько
//! потрачено». It only reads the tracker of `mika_core::costs`: numbers, model
//! names and the server name of a provider.

use async_trait::async_trait;
use chrono::{Datelike, Duration, Local, NaiveDate};
use mika_core::costs::{BudgetPeriod, BudgetState, CostReport, Costs, Money, UsageLine};
use mika_core::gate::Approval;
use mika_core::llm::ToolSpec;
use mika_core::tools::{arg_str_opt, spec, Assessment, RiskLevel, Tool, ToolError, ToolOutput};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::util::{failed, schema};

/// Model lines in the text for the model.
const MAX_MODELS: usize = 8;
/// Models named in the note about calls without a price.
const MAX_UNPRICED_NAMES: usize = 3;

/// The days a period covers, ending today, and its name in a sentence.
pub fn period(name: &str, today: NaiveDate) -> Option<(NaiveDate, NaiveDate, &'static str)> {
    Some(match name.trim() {
        "" | "today" => (today, today, "сегодня"),
        "yesterday" => {
            let y = today - Duration::days(1);
            (y, y, "вчера")
        }
        "week" => (today - Duration::days(6), today, "за неделю"),
        "month" => (today.with_day(1).unwrap_or(today), today, "за месяц"),
        _ => return None,
    })
}

/// `9,10 ₽ + 0,42 $`, or nothing.
fn money_of(cost: &[Money], joint: &str) -> String {
    cost.iter()
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join(joint)
}

/// The money part of a line: the money, `бесплатно`, or nothing when only
/// the units are known.
fn price_part(line: &UsageLine) -> Option<String> {
    if !line.cost.is_empty() {
        Some(money_of(&line.cost, " + "))
    } else if line.free {
        Some("бесплатно".into())
    } else {
        None
    }
}

/// What a reached limit holds back, in a line for the model: without it
/// «почему сценарий не сработал» would have no answer.
fn pause_note(period: BudgetPeriod) -> &'static str {
    match period {
        BudgetPeriod::Day => "Дневной лимит достигнут: платную работу, которую помощница начинает сама (сценарии по событию, задачи по расписанию, сводки, ответы в чатах за пользователя), она до завтра не начинает. Просьбы пользователя выполняются как обычно.",
        BudgetPeriod::Month => "Месячный лимит достигнут: платную работу, которую помощница начинает сама (сценарии по событию, задачи по расписанию, сводки, ответы в чатах за пользователя), она до следующего месяца не начинает. Просьбы пользователя выполняются как обычно.",
    }
}

/// The report in lines for the model.
fn report_text(report: &CostReport, budget: &BudgetState, label: &str) -> String {
    let mut lines = Vec::new();
    lines.push(if report.free {
        // A local model: nothing was spent, which is not «the price is unknown».
        format!("Расходы {label}: ничего не потрачено, все вызовы бесплатные.")
    } else if report.totals.is_empty() {
        format!("Расходы {label}: цена неизвестна, есть только единицы.")
    } else {
        format!("Расходы {label}: {}.", money_of(&report.totals, " и "))
    });
    // The budget is about today and this month, whatever period was asked for.
    if budget.day_limit > 0.0 {
        lines.push(format!(
            "Бюджет на день: {} ({}%).",
            budget.day_text, budget.day_share
        ));
    }
    if budget.month_limit > 0.0 {
        lines.push(format!(
            "Бюджет на месяц: {} ({}%).",
            budget.month_text, budget.month_share
        ));
    }
    lines.push("По видам:".into());
    for line in &report.kinds {
        let mut parts = Vec::new();
        parts.extend(price_part(line));
        parts.push(format!("{} запр.", line.requests));
        parts.push(line.units.clone());
        lines.push(format!("{} — {}", line.kind.title(), parts.join(", ")));
    }
    lines.push("По моделям:".into());
    for line in report.models.iter().take(MAX_MODELS) {
        let mut parts = Vec::new();
        parts.extend(price_part(line));
        parts.push(line.units.clone());
        lines.push(format!(
            "{} ({}) — {}",
            line.model,
            line.provider,
            parts.join(", ")
        ));
    }
    if report.models.len() > MAX_MODELS {
        lines.push(format!("…и ещё {}", report.models.len() - MAX_MODELS));
    }
    if report.unpriced > 0 {
        let mut names: Vec<&str> = Vec::new();
        for line in report.models.iter().filter(|l| l.unpriced > 0) {
            if !names.contains(&line.model.as_str()) {
                names.push(&line.model);
            }
        }
        let more = names.len() > MAX_UNPRICED_NAMES;
        names.truncate(MAX_UNPRICED_NAMES);
        lines.push(format!(
            "Без цены: {} запр. ({}{}). Цену можно указать в настройках, раздел «Расходы».",
            report.unpriced,
            names.join(", "),
            if more { " и другие" } else { "" }
        ));
    }
    if report.blind > 0 {
        lines.push(format!(
            "Без данных об объёме (провайдер не сообщил токены или длительность): {} запр.",
            report.blind
        ));
    }
    lines.join("\n")
}

/// The chat card: the totals, the budget and a row per kind.
fn report_card(report: &CostReport, budget: &BudgetState, label: &str) -> Value {
    let kinds: Vec<Value> = report
        .kinds
        .iter()
        .map(|line| {
            json!({
                "kind": line.kind,
                "name": line.kind.title(),
                "money": money_of(&line.cost, " + "),
                "units": line.units,
                "requests": line.requests,
                "free": line.free,
            })
        })
        .collect();
    json!({
        "kind": "cost_report",
        "label": label,
        "totals": report.totals,
        "budget": budget,
        "kinds": kinds,
        "unpriced": report.unpriced,
        "blind": report.blind,
        "free": report.free,
    })
}

pub struct CostReportTool(pub Costs);

#[async_trait]
impl Tool for CostReportTool {
    fn spec(&self) -> ToolSpec {
        spec(
            "cost_report",
            "Расходы на платных провайдеров (ответы модели, зрение, озвучка, распознавание, картинки, эмбеддинги): всего, по видам и моделям, и бюджет. Вызывай, когда спрашивают, сколько потрачено или сколько стоит работа помощницы.",
            schema(
                &[(
                    "period",
                    "string",
                    "one of: today, yesterday, week, month",
                )],
                &[],
            ),
        )
    }

    fn assess(&self, args: &Value) -> Result<Assessment, ToolError> {
        let name = arg_str_opt(args, "period").unwrap_or("");
        if period(name, Local::now().date_naive()).is_none() {
            return Err(ToolError::BadArguments(format!(
                "неизвестный период «{name}»: today, yesterday, week, month"
            )));
        }
        Ok(Assessment::new(
            RiskLevel::Low,
            "Посмотреть расходы на провайдеров",
            "учёт расходов",
            "только чтение",
        ))
    }

    async fn run(
        &self,
        args: Value,
        _a: Approval,
        _c: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        // Turned off, the counted spending is not read either.
        if !self.0.enabled() {
            return Ok(ToolOutput::new(
                "Учёт расходов выключен: включить можно в настройках, раздел «Расходы».",
            ));
        }
        let today = Local::now().date_naive();
        let name = arg_str_opt(&args, "period").unwrap_or("");
        let (from, to, label) = period(name, today).unwrap_or((today, today, "сегодня"));
        let costs = self.0.clone();
        let (report, budget, pause) = tokio::task::spawn_blocking(move || {
            Ok::<_, mika_core::storage::DbError>((
                costs.report(from, to)?,
                costs.budget(today)?,
                costs.pause(today)?,
            ))
        })
        .await
        .map_err(|e| failed(format!("учёт расходов недоступен: {e}")))?
        .map_err(|e| failed(format!("учёт расходов недоступен: {e}")))?;
        if report.requests == 0 {
            return Ok(ToolOutput::new(format!("Расходов {label} нет.")));
        }
        let mut text = report_text(&report, &budget, label);
        if let Some(pause) = pause {
            text.push('\n');
            text.push_str(pause_note(pause.period));
        }
        Ok(ToolOutput::new(text).with_data(report_card(&report, &budget, label)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::run;
    use mika_core::costs::{CostKind, CostsConfig, PaidCall, Tokens};
    use mika_core::storage::Db;
    use std::sync::Arc;

    const ROUTER: &str = "https://routerai.ru/api/v1";

    fn costs() -> (Costs, Arc<Db>) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let c = Costs::new(db.clone());
        c.set_settings(&CostsConfig::default(), "RUB");
        (c, db)
    }

    fn chat(base: &str, model: &str, input: u64, output: u64) -> PaidCall {
        PaidCall::new(CostKind::Chat, base, model).tokens(Tokens {
            input,
            cached: 0,
            output,
        })
    }

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[tokio::test]
    async fn a_reached_limit_says_what_it_holds_back() {
        let (c, _) = costs();
        let tool = CostReportTool(c.clone());
        c.set_settings(
            &CostsConfig {
                day_limit: 10.0,
                ..Default::default()
            },
            "RUB",
        );
        c.record(chat(ROUTER, "m", 100, 50).cost(Some(9.0)));
        let out = run(&tool, json!({})).await.unwrap();
        assert!(!out.text.contains("лимит достигнут"), "{}", out.text);
        c.record(chat(ROUTER, "m", 100, 50).cost(Some(1.0)));
        let out = run(&tool, json!({})).await.unwrap();
        assert!(
            out.text.ends_with(pause_note(BudgetPeriod::Day)),
            "{}",
            out.text
        );
        assert!(out.text.contains("до завтра не начинает"));
        // The user switched the pause off: the limit only warns.
        c.set_settings(
            &CostsConfig {
                day_limit: 10.0,
                pause_unattended: false,
                ..Default::default()
            },
            "RUB",
        );
        let out = run(&tool, json!({})).await.unwrap();
        assert!(!out.text.contains("лимит достигнут"), "{}", out.text);
        assert!(pause_note(BudgetPeriod::Month).contains("до следующего месяца"));
    }

    #[test]
    fn periods_end_today_and_a_month_starts_on_the_first() {
        let today = date("2026-11-14");
        assert_eq!(period("", today), Some((today, today, "сегодня")));
        assert_eq!(period("today", today), Some((today, today, "сегодня")));
        assert_eq!(
            period("yesterday", today),
            Some((date("2026-11-13"), date("2026-11-13"), "вчера"))
        );
        assert_eq!(
            period("week", today),
            Some((date("2026-11-08"), today, "за неделю"))
        );
        assert_eq!(
            period("month", today),
            Some((date("2026-11-01"), today, "за месяц"))
        );
        assert_eq!(
            period("month", date("2026-03-01")).unwrap().0,
            date("2026-03-01")
        );
        assert_eq!(period("год", today), None);
    }

    #[tokio::test]
    async fn the_tool_is_low_risk_and_refuses_an_unknown_period() {
        let (c, _) = costs();
        let tool = CostReportTool(c);
        assert_eq!(tool.assess(&json!({})).unwrap().level, RiskLevel::Low);
        assert!(tool.assess(&json!({"period": "month"})).is_ok());
        let refused = tool.assess(&json!({"period": "год"})).unwrap_err();
        assert!(
            matches!(&refused, ToolError::BadArguments(m)
                if m == "неизвестный период «год»: today, yesterday, week, month"),
            "{refused:?}"
        );
        assert_eq!(tool.spec().name, "cost_report");
        assert_eq!(
            tool.spec().parameters["properties"]["period"]["enum"],
            json!(["today", "yesterday", "week", "month"])
        );
        let out = run(&tool, json!({})).await.unwrap();
        assert_eq!(out.text, "Расходов сегодня нет.");
        assert!(out.data.is_none());
        let out = run(&tool, json!({"period": "week"})).await.unwrap();
        assert_eq!(out.text, "Расходов за неделю нет.");
    }

    #[tokio::test]
    async fn switched_off_it_says_so_and_reads_nothing() {
        let (c, db) = costs();
        c.record(chat(ROUTER, "m", 10, 10).cost(Some(5.0)));
        // Without its table a read would fail.
        db.with(|c| c.execute("DROP TABLE cost_usage", [])).unwrap();
        let tool = CostReportTool(c.clone());
        assert!(run(&tool, json!({})).await.is_err());
        c.set_settings(
            &CostsConfig {
                enabled: false,
                ..Default::default()
            },
            "RUB",
        );
        let out = run(&tool, json!({})).await.unwrap();
        assert_eq!(
            out.text,
            "Учёт расходов выключен: включить можно в настройках, раздел «Расходы»."
        );
        assert!(out.data.is_none());
    }

    #[tokio::test]
    async fn the_report_names_money_budget_kinds_models_and_what_has_no_price() {
        let (c, _) = costs();
        c.set_settings(
            &CostsConfig {
                day_limit: 100.0,
                month_limit: 2000.0,
                ..Default::default()
            },
            "RUB",
        );
        let gemma = "google/gemma-4-26b-a4b-it";
        c.record(chat(ROUTER, gemma, 300_000, 10_000).cost(Some(9.1)));
        c.record(PaidCall::new(CostKind::Chat, ROUTER, gemma));
        c.record(PaidCall::new(CostKind::Chat, ROUTER, gemma));
        c.record(
            PaidCall::new(CostKind::Speech, ROUTER, "fish-audio/drama-3")
                .chars(2900)
                .cost(Some(3.1)),
        );
        for _ in 0..3 {
            c.record(
                PaidCall::new(
                    CostKind::Transcription,
                    "https://api.openai.com/v1",
                    "whisper-1",
                )
                .seconds(185.0),
            );
        }
        c.record(chat("http://localhost:11434/v1", "qwen3", 20, 10));

        let tool = CostReportTool(c.clone());
        let out = run(&tool, json!({"period": "today"})).await.unwrap();
        assert_eq!(
            out.text,
            "Расходы сегодня: 12,20\u{a0}₽.\n\
             Бюджет на день: 12,20\u{a0}₽ из 100\u{a0}₽ (12%).\n\
             Бюджет на месяц: 12,20\u{a0}₽ из 2\u{a0}000\u{a0}₽ (0%).\n\
             По видам:\n\
             ответы модели — 9,10\u{a0}₽, 4 запр., 310 тыс. токенов\n\
             озвучка — 3,10\u{a0}₽, 1 запр., 2,9 тыс. знаков\n\
             распознавание речи — 3 запр., 9 мин 15 с\n\
             По моделям:\n\
             google/gemma-4-26b-a4b-it (routerai.ru) — 9,10\u{a0}₽, 310 тыс. токенов\n\
             fish-audio/drama-3 (routerai.ru) — 3,10\u{a0}₽, 2,9 тыс. знаков\n\
             whisper-1 (api.openai.com) — 9 мин 15 с\n\
             qwen3 (localhost:11434) — бесплатно, 30 ток.\n\
             Без цены: 3 запр. (whisper-1). Цену можно указать в настройках, раздел «Расходы».\n\
             Без данных об объёме (провайдер не сообщил токены или длительность): 2 запр."
        );
        assert_eq!(
            out.data.unwrap(),
            json!({
                "kind": "cost_report",
                "label": "сегодня",
                "totals": [{"currency": "RUB", "cost": 12.2, "text": "12,20\u{a0}₽"}],
                "budget": {
                    "currency": "RUB",
                    "day_spent": 12.2, "day_limit": 100.0, "day_share": 12,
                    "month_spent": 12.2, "month_limit": 2000.0, "month_share": 0,
                    "day_text": "12,20\u{a0}₽ из 100\u{a0}₽",
                    "month_text": "12,20\u{a0}₽ из 2\u{a0}000\u{a0}₽",
                },
                "kinds": [
                    {"kind": "chat", "name": "ответы модели", "money": "9,10\u{a0}₽",
                     "units": "310 тыс. токенов", "requests": 4, "free": false},
                    {"kind": "speech", "name": "озвучка", "money": "3,10\u{a0}₽",
                     "units": "2,9 тыс. знаков", "requests": 1, "free": false},
                    {"kind": "transcription", "name": "распознавание речи", "money": "",
                     "units": "9 мин 15 с", "requests": 3, "free": false},
                ],
                "unpriced": 3,
                "blind": 2,
                "free": false,
            })
        );

        // Yesterday's money is not today's; the budget lines stay about today.
        let yesterday = Local::now().date_naive() - Duration::days(1);
        c.record_at(yesterday, chat(ROUTER, gemma, 10, 10).cost(Some(5.0)));
        let out = run(&tool, json!({"period": "yesterday"})).await.unwrap();
        assert!(
            out.text.starts_with(
                "Расходы вчера: 5\u{a0}₽.\nБюджет на день: 12,20\u{a0}₽ из 100\u{a0}₽ (12%).\n"
            ),
            "{}",
            out.text
        );
        assert_eq!(out.data.unwrap()["label"], "вчера");
        let out = run(&tool, json!({"period": "week"})).await.unwrap();
        assert!(
            out.text.starts_with("Расходы за неделю: 17,20\u{a0}₽.\n"),
            "{}",
            out.text
        );
    }

    #[tokio::test]
    async fn a_day_of_local_models_is_free_not_of_unknown_price() {
        let (c, _) = costs();
        let tool = CostReportTool(c.clone());
        c.record(chat("http://localhost:11434/v1", "qwen3", 800, 40));
        // A stopped stream: no tokens were reported.
        c.record(PaidCall::new(
            CostKind::Chat,
            "http://localhost:11434/v1",
            "qwen3",
        ));
        let out = run(&tool, json!({})).await.unwrap();
        assert!(
            out.text.starts_with(
                "Расходы сегодня: ничего не потрачено, все вызовы бесплатные.\nПо видам:\n\
                 ответы модели — бесплатно, 2 запр., 840 ток.\n\
                 По моделям:\n\
                 qwen3 (localhost:11434) — бесплатно, 840 ток.\n"
            ),
            "{}",
            out.text
        );
        assert!(!out.text.contains("цена неизвестна"), "{}", out.text);
        assert!(!out.text.contains("Без цены"), "{}", out.text);
        let card = out.data.unwrap();
        assert_eq!(card["free"], true);
        assert_eq!(card["totals"], json!([]));
        assert_eq!(card["kinds"][0]["free"], true);

        // One call that waits for a price: the day is no longer «free».
        c.record(chat("https://api.openai.com/v1", "gpt", 10, 10));
        let out = run(&tool, json!({})).await.unwrap();
        assert!(
            out.text
                .starts_with("Расходы сегодня: цена неизвестна, есть только единицы.\n"),
            "{}",
            out.text
        );
        assert_eq!(out.data.unwrap()["free"], false);
    }

    #[tokio::test]
    async fn units_alone_several_currencies_and_a_long_list_of_models() {
        let (c, _) = costs();
        let tool = CostReportTool(c.clone());
        for i in 0..10 {
            c.record(chat(
                "https://api.openai.com/v1",
                &format!("model-{i}"),
                100,
                100,
            ));
        }
        let out = run(&tool, json!({})).await.unwrap();
        assert!(
            out.text
                .starts_with("Расходы сегодня: цена неизвестна, есть только единицы.\nПо видам:\n"),
            "{}",
            out.text
        );
        // No limit is set: no budget lines.
        assert!(!out.text.contains("Бюджет"), "{}", out.text);
        assert!(out
            .text
            .contains("ответы модели — 10 запр., 2 тыс. токенов"));
        assert!(out.text.contains("\nmodel-7 (api.openai.com) — 200 ток.\n"));
        assert!(!out.text.contains("model-8 ("), "{}", out.text);
        assert!(out.text.contains("\n…и ещё 2\n"), "{}", out.text);
        assert!(
            out.text
                .contains("Без цены: 10 запр. (model-0, model-1, model-2 и другие)."),
            "{}",
            out.text
        );
        assert!(!out.text.contains("Без данных об объёме"));
        assert_eq!(out.data.unwrap()["totals"], json!([]));

        c.record(chat(ROUTER, "m", 1, 1).cost(Some(3.0)));
        c.record(chat("https://openrouter.ai/api/v1", "m", 1, 1).cost(Some(0.42)));
        let out = run(&tool, json!({})).await.unwrap();
        assert!(
            out.text
                .starts_with("Расходы сегодня: 3\u{a0}₽ и 0,42\u{a0}$.\n"),
            "{}",
            out.text
        );
    }
}
