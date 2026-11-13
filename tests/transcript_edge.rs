//! Edge-case tests for `agentflow::transcript::parse` — see
//! src/transcript.rs for the JSONL contract it implements.
//!
//! The parser is DEFENSIVE by contract: non-JSON lines are ignored, no
//! input may panic, and a stream that yields no usable usage yields
//! `usage: None`. These tests pin that behavior, including what happens
//! on malformed input (that behavior IS the contract — the authoritative
//! usage is the one on the LAST record that carries a usable usage).

use agentflow::transcript::{parse, Transcript, Usage};

/// Join record strings into a JSONL stream (no trailing newline unless
/// the caller ends with one — matching how a captured stdout arrives).
fn stream(lines: &[&str]) -> String {
    lines.join("\n")
}

#[test]
fn empty_input_yields_default_transcript() {
    // Truly empty stdout.
    let t = parse("");
    assert_eq!(t, Transcript::default());
    assert_eq!(t.rendered, "");
    assert_eq!(t.usage, None);

    // Whitespace-only and blank lines are skipped, not rendered.
    let t = parse(&stream(&["   ", "\t", ""]));
    assert_eq!(t.rendered, "");
    assert_eq!(t.usage, None);
}

#[test]
fn garbage_lines_are_ignored_between_valid_records() {
    let out = parse(&stream(&[
        "<<< not JSON at all >>>",
        "[1, 2, 3]",           // valid JSON, but not an object with a type
        "{\"no_type\": true}", // object without a "type" string
        "{\"type\": 42}",      // "type" that is not a string
        "{\"type\":\"session\",\"version\":3,\"sessionId\":\"s1\"}",
        "",
        "   ",
        "{\"type\":\"turn_end\",\"usage\":{\"input\":7,\"output\":3,\"totalTokens\":10}}",
        "{\"type\":\"turn_end\",\"usage\":\"oops\"}", // usage that is not an object: unusable, ignored
        "{\"type\":\"agent_end\"}",
        "{truncated garbage",
    ]));
    // The garbage never reached the rendering, and the valid record's
    // usage survived the later unusable-usage record.
    assert_eq!(t_usage(&out), Some((7, 3, 10, None)));
    // session / turn_end / agent_end render nothing themselves — only the
    // usage summary line.
    assert_eq!(out.rendered, "usage: 7 in, 3 out, 10 total");
}

#[test]
fn missing_or_partial_usage_defaults_defensively() {
    // No record carries any usage at all → usage: None, not a failure.
    let t = parse(&stream(&[
        "{\"type\":\"message_end\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}",
        "{\"type\":\"turn_end\"}",
        "{\"type\":\"agent_end\"}",
    ]));
    assert_eq!(t.usage, None);
    assert_eq!(t.rendered, "user: hi");

    // usage WITHOUT totalTokens falls back to input+output when both exist.
    let t = parse(&stream(&[
        "{\"type\":\"agent_end\",\"usage\":{\"input\":4,\"output\":2}}",
    ]));
    assert_eq!(t.usage, Some(usage(4, 2, 6, None)));

    // usage with NEITHER totalTokens nor input+output is not usable.
    let t = parse("{\"type\":\"agent_end\",\"usage\":{\"vendor\":\"x\"}}");
    assert_eq!(t.usage, None);

    // Missing input/output default to 0 when a total is present.
    let t = parse("{\"type\":\"agent_end\",\"usage\":{\"totalTokens\":9}}");
    assert_eq!(t.usage, Some(usage(0, 0, 9, None)));
}

#[test]
fn truncated_final_line_without_trailing_newline_is_ignored() {
    // The captured stdout ends MID-JSON with no trailing newline: the
    // partial line is not valid JSON and must be dropped, while everything
    // before it still parses (usage included) and the streamed-but-never-
    // finished text is surfaced as truncated.
    let out = parse(&stream(&[
        "{\"type\":\"message_start\",\"message\":{\"role\":\"assistant\",\"content\":[]}}",
        "{\"type\":\"message_update\",\"usage\":{\"input\":10,\"output\":5,\"totalTokens\":15},\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"Hel\"}}",
        "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_delta\",\"delta\":\"lo w\"}}",
        "{\"type\":\"message_update\",\"assistantMessageEvent\":{\"type\":\"text_",
    ])); // join() adds no trailing newline: the last line is cut mid-token
    assert_eq!(t_usage(&out), Some((10, 5, 15, None)));
    assert_eq!(
        out.rendered,
        "assistant (truncated): Hello w\nusage: 10 in, 5 out, 15 total"
    );
}

