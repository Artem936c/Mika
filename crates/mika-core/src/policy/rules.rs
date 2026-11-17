//! Permanent rules and the audit log, stored in SQLite.

use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

use super::PolicyError;
use crate::storage::{now, Db};
use crate::tools::RiskLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleDecision {
    Allow,
    Deny,
}

impl RuleDecision {
    fn as_str(self) -> &'static str {
        match self {
            RuleDecision::Allow => "allow",
            RuleDecision::Deny => "deny",
        }
    }

    fn parse(s: &str) -> RuleDecision {
        if s == "allow" {
            RuleDecision::Allow
        } else {
            RuleDecision::Deny
        }
    }
}

/// A class of calls that one "always" rule covers whole (decision 0044).
/// Only the tool's own assessment puts a call into a class: the rule is
/// stored as a pattern no call scope is matched against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleClass {
    /// `run_shell` lines that only tell about the system
    /// (`CommandRisk::system_info`).
    SystemInfo,
}

impl RuleClass {
    /// What starts the pattern of every class rule.
    pub const PREFIX: &'static str = "class:";

    const ALL: [RuleClass; 1] = [RuleClass::SystemInfo];

    /// The pattern of the rule that covers this class.
    pub fn pattern(self) -> &'static str {
        match self {
            RuleClass::SystemInfo => "class:system-info",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RuleClass::SystemInfo => "system_info",
        }
    }

    /// The class a rule pattern stands for, letter case aside.
    pub fn from_pattern(pattern: &str) -> Option<RuleClass> {
        Self::ALL
            .into_iter()
            .find(|c| c.pattern().eq_ignore_ascii_case(pattern))
    }
}

/// Whether a pattern is written as a class rule, known class or not.
fn is_class_pattern(pattern: &str) -> bool {
    pattern
        .get(..RuleClass::PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(RuleClass::PREFIX))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: i64,
    pub tool: String,
    pub pattern: String,
    pub decision: RuleDecision,
    pub created_at: String,
}

fn rule_from_row(r: &Row<'_>) -> rusqlite::Result<Rule> {
    Ok(Rule {
        id: r.get(0)?,
        tool: r.get(1)?,
        pattern: r.get(2)?,
        decision: RuleDecision::parse(&r.get::<_, String>(3)?),
        created_at: r.get(4)?,
    })
}

