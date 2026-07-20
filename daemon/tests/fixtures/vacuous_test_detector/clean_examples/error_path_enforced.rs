// NON-vacuous example: the test asserts the NEGATIVE production path — that an
// invalid input is REJECTED. Reverting `validate_path` to always Ok would
// cause this test to fail.

#[derive(Debug, PartialEq)]
#[allow(dead_code)]
enum PathError {
    TooDeep,
    BadSegment,
}

#[allow(dead_code)]
fn validate_path(segments: &[&str]) -> Result<(), PathError> {
    if segments.len() > 8 {
        return Err(PathError::TooDeep);
    }
    for s in segments {
        if s.is_empty() || s.contains('..') {
            return Err(PathError::BadSegment);
        }
    }
    Ok(())
}

#[test]
fn validate_path_rejects_too_deep_real() {
    let segs = vec!["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    assert_eq!(validate_path(&segs), Err(PathError::TooDeep));
}

#[test]
fn validate_path_rejects_dotdot_real() {
    let segs = vec!["home", "..", "secret"];
    assert_eq!(validate_path(&segs), Err(PathError::BadSegment));
}

#[test]
fn validate_path_accepts_well_formed_real() {
    let segs = vec!["home", "alice", "notes"];
    assert_eq!(validate_path(&segs), Ok(()));
}
