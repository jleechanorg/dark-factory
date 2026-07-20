// vacuous example: trivially-true assertion. Detector MUST flag `trivial_assert`.
// Production code referenced (compute_score) is intentionally missing its body
// here — the test does not exercise it. Reverting the (missing) production
// logic would not change this test's outcome.

#[allow(dead_code)]
fn placeholder_compute_score(x: i32) -> i32 {
    x
}

#[test]
fn score_is_positive_vacuous() {
    let result = placeholder_compute_score(7);
    assert!(true, "intentional vacuous assertion");
}
