//! The gate every tool call passes: rule, question, audit.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use super::{AuditLog, RuleDecision, RuleStore};
use crate::confirm::{Answer, Confirmations, DecidedBy, CONFIRM_TIMEOUT};
use crate::gate::{Approval, Gate, GateDecision};
use crate::tools::{Assessment, RiskLevel};

pub struct PolicyGate {
    rules: RuleStore,
    audit: AuditLog,
    confirms: Arc<Confirmations>,
    timeout: Duration,
}

impl PolicyGate {
    pub fn new(rules: RuleStore, audit: AuditLog, confirms: Arc<Confirmations>) -> Self {
        Self {
            rules,
            audit,
            confirms,
            timeout: CONFIRM_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn log(&self, tool: &str, a: &Assessment, decision: &str, by: DecidedBy) {
        self.audit
            .record(tool, &a.target, a.level, decision, by.as_str());
    }
}

#[async_trait]
impl Gate for PolicyGate {
    async fn check(&self, tool: &str, _args: &Value, a: &Assessment) -> GateDecision {
        let risk = a.level.as_str();
        let read_failed = |e: super::PolicyError| {
            log::error!("cannot read rules, the action is asked about: tool={tool} error={e}");
            None
        };
        let mut rule = self.rules.find(tool, &a.scope).unwrap_or_else(read_failed);
        // The rule of the call's class answers when the call has none of its own.
        let mut class = "none";
        if let (None, Some(c)) = (rule, a.rule_class) {
            rule = self.rules.find_class(tool, c).unwrap_or_else(read_failed);
            if rule.is_some() {
                class = c.as_str();
            }
        }
        match rule {
            Some(RuleDecision::Deny) => {
                log::info!("action denied by a rule: tool={tool} risk={risk} class={class}");
                self.log(tool, a, "deny", DecidedBy::Rule);
                return GateDecision::Denied("запрещено правилом пользователя".into());
            }
            // High risk always asks, even if a rule slipped in.
            Some(RuleDecision::Allow) if a.level != RiskLevel::High => {
                // The record of the call itself says `decision=rule`.
                log::debug!("action allowed by a rule: tool={tool} risk={risk} class={class}");
                self.log(tool, a, "allow", DecidedBy::Rule);
                return GateDecision::Approved(Approval::given("rule"));
            }
            _ => {}
        }

        let asked = Instant::now();
        let (answer, by) = self.confirms.ask(tool, a, self.timeout).await;
        log::info!(
            "question answered: tool={tool} risk={risk} answer={} by={} waited_ms={}",
            answer_name(answer),
            by.as_str(),
            asked.elapsed().as_millis()
        );
        match answer {
            Answer::Once => {
                self.log(tool, a, "allow_once", by);
                GateDecision::Approved(Approval::given("asked_yes"))
            }
            Answer::Always => {
                let pattern = super::always_pattern(a);
                if let Err(e) = self.rules.add(tool, pattern, RuleDecision::Allow, a.level) {
                    log::error!(
                        "cannot save the rule, the action runs this once: tool={tool} error={e}"
                    );
                }
                self.log(tool, a, "allow_always", by);
                GateDecision::Approved(Approval::given("asked_always"))
            }
            Answer::Deny => {
                self.log(tool, a, "deny", by);
                GateDecision::Denied("пользователь отклонил действие".into())
            }
            Answer::Timeout => {
                self.log(tool, a, "timeout", by);
                GateDecision::Denied("нет ответа 60 секунд, действие отклонено".into())
            }
        }
    }
}

/// The answer to a question as the journal names it.
fn answer_name(answer: Answer) -> &'static str {
    match answer {
        Answer::Once => "once",
        Answer::Always => "always",
        Answer::Deny => "deny",
        Answer::Timeout => "timeout",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventBus};
    use crate::policy::RuleClass;
    use crate::state::{AssistantState, StateHandle};
    use crate::storage::Db;

    struct Fixture {
        gate: Arc<PolicyGate>,
        confirms: Arc<Confirmations>,
        rules: RuleStore,
        audit: AuditLog,
        state: StateHandle,
        bus: EventBus,
    }

    fn fixture(timeout: Duration) -> Fixture {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let bus = EventBus::new(64);
        let state = StateHandle::new(bus.clone());
        let confirms = Confirmations::new(bus.clone(), state.clone());
        let rules = RuleStore::new(db.clone());
        let audit = AuditLog::new(db);
        let gate = Arc::new(
            PolicyGate::new(rules.clone(), audit.clone(), confirms.clone()).with_timeout(timeout),
        );
        Fixture {
            gate,
            confirms,
            rules,
            audit,
            state,
            bus,
        }
    }

    fn a(level: RiskLevel, scope: &str) -> Assessment {
        Assessment::new(level, "Сделать", scope, "причина").with_scope(scope)
    }

    /// Runs a check and answers the question with `answer` once it appears.
    async fn check_answering(
        f: &Fixture,
        level: RiskLevel,
        answer: Option<Answer>,
    ) -> GateDecision {
        let gate = f.gate.clone();
        let assessment = a(level, "x");
        let task = tokio::spawn(async move { gate.check("t", &Value::Null, &assessment).await });
        if let Some(ans) = answer {
            loop {
                if let Some(p) = f.confirms.pending() {
                    f.confirms.answer(p.id, ans, DecidedBy::Button).unwrap();
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        task.await.unwrap()
    }

    #[tokio::test]
    async fn no_rule_asks_even_for_low_risk() {
        let f = fixture(CONFIRM_TIMEOUT);
        let mut rx = f.bus.subscribe();
        let d = check_answering(&f, RiskLevel::Low, Some(Answer::Once)).await;
        assert!(matches!(d, GateDecision::Approved(_)));
        assert!(matches!(
            rx.recv().await.unwrap(),
            Event::StateChanged {
                to: AssistantState::AwaitingConfirm,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn once_runs_without_creating_rule() {
        let f = fixture(CONFIRM_TIMEOUT);
        check_answering(&f, RiskLevel::Medium, Some(Answer::Once)).await;
        assert!(f.rules.list().unwrap().is_empty());
    }

    #[tokio::test]
    async fn always_creates_rule_and_next_call_skips_question() {
        let f = fixture(CONFIRM_TIMEOUT);
        check_answering(&f, RiskLevel::Medium, Some(Answer::Always)).await;
        assert_eq!(f.rules.list().unwrap().len(), 1);
        // No answer given this time: it must pass on the rule alone.
        let d = check_answering(&f, RiskLevel::Medium, None).await;
        assert!(matches!(d, GateDecision::Approved(_)));
        assert_eq!(f.audit.recent(1)[0].decided_by, "rule");
    }

    /// Runs a check of `assessment` for `run_shell` and answers with `answer`.
    async fn check_shell(
        f: &Fixture,
        assessment: Assessment,
        answer: Option<Answer>,
    ) -> GateDecision {
        let gate = f.gate.clone();
        let task =
            tokio::spawn(async move { gate.check("run_shell", &Value::Null, &assessment).await });
        if let Some(ans) = answer {
            loop {
                if let Some(p) = f.confirms.pending() {
                    f.confirms.answer(p.id, ans, DecidedBy::Button).unwrap();
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        task.await.unwrap()
    }

    fn info(scope: &str) -> Assessment {
        a(RiskLevel::Medium, scope).with_rule_class(RuleClass::SystemInfo)
    }

    #[tokio::test]
    async fn always_on_a_class_makes_one_rule_for_the_whole_class() {
        let f = fixture(Duration::from_millis(30));
        let d = check_shell(&f, info("get-process"), Some(Answer::Always)).await;
        assert!(matches!(d, GateDecision::Approved(_)));
        let rules = f.rules.list().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "class:system-info");
        // Another command of the class runs without a question.
        for scope in ["tasklist", "ipconfig /all", r"get-childitem c:\*.txt"] {
            let d = check_shell(&f, info(scope), None).await;
            assert!(matches!(d, GateDecision::Approved(_)), "{scope}");
            assert_eq!(f.audit.recent(1)[0].decided_by, "rule");
            assert_eq!(f.audit.recent(1)[0].decision, "allow");
        }
    }

    #[tokio::test]
    async fn a_class_rule_leaves_everything_else_asking() {
        let f = fixture(Duration::from_millis(30));
        f.rules
            .add(
                "run_shell",
                RuleClass::SystemInfo.pattern(),
                RuleDecision::Allow,
                RiskLevel::Medium,
            )
            .unwrap();
        // A reader of contents is medium but not in the class.
        let d = check_shell(&f, a(RiskLevel::Medium, "get-content x"), None).await;
        assert!(matches!(d, GateDecision::Denied(_)));
        // A call whose scope is the very text of the rule.
        let d = check_shell(&f, a(RiskLevel::Medium, "class:system-info"), None).await;
        assert!(matches!(d, GateDecision::Denied(_)));
        // High asks even if a class slipped onto it.
        let high = a(RiskLevel::High, r"remove-item c:\x").with_rule_class(RuleClass::SystemInfo);
        let d = check_shell(&f, high, None).await;
        assert!(matches!(d, GateDecision::Denied(_)));
        // Another tool does not share the rule.
        let gate = f.gate.clone();
        let d = gate.check("t", &Value::Null, &info("get-process")).await;
        assert!(matches!(d, GateDecision::Denied(_)));
        assert_eq!(f.audit.recent(1)[0].decision, "timeout");
    }

    #[tokio::test]
    async fn always_without_a_class_keeps_the_exact_command() {
        let f = fixture(Duration::from_millis(30));
        check_shell(
            &f,
            a(RiskLevel::Medium, "get-content x"),
            Some(Answer::Always),
        )
        .await;
        let rules = f.rules.list().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, "get-content x");
        let d = check_shell(&f, a(RiskLevel::Medium, "get-content y"), None).await;
        assert!(matches!(d, GateDecision::Denied(_)));
    }

    #[tokio::test]
    async fn a_deny_of_the_command_wins_over_the_class() {
        let f = fixture(CONFIRM_TIMEOUT);
        f.rules
            .add(
                "run_shell",
                RuleClass::SystemInfo.pattern(),
                RuleDecision::Allow,
                RiskLevel::Medium,
            )
            .unwrap();
        f.rules
            .add(
                "run_shell",
                "tasklist",
                RuleDecision::Deny,
                RiskLevel::Medium,
            )
            .unwrap();
        let d = check_shell(&f, info("tasklist"), None).await;
        assert!(matches!(d, GateDecision::Denied(_)));
    }

    #[tokio::test]
    async fn high_risk_asks_even_with_allow_rule_in_db() {
        let f = fixture(Duration::from_millis(30));
        // Bypass RuleStore::add to simulate a rule written by hand.
        f.rules
            .add("t", "x", RuleDecision::Allow, RiskLevel::Low)
            .unwrap();
        let d = check_answering(&f, RiskLevel::High, None).await;
        assert!(matches!(d, GateDecision::Denied(_)));
    }

    #[tokio::test]
    async fn deny_rule_blocks_without_question() {
        let f = fixture(CONFIRM_TIMEOUT);
        f.rules
            .add("t", "*", RuleDecision::Deny, RiskLevel::Low)
            .unwrap();
        let d = check_answering(&f, RiskLevel::Low, None).await;
        assert!(matches!(d, GateDecision::Denied(_)));
        assert_eq!(f.state.get(), AssistantState::Idle);
    }

    #[tokio::test]
    async fn timeout_denies() {
        let f = fixture(Duration::from_millis(20));
        let d = check_answering(&f, RiskLevel::Low, None).await;
        match d {
            GateDecision::Denied(reason) => assert!(reason.contains("60 секунд")),
            _ => panic!("expected deny"),
        }
        assert_eq!(f.audit.recent(1)[0].decision, "timeout");
    }

    #[tokio::test]
    async fn an_approval_remembers_how_it_was_given() {
        let f = fixture(CONFIRM_TIMEOUT);
        let how = |d: GateDecision| match d {
            GateDecision::Approved(a) => a.how(),
            GateDecision::Denied(_) => "denied",
        };
        assert_eq!(
            how(check_answering(&f, RiskLevel::Medium, Some(Answer::Once)).await),
            "asked_yes"
        );
        assert_eq!(
            how(check_answering(&f, RiskLevel::Medium, Some(Answer::Always)).await),
            "asked_always"
        );
        // The rule just made answers from now on.
        assert_eq!(
            how(check_answering(&f, RiskLevel::Medium, None).await),
            "rule"
        );
        assert_eq!(
            how(check_answering(&f, RiskLevel::High, Some(Answer::Deny)).await),
            "denied"
        );
    }

    #[tokio::test]
    async fn every_decision_is_audited() {
        let f = fixture(CONFIRM_TIMEOUT);
        check_answering(&f, RiskLevel::Low, Some(Answer::Once)).await;
        check_answering(&f, RiskLevel::Low, Some(Answer::Deny)).await;
        let log = f.audit.recent(10);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].decision, "deny");
        assert_eq!(log[1].decision, "allow_once");
        assert_eq!(log[1].decided_by, "button");
    }
}
