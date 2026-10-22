//! Refusing to connect to addresses on the machine or on the network around it
//! ([`NetConfig::block_private_addresses`]).
//!
//! An application that shows content other people wrote (a chat, a feed, markdown) lets them
//! choose which addresses it requests, and a request from the viewer's machine reaches what
//! outsiders cannot: a service on `localhost`, the router, a cloud provider's metadata
//! address. The check is made on the addresses a name resolves to, right before connecting,
//! so a name that resolves to a private address is refused like the address itself, and a
//! name cannot be changed between the check and the connection. It applies to connections
//! that are made directly: when a proxy carries the request, the proxy decides where it
//! goes, and the application that configured it is trusted with that.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};

/// The reason carried inside the `io::Error` that a refused connection fails with, which
/// the fetcher turns into `FetchError::BlockedAddress`.
#[derive(Debug)]
pub(crate) struct Blocked;

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the address is on the local machine or a private network"
        )
    }
}

impl std::error::Error for Blocked {}

/// The default resolver, with the addresses that are not public taken out of its answer.
#[derive(Debug, Default)]
pub(crate) struct PublicOnly {
    inner: DefaultResolver,
}

impl Resolver for PublicOnly {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let found = self.inner.resolve(uri, config, timeout)?;
        let mut public = self.empty();
        for address in found.iter().filter(|address| !is_private(address.ip())) {
            public.push(*address);
        }
        if public.is_empty() {
            return Err(ureq::Error::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                Blocked,
            )));
        }
        Ok(public)
    }
}

/// Whether `address` is not one that anyone on the internet can be reached at: this machine,
/// a private or shared network, link-local (where cloud metadata services live), multicast
/// or unspecified.
pub(crate) fn is_private(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => is_private_v4(address),
        IpAddr::V6(address) => is_private_v6(address),
    }
}

fn is_private_v4(a: Ipv4Addr) -> bool {
    let [first, second, ..] = a.octets();
    a.is_loopback()
        || a.is_private()
        || a.is_link_local()
        || a.is_unspecified()
        || a.is_broadcast()
        || a.is_multicast()
        || a.is_documentation()
        // 100.64.0.0/10, shared address space (carrier-grade NAT).
        || (first == 100 && (second & 0b1100_0000) == 64)
        // 0.0.0.0/8, "this network".
        || first == 0
        // 192.0.0.0/24, protocol assignments.
        || a.octets()[..3] == [192, 0, 0]
        // 198.18.0.0/15, benchmarking.
        || (first == 198 && (second & 0b1111_1110) == 18)
        // 240.0.0.0/4, reserved.
        || first >= 240
}

fn is_private_v6(a: Ipv6Addr) -> bool {
    if let Some(mapped) = a.to_ipv4_mapped() {
        return is_private_v4(mapped);
    }
    let segments = a.segments();
    let first = segments[0];
    let embedded = |high: u16, low: u16| Ipv4Addr::from((u32::from(high) << 16) | u32::from(low));
    a.is_loopback()
        || a.is_unspecified()
        || a.is_multicast()
        // ::/96, IPv4-compatible (deprecated) and the addresses next to ::1.
        || segments[..6] == [0, 0, 0, 0, 0, 0]
        // fc00::/7, unique local.
        || (first & 0xfe00) == 0xfc00
        // fe80::/10, link-local, and fec0::/10, site-local (deprecated).
        || (first & 0xffc0) == 0xfe80
        || (first & 0xffc0) == 0xfec0
        // 2001:db8::/32, documentation.
        || (first == 0x2001 && segments[1] == 0x0db8)
        // 2001::/32, Teredo, which carries a server and a client IPv4 address.
        || (first == 0x2001 && segments[1] == 0)
        // 2002::/16, 6to4: the IPv4 address is in the next 32 bits.
        || (first == 0x2002 && is_private_v4(embedded(segments[1], segments[2])))
        // 100::/64, discard-only.
        || segments[..4] == [0x100, 0, 0, 0]
        // 64:ff9b::/96 (NAT64) carries an IPv4 address in its last 32 bits; 64:ff9b:1::/48 is
        // for local use.
        || (segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0]
            && is_private_v4(embedded(segments[6], segments[7])))
        || segments[..3] == [0x64, 0xff9b, 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private(address: &str) -> bool {
        is_private(address.parse().unwrap())
    }

    #[test]
    fn addresses_of_this_machine_and_the_networks_around_it_are_private() {
        for address in [
            "127.0.0.1",
            "127.255.255.254",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.127.255.255",
            "0.0.0.0",
            "0.1.2.3",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd12:3456::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.1.2.3",
            "64:ff9b::7f00:1",
            "2001:db8::1",
            "2002:7f00:1::",
            "2002:0a00:1::",
            "2002:c0a8:1::1",
            "2001:0:4136:e378:8000:63bf:3fff:fdd2",
            "::127.0.0.1",
            "::7f00:1",
            "fec0::1",
            "100::1",
            "64:ff9b:1::1",
        ] {
            assert!(private(address), "{address}");
        }
    }

    #[test]
    fn public_addresses_are_not() {
        for address in [
            "8.8.8.8",
            "1.1.1.1",
            "93.184.216.34",
            "172.15.0.1",
            "172.32.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "198.17.0.1",
            "198.20.0.1",
            "2606:4700:4700::1111",
            "2a00:1450:4001::200e",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2002:0808:0808::1",
            "2001:4860:4860::8888",
        ] {
            assert!(!private(address), "{address}");
        }
    }
}
