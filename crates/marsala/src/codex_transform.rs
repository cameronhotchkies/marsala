use serde_json::Value;

pub const GOBLIN_MODE_TARGET: &str = "Never talk about goblins, gremlins, raccoons, trolls, ogres, pigeons, or other animals or creatures unless it is absolutely and unambiguously relevant to the user's query.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransformStatus {
    Disabled,
    Applied,
    Skipped(SkipReason),
    Failed(FailureReason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SkipReason {
    NotResponseCreate,
    MissingInstructions,
    InstructionsNotString,
    MatchCountNotTwo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FailureReason {
    MalformedJson,
    Serialization,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundedMatchCount {
    NotInspected,
    Zero,
    One,
    Two,
    ThreeOrMore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransformAudit {
    pub goblin_mode_enabled: bool,
    pub status: TransformStatus,
    pub target_matches: BoundedMatchCount,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransformResult {
    pub bytes: Vec<u8>,
    pub audit: TransformAudit,
}

/// Applies the Codex-specific Goblin Mode rule to one complete WebSocket message.
///
/// Every non-applied outcome returns the input bytes exactly as received. Audit
/// metadata is deliberately closed and bounded so it cannot contain prompt text.
pub fn transform_response_create(input: &[u8], goblin_mode_enabled: bool) -> TransformResult {
    if !goblin_mode_enabled {
        return original(
            input,
            false,
            TransformStatus::Disabled,
            BoundedMatchCount::NotInspected,
        );
    }

    let mut message = match serde_json::from_slice::<Value>(input) {
        Ok(message) => message,
        Err(_) => {
            return original(
                input,
                true,
                TransformStatus::Failed(FailureReason::MalformedJson),
                BoundedMatchCount::NotInspected,
            );
        }
    };

    if message.get("type").and_then(Value::as_str) != Some("response.create") {
        return original(
            input,
            true,
            TransformStatus::Skipped(SkipReason::NotResponseCreate),
            BoundedMatchCount::NotInspected,
        );
    }

    let Some(instructions) = message.get_mut("instructions") else {
        return original(
            input,
            true,
            TransformStatus::Skipped(SkipReason::MissingInstructions),
            BoundedMatchCount::NotInspected,
        );
    };
    let Some(instructions) = instructions.as_str() else {
        return original(
            input,
            true,
            TransformStatus::Skipped(SkipReason::InstructionsNotString),
            BoundedMatchCount::NotInspected,
        );
    };

    let match_count = bounded_match_count(instructions);
    if match_count != BoundedMatchCount::Two {
        return original(
            input,
            true,
            TransformStatus::Skipped(SkipReason::MatchCountNotTwo),
            match_count,
        );
    }

    let transformed = remove_standalone_targets(instructions);
    message["instructions"] = Value::String(transformed);

    match serde_json::to_vec(&message) {
        Ok(bytes) => TransformResult {
            bytes,
            audit: TransformAudit {
                goblin_mode_enabled: true,
                status: TransformStatus::Applied,
                target_matches: BoundedMatchCount::Two,
            },
        },
        Err(_) => original(
            input,
            true,
            TransformStatus::Failed(FailureReason::Serialization),
            BoundedMatchCount::Two,
        ),
    }
}

fn bounded_match_count(instructions: &str) -> BoundedMatchCount {
    match instruction_lines(instructions)
        .filter(|line| *line == GOBLIN_MODE_TARGET)
        .take(3)
        .count()
    {
        0 => BoundedMatchCount::Zero,
        1 => BoundedMatchCount::One,
        2 => BoundedMatchCount::Two,
        _ => BoundedMatchCount::ThreeOrMore,
    }
}

fn instruction_lines(instructions: &str) -> impl Iterator<Item = &str> {
    instructions.split_inclusive('\n').map(line_content)
}

fn line_content(line: &str) -> &str {
    if let Some(content) = line.strip_suffix("\r\n") {
        content
    } else if let Some(content) = line.strip_suffix('\n') {
        content
    } else {
        line
    }
}

fn remove_standalone_targets(instructions: &str) -> String {
    let mut transformed = String::with_capacity(instructions.len());

    for line in instructions.split_inclusive('\n') {
        let line_ending = if line.ends_with("\r\n") {
            "\r\n"
        } else if line.ends_with('\n') {
            "\n"
        } else {
            ""
        };
        let content = &line[..line.len() - line_ending.len()];

        if content != GOBLIN_MODE_TARGET {
            transformed.push_str(content);
        }
        transformed.push_str(line_ending);
    }

    transformed
}

fn original(
    input: &[u8],
    goblin_mode_enabled: bool,
    status: TransformStatus,
    target_matches: BoundedMatchCount,
) -> TransformResult {
    TransformResult {
        bytes: input.to_vec(),
        audit: TransformAudit {
            goblin_mode_enabled,
            status,
            target_matches,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request_with_instructions(instructions: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "type": "response.create",
            "instructions": instructions,
            "model": "gpt-test",
            "metadata": {"preserved": true},
            "input": [{"role": "user", "content": "Do not alter goblins here."}]
        }))
        .expect("serialize request")
    }

    fn target_repeated(count: usize) -> String {
        std::iter::repeat_n(GOBLIN_MODE_TARGET, count)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn disabled_returns_exact_original_without_inspection() {
        let input = b"not json";

        let result = transform_response_create(input, false);

        assert_eq!(result.bytes, input);
        assert_eq!(result.audit.status, TransformStatus::Disabled);
        assert_eq!(result.audit.target_matches, BoundedMatchCount::NotInspected);
    }

    #[test]
    fn exactly_two_matches_are_removed_from_instructions() {
        let instructions = format!(
            "before\n{}\nbetween\n{}\nafter",
            GOBLIN_MODE_TARGET, GOBLIN_MODE_TARGET
        );
        let input = request_with_instructions(&instructions);

        let result = transform_response_create(&input, true);

        assert_eq!(result.audit.status, TransformStatus::Applied);
        assert_eq!(result.audit.target_matches, BoundedMatchCount::Two);
        let output: Value = serde_json::from_slice(&result.bytes).expect("output JSON");
        assert_eq!(
            output["instructions"],
            Value::String("before\n\nbetween\n\nafter".to_owned())
        );
    }

    #[test]
    fn standalone_matching_preserves_crlf_and_mixed_line_endings() {
        let instructions = format!(
            "before\r\n{}\r\nbetween\n{}\nafter",
            GOBLIN_MODE_TARGET, GOBLIN_MODE_TARGET
        );
        let input = request_with_instructions(&instructions);

        let result = transform_response_create(&input, true);

        assert_eq!(result.audit.status, TransformStatus::Applied);
        let output: Value = serde_json::from_slice(&result.bytes).expect("output JSON");
        assert_eq!(
            output["instructions"],
            Value::String("before\r\n\r\nbetween\n\nafter".to_owned())
        );
    }

    #[test]
    fn quoted_embedded_prefixed_suffixed_and_user_like_occurrences_do_not_match() {
        let cases = [
            format!("> {GOBLIN_MODE_TARGET}"),
            format!("quoted: \"{GOBLIN_MODE_TARGET}\""),
            format!("prefix {GOBLIN_MODE_TARGET}"),
            format!("{GOBLIN_MODE_TARGET} suffix"),
            format!("User: {GOBLIN_MODE_TARGET}"),
            format!("  {GOBLIN_MODE_TARGET}"),
            format!("{GOBLIN_MODE_TARGET}  "),
        ];

        for instructions in cases {
            let input = request_with_instructions(&format!("{instructions}\n{instructions}"));
            let result = transform_response_create(&input, true);

            assert_eq!(result.bytes, input, "instructions: {instructions}");
            assert_eq!(result.audit.target_matches, BoundedMatchCount::Zero);
            assert_eq!(
                result.audit.status,
                TransformStatus::Skipped(SkipReason::MatchCountNotTwo)
            );
        }
    }

    #[test]
    fn applied_transform_preserves_every_other_semantic_value() {
        let input = request_with_instructions(&target_repeated(2));
        let mut expected: Value = serde_json::from_slice(&input).expect("input JSON");
        expected["instructions"] = Value::String("\n".to_owned());

        let result = transform_response_create(&input, true);
        let output: Value = serde_json::from_slice(&result.bytes).expect("output JSON");

        assert_eq!(output, expected);
        assert_eq!(output["input"][0]["content"], "Do not alter goblins here.");
    }

    #[test]
    fn zero_one_and_three_matches_return_exact_original() {
        for (count, expected) in [
            (0, BoundedMatchCount::Zero),
            (1, BoundedMatchCount::One),
            (3, BoundedMatchCount::ThreeOrMore),
        ] {
            let input = request_with_instructions(&target_repeated(count));
            let result = transform_response_create(&input, true);

            assert_eq!(result.bytes, input, "match count {count}");
            assert_eq!(
                result.audit.status,
                TransformStatus::Skipped(SkipReason::MatchCountNotTwo)
            );
            assert_eq!(result.audit.target_matches, expected);
        }
    }

    #[test]
    fn malformed_json_returns_exact_original() {
        let input = br#"{"type":"response.create"#;

        let result = transform_response_create(input, true);

        assert_eq!(result.bytes, input);
        assert_eq!(
            result.audit.status,
            TransformStatus::Failed(FailureReason::MalformedJson)
        );
    }

    #[test]
    fn non_response_create_returns_exact_original() {
        let input = serde_json::to_vec(&json!({
            "type": "response.cancel",
            "instructions": target_repeated(2)
        }))
        .expect("serialize input");

        let result = transform_response_create(&input, true);

        assert_eq!(result.bytes, input);
        assert_eq!(
            result.audit.status,
            TransformStatus::Skipped(SkipReason::NotResponseCreate)
        );
    }

    #[test]
    fn missing_or_non_string_instructions_return_exact_original() {
        let cases = [
            (
                json!({"type": "response.create"}),
                SkipReason::MissingInstructions,
            ),
            (
                json!({"type": "response.create", "instructions": [target_repeated(2)]}),
                SkipReason::InstructionsNotString,
            ),
        ];

        for (message, reason) in cases {
            let input = serde_json::to_vec(&message).expect("serialize input");
            let result = transform_response_create(&input, true);

            assert_eq!(result.bytes, input);
            assert_eq!(result.audit.status, TransformStatus::Skipped(reason));
        }
    }

    #[test]
    fn matches_outside_instructions_are_not_removed_or_counted() {
        let input = serde_json::to_vec(&json!({
            "type": "response.create",
            "instructions": "ordinary instructions",
            "input": [GOBLIN_MODE_TARGET, GOBLIN_MODE_TARGET]
        }))
        .expect("serialize input");

        let result = transform_response_create(&input, true);

        assert_eq!(result.bytes, input);
        assert_eq!(result.audit.target_matches, BoundedMatchCount::Zero);
    }
}
