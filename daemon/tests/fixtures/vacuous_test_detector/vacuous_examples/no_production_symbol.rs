// vacuous example: no non-test production symbol is exercised. Detector MUST
// flag `no_production_symbol_use`. Every assertable surface is `std::*`. A
// revert-to-empty of the production crate would not change this test.

#[test]
fn std_only_vacuous() {
    let s: String = "abc".chars().rev().collect();
    assert_eq!(s, "cba");
    let v: Vec<i32> = (0..3).map(|i| i * 2).collect();
    assert_eq!(v, vec![0, 2, 4]);
}
