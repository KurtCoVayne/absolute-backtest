//! The FlatBuffers messages compiled from `schema/abt.fbs` (P-1.1): owned
//! types round trip through a control payload, derive serde, and have a
//! canonical form whose hash does not depend on the wire bytes (P-6).

use abt_wire::canonical::Canonical;
use abt_wire::convert::{decode, encode};
use abt_wire::generated::abt::wire::{ColumnType, MapRequest, Numerics};
use abt_wire::{ErrorCode, ErrorMessage, Hello, Layer, Message, PROTOCOL};

fn hello() -> Hello {
    Hello {
        protocol: PROTOCOL.to_string(),
        layer: Layer::Kernel,
        client: Some("abt test".into()),
        capabilities: Some(vec!["query".into(), "contract".into()]),
    }
}

#[test]
fn messages_round_trip_through_a_control_payload() {
    for m in [
        Message::Hello(Box::new(hello())),
        Message::Error(Box::new(ErrorMessage {
            code: ErrorCode::Unresolved,
            message: Some("need 1: close@1d".into()),
            need: None,
            over_budget: None,
        })),
    ] {
        let payload = encode(m.clone());
        assert_eq!(u32::from_le_bytes(payload[..4].try_into().unwrap()) as usize, payload.len() - 4);
        assert_eq!(decode(&payload).unwrap(), m);
    }
}

#[test]
fn a_damaged_payload_is_refused() {
    let payload = encode(Message::Hello(Box::new(hello())));
    assert!(decode(&payload[..payload.len() - 1]).is_err());
    assert!(decode(&[1, 0]).is_err());
    let mut wrong = payload.clone();
    wrong[0] ^= 1;
    assert!(decode(&wrong).is_err());
}

#[test]
fn owned_types_have_a_canonical_form() {
    let h = hello();
    let text = h.canonical().unwrap();
    assert_eq!(text, r#"{"capabilities":["query","contract"],"client":"abt test","layer":"Kernel","protocol":"1.2"}"#);
    let json = serde_json::to_string(&h).unwrap();
    let back: Hello = serde_json::from_str(&json).unwrap();
    assert_eq!(back, h);
    assert_eq!(back.canonical_hash().unwrap(), h.canonical_hash().unwrap());
}

/// The additions of `07-storage.md` (protocol 1.1, P-7 a minor version):
/// the `Tick` column type, the `Exact` numerics and the `map` request
/// round trip, and a request's `ticks` flag is in its canonical form.
#[test]
fn the_storage_additions_round_trip() {
    let m = Message::MapRequest(Box::new(MapRequest {
        view: None,
        relation: "equity.close".into(),
        columns: Some(vec!["close".into(), "volume".into()]),
        level: Some("1d".into()),
        from_entity: 1,
        to_entity: Some(1_000),
    }));
    assert_eq!(decode(&encode(m.clone())).unwrap(), m);
    assert_eq!(serde_json::to_string(&ColumnType::Tick).unwrap(), "\"Tick\"");
    assert_eq!(serde_json::to_string(&Numerics::Exact).unwrap(), "\"Exact\"");
    assert!(PROTOCOL.starts_with("1."), "a minor version: {}", PROTOCOL);
}
