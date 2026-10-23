//! H.264 out of RTP (RFC 6184): NAL units gathered into frames, and the
//! parameter sets that tell a decoder how to read them.
//!
//! A [`Depacketizer`] takes the camera's RTP packets in order and gives back
//! a [`Stream`] whenever the sequence and picture parameter sets first come
//! or change, and every complete [`Frame`] after it, in the `avc` layout of
//! WebCodecs: each NAL unit behind its length as a big-endian `u32`, the
//! parameter sets left to the `avcC` record of the [`Stream`].

use x2d_protocol::video::VideoConfig;

/// An RTP packet of the stream.
#[derive(Clone, Debug)]
pub struct Packet {
    /// The last packet of a frame.
    pub marker: bool,
    /// Its sequence number.
    pub sequence: u16,
    /// Its timestamp, on the 90 kHz clock.
    pub timestamp: u32,
    /// Its H.264 payload.
    pub payload: Vec<u8>,
}

/// How to decode the frames that follow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    /// The configuration a page decodes with.
    pub config: VideoConfig,
    /// The sequence parameter set, as a NAL unit.
    pub sps: Vec<u8>,
    /// The picture parameter set, as a NAL unit.
    pub pps: Vec<u8>,
}

/// One frame: the NAL units of one timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Whether it decodes on its own: it holds an IDR slice.
    pub key: bool,
    /// Its timestamp, on the 90 kHz clock.
    pub timestamp: u32,
    /// Its NAL units, each behind its big-endian `u32` length.
    pub data: Vec<u8>,
}

/// What a packet completes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Output {
    /// The parameter sets came, or changed.
    Stream(Stream),
    /// A frame is complete.
    Frame(Frame),
}

/// Gathers packets into frames.
#[derive(Default)]
pub struct Depacketizer {
    /// The NAL units of the frame being gathered, and its timestamp.
    units: Vec<Vec<u8>>,
    timestamp: Option<u32>,
    /// The NAL unit being gathered from fragments.
    fragment: Option<Vec<u8>>,
    /// Whether a packet of this frame was lost.
    damaged: bool,
    /// Whether frames wait for a key frame, after a loss or at the start.
    awaiting_key: bool,
    last_sequence: Option<u16>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    stream: Option<Stream>,
}

const SPS: u8 = 7;
const PPS: u8 = 8;
const IDR: u8 = 5;
const AUD: u8 = 9;
const STAP_A: u8 = 24;
const FU_A: u8 = 28;

impl Depacketizer {
    /// A depacketizer that starts with the parameter sets the session
    /// description announced, if any.
    #[must_use]
    pub fn new(parameter_sets: &[Vec<u8>]) -> Self {
        let mut depacketizer = Self { awaiting_key: true, ..Self::default() };
        for unit in parameter_sets {
            depacketizer.keep_parameter_set(unit);
        }
        depacketizer
    }

    /// Take `packet`, and return what it completes.
    pub fn push(&mut self, packet: Packet) -> Vec<Output> {
        let mut out = Vec::new();
        if let Some(last) = self.last_sequence
            && packet.sequence != last.wrapping_add(1)
        {
            self.damaged = true;
            self.fragment = None;
        }
        self.last_sequence = Some(packet.sequence);
        if self.timestamp.is_some_and(|timestamp| timestamp != packet.timestamp) {
            self.finish(&mut out);
        }
        self.timestamp = Some(packet.timestamp);
        self.take_payload(&packet.payload);
        if packet.marker {
            self.finish(&mut out);
        }
        out
    }

