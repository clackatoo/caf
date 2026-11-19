//! Matching a message against a script's pattern.

use serde_json::Value;

/// Whether `value` has everything `pattern` has.
///
/// An object matches if each of the pattern's keys is there and matches;
/// other keys are ignored. An array matches an array of the same length
/// whose items match in order. Anything else must be equal.
pub(crate) fn matches(pattern: &Value, value: &Value) -> bool {
    match (pattern, value) {
        (Value::Object(want), Value::Object(have)) => want
            .iter()
            .all(|(key, want)| have.get(key).is_some_and(|have| matches(want, have))),
        (Value::Array(want), Value::Array(have)) => {
            want.len() == have.len()
                && want
                    .iter()
                    .zip(have)
                    .all(|(want, have)| matches(want, have))
        }
        _ => pattern == value,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::matches;

    #[test]
    fn objects_match_on_the_keys_asked_for() {
        let message = json!({"id": 1, "method": "session/prompt", "params": {"sessionId": "s1"}});
        assert!(matches(&json!({}), &message));
        assert!(matches(&json!({"params": {"sessionId": "s1"}}), &message));
        assert!(!matches(&json!({"params": {"sessionId": "s2"}}), &message));
        assert!(!matches(&json!({"result": null}), &message));
    }

    #[test]
    fn arrays_match_item_by_item() {
        let blocks = json!([{"type": "text", "text": "hi"}, {"type": "image"}]);
        assert!(matches(&json!([{"text": "hi"}, {}]), &blocks));
        assert!(!matches(&json!([{"text": "hi"}]), &blocks));
    }

    #[test]
    fn everything_else_is_equal_or_not() {
        assert!(matches(&json!(null), &json!(null)));
        assert!(!matches(&json!(1), &json!(1.0)));
        assert!(!matches(&json!("1"), &json!(1)));
    }
}
