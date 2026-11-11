use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
const EVENTS_PATH: &str = "/monitorhub.plugin.v1.Plugin/Events";
const STREAM: u32 = 1;
const READ_LIMIT: Duration = Duration::from_secs(10);

const DATA: u8 = 0x0;
const HEADERS: u8 = 0x1;
const RST_STREAM: u8 = 0x3;
const SETTINGS: u8 = 0x4;
const PING: u8 = 0x6;
const GOAWAY: u8 = 0x7;

const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;

const METHOD_POST: u8 = 0x83;
const SCHEME_HTTP: u8 = 0x86;
const PATH_NAME: u8 = 0x04;
const AUTHORITY_NAME: u8 = 0x01;
const NEW_NAME: u8 = 0x00;

#[derive(Debug, PartialEq, Eq)]
pub struct EventSummary {
    pub seq: i64,
    pub id: String,
}

struct Frame {
    kind: u8,
    flags: u8,
    stream: u32,
    payload: Vec<u8>,
}

pub fn events(socket: &Path, after: i64, limit: i32) -> Vec<EventSummary> {
    let mut stream = UnixStream::connect(socket).expect("connect to the plugin socket");
    stream
        .set_read_timeout(Some(READ_LIMIT))
        .expect("set a read limit");
    stream.write_all(PREFACE).expect("write the preface");
    write_frame(&mut stream, SETTINGS, 0, 0, &[]);
    write_frame(
        &mut stream,
        HEADERS,
        END_HEADERS,
        STREAM,
        &request_headers(),
    );
    let request = events_request(after, limit);
    write_frame(
        &mut stream,
        DATA,
        END_STREAM,
        STREAM,
        &grpc_message(&request),
    );
    let body = response_body(&mut stream);
    let message = body
        .get(5..)
        .unwrap_or_else(|| panic!("the call returned no message: {body:?}"));
    events_response(message)
}

fn request_headers() -> Vec<u8> {
    let mut block = vec![METHOD_POST, SCHEME_HTTP, PATH_NAME];
    push_string(&mut block, EVENTS_PATH);
    block.push(AUTHORITY_NAME);
    push_string(&mut block, "localhost");
    for (name, value) in [("content-type", "application/grpc"), ("te", "trailers")] {
        block.push(NEW_NAME);
        push_string(&mut block, name);
        push_string(&mut block, value);
    }
    block
}

fn push_string(block: &mut Vec<u8>, text: &str) {
    block.push(u8::try_from(text.len()).expect("a short header"));
    block.extend_from_slice(text.as_bytes());
}

fn grpc_message(message: &[u8]) -> Vec<u8> {
    let length = u32::try_from(message.len()).expect("a short message");
    let mut framed = vec![0];
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(message);
    framed
}

fn events_request(after: i64, limit: i32) -> Vec<u8> {
    let mut message = vec![0x08];
    push_varint(&mut message, after as u64);
    message.push(0x10);
    push_varint(&mut message, i64::from(limit) as u64);
    message
}

fn push_varint(message: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        message.push((value as u8) | 0x80);
        value >>= 7;
    }
    message.push(value as u8);
}

fn write_frame(stream: &mut UnixStream, kind: u8, flags: u8, id: u32, payload: &[u8]) {
    let length = u32::try_from(payload.len()).expect("a short frame");
    let mut frame = length.to_be_bytes()[1..].to_vec();
    frame.extend_from_slice(&[kind, flags]);
    frame.extend_from_slice(&id.to_be_bytes());
    frame.extend_from_slice(payload);
    stream.write_all(&frame).expect("write a frame");
}

fn read_frame(stream: &mut UnixStream) -> Frame {
    let mut header = [0; 9];
    stream.read_exact(&mut header).expect("read a frame header");
    let length = u32::from_be_bytes([0, header[0], header[1], header[2]]);
    let mut payload = vec![0; length as usize];
    stream
        .read_exact(&mut payload)
        .expect("read a frame payload");
    Frame {
        kind: header[3],
        flags: header[4],
        stream: u32::from_be_bytes([header[5], header[6], header[7], header[8]]) & 0x7fff_ffff,
        payload,
    }
}

fn response_body(stream: &mut UnixStream) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let frame = read_frame(stream);
        if frame.kind == SETTINGS && frame.flags & ACK == 0 {
            write_frame(stream, SETTINGS, ACK, 0, &[]);
        }
        if frame.kind == PING && frame.flags & ACK == 0 {
            write_frame(stream, PING, ACK, 0, &frame.payload);
        }
        assert!(
            frame.kind != RST_STREAM && frame.kind != GOAWAY,
            "the plugin ended the call: frame type {}",
            frame.kind
        );
        if frame.stream != STREAM {
            continue;
        }
        if frame.kind == DATA {
            body.extend_from_slice(&frame.payload);
        }
        if frame.flags & END_STREAM != 0 {
            return body;
        }
    }
}

fn events_response(mut message: &[u8]) -> Vec<EventSummary> {
    let mut events = Vec::new();
    while !message.is_empty() {
        let (field, value) = next_field(&mut message);
        if let (1, Value::Bytes(event)) = (field, value) {
            events.push(event_summary(event));
        }
    }
    events
}

fn event_summary(mut message: &[u8]) -> EventSummary {
    let mut summary = EventSummary {
        seq: 0,
        id: String::new(),
    };
    while !message.is_empty() {
        match next_field(&mut message) {
            (1, Value::Varint(seq)) => summary.seq = seq as i64,
            (2, Value::Bytes(id)) => {
                summary.id = String::from_utf8(id.to_vec()).expect("a UTF-8 id")
            }
            _ => {}
        }
    }
    summary
}

enum Value<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
    Fixed,
}

fn next_field<'a>(message: &mut &'a [u8]) -> (u64, Value<'a>) {
    let key = varint(message);
    let value = match key & 0x7 {
        0 => Value::Varint(varint(message)),
        1 => skip(message, 8),
        2 => {
            let length = varint(message) as usize;
            let (bytes, rest) = message.split_at(length);
            *message = rest;
            Value::Bytes(bytes)
        }
        5 => skip(message, 4),
        wire => panic!("unexpected wire type {wire}"),
    };
    (key >> 3, value)
}

fn skip<'a>(message: &mut &'a [u8], width: usize) -> Value<'a> {
    *message = &message[width..];
    Value::Fixed
}

fn varint(message: &mut &[u8]) -> u64 {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let (&byte, rest) = message.split_first().expect("a complete varint");
        *message = rest;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
    }
    panic!("a varint longer than ten bytes")
}
