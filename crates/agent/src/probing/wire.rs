//! UDP probe wire format (spec 02 § UDP wire protocol).
//!
//! 12-byte fixed-length packet:
//!
//! ```text
//! offset  length  field
//! 0       8       secret (deployment-wide shared secret)
//! 8       4       nonce  (big-endian u32; 0xFFFFFFFF is the rejection marker)
//! ```
//!
//! Secret comparison in the decoders is constant-time
//! ([`subtle::ConstantTimeEq`]). The secret is short and shared, but there
//! is no reason to leak timing information about which prefix bytes matched
//! — an off-path observer with enough samples could otherwise learn the
//! secret byte by byte.

use subtle::ConstantTimeEq;

/// Wire size of every probe, echo, and rejection packet.
pub const PACKET_LEN: usize = 12;

/// Size of the shared secret prefix.
pub const SECRET_LEN: usize = 8;

/// Reserved nonce value that marks a "not allowlisted" rejection from the
/// echo listener. Probers skip this value when advancing their monotonic
/// nonce counter.
pub const REJECTION_MARKER: u32 = 0xFFFF_FFFF;

/// What the dispatcher saw in an incoming response.
#[derive(Debug, PartialEq, Eq)]
pub enum DecodedResponse {
    /// Echo of a probe we sent — caller correlates via `nonce`.
    Echo { nonce: u32 },
    /// Listener told us its allowlist doesn't yet include us; pause this
    /// target for 60 s.
    Rejection,
}

/// Encode a probe packet: `[secret ++ nonce_be]`.
pub fn encode_probe(secret: &[u8; SECRET_LEN], nonce: u32) -> [u8; PACKET_LEN] {
    debug_assert!(
        nonce != REJECTION_MARKER,
        "callers must skip REJECTION_MARKER when generating nonces"
    );
    let mut out = [0u8; PACKET_LEN];
    out[..SECRET_LEN].copy_from_slice(secret);
    out[SECRET_LEN..].copy_from_slice(&nonce.to_be_bytes());
    out
}

/// Encode a rejection response: `[secret ++ 0xFFFFFFFF]`. Emitted by the
/// UDP echo listener when the peer's source IP is not yet in the allowlist.
pub fn encode_rejection(secret: &[u8; SECRET_LEN]) -> [u8; PACKET_LEN] {
    let mut out = [0u8; PACKET_LEN];
    out[..SECRET_LEN].copy_from_slice(secret);
    out[SECRET_LEN..].copy_from_slice(&REJECTION_MARKER.to_be_bytes());
    out
}

/// Try to decode a received packet as a response to one of our probes.
///
/// Returns `None` if:
/// * length is not exactly 12 bytes;
/// * the secret prefix matches neither `current_secret` nor `previous_secret`.
///
/// Secret comparison uses [`subtle::ConstantTimeEq`]; no short-circuit on
/// the first differing byte.
///
/// Otherwise returns either [`DecodedResponse::Rejection`] (nonce ==
/// `REJECTION_MARKER`) or [`DecodedResponse::Echo`] carrying the received
/// nonce. Callers still correlate the echo against their own pending map —
/// an attacker who knows the secret could replay arbitrary nonces, but
/// would not match any pending entry.
pub fn decode_response(
    bytes: &[u8],
    current_secret: &[u8; SECRET_LEN],
    previous_secret: Option<&[u8; SECRET_LEN]>,
) -> Option<DecodedResponse> {
    if bytes.len() != PACKET_LEN {
        return None;
    }
    let (secret, nonce_bytes) = bytes.split_at(SECRET_LEN);
    if !secret_matches(secret, current_secret, previous_secret) {
        return None;
    }
    let nonce = u32::from_be_bytes(nonce_bytes.try_into().expect("4 bytes"));
    Some(if nonce == REJECTION_MARKER {
        DecodedResponse::Rejection
    } else {
        DecodedResponse::Echo { nonce }
    })
}

