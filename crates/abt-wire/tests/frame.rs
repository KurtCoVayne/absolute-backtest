//! The frame codec (P-2.2): round trips, the size limits, truncation.

use abt_wire::frame::{Frame, FrameError, FrameKind, MAX_CONTROL, MAX_FRAME};

#[test]
fn frames_round_trip_through_a_byte_stream() {
    let frames = vec![
        Frame::new(FrameKind::Hello, 0, b"hi".to_vec()),
        Frame::new(FrameKind::Request, 7, vec![1, 2, 3]),
        Frame::new(FrameKind::Batch, 7, vec![9; 100_000]),
        Frame::new(FrameKind::End, 7, vec![]),
        Frame::new(FrameKind::Ping, u64::MAX, vec![]),
    ];
    let mut bytes = Vec::new();
    for f in &frames {
        f.write_to(&mut bytes).unwrap();
    }
    let mut r = bytes.as_slice();
    let mut back = Vec::new();
    while let Some(f) = Frame::read_from(&mut r).unwrap() {
        back.push(f);
    }
    assert_eq!(back, frames);
    // The header: length counts kind, correlation and payload.
    let one = frames[1].encode().unwrap();
    assert_eq!(&one[..4], &(9u32 + 3).to_le_bytes());
    assert_eq!(one[4], FrameKind::Request as u8);
    assert_eq!(&one[5..13], &7u64.to_le_bytes());
}

#[test]
fn a_partial_buffer_decodes_nothing_until_the_frame_is_whole() {
    let bytes = Frame::new(FrameKind::Response, 3, vec![5; 40]).encode().unwrap();
    for cut in 0..bytes.len() {
        assert!(Frame::decode_prefix(&bytes[..cut]).unwrap().is_none(), "cut at {}", cut);
    }
    let (f, used) = Frame::decode_prefix(&bytes).unwrap().unwrap();
    assert_eq!((f.correlation, used), (3, bytes.len()));
}

#[test]
fn limits_and_malformed_frames_are_refused() {
    let big_control = Frame::new(FrameKind::Request, 1, vec![0; MAX_CONTROL + 1]);
    assert!(matches!(big_control.encode(), Err(FrameError::TooLarge { .. })));
    // A batch may be larger than a control payload, not than a frame.
    assert!(Frame::new(FrameKind::Batch, 1, vec![0; MAX_CONTROL + 1]).encode().is_ok());
    assert!(matches!(Frame::new(FrameKind::Batch, 1, vec![0; MAX_FRAME]).encode(), Err(FrameError::TooLarge { .. })));
    // An unknown kind, a short length, a truncated stream.
    let mut bytes = Frame::new(FrameKind::Ack, 1, vec![1]).encode().unwrap();
    bytes[4] = 200;
    assert!(matches!(Frame::read_from(&mut bytes.as_slice()), Err(FrameError::UnknownKind(200))));
    let short = 3u32.to_le_bytes();
    assert!(matches!(Frame::read_from(&mut short.as_slice()), Err(FrameError::Malformed(_))));
    let whole = Frame::new(FrameKind::Ack, 1, vec![1, 2, 3]).encode().unwrap();
    assert!(matches!(Frame::read_from(&mut &whole[..whole.len() - 1]), Err(FrameError::Truncated)));
}
