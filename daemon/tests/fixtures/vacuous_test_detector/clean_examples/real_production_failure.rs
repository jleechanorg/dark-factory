// NON-vacuous example: the test exercises a production function on a real
// input and asserts a property that ONLY holds when the production logic is
// correct. Reverting `parse_score` to return 0 unconditionally would cause this
// test to fail.

#[derive(Debug, PartialEq)]
#[allow(dead_code)]
enum ScoreClass {
    Low,
    Mid,
    High,
}

#[allow(dead_code)]
fn parse_score(raw: &str) -> ScoreClass {
    // Production logic: < 50 -> Low, 50..=80 -> Mid, > 80 -> High.
    let n: i32 = raw.parse().unwrap_or(-1);
    if n < 50 {
        ScoreClass::Low
    } else if n <= 80 {
        ScoreClass::Mid
    } else {
        ScoreClass::High
    }
}

#[test]
fn parse_score_classifies_buckets_real() {
    assert_eq!(parse_score("42"), ScoreClass::Low);
    assert_eq!(parse_score("75"), ScoreClass::Mid);
    assert_eq!(parse_score("99"), ScoreClass::High);
}

#[test]
fn parse_score_rejects_garbage_input() {
    // The negative-input sentinel path is a production-only branch; no
    // symmetry trick can make this green without doing real work.
    assert_eq!(parse_score("not-a-number"), ScoreClass::Low);
}
