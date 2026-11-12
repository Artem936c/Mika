//! Risk policy: rules, classifiers, audit log and the gate that runs tools.

pub mod blind;
pub mod command;
pub mod gate;
pub mod paths;
pub mod rules;

pub use gate::PolicyGate;
pub use rules::{AuditEntry, AuditLog, Rule, RuleDecision, RuleStore};

use crate::tools::RiskLevel;

pub const HIGH_RISK_FOREVER: &str = "действие высокого риска нельзя разрешить навсегда";
pub const WILDCARD_FOREVER: &str =
    "в цели действия есть «*», постоянное правило охватило бы лишнее, поэтому навсегда нельзя";

/// Why a permanent allow is not possible for this call, if it is not.
/// A `*` in a scope built from arguments would turn into a wildcard rule.
pub fn forever_block(level: RiskLevel, scope: &str) -> Option<&'static str> {
    if level == RiskLevel::High {
        Some(HIGH_RISK_FOREVER)
    } else if scope != "*" && scope.contains('*') {
        Some(WILDCARD_FOREVER)
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    ForeverNotAllowed(&'static str),
    #[error(transparent)]
    Db(#[from] crate::storage::DbError),
}
