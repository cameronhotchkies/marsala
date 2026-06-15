use serde_json::Value;
use std::collections::BTreeMap;

const FIXTURE: &str = include_str!("../testdata/codex/response-create.json");
const TARGET: &str = "Never talk about goblins, gremlins, raccoons, trolls, ogres, pigeons, or other animals or creatures unless it is absolutely and unambiguously relevant to the user's query.";

#[test]
fn sanitized_response_create_fixture_has_expected_shape() {
    let request: Value = serde_json::from_str(FIXTURE).expect("fixture must be valid JSON");

    assert_eq!(request["type"], "response.create");
    assert!(request["instructions"].is_string());
    assert!(request["input"].is_array());
    assert!(request["tools"].is_array());
    assert_eq!(request["stream"], true);
    assert_eq!(
        target_occurrences_by_path(&request),
        BTreeMap::from([("$.instructions".to_owned(), 2)])
    );
}

fn target_occurrences_by_path(value: &Value) -> BTreeMap<String, usize> {
    fn visit(value: &Value, path: &str, occurrences: &mut BTreeMap<String, usize>) {
        match value {
            Value::String(text) => {
                let count = text.matches(TARGET).count();
                if count > 0 {
                    occurrences.insert(path.to_owned(), count);
                }
            }
            Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    visit(value, &format!("{path}[{index}]"), occurrences);
                }
            }
            Value::Object(fields) => {
                for (name, value) in fields {
                    visit(value, &format!("{path}.{name}"), occurrences);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) => {}
        }
    }

    let mut occurrences = BTreeMap::new();
    visit(value, "$", &mut occurrences);
    occurrences
}

#[test]
fn sanitized_response_create_fixture_has_no_obvious_secret_markers() {
    let fixture = FIXTURE.to_ascii_lowercase();
    let forbidden = [
        "authorization",
        "bearer ",
        "api_key",
        "api-key",
        "access_token",
        "refresh_token",
        "cookie",
        "prompt_cache_key",
        "installation_id",
        "session_id",
        "thread_id",
        "user@example",
    ];

    for marker in forbidden {
        assert!(
            !fixture.contains(marker),
            "fixture contains forbidden marker: {marker}"
        );
    }
}