#[test]
fn multibyte_utf8_survives_and_long_lines_are_bounded() {
    // Multi-byte content round-trips verbatim into the rendering.
    let text = "こんにちは 🌍 — café naïve résumé";
    let out = parse(&stream(&[
        "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"IGNORED\"}]}}",
        &format!(
            "{{\"type\":\"message_end\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{text}\"}}]}}}}"
        ),
    ]));
    assert!(
        out.rendered.contains(&format!("assistant: {text}")),
        "rendered: {}",
        out.rendered
    );

    // One very long single line: a tool call whose arguments exceed the
    // bounded summary — cut at 160 CHARS (char boundary, not bytes) with
    // an ellipsis marker, so no multi-byte char is split.
    let long_command = "ä".repeat(200);
    let out = parse(&format!(
        "{{\"type\":\"message_update\",\"assistantMessageEvent\":{{\"type\":\"toolcall_end\",\"toolCall\":{{\"type\":\"toolCall\",\"id\":\"c1\",\"name\":\"bash\",\"arguments\":{{\"command\":\"{long_command}\"}}}}}}}}",
    ));
    let expected = format!("[tool] bash: {}…", "ä".repeat(160));
    assert_eq!(out.rendered, expected);
}

#[test]
fn usage_is_from_the_last_record_that_carries_one() {
    // Partial usage rides early records; the FINAL totals ride the last
    // ones. The LAST usable usage wins — and a float input is accepted.
    let out = parse(&stream(&[
        "{\"type\":\"turn_end\",\"usage\":{\"input\":100,\"output\":50,\"totalTokens\":150}}",
        "{\"type\":\"agent_end\",\"usage\":{\"input\":200.0,\"output\":100,\"totalTokens\":300,\"cost\":{\"total\":0.0042}}}",
    ]));
    assert_eq!(t_usage(&out), Some((200, 100, 300, Some(4200))));
    assert_eq!(
        out.usage,
        Some(Usage {
            input: 200,
            output: 100,
            total_tokens: 300,
            cost_micros: Some(4200), // $0.0042 in micro-USD
        })
    );
    assert_eq!(out.rendered, "usage: 200 in, 100 out, 300 total ($0.0042)");

    // A reported cost of 0 is NOT a measurement (indistinguishable from a
    // free run): the usage counts, the cost does not.
    let t = parse("{\"type\":\"agent_end\",\"usage\":{\"input\":1,\"output\":1,\"totalTokens\":2,\"cost\":{\"total\":0}}}");
    assert_eq!(t_usage(&t), Some((1, 1, 2, None)));

    // A malformed cost (negative total) is likewise not a measurement.
    let t = parse("{\"type\":\"agent_end\",\"usage\":{\"input\":1,\"output\":1,\"totalTokens\":2,\"cost\":{\"total\":-1}}}");
    assert_eq!(t_usage(&t), Some((1, 1, 2, None)));
}

/// The parsed usage as a plain tuple, for readable assertions:
/// `(input, output, total_tokens, cost_micros)`.
fn t_usage(t: &agentflow::transcript::Transcript) -> Option<(u64, u64, u64, Option<u64>)> {
    t.usage
        .map(|u| (u.input, u.output, u.total_tokens, u.cost_micros))
}

/// `(input, output, total, cost_micros)` → a [`Usage`], so assertions can
/// stay on one line instead of spelling every field.
fn usage(i: u64, o: u64, total: u64, cost_micros: Option<u64>) -> Usage {
    Usage {
        input: i,
        output: o,
        total_tokens: total,
        cost_micros,
    }
}
