//! Risk policy: rules, classifiers, audit log and the gate that runs tools.

pub mod blind;
pub mod command;
pub mod gate;
pub mod paths;
pub mod rules;

pub use gate::PolicyGate;
pub use rules::{AuditEntry, AuditLog, Rule, RuleClass, RuleDecision, RuleStore};

use crate::tools::{Assessment, RiskLevel};

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

/// The pattern an "always" answer saves for this call: the class when the
/// tool put the call into one, the call's own scope otherwise.
pub fn always_pattern(a: &Assessment) -> &str {
    match a.rule_class {
        Some(class) => class.pattern(),
        None => &a.scope,
    }
}

/// `forever_block` for the rule an "always" answer would really save.
/// A class rule is wide on purpose: the class is bounded by the tool's own
/// classifier, not by text from the arguments, so the `*` check of the scope
/// does not apply to it. High risk stays refused.
pub fn forever_block_for(a: &Assessment) -> Option<&'static str> {
    forever_block(a.level, always_pattern(a))
}

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0}")]
    ForeverNotAllowed(&'static str),
    #[error(transparent)]
    Db(#[from] crate::storage::DbError),
}
