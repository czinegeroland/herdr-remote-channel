//! RFC 8785 conformance vectors for the pinned canonicalization crate.
//!
//! PRD decision DEC-016 delegates canonical JSON to `serde_jcs` and requires
//! protocol vectors to guard that choice, because signed bytes stop being
//! interoperable the moment canonicalization drifts. PRD open question
//! OQ-002 asks whether the selected version actually passes; these vectors
//! are the answer, and they fail loudly if a future version regresses.
//!
//! Expected values were confirmed against an independent implementation
//! rather than recorded from the crate under test.

use hrc_protocol::canonical::{from_json_str, to_canonical_json};
use serde_json::json;

/// Canonicalizes JSON text and returns the result.
fn canonicalize(input: &str) -> String {
    let value: serde_json::Value = from_json_str(input).expect("input should parse");
    to_canonical_json(&value).expect("value should canonicalize")
}

#[test]
fn object_keys_sort_by_utf16_code_unit() {
    // RFC 8785 section 3.2.3 sorts by UTF-16 code units, not by code point
    // and not by locale. The distinction shows above the BMP: U+1F600 sorts
    // by its leading surrogate (0xD83D), which places it before U+FB33 even
    // though its code point is far larger.
    let input = concat!(
        r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","#,
        r#""\ufb33":"Hebrew Letter Dalet With Dagesh","1":"One","#,
        r#""\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","#,
        r#""\u00f6":"Latin Small Letter O With Diaeresis"}"#
    );

    let expected = concat!(
        "{\"\\r\":\"Carriage Return\",",
        "\"1\":\"One\",",
        "\"\u{80}\":\"Control\",",
        "\"\u{f6}\":\"Latin Small Letter O With Diaeresis\",",
        "\"\u{20ac}\":\"Euro Sign\",",
        "\"\u{1f600}\":\"Emoji: Grinning Face\",",
        "\"\u{fb33}\":\"Hebrew Letter Dalet With Dagesh\"}"
    );

    assert_eq!(canonicalize(input), expected);
}

#[test]
fn whitespace_is_removed_and_nesting_is_preserved() {
    let input = r#"
        {
            "outer" : {
                "b" : [ 1 , 2 , 3 ],
                "a" : null
            },
            "after" : true
        }
    "#;

    assert_eq!(
        canonicalize(input),
        r#"{"after":true,"outer":{"a":null,"b":[1,2,3]}}"#
    );
}

#[test]
fn array_order_is_never_changed() {
    // Arrays carry meaning in their order, so canonicalization must leave
    // them alone even when the elements would sort differently.
    assert_eq!(canonicalize("[3,1,2]"), "[3,1,2]");
    assert_eq!(
        canonicalize(r#"[{"b":1,"a":2},{"d":3,"c":4}]"#),
        r#"[{"a":2,"b":1},{"c":4,"d":3}]"#
    );
}

#[test]
fn numbers_use_the_shortest_ecmascript_form() {
    // RFC 8785 section 3.2.2.3 defers to ECMAScript `Number::toString`.
    // These are the cases where a naive printer diverges: trailing zeros,
    // the exponent thresholds at 1e21 and 1e-7, and negative zero.
    let cases = [
        ("1.0", "1"),
        ("1.50", "1.5"),
        ("-0", "0"),
        ("1e2", "100"),
        ("1e21", "1e+21"),
        ("1e-7", "1e-7"),
        ("0.000001", "0.000001"),
        ("333333333.33333329", "333333333.3333333"),
    ];

    for (input, expected) in cases {
        assert_eq!(canonicalize(input), expected, "canonicalizing {input}");
    }
}

#[test]
fn integers_beyond_the_double_safe_range_lose_precision() {
    // RFC 8785 numbers are ECMAScript doubles, so an integer above
    // 2^53 - 1 does not survive canonicalization. This is correct
    // behavior, and it is a hard constraint on the protocol: any field
    // that must round-trip exactly and can exceed 9007199254740991 has to
    // be carried as a string, not a JSON number. See PRD section 18.0.
    assert_eq!(canonicalize("9007199254740991"), "9007199254740991");
    assert_eq!(canonicalize("-9007199254740991"), "-9007199254740991");

    assert_eq!(canonicalize("9007199254740993"), "9007199254740992");
    assert_eq!(canonicalize("18446744073709551615"), "18446744073709552000");
}

#[test]
fn strings_use_the_shortest_escapes() {
    // RFC 8785 section 3.2.2.2: use the two-character forms where they
    // exist, fall back to lowercase \u00xx for the remaining C0 controls,
    // never escape the forward slash, and leave C1 controls literal.
    let value = json!("\u{0}\u{1}\u{8}\u{9}\u{a}\u{c}\u{d}\"\\/\u{7f}");

    assert_eq!(
        to_canonical_json(&value).unwrap(),
        "\"\\u0000\\u0001\\b\\t\\n\\f\\r\\\"\\\\/\u{7f}\""
    );
}

#[test]
fn non_ascii_text_stays_literal() {
    // Canonical output is UTF-8, so characters outside ASCII are emitted as
    // themselves rather than as \u escapes.
    assert_eq!(canonicalize(r#"{"k":"\u00e9\u4e2d"}"#), "{\"k\":\"é中\"}");
}

#[test]
fn canonical_output_is_idempotent() {
    let input = r#"{"z":[{"b":1.50,"a":"\u00e9"}],"a":1e2}"#;
    let once = canonicalize(input);
    assert_eq!(canonicalize(&once), once);
    assert_eq!(once, r#"{"a":100,"z":[{"a":"é","b":1.5}]}"#);
}

#[test]
fn a_realistic_protocol_payload_canonicalizes_stably() {
    // A control-entry-shaped object, to keep the vectors anchored to the
    // objects HRC actually signs rather than to synthetic edge cases.
    let input = r#"{
        "version": 1,
        "channelId": "9f2c",
        "sequence": 2,
        "previousHash": "1a2b",
        "epoch": 1,
        "createdAt": "2026-09-13T00:00:00Z",
        "operation": "add_member",
        "body": { "principalId": "p-1", "devices": ["d-2", "d-1"] }
    }"#;

    assert_eq!(
        canonicalize(input),
        concat!(
            r#"{"body":{"devices":["d-2","d-1"],"principalId":"p-1"},"#,
            r#""channelId":"9f2c","createdAt":"2026-09-13T00:00:00Z","epoch":1,"#,
            r#""operation":"add_member","previousHash":"1a2b","sequence":2,"version":1}"#
        )
    );
}
