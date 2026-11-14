//! The only way to get permission to run a tool.
//!
//! `Tool::run` takes an `Approval`. Only code inside `mika-core` can create one,
//! so a tool cannot be run without passing a `Gate`.

use async_trait::async_trait;
use serde_json::Value;

use crate::tools::Assessment;
#[cfg(test)]
use crate::tools::RiskLevel;

/// Proof that a gate approved one tool call. Not `Clone`, not `Default`.
#[derive(Debug)]
pub struct Approval {
    /// How it was given, for the journal: `rule`, `asked_yes`, `asked_always`.
    how: &'static str,
}

impl Approval {
    /// An approval that remembers how it was given.
    pub(crate) fn given(how: &'static str) -> Self {
        Self { how }
    }

    /// How the gate came to approve the call, for the journal.
    pub fn how(&self) -> &'static str {
        self.how
    }

    /// Lets tool crates unit-test `run` without a gate.
    #[cfg(any(test, feature = "testing"))]
    pub fn for_tests() -> Self {
        Self::given("test")
    }
}

#[derive(Debug)]
pub enum GateDecision {
    Approved(Approval),
    Denied(String),
}

#[async_trait]
pub trait Gate: Send + Sync {
    async fn check(&self, tool: &str, args: &Value, assessment: &Assessment) -> GateDecision;
}

/// Approves low risk, denies everything else. Tests only; the product uses `PolicyGate`.
#[cfg(test)]
pub struct LowRiskOnlyGate;

#[cfg(test)]
#[async_trait]
impl Gate for LowRiskOnlyGate {
    async fn check(&self, _tool: &str, _args: &Value, assessment: &Assessment) -> GateDecision {
        if assessment.level == RiskLevel::Low {
            GateDecision::Approved(Approval::given("test"))
        } else {
            GateDecision::Denied("действие требует подтверждения".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assessment(level: RiskLevel) -> Assessment {
        Assessment::new(level, "x", "x", "x")
    }

    #[tokio::test]
    async fn low_risk_gate_approves_low_and_denies_medium() {
        let g = LowRiskOnlyGate;
        let args = serde_json::json!({});
        assert!(matches!(
            g.check("t", &args, &assessment(RiskLevel::Low)).await,
            GateDecision::Approved(_)
        ));
        assert!(matches!(
            g.check("t", &args, &assessment(RiskLevel::Medium)).await,
            GateDecision::Denied(_)
        ));
    }
}