    fn take_payload(&mut self, payload: &[u8]) {
        let Some(&first) = payload.first() else { return };
        match first & 0x1F {
            1..=23 => self.units.push(payload.to_vec()),
            STAP_A => {
                let mut rest = &payload[1..];
                while let [high, low, tail @ ..] = rest {
                    let size = usize::from(u16::from_be_bytes([*high, *low]));
                    let Some((unit, tail)) = tail.split_at_checked(size) else {
                        self.damaged = true;
                        return;
                    };
                    if !unit.is_empty() {
                        self.units.push(unit.to_vec());
                    }
                    rest = tail;
                }
            }
            FU_A => {
                let Some(&header) = payload.get(1) else { return };
                let (start, end) = (header & 0x80 != 0, header & 0x40 != 0);
                if start {
                    let mut unit = vec![(first & 0xE0) | (header & 0x1F)];
                    unit.extend_from_slice(&payload[2..]);
                    self.fragment = Some(unit);
                } else if let Some(unit) = &mut self.fragment {
                    unit.extend_from_slice(&payload[2..]);
                } else {
                    self.damaged = true;
                }
                if end && let Some(unit) = self.fragment.take() {
                    self.units.push(unit);
                }
            }
            _ => self.damaged = true,
        }
    }

    /// End the frame being gathered: keep its parameter sets, and give it
    /// out unless it was damaged or cannot decode yet.
    fn finish(&mut self, out: &mut Vec<Output>) {
        let units = std::mem::take(&mut self.units);
        let Some(timestamp) = self.timestamp.take() else { return };
        if std::mem::take(&mut self.damaged) || self.fragment.take().is_some() {
            self.awaiting_key = true;
            return;
        }
        let mut key = false;
        let mut data = Vec::new();
        for unit in units {
            match unit[0] & 0x1F {
                SPS | PPS => self.keep_parameter_set(&unit),
                AUD => {}
                kind => {
                    key |= kind == IDR;
                    data.extend_from_slice(&u32::try_from(unit.len()).unwrap_or(u32::MAX).to_be_bytes());
                    data.extend_from_slice(&unit);
                }
            }
        }
        if let (Some(sps), Some(pps)) = (&self.sps, &self.pps) {
            let changed = self.stream.as_ref().is_none_or(|stream| stream.sps != *sps || stream.pps != *pps);
            if changed && let Some(stream) = stream(sps, pps) {
                out.push(Output::Stream(stream.clone()));
                self.stream = Some(stream);
                self.awaiting_key = true;
            }
        }
        if data.is_empty() || self.stream.is_none() || (self.awaiting_key && !key) {
            return;
        }
        self.awaiting_key = false;
        out.push(Output::Frame(Frame { key, timestamp, data }));
    }

    fn keep_parameter_set(&mut self, unit: &[u8]) {
        match unit.first().map(|header| header & 0x1F) {
            Some(SPS) => self.sps = Some(unit.to_vec()),
            Some(PPS) => self.pps = Some(unit.to_vec()),
            _ => {}
        }
    }
}

/// The stream that `sps` and `pps` describe, if `sps` reads.
fn stream(sps: &[u8], pps: &[u8]) -> Option<Stream> {
    let parsed = Sps::parse(sps)?;
    let mut description = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    description.extend_from_slice(&u16::try_from(sps.len()).ok()?.to_be_bytes());
    description.extend_from_slice(sps);
    description.push(1);
    description.extend_from_slice(&u16::try_from(pps.len()).ok()?.to_be_bytes());
    description.extend_from_slice(pps);
    if parsed.high {
        description.extend_from_slice(&[
            0xFC | parsed.chroma_format,
            0xF8 | parsed.luma_depth_minus_8,
            0xF8 | parsed.chroma_depth_minus_8,
            0,
        ]);
    }
    Some(Stream {
        config: VideoConfig {
            codec: format!("avc1.{:02X}{:02X}{:02X}", sps[1], sps[2], sps[3]),
            description,
            width: parsed.width,
            height: parsed.height,
        },
        sps: sps.to_vec(),
        pps: pps.to_vec(),
    })
}

/// What the `avcC` record and the page need of a sequence parameter set.
#[derive(Debug, PartialEq, Eq)]
struct Sps {
    /// Whether its profile carries the chroma and bit depth fields.
    high: bool,
    chroma_format: u8,
    luma_depth_minus_8: u8,
    chroma_depth_minus_8: u8,
    /// The picture's size once cropped, in pixels.
    width: u32,
    height: u32,
}

