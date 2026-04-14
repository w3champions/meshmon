//! IP address helpers.
//!
//! Bridges between the raw `bytes` representation on the wire (4 bytes IPv4,
//! 16 bytes IPv6) and Rust's `std::net::IpAddr`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use prost::bytes::Bytes;

/// Error returned by [`to_ipaddr`] when the input length is not 4 or 16.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidIpLength(pub usize);

impl std::fmt::Display for InvalidIpLength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "invalid IP byte length {}; expected 4 (IPv4) or 16 (IPv6)",
            self.0
        )
    }
}

impl std::error::Error for InvalidIpLength {}

/// Encode an [`IpAddr`] as the wire representation (4 bytes for IPv4, 16 for IPv6).
pub fn from_ipaddr(addr: IpAddr) -> Bytes {
    match addr {
        IpAddr::V4(v4) => Bytes::copy_from_slice(&v4.octets()),
        IpAddr::V6(v6) => Bytes::copy_from_slice(&v6.octets()),
    }
}

/// Decode the wire representation into an [`IpAddr`].
///
/// Accepts exactly 4 bytes (IPv4) or 16 bytes (IPv6). Returns
/// [`InvalidIpLength`] otherwise.
pub fn to_ipaddr(bytes: &[u8]) -> Result<IpAddr, InvalidIpLength> {
    match bytes.len() {
        4 => {
            let octets: [u8; 4] = bytes.try_into().expect("len==4 above");
            Ok(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        16 => {
            let octets: [u8; 16] = bytes.try_into().expect("len==16 above");
            Ok(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        n => Err(InvalidIpLength(n)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv4_roundtrip() {
        let addr: IpAddr = "170.80.110.90".parse().unwrap();
        let wire = from_ipaddr(addr);
        assert_eq!(wire.len(), 4);
        assert_eq!(to_ipaddr(&wire).unwrap(), addr);
    }

    #[test]
    fn ipv6_roundtrip() {
        let addr: IpAddr = "2001:db8::1".parse().unwrap();
        let wire = from_ipaddr(addr);
        assert_eq!(wire.len(), 16);
        assert_eq!(to_ipaddr(&wire).unwrap(), addr);
    }

    #[test]
    fn rejects_wrong_length() {
        assert_eq!(to_ipaddr(&[]).unwrap_err(), InvalidIpLength(0));
        assert_eq!(to_ipaddr(&[1, 2, 3]).unwrap_err(), InvalidIpLength(3));
        assert_eq!(to_ipaddr(&[0; 8]).unwrap_err(), InvalidIpLength(8));
        assert_eq!(to_ipaddr(&[0; 17]).unwrap_err(), InvalidIpLength(17));
    }

    #[test]
    fn ipv4_mapped_ipv6_stays_ipv6() {
        // ::ffff:170.80.110.90 — the IPv4-mapped form is still 16 bytes on the
        // wire and should decode as IPv6. The caller decides whether to treat
        // it as the underlying IPv4; we just preserve the encoding.
        let addr: IpAddr = "::ffff:170.80.110.90".parse().unwrap();
        let wire = from_ipaddr(addr);
        assert_eq!(wire.len(), 16);
        let decoded = to_ipaddr(&wire).unwrap();
        assert!(matches!(decoded, IpAddr::V6(_)));
        assert_eq!(decoded, addr);
    }
}
