//! Custom fields, in a form a person can actually edit [M2.3].
//!
//! Jira returns custom fields in whatever shape their type happens to use: a
//! single-select is `{"value": "Platform"}`, a multi-select an array of those, a
//! version `{"name": "2.4.0"}`, a user picker an object of ids. Writing that by
//! hand into YAML is miserable and easy to get wrong, and getting it wrong means
//! a 400 from Jira rather than a helpful error.
//!
//! So the file shows the simple form — `Platform` — and this module puts the
//! wrapper back on the way out. What shape to rebuild is decided from the value
//! the server last sent for that field, and only failing that from the field's
//! schema: the previous value is evidence about *this* field on *this* instance,
//! while a schema is a description that may not cover the plugin types.
//!
//! Anything that does not collapse cleanly — a cascading select, a rich-text
//! field, some plugin's nested structure — is left exactly as it arrived and
//! sent back byte for byte. Round-tripping unknown content matters more than
//! prettifying it.

use crate::api::{Deployment, FieldMeta};
use serde_json::{json, Value};

/// Collapse a raw custom-field value into the simplest form that still says the
/// same thing, or leave it alone when it does not collapse unambiguously.
pub fn simplify(v: &Value) -> Value {
    match v {
        Value::Array(items) => {
            let simplified: Vec<Value> = items.iter().map(simplify).collect();
            // A mixed array could not be expanded again without guessing.
            if simplified.iter().all(is_scalar) {
                Value::Array(simplified)
            } else {
                v.clone()
            }
        }
        Value::Object(map) => {
            if let Some(mention) = user_mention(v) {
                return Value::String(mention);
            }
            // A cascading select's child is half the value; flattening loses it.
            if map.contains_key("child") {
                return v.clone();
            }
            for key in ["value", "name"] {
                match map.get(key) {
                    Some(scalar) if is_scalar(scalar) => return scalar.clone(),
                    _ => {}
                }
            }
            v.clone()
        }
        other => other.clone(),
    }
}

/// Rebuild the shape Jira expects for a value someone typed into the file.
///
/// `previous` is the raw value the server last sent for this field, and
/// `meta` its schema; either may be missing, and with both missing the value
/// goes out as written — which is right for the plain string, number and date
/// fields that need no wrapper at all.
pub fn expand(
    edited: &Value,
    previous: Option<&Value>,
    meta: Option<&FieldMeta>,
    deployment: Deployment,
) -> Value {
    // Someone who kept the full Jira shape gets it sent back untouched.
    if is_wire_shape(edited) {
        return edited.clone();
    }
    if is_empty(edited) {
        // Jira clears a field by being sent null, whatever its type.
        return Value::Null;
    }

    let Some(target) = previous
        .and_then(target_from_value)
        .or_else(|| meta.and_then(target_from_schema))
    else {
        return edited.clone();
    };

    match (target.many, edited) {
        (true, Value::Array(items)) => Value::Array(
            items
                .iter()
                .map(|i| wrap(i, target.element, deployment))
                .collect(),
        ),
        // One value written where the field takes several is an obvious
        // intention, not an error worth failing the push over.
        (true, one) => json!([wrap(one, target.element, deployment)]),
        (false, Value::Array(items)) => match items.first() {
            Some(first) => wrap(first, target.element, deployment),
            None => Value::Null,
        },
        (false, one) => wrap(one, target.element, deployment),
    }
}

/// How one value of a field has to be written on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Element {
    /// `{"value": …}` — select lists and radio buttons.
    Option_,
    /// `{"name": …}` — versions, components, groups, projects.
    Named,
    /// `{"accountId": …}` on Cloud, `{"name": …}` on Data Center.
    User,
    /// A number, even if the file spells it as a string.
    Number,
    /// Whatever was written.
    Plain,
}

#[derive(Debug, Clone, Copy)]
struct Target {
    many: bool,
    element: Element,
}

fn wrap(scalar: &Value, element: Element, deployment: Deployment) -> Value {
    match element {
        Element::Option_ => json!({ "value": scalar }),
        Element::Named => json!({ "name": scalar }),
        Element::User => {
            let id = scalar
                .as_str()
                .and_then(crate::doc::frontmatter::parse_user)
                .unwrap_or_default();
            crate::api::user_field_value(deployment, Some(&id))
        }
        Element::Number => match scalar {
            Value::String(s) => s
                .trim()
                .parse::<f64>()
                .ok()
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or_else(|| scalar.clone()),
            other => other.clone(),
        },
        Element::Plain => scalar.clone(),
    }
}

