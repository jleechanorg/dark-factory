// vacuous example: the test asserts `f(x) == x` on a value constructed in the
// test, with `f` either identity or never invoked. Detector MUST flag
// `symmetric_tautology`. A revert of `normalize_header` to return its input
// unchanged would still leave this green.

#[allow(dead_code)]
fn normalize_header(s: &str) -> String {
    s.to_string()
}

#[test]
fn normalize_header_is_identity_vacuous() {
    let raw = String::from("X-Trace-Id: abc123");
    let out = normalize_header(&raw);
    assert_eq!(out, raw, "only proves identity, not that normalize did work");
}