impl Sps {
    /// Read the fields of H.264 §7.3.2.1.1 up to the cropping window.
    fn parse(unit: &[u8]) -> Option<Self> {
        let rbsp = unescape(unit.get(1..)?);
        let mut bits = Bits { data: &rbsp, at: 0 };
        let profile = bits.take(8)?;
        bits.take(16)?;
        bits.exp_golomb()?;
        let high = matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135);
        let (mut chroma_format, mut luma_depth, mut chroma_depth) = (1, 0, 0);
        if high {
            chroma_format = bits.exp_golomb()?;
            if chroma_format == 3 {
                bits.take(1)?;
            }
            luma_depth = bits.exp_golomb()?;
            chroma_depth = bits.exp_golomb()?;
            bits.take(1)?;
            if bits.take(1)? == 1 {
                for list in 0..if chroma_format == 3 { 12 } else { 8 } {
                    if bits.take(1)? == 1 {
                        bits.skip_scaling_list(if list < 6 { 16 } else { 64 })?;
                    }
                }
            }
        }
        bits.exp_golomb()?;
        match bits.exp_golomb()? {
            0 => {
                bits.exp_golomb()?;
            }
            1 => {
                bits.take(1)?;
                bits.signed_exp_golomb()?;
                bits.signed_exp_golomb()?;
                for _ in 0..bits.exp_golomb()? {
                    bits.signed_exp_golomb()?;
                }
            }
            _ => {}
        }
        bits.exp_golomb()?;
        bits.take(1)?;
        let width_in_macroblocks = bits.exp_golomb()? + 1;
        let height_in_map_units = bits.exp_golomb()? + 1;
        let frames_only = bits.take(1)?;
        if frames_only == 0 {
            bits.take(1)?;
        }
        bits.take(1)?;
        let (mut left, mut right, mut top, mut bottom) = (0, 0, 0, 0);
        if bits.take(1)? == 1 {
            (left, right, top, bottom) =
                (bits.exp_golomb()?, bits.exp_golomb()?, bits.exp_golomb()?, bits.exp_golomb()?);
        }
        let field_factor = 2 - frames_only;
        let (crop_x, crop_y) = match chroma_format {
            1 => (2, 2 * field_factor),
            2 => (2, field_factor),
            _ => (1, field_factor),
        };
        Some(Self {
            high,
            chroma_format: u8::try_from(chroma_format).ok()?,
            luma_depth_minus_8: u8::try_from(luma_depth).ok()?,
            chroma_depth_minus_8: u8::try_from(chroma_depth).ok()?,
            width: (width_in_macroblocks * 16).checked_sub(crop_x * (left + right))?,
            height: (field_factor * height_in_map_units * 16).checked_sub(crop_y * (top + bottom))?,
        })
    }
}

/// The payload of a NAL unit without its emulation prevention bytes: each
/// `00 00 03` read as `00 00`.
fn unescape(escaped: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(escaped.len());
    let mut zeros = 0;
    for &byte in escaped {
        if zeros >= 2 && byte == 3 {
            zeros = 0;
            continue;
        }
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        out.push(byte);
    }
    out
}

/// A bit reader, most significant bit first.
struct Bits<'a> {
    data: &'a [u8],
    at: usize,
}

