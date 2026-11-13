//! Tests of the codec and wire shapes.

use serde_json::json;

use crate::frame::*;
use crate::protocol::{Event, Method};
use crate::types::*;

fn lines(d: &mut FrameDecoder) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(l) = d.next_line().unwrap() {
        out.push(String::from_utf8(l).unwrap());
    }
    out
}

#[test]
fn splits_arbitrary_chunks() {
    let data = b"{\"a\":1}\n{\"b\":2}\n\n{\"c\":3}\n";
    for chunk in 1..data.len() {
        let mut d = FrameDecoder::new();
        let mut got = Vec::new();
        for part in data.chunks(chunk) {
            d.push(part);
            got.extend(lines(&mut d));
        }
        assert_eq!(
            got,
            ["{\"a\":1}", "{\"b\":2}", "{\"c\":3}"],
            "chunk {chunk}"
        );
        assert_eq!(d.pending(), 0);
    }
}

#[test]
fn partial_line_waits() {
    let mut d = FrameDecoder::new();
    d.push(b"{\"a\"");
    assert!(d.next_line().unwrap().is_none());
    assert_eq!(d.pending(), 4);
    d.push(b":1}\n");
    assert_eq!(lines(&mut d), ["{\"a\":1}"]);
}

#[test]
fn line_at_the_limit_is_accepted_and_one_over_fails() {
    let mut d = FrameDecoder::new();
    d.push(&vec![b'x'; MAX_LINE]);
    assert!(d.next_line().unwrap().is_none());
    d.push(b"\n");
    assert_eq!(d.next_line().unwrap().unwrap().len(), MAX_LINE);

    let mut d = FrameDecoder::new();
    d.push(&vec![b'x'; MAX_LINE + 1]);
    assert!(matches!(d.next_line(), Err(FrameError::TooLong)));
    d.push(b"\n{}\n");
    assert!(matches!(d.next_line(), Err(FrameError::TooLong)));
}

#[test]
fn complete_overlong_line_in_one_chunk_fails() {
    let mut d = FrameDecoder::new();
    let mut data = vec![b'x'; MAX_LINE + 1];
    data.push(b'\n');
    d.push(&data);
    assert!(matches!(d.next_line(), Err(FrameError::TooLong)));
}

#[test]
fn many_small_frames_do_not_grow_the_buffer() {
    let mut d = FrameDecoder::new();
    for _ in 0..50_000 {
        d.push(b"{\"a\":1}\n");
        assert!(d.next_line().unwrap().is_some());
    }
    assert_eq!(d.pending(), 0);
}

#[test]
fn encode_rejects_overlong_and_terminates_with_newline() {
    let ok = encode_line(&json!({"a": 1})).unwrap();
    assert_eq!(ok.last(), Some(&b'\n'));
    assert_eq!(ok.iter().filter(|&&b| b == b'\n').count(), 1);
    let big = json!("a".repeat(MAX_LINE));
    assert!(matches!(encode_line(&big), Err(FrameError::TooLong)));
}

#[test]
fn request_wire_shape() {
    let m = Method::ChatSend(ChatSendParams {
        chat: "main".into(),
        text: "hi".into(),
    });
    let f = RequestFrame::new(12, &m).unwrap();
    assert_eq!(
        serde_json::to_value(&f).unwrap(),
        json!({"id": 12, "method": "chat.send", "params": {"chat": "main", "text": "hi"}})
    );
    assert_eq!(f.method().unwrap(), m);
}

