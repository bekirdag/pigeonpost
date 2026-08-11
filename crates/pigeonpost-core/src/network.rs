//! Network-address classification shared by every outbound HTTP boundary.
//!
//! This deliberately answers a narrower question than "is syntactically global": may an
//! untrusted hostname resolution be used as an Internet destination without creating an SSRF
//! path? Unknown and special-purpose space fails closed. Callers separately decide whether an
//! explicitly configured, exact numeric loopback origin is allowed for local development.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A fixed compatibility/security vector for the outbound-address classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetworkAddressVector {
    pub address: &'static str,
    pub public: bool,
}

/// Adversarial and positive vectors shared by all consumers of [`is_public_network_address`].
///
/// Keep this table fixed and review additions against the IANA special-purpose registries. It is
/// public so downstream conformance suites can exercise the exact same boundary without copying
/// address-range logic.
pub const NETWORK_ADDRESS_VECTORS: &[NetworkAddressVector] = &[
    NetworkAddressVector {
        address: "0.0.0.0",
        public: false,
    },
    NetworkAddressVector {
        address: "10.0.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "100.64.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "127.0.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "169.254.169.254",
        public: false,
    },
    NetworkAddressVector {
        address: "172.16.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "192.0.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "192.0.2.1",
        public: false,
    },
    NetworkAddressVector {
        address: "192.88.99.1",
        public: false,
    },
    NetworkAddressVector {
        address: "192.168.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "198.18.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "198.51.100.1",
        public: false,
    },
    NetworkAddressVector {
        address: "203.0.113.1",
        public: false,
    },
    NetworkAddressVector {
        address: "224.0.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "240.0.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "255.255.255.255",
        public: false,
    },
    NetworkAddressVector {
        address: "8.8.8.8",
        public: true,
    },
    NetworkAddressVector {
        address: "93.184.216.34",
        public: true,
    },
    NetworkAddressVector {
        address: "::",
        public: false,
    },
    NetworkAddressVector {
        address: "::1",
        public: false,
    },
    NetworkAddressVector {
        address: "::ffff:127.0.0.1",
        public: false,
    },
    NetworkAddressVector {
        address: "::ffff:8.8.8.8",
        public: true,
    },
    NetworkAddressVector {
        address: "64:ff9b::808:808",
        public: false,
    },
    NetworkAddressVector {
        address: "100::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2001::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2001:2::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2001:db8::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2001:10::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2001:20::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2002:0a00:0001::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2002:0808:0808::1",
        public: true,
    },
    NetworkAddressVector {
        address: "3fff::1",
        public: false,
    },
    NetworkAddressVector {
        address: "4000::1",
        public: false,
    },
    NetworkAddressVector {
        address: "5f00::1",
        public: false,
    },
    NetworkAddressVector {
        address: "fc00::1",
        public: false,
    },
    NetworkAddressVector {
        address: "fe80::1",
        public: false,
    },
    NetworkAddressVector {
        address: "fec0::1",
        public: false,
    },
    NetworkAddressVector {
        address: "2001:4860:4860::8888",
        public: true,
    },
    NetworkAddressVector {
        address: "2606:4700:4700::1111",
        public: true,
    },
];

/// Return whether an address is safe for an untrusted, Internet-only outbound destination.
pub fn is_public_network_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_public_ipv4(address),
        IpAddr::V6(address) => address
            .to_ipv4_mapped()
            .map_or_else(|| is_public_ipv6(address), is_public_ipv4),
    }
}

/// Return whether a URL host is a literal loopback address, never a DNS name.
///
/// `url::Url::host_str` preserves brackets around IPv6 hosts in some supported versions, so both
/// bracketed and unbracketed IPv6 literals are accepted. Names such as `localhost` are deliberately
/// rejected: allowing cleartext based on a name would make resolver or hosts-file changes an SSRF
/// and confidentiality boundary.
pub fn is_numeric_loopback_host(host: &str) -> bool {
    let candidate = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    candidate
        .parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

/// Return whether a URL host is the special-use `localhost` DNS name or one of its subdomains.
///
/// This check is deliberately scheme-independent. TLS does not turn a process-local or
/// hosts-file-controlled name into an Internet service, and accepting it in a configured outbound
/// URL would contradict the numeric-only local-development boundary.
pub fn is_localhost_name(host: &str) -> bool {
    let candidate = host.trim_end_matches('.');
    candidate.eq_ignore_ascii_case("localhost")
        || candidate
            .rsplit_once('.')
            .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("localhost"))
}

fn is_public_ipv4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_ipv6(address: Ipv6Addr) -> bool {
    let segments = address.segments();
    !(address.is_unspecified()
        || address.is_loopback()
        || address.is_multicast()
        // Fail closed outside currently allocated 2000::/3 global unicast. This includes old
        // 6bone, discard-only, NAT64, unique-local, link-local, and future-use space.
        || (segments[0] & 0xe000) != 0x2000
        || (segments[0] == 0x2001 && segments[1] == 0x0000) // Teredo
        || (segments[0] == 0x2001 && segments[1] == 0x0002) // benchmarking
        || (segments[0] == 0x2001 && segments[1] == 0x0db8) // documentation
        || (segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0010) // ORCHIDv1
        || (segments[0] == 0x2001 && (segments[1] & 0xfff0) == 0x0020) // ORCHIDv2
        || (segments[0] == 0x3fff && (segments[1] & 0xf000) == 0) // documentation
        || (segments[0] == 0x2002
            && !is_public_ipv4(Ipv4Addr::new(
                (segments[1] >> 8) as u8,
                segments[1] as u8,
                (segments[2] >> 8) as u8,
                segments[2] as u8,
            )))) // 6to4 embeds an IPv4 route
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_security_vectors_define_the_shared_boundary() {
        for vector in NETWORK_ADDRESS_VECTORS {
            let address: IpAddr = vector.address.parse().unwrap();
            assert_eq!(
                is_public_network_address(address),
                vector.public,
                "classification drifted for {}",
                vector.address
            );
        }
    }

    #[test]
    fn cleartext_local_exception_requires_a_numeric_loopback_host() {
        for accepted in ["127.0.0.1", "127.0.0.42", "::1", "[::1]"] {
            assert!(is_numeric_loopback_host(accepted), "rejected {accepted}");
        }
        for rejected in [
            "",
            "localhost",
            "LOCALHOST",
            "localhost.",
            "127.0.0.1.example",
            "192.0.2.1",
            "[::1",
            "::1]",
        ] {
            assert!(!is_numeric_loopback_host(rejected), "accepted {rejected}");
        }
    }

    #[test]
    fn localhost_dns_names_are_recognized_on_every_scheme_boundary() {
        for rejected in [
            "localhost",
            "LOCALHOST",
            "localhost.",
            "api.localhost",
            "api.localhost.",
            "deep.api.LOCALHOST",
        ] {
            assert!(is_localhost_name(rejected), "missed {rejected}");
        }
        for accepted in [
            "127.0.0.1",
            "::1",
            "localhost.example",
            "notlocalhost",
            "api.localhost.example",
        ] {
            assert!(!is_localhost_name(accepted), "misclassified {accepted}");
        }
    }
}