/// What the last value from the server says this field's shape is.
fn target_from_value(v: &Value) -> Option<Target> {
    match v {
        Value::Array(items) => Some(Target {
            many: true,
            element: element_of(items.first()?)?,
        }),
        other => Some(Target {
            many: false,
            element: element_of(other)?,
        }),
    }
}

fn element_of(v: &Value) -> Option<Element> {
    let map = v.as_object()?;
    if user_mention(v).is_some() {
        return Some(Element::User);
    }
    if map.contains_key("value") {
        return Some(Element::Option_);
    }
    if map.contains_key("name") {
        return Some(Element::Named);
    }
    None
}

/// What the field's declared schema says, when there is no previous value.
fn target_from_schema(meta: &FieldMeta) -> Option<Target> {
    let (many, kind) = match meta.schema_type.as_deref()? {
        "array" => (true, meta.items.as_deref()?),
        other => (false, other),
    };
    let element = match kind {
        "option" | "option-with-child" => Element::Option_,
        "version" | "component" | "group" | "project" | "priority" | "resolution" | "issuetype" => {
            Element::Named
        }
        "user" => Element::User,
        "number" => Element::Number,
        "string" | "date" | "datetime" | "any" => Element::Plain,
        _ => return None,
    };
    Some(Target { many, element })
}

/// True when the value is already the shape Jira sends and expects.
fn is_wire_shape(v: &Value) -> bool {
    match v {
        Value::Object(_) => true,
        Value::Array(items) => items.first().is_some_and(Value::is_object),
        _ => false,
    }
}

fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

fn is_scalar(v: &Value) -> bool {
    matches!(v, Value::String(_) | Value::Number(_) | Value::Bool(_))
}