/// `*` matches any run of characters. Case-insensitive.
pub fn pattern_matches(pattern: &str, value: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let v: Vec<char> = value.to_lowercase().chars().collect();
    // Greedy wildcard match with backtracking to the last star.
    let (mut pi, mut vi) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while vi < v.len() {
        if pi < p.len() && p[pi] != '*' && p[pi] == v[vi] {
            pi += 1;
            vi += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = vi;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            vi = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[derive(Clone)]
pub struct RuleStore {
    db: Arc<Db>,
}

impl RuleStore {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    /// Refuses a permanent allow for high risk.
    pub fn add(
        &self,
        tool: &str,
        pattern: &str,
        decision: RuleDecision,
        level: RiskLevel,
    ) -> Result<Rule, PolicyError> {
        if decision == RuleDecision::Allow {
            if let Some(why) = super::forever_block(level, pattern) {
                return Err(PolicyError::ForeverNotAllowed(why));
            }
        }
        let created_at = now();
        let id = self.db.with(|c| {
            c.execute(
                "INSERT INTO rules (tool, pattern, decision, created_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(tool, pattern) DO UPDATE SET decision = excluded.decision, created_at = excluded.created_at",
                params![tool, pattern, decision.as_str(), created_at],
            )?;
            c.query_row(
                "SELECT id FROM rules WHERE tool = ?1 AND pattern = ?2",
                params![tool, pattern],
                |r| r.get(0),
            )
        })?;
        // What the rule covers is a command, a folder or a site: the user's.
        log::info!(
            "rule added: id={id} tool={tool} decision={} risk={} pattern={}",
            decision.as_str(),
            level.as_str(),
            crate::journal::guarded(pattern)
        );
        Ok(Rule {
            id,
            tool: tool.into(),
            pattern: pattern.into(),
            decision,
            created_at,
        })
    }

    pub fn list(&self) -> Result<Vec<Rule>, PolicyError> {
        Ok(self.db.with(|c| {
            let mut st = c.prepare(
                "SELECT id, tool, pattern, decision, created_at FROM rules ORDER BY id DESC",
            )?;
            let rows = st.query_map([], rule_from_row)?;
            rows.collect()
        })?)
    }

    pub fn get(&self, id: i64) -> Result<Option<Rule>, PolicyError> {
        Ok(self.db.with(|c| {
            c.query_row(
                "SELECT id, tool, pattern, decision, created_at FROM rules WHERE id = ?1",
                [id],
                rule_from_row,
            )
            .optional()
        })?)
    }

    pub fn remove(&self, id: i64) -> Result<bool, PolicyError> {
        let removed = self
            .db
            .with(|c| c.execute("DELETE FROM rules WHERE id = ?1", [id]))?
            > 0;
        if removed {
            log::info!("rule removed: id={id}");
        }
        Ok(removed)
    }

    /// A matching deny wins over a matching allow.
    ///
    /// A deny is a pattern: its `*` matches anything. An allow covers only
    /// the very scope it was given for, letter case aside: the rule `*`
    /// covers the calls whose own scope is `*`, and never the calls of a
    /// tool that names what it touches (a folder sent to paid recognition, a
    /// scenario with its limits). No such rule is made here (`forever_block`),
    /// but a backup from elsewhere can bring one, and a permanent allow must
    /// never cover more than the question it was given for showed.
    ///
    /// An allow of a class (`class:...`) never covers a scope here, even one
    /// with the same text: only `find_class` reads it.
    pub fn find(&self, tool: &str, scope: &str) -> Result<Option<RuleDecision>, PolicyError> {
        let covers = |r: &Rule| match r.decision {
            RuleDecision::Deny => pattern_matches(&r.pattern, scope),
            RuleDecision::Allow => {
                !is_class_pattern(&r.pattern) && r.pattern.to_lowercase() == scope.to_lowercase()
            }
        };
        let mut found = None;
        for r in self.list()?.into_iter().filter(|r| r.tool == tool) {
            if covers(&r) {
                if r.decision == RuleDecision::Deny {
                    return Ok(Some(RuleDecision::Deny));
                }
                found = Some(RuleDecision::Allow);
            }
        }
        Ok(found)
    }

    /// The rule of a whole class for a tool: an allow or a deny made for it.
    pub fn find_class(
        &self,
        tool: &str,
        class: RuleClass,
    ) -> Result<Option<RuleDecision>, PolicyError> {
        let mut found = None;
        for r in self.list()?.into_iter().filter(|r| r.tool == tool) {
            if RuleClass::from_pattern(&r.pattern) == Some(class) {
                if r.decision == RuleDecision::Deny {
                    return Ok(Some(RuleDecision::Deny));
                }
                found = Some(RuleDecision::Allow);
            }
        }
        Ok(found)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: String,
    pub tool: String,
    pub target: String,
    pub risk: String,
    pub decision: String,
    pub decided_by: String,
}

#[derive(Clone)]
pub struct AuditLog {
    db: Arc<Db>,
}

impl AuditLog {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    pub fn record(&self, tool: &str, target: &str, risk: RiskLevel, decision: &str, by: &str) {
        let res = self.db.with(|c| {
            c.execute(
                "INSERT INTO audit (at, tool, target, risk, decision, decided_by) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![now(), tool, target, risk.as_str(), decision, by],
            )
        });
        if let Err(e) = res {
            log::error!("cannot write the audit log: tool={tool} decision={decision} error={e}");
        }
    }

    pub fn recent(&self, limit: usize) -> Vec<AuditEntry> {
        self.db
            .with(|c| {
                let mut st = c.prepare(
                    "SELECT at, tool, target, risk, decision, decided_by FROM audit ORDER BY id DESC LIMIT ?1",
                )?;
                let rows = st.query_map([limit as i64], |r| {
                    Ok(AuditEntry {
                        at: r.get(0)?,
                        tool: r.get(1)?,
                        target: r.get(2)?,
                        risk: r.get(3)?,
                        decision: r.get(4)?,
                        decided_by: r.get(5)?,
                    })
                })?;
                rows.collect()
            })
            .unwrap_or_else(|e| {
                log::warn!("cannot read the audit log, it is shown empty: {e}");
                Vec::new()
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> RuleStore {
        RuleStore::new(Arc::new(Db::open_in_memory().unwrap()))
    }

    #[test]
    fn add_and_find_allow_rule() {
        let s = store();
        s.add("open_app", "notepad", RuleDecision::Allow, RiskLevel::Low)
            .unwrap();
        assert_eq!(
            s.find("open_app", "Notepad").unwrap(),
            Some(RuleDecision::Allow)
        );
        assert_eq!(s.find("open_app", "calc").unwrap(), None);
        assert_eq!(s.find("other", "notepad").unwrap(), None);
    }

    #[test]
    fn deny_beats_allow() {
        let s = store();
        for scope in ["get-process -name x", "whoami"] {
            s.add("run_shell", scope, RuleDecision::Allow, RiskLevel::Medium)
                .unwrap();
        }
        // Written as a deny, so wildcards are allowed.
        s.add(
            "run_shell",
            "get-process*",
            RuleDecision::Deny,
            RiskLevel::Medium,
        )
        .unwrap();
        assert_eq!(
            s.find("run_shell", "get-process -name x").unwrap(),
            Some(RuleDecision::Deny)
        );
        assert_eq!(
            s.find("run_shell", "whoami").unwrap(),
            Some(RuleDecision::Allow)
        );
    }

    #[test]
    fn an_allow_covers_only_the_scope_it_was_given_for() {
        let s = store();
        // A tool without a scope of its own: every call has the scope `*`.
        s.add("get_time", "*", RuleDecision::Allow, RiskLevel::Low)
            .unwrap();
        assert_eq!(s.find("get_time", "*").unwrap(), Some(RuleDecision::Allow));
        // A rule `*` that came from elsewhere (a backup) for tools that name
        // what they touch. Here it cannot even be made: the scope of a whole
        // folder holds a `*` on purpose.
        s.db.with(|c| {
            c.execute_batch(
                "INSERT INTO rules (tool, pattern, decision, created_at) VALUES
                    ('av_folder', '*', 'allow', 'd'),
                    ('trigger_create', '*', 'allow', 'd'),
                    ('write_file', 'c:/users/me/*', 'allow', 'd');",
            )
        })
        .unwrap();
        for (tool, scope) in [
            ("av_folder", r"transcribe:c:\\videos\\*"),
            ("av_folder", "compress:c:/videos"),
            (
                "trigger_create",
                "morning|app_started|code.exe|0123456789abcdef|60|20",
            ),
            ("write_file", "c:/users/me/notes.txt"),
            ("write_file", "c:/users/me/"),
        ] {
            assert_eq!(s.find(tool, scope).unwrap(), None, "{tool} {scope}");
        }
        // An allow with a `*` inside covers the scope that is that very text
        // and nothing else; the letter case does not matter, as before.
        assert_eq!(
            s.find("write_file", "C:/Users/Me/*").unwrap(),
            Some(RuleDecision::Allow)
        );
        // A deny stays a pattern.
        s.add("av_folder", "*", RuleDecision::Deny, RiskLevel::Medium)
            .unwrap();
        assert_eq!(
            s.find("av_folder", "compress:c:/videos").unwrap(),
            Some(RuleDecision::Deny)
        );
    }

    #[test]
    fn a_class_rule_is_found_only_by_its_class() {
        let s = store();
        let class = RuleClass::SystemInfo;
        s.add(
            "run_shell",
            class.pattern(),
            RuleDecision::Allow,
            RiskLevel::Medium,
        )
        .unwrap();
        assert_eq!(
            s.find_class("run_shell", class).unwrap(),
            Some(RuleDecision::Allow)
        );
        assert_eq!(s.find_class("other", class).unwrap(), None);
        // A call whose own scope happens to be the text of the class rule is
        // not covered: only the classifier puts a call into the class.
        assert_eq!(s.find("run_shell", class.pattern()).unwrap(), None);
        assert_eq!(s.find("run_shell", "get-process").unwrap(), None);
        // A deny made by hand for the class closes it.
        s.add(
            "run_shell",
            class.pattern(),
            RuleDecision::Deny,
            RiskLevel::Medium,
        )
        .unwrap();
        assert_eq!(
            s.find_class("run_shell", class).unwrap(),
            Some(RuleDecision::Deny)
        );
    }

    #[test]
    fn a_class_has_a_fixed_pattern() {
        assert_eq!(RuleClass::SystemInfo.pattern(), "class:system-info");
        assert_eq!(
            RuleClass::from_pattern("CLASS:System-Info"),
            Some(RuleClass::SystemInfo)
        );
        assert_eq!(RuleClass::from_pattern("class:other"), None);
        assert_eq!(RuleClass::from_pattern("get-process"), None);
    }

    #[test]
    fn high_risk_cannot_be_allowed_forever() {
        let s = store();
        assert!(matches!(
            s.add("power", "*", RuleDecision::Allow, RiskLevel::High),
            Err(PolicyError::ForeverNotAllowed(_))
        ));
        assert!(s.list().unwrap().is_empty());
    }

    #[test]
    fn wildcard_in_arg_scope_cannot_be_allowed_forever() {
        let s = store();
        assert!(matches!(
            s.add(
                "write_file",
                "c:/users/me/documents/*",
                RuleDecision::Allow,
                RiskLevel::Medium
            ),
            Err(PolicyError::ForeverNotAllowed(
                super::super::WILDCARD_FOREVER
            ))
        ));
        // A plain `*` scope set by the tool itself is fine.
        assert!(s
            .add("get_time", "*", RuleDecision::Allow, RiskLevel::Low)
            .is_ok());
        // Deny rules may use wildcards.
        assert!(s
            .add(
                "write_file",
                "c:/windows/*",
                RuleDecision::Deny,
                RiskLevel::High
            )
            .is_ok());
    }

    #[test]
    fn high_risk_can_be_denied_forever() {
        let s = store();
        s.add("power", "*", RuleDecision::Deny, RiskLevel::High)
            .unwrap();
        assert_eq!(s.find("power", "reboot").unwrap(), Some(RuleDecision::Deny));
    }

    #[test]
    fn remove_rule() {
        let s = store();
        let r = s
            .add("t", "*", RuleDecision::Allow, RiskLevel::Low)
            .unwrap();
        assert!(s.get(r.id).unwrap().is_some());
        assert!(s.remove(r.id).unwrap());
        assert!(!s.remove(r.id).unwrap());
        assert_eq!(s.find("t", "x").unwrap(), None);
    }

    #[test]
    fn same_rule_twice_updates_instead_of_duplicating() {
        let s = store();
        s.add("t", "a", RuleDecision::Allow, RiskLevel::Low)
            .unwrap();
        s.add("t", "a", RuleDecision::Deny, RiskLevel::Low).unwrap();
        assert_eq!(s.list().unwrap().len(), 1);
        assert_eq!(s.find("t", "a").unwrap(), Some(RuleDecision::Deny));
    }

    #[test]
    fn pattern_star_matches_any() {
        assert!(pattern_matches("*", ""));
        assert!(pattern_matches("*", "anything"));
        assert!(pattern_matches("get-*", "get-process"));
        assert!(pattern_matches("*.example.com", "a.b.example.com"));
        assert!(pattern_matches("a*c*e", "abcde"));
        assert!(!pattern_matches("get-*", "set-item"));
        assert!(!pattern_matches("abc", "abcd"));
    }

    #[test]
    fn pattern_is_case_insensitive() {
        assert!(pattern_matches("Notepad", "NOTEPAD"));
        assert!(pattern_matches("блокнот", "Блокнот"));
    }

    #[test]
    fn audit_records_newest_first() {
        let a = AuditLog::new(Arc::new(Db::open_in_memory().unwrap()));
        a.record("t1", "x", RiskLevel::Low, "allow", "rule");
        a.record("t2", "y", RiskLevel::High, "deny", "button");
        let r = a.recent(10);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].tool, "t2");
        assert_eq!(r[0].risk, "high");
        assert_eq!(r[1].decided_by, "rule");
    }
}
