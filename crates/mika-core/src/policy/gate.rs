//! The gate every tool call passes: rule, question, audit.

use std::sync::Arc;
use std::time::Duration;

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
        let rule = self.rules.find(tool, &a.scope).unwrap_or_else(|e| {
            log::error!("cannot read rules: {e}");
            None
        });
        match rule {
            Some(RuleDecision::Deny) => {
                self.log(tool, a, "deny", DecidedBy::Rule);
                return GateDecision::Denied("запрещено правилом пользователя".into());
            }
            // High risk always asks, even if a rule slipped in.
            Some(RuleDecision::Allow) if a.level != RiskLevel::High => {
                self.log(tool, a, "allow", DecidedBy::Rule);
                return GateDecision::Approved(Approval::new());
            }
            _ => {}
        }

        let (answer, by) = self.confirms.ask(tool, a, self.timeout).await;
        match answer {
            Answer::Once => {
                self.log(tool, a, "allow_once", by);
                GateDecision::Approved(Approval::new())
            }
            Answer::Always => {
                if let Err(e) = self.rules.add(tool, &a.scope, RuleDecision::Allow, a.level) {
                    log::error!("cannot save rule: {e}");
                }
                self.log(tool, a, "allow_always", by);
                GateDecision::Approved(Approval::new())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventBus};
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
