use std::fmt;

pub const FRAME_VERSION: u8 = 1;

const TAG_TEXT: u8 = 0x01;
const TAG_PING: u8 = 0x02;
const TAG_PONG: u8 = 0x03;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Text(String),
    Ping,
    Pong,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    Empty,
    UnsupportedVersion(u8),
    UnknownType(u8),
    BadUtf8,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Empty => write!(f, "frame is empty"),
            FrameError::UnsupportedVersion(v) => write!(f, "unsupported frame version {v}"),
            FrameError::UnknownType(t) => write!(f, "unknown frame type 0x{t:02x}"),
            FrameError::BadUtf8 => write!(f, "text frame payload is not valid UTF-8"),
        }
    }
}

impl std::error::Error for FrameError {}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Frame::Text(text) => {
                let bytes = text.as_bytes();
                let mut out = Vec::with_capacity(2 + bytes.len());
                out.push(FRAME_VERSION);
                out.push(TAG_TEXT);
                out.extend_from_slice(bytes);
                out
            }
            Frame::Ping => vec![FRAME_VERSION, TAG_PING],
            Frame::Pong => vec![FRAME_VERSION, TAG_PONG],
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < 2 {
            return Err(FrameError::Empty);
        }
        let version = bytes[0];
        if version != FRAME_VERSION {
            return Err(FrameError::UnsupportedVersion(version));
        }
        let tag = bytes[1];
        let payload = &bytes[2..];
        match tag {
            TAG_TEXT => {
                let text = std::str::from_utf8(payload).map_err(|_| FrameError::BadUtf8)?;
                Ok(Frame::Text(text.to_string()))
            }
            TAG_PING => Ok(Frame::Ping),
            TAG_PONG => Ok(Frame::Pong),
            other => Err(FrameError::UnknownType(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let original = Frame::Text("hello, world".to_string());
        let bytes = original.encode();
        let decoded = Frame::decode(&bytes).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn empty_text_round_trip() {
        let original = Frame::Text(String::new());
        let decoded = Frame::decode(&original.encode()).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn ping_and_pong_round_trip() {
        for f in [Frame::Ping, Frame::Pong] {
            let decoded = Frame::decode(&f.encode()).unwrap();
            assert_eq!(decoded, f);
        }
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(Frame::decode(&[]), Err(FrameError::Empty));
        assert_eq!(Frame::decode(&[1]), Err(FrameError::Empty));
    }

    #[test]
    fn rejects_unsupported_version() {
        assert_eq!(
            Frame::decode(&[2, TAG_TEXT, b'h', b'i']),
            Err(FrameError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn rejects_unknown_type() {
        assert_eq!(
            Frame::decode(&[FRAME_VERSION, 0xff]),
            Err(FrameError::UnknownType(0xff))
        );
    }

    #[test]
    fn rejects_bad_utf8() {
        assert_eq!(
            Frame::decode(&[FRAME_VERSION, TAG_TEXT, 0xff, 0xfe]),
            Err(FrameError::BadUtf8)
        );
    }
}