impl Bits<'_> {
    fn take(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            let byte = self.data.get(self.at / 8)?;
            value = value << 1 | u32::from(byte >> (7 - self.at % 8) & 1);
            self.at += 1;
        }
        Some(value)
    }

    fn exp_golomb(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.take(1)? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1 << zeros) - 1 + self.take(zeros)?)
    }

    fn signed_exp_golomb(&mut self) -> Option<i64> {
        let code = i64::from(self.exp_golomb()?);
        Some(if code % 2 == 1 { (code + 1) / 2 } else { -code / 2 })
    }

    fn skip_scaling_list(&mut self, size: usize) -> Option<()> {
        let (mut last, mut next) = (8_i64, 8_i64);
        for _ in 0..size {
            if next != 0 {
                next = (last + self.signed_exp_golomb()? + 256) % 256;
            }
            last = if next == 0 { last } else { next };
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;

    use super::*;

    /// The parameter sets the X2D's camera announced on 2026-10-23.
    fn x2d_parameter_sets() -> (Vec<u8>, Vec<u8>) {
        let engine = base64::engine::general_purpose::STANDARD;
        (engine.decode("Z2QQKawbGqB4AiflhAAAAwAEAAADAPI8IBCo").unwrap(), engine.decode("aO48sA==").unwrap())
    }

    #[test]
    fn the_x2d_camera_is_1080p_high_profile() {
        let (sps, pps) = x2d_parameter_sets();
        let stream = stream(&sps, &pps).unwrap();
        assert_eq!(stream.config.codec, "avc1.641029");
        assert_eq!((stream.config.width, stream.config.height), (1920, 1080));
        let description = &stream.config.description;
        assert_eq!(&description[..6], &[1, 0x64, 0x10, 0x29, 0xFF, 0xE1]);
        assert_eq!(&description[description.len() - 4..], &[0xFD, 0xF8, 0xF8, 0]);
    }

    fn packet(sequence: u16, timestamp: u32, marker: bool, payload: &[u8]) -> Packet {
        Packet { marker, sequence, timestamp, payload: payload.to_vec() }
    }

    #[test]
    fn fragments_and_aggregates_make_frames() {
        let (sps, pps) = x2d_parameter_sets();
        let mut depacketizer = Depacketizer::new(&[]);
        assert!(depacketizer.push(packet(1, 0, false, &sps)).is_empty());
        assert!(depacketizer.push(packet(2, 0, false, &pps)).is_empty());
        assert!(depacketizer.push(packet(3, 0, false, &[0x7C, 0x85, 0xAA])).is_empty());
        let out = depacketizer.push(packet(4, 0, true, &[0x7C, 0x45, 0xBB]));
        let [Output::Stream(stream), Output::Frame(frame)] = &out[..] else { panic!("{out:?}") };
        assert_eq!(stream.sps, sps);
        assert_eq!(frame, &Frame { key: true, timestamp: 0, data: vec![0, 0, 0, 3, 0x65, 0xAA, 0xBB] });
        let out = depacketizer.push(packet(5, 3000, true, &[0x18, 0, 2, 0x41, 0x01, 0, 1, 0x06]));
        assert_eq!(
            out,
            [Output::Frame(Frame {
                key: false,
                timestamp: 3000,
                data: vec![0, 0, 0, 2, 0x41, 0x01, 0, 0, 0, 1, 0x06]
            })]
        );
    }

    #[test]
    fn a_loss_waits_for_the_next_key_frame() {
        let (sps, pps) = x2d_parameter_sets();
        let mut depacketizer = Depacketizer::new(&[sps, pps]);
        assert_eq!(depacketizer.push(packet(1, 0, true, &[0x41, 1])).len(), 1, "the stream, but no frame before a key");
        assert!(depacketizer.push(packet(2, 3000, true, &[0x65, 1])).iter().any(|out| matches!(out, Output::Frame(_))));
        assert!(depacketizer.push(packet(4, 6000, true, &[0x41, 2])).is_empty(), "packet 3 was lost");
        assert!(depacketizer.push(packet(5, 9000, true, &[0x41, 3])).is_empty(), "no reference to decode from");
        assert_eq!(depacketizer.push(packet(6, 12000, true, &[0x65, 4])).len(), 1);
    }

    #[test]
    fn exp_golomb_reads() {
        let mut bits = Bits { data: &[0b1010_0110, 0b0100_0000], at: 0 };
        assert_eq!(bits.exp_golomb(), Some(0));
        assert_eq!(bits.exp_golomb(), Some(1));
        assert_eq!(bits.exp_golomb(), Some(2));
        assert_eq!(bits.signed_exp_golomb(), Some(2));
        assert_eq!(unescape(&[0, 0, 3, 1, 0, 0, 3]), [0, 0, 1, 0, 0]);
    }
}
