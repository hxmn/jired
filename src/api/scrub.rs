//! Scrubbing recorded API payloads [M1.12].
//!
//! Fixtures are committed to the repository, so anything recorded from a real
//! instance has to lose its identifying detail first. The rule is conservative:
//! replace by *field name* rather than by pattern matching values, because a
//! pattern that almost matches is how a credential ends up in a public repo.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Field names whose values are replaced with a stable pseudonym.
const IDENTITY_FIELDS: &[&str] = &[
    "accountId",
    "emailAddress",
    "name",
    "key",
    "displayName",
    "author",
    "username",
];

/// Field names dropped outright: bulky, useless in a fixture, and often
/// carrying signed URLs.
const DROPPED_FIELDS: &[&str] = &[
    "avatarUrls",
    "avatarId",
    "iconUrl",
    "self",
    "thumbnail",
    "renderedFields",
];

/// Replaces identities consistently so a fixture stays internally coherent:
/// the same person is the same pseudonym everywhere.
#[derive(Debug, Default)]
pub struct Scrubber {
    people: BTreeMap<String, String>,
    /// Extra literals to remove wherever they appear, e.g. the real host.
    literals: Vec<(String, String)>,
}

impl Scrubber {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace every occurrence of `literal` anywhere in any string value.
    ///
    /// Used for the instance host and for the token itself: belt and braces, so
    /// that even a field jired does not model cannot leak one.
    pub fn redact_literal(&mut self, literal: impl Into<String>, replacement: impl Into<String>) {
        let literal = literal.into();
        if !literal.is_empty() {
            self.literals.push((literal, replacement.into()));
        }
    }

    fn pseudonym(&mut self, field: &str, original: &str) -> String {
        if original.is_empty() {
            return original.to_string();
        }
        let next = self.people.len() + 1;
        let assigned = self
            .people
            .entry(original.to_string())
            .or_insert_with(|| format!("person{next}"))
            .clone();

        match field {
            "emailAddress" => format!("{assigned}@example.invalid"),
            "displayName" => format!("Person {}", &assigned[6..]),
            // Issue keys are structural, not identifying: PROJ-1 must stay PROJ-1.
            "key" if is_issue_key(original) => original.to_string(),
            _ => assigned,
        }
    }

    pub fn scrub(&mut self, value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let mut out = Map::new();
                for (k, v) in map {
                    if DROPPED_FIELDS.contains(&k.as_str()) {
                        continue;
                    }
                    if IDENTITY_FIELDS.contains(&k.as_str()) {
                        if let Value::String(s) = v {
                            out.insert(k.clone(), Value::String(self.pseudonym(k, s)));
                            continue;
                        }
                    }
                    out.insert(k.clone(), self.scrub(v));
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(|v| self.scrub(v)).collect()),
            Value::String(s) => Value::String(self.apply_literals(s)),
            other => other.clone(),
        }
    }

    fn apply_literals(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (needle, replacement) in &self.literals {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), replacement);
            }
        }
        out
    }
}

/// `PROJ-142` — structural, and safe to keep.
fn is_issue_key(s: &str) -> bool {
    crate::doc::is_issue_key(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identities_become_stable_pseudonyms() {
        let mut s = Scrubber::new();
        let input = json!({
            "assignee": {"accountId": "5b10a2real", "displayName": "Ilya Ashbourne",
                         "emailAddress": "ilya@corp.example"},
            "reporter": {"accountId": "5b10a2real", "displayName": "Ilya Ashbourne"}
        });
        let out = s.scrub(&input);

        // No original value survives anywhere.
        let text = out.to_string();
        for original in ["5b10a2real", "Ilya Ashbourne", "ilya@corp.example"] {
            assert!(!text.contains(original), "{original} survived in {text}");
        }

        // A fixture has to stay internally coherent: the same person must be
        // the same pseudonym in every place they appear, or a test asserting
        // "the assignee is the reporter" would break for the wrong reason.
        assert_eq!(out["assignee"]["accountId"], out["reporter"]["accountId"]);
        assert_eq!(
            out["assignee"]["displayName"],
            out["reporter"]["displayName"]
        );

        // Shapes are preserved so downstream parsing still exercises the real
        // code: an email still looks like an email.
        assert!(out["assignee"]["emailAddress"]
            .as_str()
            .unwrap()
            .ends_with("@example.invalid"));
    }

    #[test]
    fn scrubbing_the_same_value_twice_yields_the_same_pseudonym() {
        let mut s = Scrubber::new();
        let first = s.scrub(&json!({"accountId": "abc"}));
        let second = s.scrub(&json!({"accountId": "abc"}));
        assert_eq!(first, second);
        assert_ne!(s.scrub(&json!({"accountId": "def"})), first);
    }

    #[test]
    fn issue_keys_are_structural_and_kept() {
        let mut s = Scrubber::new();
        let out = s.scrub(&json!({"key": "PROJ-142", "fields": {"parent": {"key": "PROJ-100"}}}));
        assert_eq!(out["key"], "PROJ-142");
        assert_eq!(out["fields"]["parent"]["key"], "PROJ-100");
    }

    #[test]
    fn a_user_key_is_still_pseudonymised() {
        let mut s = Scrubber::new();
        let out = s.scrub(&json!({"key": "JIRAUSER10100"}));
        assert_ne!(out["key"], "JIRAUSER10100");
    }

    #[test]
    fn bulky_and_signed_fields_are_dropped() {
        let mut s = Scrubber::new();
        let out = s.scrub(&json!({
            "avatarUrls": {"48x48": "https://real/avatar?token=secret"},
            "self": "https://real/rest/api/3/issue/1",
            "summary": "kept"
        }));
        assert!(out.get("avatarUrls").is_none());
        assert!(out.get("self").is_none());
        assert_eq!(out["summary"], "kept");
    }

    #[test]
    fn literals_are_removed_wherever_they_appear() {
        let mut s = Scrubber::new();
        s.redact_literal("jira.corp.example", "fixture.invalid");
        s.redact_literal("supersecrettoken", "REDACTED");

        let out = s.scrub(&json!({
            "content": "https://jira.corp.example/secure/attachment/1/x.log",
            "description": "call with ?token=supersecrettoken please",
            "nested": [{"url": "https://jira.corp.example/browse/PROJ-1"}]
        }));

        let text = out.to_string();
        assert!(!text.contains("jira.corp.example"), "{text}");
        assert!(!text.contains("supersecrettoken"), "{text}");
        assert!(text.contains("fixture.invalid"));
    }

    #[test]
    fn scrubbing_preserves_structure_and_non_identity_values() {
        let mut s = Scrubber::new();
        let input = json!({
            "id": "10023",
            "fields": {"summary": "Fix login", "labels": ["a", "b"], "customfield_1": 5}
        });
        let out = s.scrub(&input);
        assert_eq!(out["id"], "10023");
        assert_eq!(out["fields"]["summary"], "Fix login");
        assert_eq!(out["fields"]["labels"], json!(["a", "b"]));
        assert_eq!(out["fields"]["customfield_1"], 5);
    }
}
