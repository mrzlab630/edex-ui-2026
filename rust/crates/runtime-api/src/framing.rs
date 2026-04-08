use serde::de::DeserializeOwned;
use serde::Serialize;
use thiserror::Error;

pub const LOCAL_TRANSPORT: &str = "uds";
pub const FRAME_CONTENT_TYPE: &str = "application/jsonl";
pub const FRAME_DELIMITER: u8 = b'\n';
pub const FRAME_DELIMITER_STR: &str = "\n";
pub const MAX_FRAME_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameEncoding {
    JsonLines,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAssumptions {
    pub transport: &'static str,
    pub content_type: &'static str,
    pub encoding: FrameEncoding,
    pub delimiter: u8,
    pub max_frame_bytes: usize,
}

impl FrameAssumptions {
    pub const fn local_json_over_uds() -> Self {
        Self {
            transport: LOCAL_TRANSPORT,
            content_type: FRAME_CONTENT_TYPE,
            encoding: FrameEncoding::JsonLines,
            delimiter: FRAME_DELIMITER,
            max_frame_bytes: MAX_FRAME_BYTES,
        }
    }
}

impl Default for FrameAssumptions {
    fn default() -> Self {
        Self::local_json_over_uds()
    }
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("json frame exceeds max size: {frame_bytes} > {max_frame_bytes}")]
    FrameTooLarge {
        frame_bytes: usize,
        max_frame_bytes: usize,
    },
    #[error("failed to encode json frame")]
    Encode(#[source] serde_json::Error),
    #[error("failed to decode json frame")]
    Decode(#[source] serde_json::Error),
}

pub fn encode_json_frame<T>(value: &T) -> Result<Vec<u8>, FrameError>
where
    T: Serialize,
{
    let mut frame = serde_json::to_vec(value).map_err(FrameError::Encode)?;

    if frame.len() > MAX_FRAME_BYTES {
        return Err(FrameError::FrameTooLarge {
            frame_bytes: frame.len(),
            max_frame_bytes: MAX_FRAME_BYTES,
        });
    }

    frame.push(FRAME_DELIMITER);
    Ok(frame)
}

pub fn decode_json_frame<T>(line: &str) -> Result<Option<T>, FrameError>
where
    T: DeserializeOwned,
{
    let trimmed = line.trim_end_matches(['\r', '\n']);

    if trimmed.trim().is_empty() {
        return Ok(None);
    }

    if trimmed.len() > MAX_FRAME_BYTES {
        return Err(FrameError::FrameTooLarge {
            frame_bytes: trimmed.len(),
            max_frame_bytes: MAX_FRAME_BYTES,
        });
    }

    serde_json::from_str(trimmed)
        .map(Some)
        .map_err(FrameError::Decode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, RequestEnvelope};

    #[test]
    fn encode_appends_newline_delimiter() {
        let request = RequestEnvelope::command("req-1", Command::Ping);
        let encoded = encode_json_frame(&request).expect("frame encodes");

        assert_eq!(encoded.last(), Some(&FRAME_DELIMITER));
    }

    #[test]
    fn decode_ignores_blank_lines() {
        let decoded: Option<RequestEnvelope> =
            decode_json_frame(FRAME_DELIMITER_STR).expect("blank line is ignored");

        assert!(decoded.is_none());
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let oversized = "x".repeat(MAX_FRAME_BYTES + 1);
        let error = decode_json_frame::<RequestEnvelope>(&oversized).expect_err("frame rejected");

        assert!(matches!(error, FrameError::FrameTooLarge { .. }));
    }
}
