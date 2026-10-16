//! Client IP filtering: which client addresses may connect to a bridge.

use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

use serde::Deserialize;

/// An address with a prefix length: `10.0.0.0/24`, `fd00::/8`, or a single
/// address such as `10.0.0.5`.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq)]
#[serde(try_from = "String")]
pub(crate) struct Network {
    addr: IpAddr,
    prefix: u8,
}

impl Network {
    fn contains(&self, ip: IpAddr) -> bool {
        // clients of a dual stack listener show up as ::ffff:a.b.c.d
        match (self.addr, ip.to_canonical()) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                let mask = u32::MAX.checked_shl(32 - self.prefix as u32).unwrap_or(0);
                u32::from(net) & mask == u32::from(ip) & mask
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                let mask = u128::MAX.checked_shl(128 - self.prefix as u32).unwrap_or(0);
                u128::from(net) & mask == u128::from(ip) & mask
            }
            _ => false,
        }
    }
}

impl FromStr for Network {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("invalid IP network {:?}", s);
        let (addr, prefix) = match s.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (s, None),
        };
        let addr: IpAddr = addr.trim().parse().map_err(|_| invalid())?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(prefix) => prefix.trim().parse().map_err(|_| invalid())?,
            None => max,
        };
        if prefix > max {
            return Err(invalid());
        }
        Ok(Network { addr, prefix })
    }
}

impl TryFrom<String> for Network {
    type Error = String;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Clients matching `deny` are refused; when `allow` is not empty, only
/// clients matching it are accepted. The default accepts everyone.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct IpFilter {
    #[serde(default)]
    pub(crate) allow: Vec<Network>,
    #[serde(default)]
    pub(crate) deny: Vec<Network>,
}

impl IpFilter {
    pub(crate) fn permits(&self, ip: IpAddr) -> bool {
        let matches = |networks: &[Network]| networks.iter().any(|n| n.contains(ip));
        !matches(&self.deny) && (self.allow.is_empty() || matches(&self.allow))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(s: &str) -> Network {
        s.parse().unwrap()
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn parse() {
        assert_eq!(net("10.0.0.0/24").to_string(), "10.0.0.0/24");
        assert_eq!(net("10.0.0.5").to_string(), "10.0.0.5/32");
        assert_eq!(net(" fd00::/8 ").to_string(), "fd00::/8");
        assert_eq!(net("::1").to_string(), "::1/128");
        assert_eq!(net("0.0.0.0/0").to_string(), "0.0.0.0/0");
        for bad in [
            "",
            "host",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/x",
            "10.0.0/8",
        ] {
            assert_eq!(
                bad.parse::<Network>().unwrap_err(),
                format!("invalid IP network {:?}", bad)
            );
        }
    }

    #[test]
    fn contains() {
        assert!(net("10.0.0.0/24").contains(ip("10.0.0.255")));
        assert!(!net("10.0.0.0/24").contains(ip("10.0.1.0")));
        assert!(net("10.0.0.5").contains(ip("10.0.0.5")));
        assert!(!net("10.0.0.5").contains(ip("10.0.0.6")));
        assert!(net("0.0.0.0/0").contains(ip("192.0.2.1")));
        assert!(net("fd00::/8").contains(ip("fd12::1")));
        assert!(!net("fd00::/8").contains(ip("fe80::1")));
        assert!(net("::/0").contains(ip("2001:db8::1")));
        // no cross family matches, except IPv4 clients of a dual stack socket
        assert!(!net("::/0").contains(ip("10.0.0.1")));
        assert!(!net("0.0.0.0/0").contains(ip("::1")));
        assert!(net("10.0.0.0/24").contains(ip("::ffff:10.0.0.7")));
    }

    #[test]
    fn permits() {
        let filter = |allow: &[&str], deny: &[&str]| IpFilter {
            allow: allow.iter().map(|s| net(s)).collect(),
            deny: deny.iter().map(|s| net(s)).collect(),
        };
        assert!(IpFilter::default().permits(ip("192.0.2.1")));
        let f = filter(&["10.0.0.0/24", "fd00::/8"], &[]);
        assert!(f.permits(ip("10.0.0.1")));
        assert!(f.permits(ip("fd00::1")));
        assert!(!f.permits(ip("192.0.2.1")));
        let f = filter(&[], &["10.0.0.66"]);
        assert!(f.permits(ip("10.0.0.1")));
        assert!(!f.permits(ip("10.0.0.66")));
        // deny wins over allow
        let f = filter(&["10.0.0.0/24"], &["10.0.0.66"]);
        assert!(f.permits(ip("10.0.0.1")));
        assert!(!f.permits(ip("10.0.0.66")));
    }
}
