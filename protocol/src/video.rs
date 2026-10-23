//! The camera's H.264 video as the page decodes it with WebCodecs.
//!
//! The server passes the camera's frames through untouched, in the `avc`
//! layout WebCodecs takes: a [`VideoConfig`] whose `description` is the
//! `avcC` record of the stream's parameter sets, then one binary WebSocket
//! message per frame, framed by [`encode_chunk`]: a flags byte, the
//! timestamp in microseconds as a little-endian `u64`, then the frame's NAL
//! units, each behind its length as a big-endian `u32`.

use serde::{Deserialize, Serialize};

/// How to decode the chunks that follow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoConfig {
    /// The WebCodecs codec string, such as `avc1.641029`.
    pub codec: String,
    /// The `avcC` record: the stream's sequence and picture parameter sets.
    pub description: Vec<u8>,
    /// The width of the coded picture, in pixels.
    pub width: u32,
    /// The height of the coded picture, in pixels.
    pub height: u32,
}

/// One frame of video, as a page receives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk<'a> {
    /// Whether it decodes on its own (an IDR frame).
    pub key: bool,
    /// When it shows, in microseconds since the video started.
    pub timestamp_us: u64,
    /// Its NAL units, each behind its big-endian `u32` length.
    pub data: &'a [u8],
}

const KEY: u8 = 1;
const HEADER: usize = 9;

/// The binary message that carries a frame.
#[must_use]
pub fn encode_chunk(key: bool, timestamp_us: u64, data: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(HEADER + data.len());
    message.push(if key { KEY } else { 0 });
    message.extend_from_slice(&timestamp_us.to_le_bytes());
    message.extend_from_slice(data);
    message
}

impl<'a> Chunk<'a> {
    /// The frame a binary message carries, if it is one.
    #[must_use]
    pub fn decode(message: &'a [u8]) -> Option<Self> {
        let (header, data) = message.split_at_checked(HEADER)?;
        let timestamp_us = u64::from_le_bytes(header[1..].try_into().ok()?);
        Some(Self { key: header[0] & KEY != 0, timestamp_us, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_reads_back() {
        let message = encode_chunk(true, 33_366, &[0, 0, 0, 1, 0x65]);
        let chunk = Chunk::decode(&message).unwrap();
        assert!(chunk.key);
        assert_eq!(chunk.timestamp_us, 33_366);
        assert_eq!(chunk.data, &[0, 0, 0, 1, 0x65]);
        assert_eq!(Chunk::decode(&message[..5]), None);
        assert!(!Chunk::decode(&encode_chunk(false, 0, &[])).unwrap().key);
    }
}
