use std::net::IpAddr;

/// Normalize an IP address so dual-stack views don't cache two rows
/// for the same logical host. Maps IPv4-mapped IPv6 (`::ffff:a.b.c.d`)
/// to the bare IPv4; every other address passes through unchanged.
pub fn canonicalize(ip: IpAddr) -> IpAddr {
    match ip {
        // Use to_ipv4_mapped() (not to_canonical()): we normalise only the
        // ::ffff:a.b.c.d form produced by dual-stack sockets; the deprecated
        // v4-compatible form (::a.b.c.d) is left as IPv6.
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        IpAddr::V4(v4) => IpAddr::V4(v4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn maps_ipv4_mapped_v6_to_v4() {
        let mapped: IpAddr = "::ffff:203.0.113.10".parse().unwrap();
        let canonical = canonicalize(mapped);
        assert_eq!(canonical, IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)));
    }

    #[test]
    fn plain_ipv4_passes_through() {
        let ip: IpAddr = "203.0.113.10".parse().unwrap();
        assert_eq!(canonicalize(ip), ip);
    }

    #[test]
    fn plain_ipv6_passes_through() {
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(canonicalize(ip), ip);
    }

    #[test]
    fn v6_loopback_not_mapped() {
        let ip: IpAddr = IpAddr::V6(Ipv6Addr::LOCALHOST);
        assert_eq!(canonicalize(ip), ip);
    }

    #[test]
    fn maps_ipv4_mapped_loopback_to_v4() {
        let mapped: IpAddr = "::ffff:127.0.0.1".parse().unwrap();
        assert_eq!(canonicalize(mapped), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
}
