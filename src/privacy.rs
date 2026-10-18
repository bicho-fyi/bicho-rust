use regex::Regex;
use serde_json::{Map, Value};
use std::sync::LazyLock;

static SENSITIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)password|passwd|secret|token|authorization|cookie|api[_-]?key|sentry_key|email|phone|address|^user$|^ip$").unwrap()
});
static AUTH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:Bearer|Basic)\s+[A-Za-z0-9+/=_.-]+").unwrap());
static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)https?://[^\s<>\"')]+"#).unwrap());

pub(crate) fn sensitive(key: &str) -> bool {
    SENSITIVE.is_match(key)
}

pub(crate) fn clip(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(crate) fn scrub(value: &str, limit: usize) -> String {
    let value = AUTH.replace_all(clip(value, limit), "[Filtered]");
    URL.replace_all(&value, |captures: &regex::Captures<'_>| {
        let raw = captures[0].split(['?', '#']).next().unwrap_or_default();
        match reqwest::Url::parse(raw) {
            Ok(mut url) => {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.to_string()
            }
            Err(_) => "[Filtered URL]".to_owned(),
        }
    })
    .into_owned()
}

pub(crate) fn normalize(value: &Value) -> Value {
    fn visit(value: &Value, depth: usize, budget: &mut usize) -> Value {
        if depth > 5 || *budget == 0 {
            return Value::String("[Truncated]".into());
        }
        *budget -= 1;
        match value {
            Value::String(value) => Value::String(scrub(value, 2048)),
            Value::Array(values) => {
                if values.len() == 2 && values[0].as_str().is_some_and(sensitive) {
                    return Value::Array(vec![
                        values[0].clone(),
                        Value::String("[Filtered]".into()),
                    ]);
                }
                Value::Array(
                    values
                        .iter()
                        .take(50)
                        .map(|item| visit(item, depth + 1, budget))
                        .collect(),
                )
            }
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .take(50)
                    .map(|(key, value)| {
                        (
                            scrub(key, 128),
                            if sensitive(key) {
                                Value::String("[Filtered]".into())
                            } else {
                                visit(value, depth + 1, budget)
                            },
                        )
                    })
                    .collect::<Map<_, _>>(),
            ),
            _ => value.clone(),
        }
    }
    visit(value, 0, &mut 300)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_structured_credentials_and_urls_without_breaking_unicode() {
        let input = json!({"password": "hidden", "nested": {"headers": [["Authorization", "hidden"]]}, "url": "https://name:hidden@example.test/api?token=hidden#hidden", "text": "Bearer hidden", "safe": 2});
        let result = normalize(&input);
        assert!(!result.to_string().contains("hidden"));
        assert_eq!(result["safe"], 2);
        assert_eq!(result["url"], "https://example.test/api");
        assert_eq!(scrub("ééé", 3), "é");
        let deep = (0..20).fold(json!("value"), |value, _| json!({"nested": value}));
        assert!(normalize(&deep).to_string().contains("[Truncated]"));
    }
}
