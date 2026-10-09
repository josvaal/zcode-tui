//! Layer-2 framing: the 13-byte message header used over sockets/WebSocket.
//!
//! ```text
//! | type(1) | id(4, BE) | ack(4, BE) | length(4, BE) | payload(length) |
//! ```
//!
//! The channel layer always sends `Regular` frames with `id = 0`, `ack = 0`;
//! ACK/replay bookkeeping (PersistentProtocol) is not needed for a fresh client
//! connection.

pub const HEADER_SIZE: usize = 13;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProtocolMessageType {
    None = 0,
    Regular = 1,
    Control = 2,
    Ack = 3,
    Disconnect = 5,
    ReplayRequest = 6,
    Pause = 7,
    Resume = 8,
    KeepAlive = 9,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub msg_type: u8,
    pub id: u32,
    pub ack: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn regular(payload: Vec<u8>) -> Frame {
        Frame {
            msg_type: ProtocolMessageType::Regular as u8,
            id: 0,
            ack: 0,
            payload,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_SIZE + self.payload.len());
        out.push(self.msg_type);
        out.extend_from_slice(&self.id.to_be_bytes());
        out.extend_from_slice(&self.ack.to_be_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Incremental frame parser: feed it arbitrary byte chunks, pull out complete frames.
#[derive(Default)]
pub struct FrameParser {
    buf: Vec<u8>,
}

impl FrameParser {
    pub fn new() -> FrameParser {
        FrameParser { buf: Vec::new() }
    }

    pub fn feed(&mut self, chunk: &[u8]) {
        self.buf.extend_from_slice(chunk);
    }

    /// Returns the next complete frame, if one has fully arrived.
    pub fn next_frame(&mut self) -> Option<Frame> {
        if self.buf.len() < HEADER_SIZE {
            return None;
        }
        let length = u32::from_be_bytes([
            self.buf[9],
            self.buf[10],
            self.buf[11],
            self.buf[12],
        ]) as usize;
        let total = HEADER_SIZE + length;
        if self.buf.len() < total {
            return None;
        }
        let frame_bytes: Vec<u8> = self.buf.drain(..total).collect();
        Some(Frame {
            msg_type: frame_bytes[0],
            id: u32::from_be_bytes([frame_bytes[1], frame_bytes[2], frame_bytes[3], frame_bytes[4]]),
            ack: u32::from_be_bytes([frame_bytes[5], frame_bytes[6], frame_bytes[7], frame_bytes[8]]),
            payload: frame_bytes[HEADER_SIZE..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_with_fragmentation() {
        let frame = Frame::regular(vec![1, 2, 3, 4, 5]);
        let bytes = frame.encode();
        let mut parser = FrameParser::new();
        // feed in tiny fragments to exercise reassembly
        let chunks: Vec<&[u8]> = bytes.chunks(3).collect();
        let mut got = None;
        for (i, chunk) in chunks.iter().enumerate() {
            parser.feed(chunk);
            got = parser.next_frame();
            let done = i == chunks.len() - 1;
            assert_eq!(got.is_some(), done);
        }
        let got = got.unwrap();
        assert_eq!(got, frame);
        assert!(parser.next_frame().is_none());
    }
}