#[test]
fn request_without_params_is_accepted() {
    let f: RequestFrame = serde_json::from_str(r#"{"id":1,"method":"daemon.status"}"#).unwrap();
    assert!(matches!(f.method().unwrap(), Method::DaemonStatus(_)));
}

#[test]
fn unknown_method_and_bad_params_are_distinguished() {
    let e = Method::from_parts("nope.nothing", json!({})).unwrap_err();
    assert_eq!(e.code, ErrorCode::UnknownMethod);
    let e = Method::from_parts("chat.send", json!({"chat": 3})).unwrap_err();
    assert_eq!(e.code, ErrorCode::BadRequest);
}

#[test]
fn unknown_params_fields_are_ignored() {
    let m = Method::from_parts(
        "chat.mark_seen",
        json!({"chat": "main", "message_id": "m", "future": [1]}),
    );
    assert!(m.is_ok());
}

#[test]
fn response_wire_shapes() {
    let ok = Response::success(7, json!({"turn": "t-19"}));
    assert_eq!(
        serde_json::to_value(&ok).unwrap(),
        json!({"id": 7, "ok": true, "result": {"turn": "t-19"}})
    );
    let err = Response::failure(7, IpcError::new(ErrorCode::NotPaired, "no"));
    assert_eq!(
        serde_json::to_value(&err).unwrap(),
        json!({"id": 7, "ok": false, "error": {"code": "not_paired", "message": "no"}})
    );
    assert_eq!(err.into_value().unwrap_err().code, ErrorCode::NotPaired);
}

#[test]
fn event_wire_shape_and_unknown_events() {
    let e = Event::NotifyCount(NotifyCountEvent { unseen: 2 });
    let f = EventFrame::new(4411, &e).unwrap();
    assert_eq!(
        serde_json::to_value(&f).unwrap(),
        json!({"seq": 4411, "event": "notify.count", "data": {"unseen": 2}})
    );
    assert_eq!(f.typed().unwrap(), e);
    let line = br#"{"seq":5,"event":"future.thing","data":{"x":1}}"#;
    let ServerFrame::Event(f) = ServerFrame::decode(line).unwrap() else {
        panic!("expected event")
    };
    let ev = f.typed().unwrap();
    assert!(matches!(&ev, Event::Unknown { event, .. } if event == "future.thing"));
    assert_eq!(ev.topic(), Topic::Unknown);
}

#[test]
fn server_frame_discrimination() {
    assert!(matches!(
        ServerFrame::decode(br#"{"id":1,"ok":true,"result":{}}"#).unwrap(),
        ServerFrame::Response(_)
    ));
    assert!(matches!(
        ServerFrame::decode(br#"{"hello":1}"#),
        Err(FrameError::Shape)
    ));
    assert!(matches!(
        ServerFrame::decode(b"[1]"),
        Err(FrameError::Shape)
    ));
    assert!(matches!(
        ServerFrame::decode(b"{nope"),
        Err(FrameError::Json(_))
    ));
    assert!(matches!(
        ServerFrame::decode(b"\xff\xfe"),
        Err(FrameError::Json(_))
    ));
}

#[test]
fn malformed_replies_become_internal_errors() {
    let r: Response = serde_json::from_str(r#"{"id":1,"ok":true}"#).unwrap();
    assert_eq!(r.into_value().unwrap_err().code, ErrorCode::Internal);
    let r: Response = serde_json::from_str(r#"{"id":1,"ok":false}"#).unwrap();
    assert_eq!(r.into_value().unwrap_err().code, ErrorCode::Internal);
}

#[test]
fn unknown_enum_values_deserialize_to_unknown() {
    let de = |s: &str| serde_json::from_value::<LinkState>(json!(s)).unwrap();
    assert_eq!(de("hibernating"), LinkState::Unknown);
    assert_eq!(de("online"), LinkState::Online);
    assert_eq!(
        serde_json::from_value::<ErrorCode>(json!("novel")).unwrap(),
        ErrorCode::Unknown
    );
    assert_eq!(
        serde_json::from_value::<Topic>(json!("future")).unwrap(),
        Topic::Unknown
    );
    assert_eq!(
        serde_json::from_value::<NoticeKind>(json!("x")).unwrap(),
        NoticeKind::Unknown
    );
    assert_eq!(
        serde_json::to_value(LinkState::AuthFailed).unwrap(),
        json!("auth_failed")
    );
}

#[test]
fn unknown_enums_and_extra_fields_inside_structs() {
    let m: Message = serde_json::from_value(json!({
        "id": "m", "chat": "main", "role": "robot", "text": "x", "at": 1,
        "state": "teleporting", "origin": "mars", "extra": {"a": 1}
    }))
    .unwrap();
    assert_eq!(m.role, Role::Unknown);
    assert_eq!(m.state, MessageState::Unknown);
    assert_eq!(m.origin, Origin::Unknown);
    let e = Event::from_parts(
        "link.state",
        json!({"state": "new_state", "since": 1, "other": true}),
    )
    .unwrap();
    assert!(matches!(e, Event::LinkState(s) if s.state == LinkState::Unknown));
}

#[test]
fn as_str_matches_serde() {
    for t in [Topic::Daemon, Topic::Settings, Topic::Unknown] {
        assert_eq!(serde_json::to_value(t).unwrap(), json!(t.as_str()));
    }
}

#[test]
fn caption_uses_final_key() {
    let c: VoiceCaptionEvent =
        serde_json::from_value(json!({"role": "user", "text": "a", "final": false})).unwrap();
    assert!(!c.is_final);
}

#[test]
fn consent_partial_update() {
    let mut c = Consent::default();
    c.apply(&ConsentUpdate {
        shell: Some(false),
        ..Default::default()
    });
    assert!(!c.shell && c.file_read && c.file_write && c.drop_ins);
}