/// Try to decode a received packet as a *probe* from a peer (listener
/// side). Validates length + secret; returns the nonce on match. Returns
/// `None` for drops.
///
/// Secret comparison uses [`subtle::ConstantTimeEq`]; no short-circuit on
/// the first differing byte.
pub fn decode_probe(
    bytes: &[u8],
    current_secret: &[u8; SECRET_LEN],
    previous_secret: Option<&[u8; SECRET_LEN]>,
) -> Option<u32> {
    if bytes.len() != PACKET_LEN {
        return None;
    }
    let (secret, nonce_bytes) = bytes.split_at(SECRET_LEN);
    if !secret_matches(secret, current_secret, previous_secret) {
        return None;
    }
    Some(u32::from_be_bytes(nonce_bytes.try_into().expect("4 bytes")))
}

/// Constant-time check that `candidate` matches `current` or, if supplied,
/// `previous`. Uses [`subtle::ConstantTimeEq`] so the comparison cost does
/// not depend on how many leading bytes of the candidate matched. The
/// check may short-circuit on `current` without probing `previous` — but
/// each individual comparison is already constant-time, so the rotation
/// path never leaks byte-level information about either secret.
fn secret_matches(
    candidate: &[u8],
    current: &[u8; SECRET_LEN],
    previous: Option<&[u8; SECRET_LEN]>,
) -> bool {
    if bool::from(candidate.ct_eq(current.as_slice())) {
        return true;
    }
    match previous {
        Some(prev) => bool::from(candidate.ct_eq(prev.as_slice())),
        None => false,
    }
}

/// Advance a nonce counter, skipping the reserved [`REJECTION_MARKER`].
/// Wraps at `0xFFFFFFFE`.
pub fn next_nonce(cur: u32) -> u32 {
    let nxt = cur.wrapping_add(1);
    if nxt == REJECTION_MARKER {
        0
    } else {
        nxt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
    const PREVIOUS: [u8; 8] = [9, 9, 9, 9, 9, 9, 9, 9];

    #[test]
    fn encode_probe_layout() {
        let bytes = encode_probe(&SECRET, 0x11223344);
        assert_eq!(bytes.len(), PACKET_LEN);
        assert_eq!(&bytes[..8], &SECRET);
        assert_eq!(&bytes[8..], &[0x11, 0x22, 0x33, 0x44]);
    }

    #[test]
    fn encode_rejection_layout() {
        let bytes = encode_rejection(&SECRET);
        assert_eq!(&bytes[..8], &SECRET);
        assert_eq!(&bytes[8..], &[0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn decode_response_recognises_echo() {
        let probe = encode_probe(&SECRET, 42);
        assert_eq!(
            decode_response(&probe, &SECRET, None),
            Some(DecodedResponse::Echo { nonce: 42 }),
        );
    }

    #[test]
    fn decode_response_recognises_rejection() {
        let rej = encode_rejection(&SECRET);
        assert_eq!(
            decode_response(&rej, &SECRET, None),
            Some(DecodedResponse::Rejection),
        );
    }

    #[test]
    fn decode_response_tolerates_previous_secret() {
        let probe = encode_probe(&PREVIOUS, 7);
        assert_eq!(decode_response(&probe, &SECRET, None), None);
        assert_eq!(
            decode_response(&probe, &SECRET, Some(&PREVIOUS)),
            Some(DecodedResponse::Echo { nonce: 7 }),
        );
    }

    #[test]
    fn decode_response_drops_wrong_length() {
        assert_eq!(decode_response(&[0u8; 11], &SECRET, None), None);
        assert_eq!(decode_response(&[0u8; 13], &SECRET, None), None);
    }

    #[test]
    fn decode_response_drops_bad_secret() {
        let bad = [0u8; PACKET_LEN];
        assert_eq!(decode_response(&bad, &SECRET, None), None);
    }

    #[test]
    fn next_nonce_skips_rejection_marker() {
        assert_eq!(next_nonce(0xFFFFFFFD), 0xFFFFFFFE);
        assert_eq!(next_nonce(0xFFFFFFFE), 0); // wraps past REJECTION_MARKER
    }
}
