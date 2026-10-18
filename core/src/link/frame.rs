//! Framing of one application message. A Noise message is at most 65535 bytes, so a longer
//! JSON message is split: the WebSocket binary message carries a sequence of
//! `[u16 big-endian length][Noise ciphertext]`, each ciphertext at most 65535 bytes, so each
//! plaintext chunk is at most 65519. The receiver reassembles and stops at its own cap.

use std::fmt;

use super::noise::{MAX_NOISE_MESSAGE, NoiseError, TAG_BYTES, Transport};

/// The most plaintext one Noise message carries.
pub const MAX_CHUNK: usize = MAX_NOISE_MESSAGE - TAG_BYTES;
const LEN_BYTES: usize = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Nothing to read, a length that runs past the end, or a chunk shorter than a tag.
    Malformed,
    /// The reassembled message is longer than the cap.
    TooLarge,
    /// A chunk did not decrypt.
    Decrypt,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            FrameError::Malformed => "malformed frame",
            FrameError::TooLarge => "message too large",
            FrameError::Decrypt => "a frame did not decrypt",
        })
    }
}

impl std::error::Error for FrameError {}

impl From<NoiseError> for FrameError {
    fn from(_: NoiseError) -> Self {
        FrameError::Decrypt
    }
}

/// The most bytes a WebSocket message can hold for a plaintext of `max_plain`: the bound to
/// give the WebSocket layer so memory is capped before anything is decrypted.
pub fn max_wire_bytes(max_plain: usize) -> usize {
    let chunks = max_plain.div_ceil(MAX_CHUNK).max(1);
    chunks * (LEN_BYTES + MAX_NOISE_MESSAGE)
}

/// Split `plaintext` into chunks and encrypt each. An empty message is one empty chunk.
pub fn seal(transport: &mut Transport, plaintext: &[u8]) -> Result<Vec<u8>, FrameError> {
    let mut out = Vec::with_capacity(plaintext.len() + LEN_BYTES + TAG_BYTES);
    let mut chunks = plaintext.chunks(MAX_CHUNK).peekable();
    if chunks.peek().is_none() {
        let ct = transport.encrypt(&[])?;
        out.extend_from_slice(&(ct.len() as u16).to_be_bytes());
        out.extend_from_slice(&ct);
        return Ok(out);
    }
    for chunk in chunks {
        let ct = transport.encrypt(chunk)?;
        // At most 65535, so it fits the two byte length.
        out.extend_from_slice(&(ct.len() as u16).to_be_bytes());
        out.extend_from_slice(&ct);
    }
    Ok(out)
}

/// Decrypt and join the chunks of one WebSocket message. Refuses as soon as the plaintext
/// would pass `max_plain`, before decrypting the chunk that would overflow it.
pub fn open(transport: &mut Transport, wire: &[u8], max_plain: usize) -> Result<Vec<u8>, FrameError> {
    if wire.is_empty() {
        return Err(FrameError::Malformed);
    }
    let mut out = Vec::new();
    let mut rest = wire;
    while !rest.is_empty() {
        if rest.len() < LEN_BYTES {
            return Err(FrameError::Malformed);
        }
        let len = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
        rest = &rest[LEN_BYTES..];
        if len < TAG_BYTES || len > rest.len() {
            return Err(FrameError::Malformed);
        }
        if out.len() + (len - TAG_BYTES) > max_plain {
            return Err(FrameError::TooLarge);
        }
        let (ct, tail) = rest.split_at(len);
        out.extend_from_slice(&transport.decrypt(ct)?);
        rest = tail;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::noise::test_transports;
    use super::*;

    fn bytes(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 251) as u8).collect()
    }

    fn chunks_in(wire: &[u8]) -> usize {
        let (mut n, mut rest) = (0, wire);
        while !rest.is_empty() {
            let len = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
            rest = &rest[2 + len..];
            n += 1;
        }
        n
    }

    #[test]
    fn frame_split_at_the_65519_65520_boundary() {
        assert_eq!(MAX_CHUNK, 65519);
        let (mut a, mut b) = test_transports();
        for (len, chunks) in [(0, 1), (1, 1), (65519, 1), (65520, 2), (2 * 65519, 2), (2 * 65519 + 1, 3)] {
            let msg = bytes(len);
            let wire = seal(&mut a, &msg).unwrap();
            assert_eq!(chunks_in(&wire), chunks, "len {len}");
            assert_eq!(open(&mut b, &wire, usize::MAX).unwrap(), msg, "len {len}");
        }
    }

    #[test]
    fn frame_four_mib_round_trip_both_ways() {
        let (mut a, mut b) = test_transports();
        let msg = bytes(4 * 1024 * 1024);
        let wire = seal(&mut a, &msg).unwrap();
        assert!(wire.len() <= max_wire_bytes(msg.len()));
        assert_eq!(open(&mut b, &wire, msg.len()).unwrap(), msg);
        let wire = seal(&mut b, &msg).unwrap();
        assert_eq!(open(&mut a, &wire, msg.len()).unwrap(), msg);
    }

    #[test]
    fn frame_over_cap_reassembly_is_refused() {
        let (mut a, mut b) = test_transports();
        let wire = seal(&mut a, &bytes(200_000)).unwrap();
        assert_eq!(open(&mut b, &wire, 199_999), Err(FrameError::TooLarge));
        // Exactly at the cap is fine, on a fresh pair.
        let (mut a, mut b) = test_transports();
        let wire = seal(&mut a, &bytes(200_000)).unwrap();
        assert_eq!(open(&mut b, &wire, 200_000).unwrap().len(), 200_000);
    }

    #[test]
    fn frame_malformed_input_is_refused() {
        let (mut a, mut b) = test_transports();
        assert_eq!(open(&mut b, &[], 100), Err(FrameError::Malformed));
        assert_eq!(open(&mut b, &[0], 100), Err(FrameError::Malformed));
        assert_eq!(open(&mut b, &[0, 5, 1, 2, 3, 4, 5], 100), Err(FrameError::Malformed));
        assert_eq!(open(&mut b, &[0, 40, 1, 2], 100), Err(FrameError::Malformed));
        let mut wire = seal(&mut a, b"hi").unwrap();
        wire.push(0);
        assert_eq!(open(&mut b, &wire, 100), Err(FrameError::Malformed));
        let (mut a, mut b) = test_transports();
        let mut wire = seal(&mut a, b"hi").unwrap();
        let last = wire.len() - 1;
        wire[last] ^= 1;
        assert_eq!(open(&mut b, &wire, 100), Err(FrameError::Decrypt));
    }

    #[test]
    fn frame_wire_bound_covers_the_largest_message() {
        assert_eq!(max_wire_bytes(0), 65537);
        assert_eq!(max_wire_bytes(MAX_CHUNK), 65537);
        assert_eq!(max_wire_bytes(MAX_CHUNK + 1), 2 * 65537);
    }
}