/// `@[Ilya A](5b10a2)` for anything that looks like a user object.
fn user_mention(v: &Value) -> Option<String> {
    let map = v.as_object()?;
    let display = map.get("displayName")?.as_str()?;
    let id = ["accountId", "name", "key"]
        .iter()
        .find_map(|k| map.get(*k).and_then(Value::as_str))?;
    Some(format!("@[{display}]({id})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(schema_type: &str, items: Option<&str>) -> FieldMeta {
        FieldMeta {
            id: "customfield_10205".into(),
            name: "Team".into(),
            custom: true,
            schema_type: Some(schema_type.into()),
            items: items.map(str::to_string),
        }
    }

    #[test]
    fn the_common_shapes_collapse_to_something_editable() {
        assert_eq!(simplify(&json!({"value": "Platform"})), json!("Platform"));
        assert_eq!(simplify(&json!({"name": "2.4.0"})), json!("2.4.0"));
        assert_eq!(
            simplify(&json!([{"value": "A"}, {"value": "B"}])),
            json!(["A", "B"])
        );
        assert_eq!(
            simplify(&json!({"accountId": "5b10a2", "displayName": "Ilya A"})),
            json!("@[Ilya A](5b10a2)")
        );
        // Data Center users are identified by name rather than accountId.
        assert_eq!(
            simplify(&json!({"name": "ilya", "displayName": "Ilya A"})),
            json!("@[Ilya A](ilya)")
        );
        assert_eq!(simplify(&json!(7)), json!(7));
        assert_eq!(simplify(&json!("plain")), json!("plain"));
    }

    #[test]
    fn what_cannot_be_collapsed_is_left_exactly_as_it_arrived() {
        let cascading = json!({"value": "Parent", "child": {"value": "Child"}});
        assert_eq!(simplify(&cascading), cascading);

        let adf = json!({"type": "doc", "version": 1, "content": []});
        assert_eq!(simplify(&adf), adf);

        let mixed = json!([{"value": "A"}, {"weird": 1}]);
        assert_eq!(simplify(&mixed), mixed);
    }

    /// The case this exists for: the file says `Platform`, the field wants
    /// `{"value": "Platform"}`, and the previous value is the proof.
    #[test]
    fn the_previous_value_decides_the_shape() {
        let previous = json!({"value": "Platform"});
        assert_eq!(
            expand(
                &json!("Engineering"),
                Some(&previous),
                None,
                Deployment::Cloud
            ),
            json!({"value": "Engineering"})
        );

        let previous = json!([{"value": "A"}]);
        assert_eq!(
            expand(&json!(["A", "B"]), Some(&previous), None, Deployment::Cloud),
            json!([{"value": "A"}, {"value": "B"}])
        );

        let previous = json!({"name": "2.4.0"});
        assert_eq!(
            expand(&json!("2.5.0"), Some(&previous), None, Deployment::Cloud),
            json!({"name": "2.5.0"})
        );

        let previous = json!({"accountId": "5b10a2", "displayName": "Ilya A"});
        assert_eq!(
            expand(
                &json!("@[Jane D](5b1af9)"),
                Some(&previous),
                None,
                Deployment::Cloud
            ),
            json!({"accountId": "5b1af9"})
        );
    }

    #[test]
    fn a_field_with_no_previous_value_falls_back_to_its_schema() {
        assert_eq!(
            expand(
                &json!("Platform"),
                None,
                Some(&meta("option", None)),
                Deployment::Cloud
            ),
            json!({"value": "Platform"})
        );
        assert_eq!(
            expand(
                &json!(["A", "B"]),
                None,
                Some(&meta("array", Some("option"))),
                Deployment::Cloud
            ),
            json!([{"value": "A"}, {"value": "B"}])
        );
        assert_eq!(
            expand(
                &json!("ilya"),
                None,
                Some(&meta("user", None)),
                Deployment::DataCenter
            ),
            json!({"name": "ilya"})
        );
        // A number typed as a string still goes out as a number.
        assert_eq!(
            expand(
                &json!("13"),
                None,
                Some(&meta("number", None)),
                Deployment::Cloud
            ),
            json!(13.0)
        );
        // Plain text needs no wrapper at all.
        assert_eq!(
            expand(
                &json!("free text"),
                None,
                Some(&meta("string", None)),
                Deployment::Cloud
            ),
            json!("free text")
        );
    }

    #[test]
    fn one_value_written_for_a_multi_value_field_is_taken_as_meant() {
        let previous = json!([{"value": "A"}]);
        assert_eq!(
            expand(&json!("B"), Some(&previous), None, Deployment::Cloud),
            json!([{"value": "B"}])
        );
    }

    #[test]
    fn emptying_a_field_clears_it() {
        let previous = json!({"value": "Platform"});
        for empty in [json!(null), json!(""), json!([])] {
            assert_eq!(
                expand(&empty, Some(&previous), None, Deployment::Cloud),
                Value::Null,
                "{empty}"
            );
        }
    }

    /// Nothing is lost by keeping the raw form: it is sent back verbatim.
    #[test]
    fn a_value_left_in_its_raw_shape_is_sent_unchanged() {
        let raw = json!({"value": "Parent", "child": {"value": "Child"}});
        assert_eq!(expand(&raw, None, None, Deployment::Cloud), raw);

        let adf = json!({"type": "doc", "version": 1, "content": []});
        assert_eq!(
            expand(&adf, Some(&adf), None, Deployment::Cloud),
            adf,
            "a rich-text custom field must round-trip"
        );
    }

    /// Without evidence, jired must not invent a wrapper.
    #[test]
    fn an_unknown_field_is_sent_as_written() {
        assert_eq!(
            expand(&json!("whatever"), None, None, Deployment::Cloud),
            json!("whatever")
        );
        assert_eq!(
            expand(
                &json!("whatever"),
                None,
                Some(&meta("plugin-thing", None)),
                Deployment::Cloud
            ),
            json!("whatever")
        );
    }

    /// simplify and expand have to be inverses for every shape jired collapses,
    /// or a pull followed by a push would rewrite values nobody touched.
    #[test]
    fn simplify_and_expand_round_trip() {
        for raw in [
            json!({"value": "Platform"}),
            json!({"name": "2.4.0"}),
            json!([{"value": "A"}, {"value": "B"}]),
            json!("plain"),
            json!(5),
        ] {
            let simple = simplify(&raw);
            let back = expand(&simple, Some(&raw), None, Deployment::Cloud);
            assert_eq!(back, raw, "{raw} did not survive the round trip");
        }

        // Users lose their display name, which the server fills back in.
        let user = json!({"accountId": "5b10a2", "displayName": "Ilya A"});
        assert_eq!(
            expand(&simplify(&user), Some(&user), None, Deployment::Cloud),
            json!({"accountId": "5b10a2"})
        );
    }
}
