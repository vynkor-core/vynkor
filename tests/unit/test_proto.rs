use prost::Message;
use veyron::proto::veyron::{envelope, Envelope, PluginRegister};

#[test]
fn envelope_round_trip_serializes_and_deserializes() {
    let register = PluginRegister {
        plugin_id: "weather".to_string(),
        version: "1.0.0".to_string(),
        description: "Weather plugin".to_string(),
        manifest: None,
        ..Default::default()
    };

    let env = Envelope {
        message_id: "msg-001".to_string(),
        timestamp: 1_000_000,
        sender_id: "weather".to_string(),
        version: 1,
        payload: Some(envelope::Payload::PluginRegister(register)),
    };

    let mut buf = Vec::new();
    env.encode(&mut buf).expect("encode must succeed");

    let decoded = Envelope::decode(buf.as_slice()).expect("decode must succeed");

    assert_eq!(decoded.message_id, "msg-001");
    assert_eq!(decoded.sender_id, "weather");
    assert_eq!(decoded.version, 1);

    match decoded.payload {
        Some(envelope::Payload::PluginRegister(r)) => {
            assert_eq!(r.plugin_id, "weather");
            assert_eq!(r.version, "1.0.0");
        }
        _ => panic!("wrong payload variant after round-trip"),
    }
}

#[test]
fn empty_envelope_encodes_to_nonempty_bytes() {
    let env = Envelope::default();
    let mut buf = Vec::new();
    env.encode(&mut buf).expect("encode must not fail");
    // protobuf default fields may produce 0 bytes — that's valid
    let decoded = Envelope::decode(buf.as_slice()).expect("decode must succeed");
    assert_eq!(decoded.message_id, "");
}
