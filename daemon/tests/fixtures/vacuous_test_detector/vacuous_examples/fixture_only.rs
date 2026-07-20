// vacuous example: assertion against a value constructed entirely inside the
// test. Detector MUST flag `fixture_only_assert`. This pattern would still pass
// even if every production function it touched were deleted, because the test
// never observes production output — it only observes its own input.

#[derive(Debug, PartialEq)]
#[allow(dead_code)]
struct Packet {
    seq: u32,
    payload: Vec<u8>,
}

#[allow(dead_code)]
fn build_packet(seq: u32, payload: Vec<u8>) -> Packet {
    Packet { seq, payload }
}

#[test]
fn packet_roundtrip_vacuous() {
    let p = build_packet(1, vec![0xAA]);
    assert_eq!(p.seq, 1, "seq echoed back exactly what the test put in");
    assert_eq!(p.payload, vec![0xAA]);
}
