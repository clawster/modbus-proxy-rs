//! Request statistics of a device, for the periodic status log.

use std::collections::BTreeMap;
use std::fmt;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use crate::modbus::Kind;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Counters {
    reads: u64,
    writes: u64,
    errors: u64,
}

impl Counters {
    fn count(&mut self, kind: Kind, failed: bool) {
        match kind {
            Kind::Read => self.reads += 1,
            Kind::Write => self.writes += 1,
            Kind::Other => {}
        }
        if failed {
            self.errors += 1;
        }
    }
}

impl fmt::Display for Counters {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{} reads, {} writes, {} errors",
            self.reads, self.writes, self.errors
        )
    }
}

struct Client {
    since: Instant,
    counters: Counters,
}

impl fmt::Display for Client {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "connected {}: {}",
            format_duration(self.since.elapsed()),
            self.counters
        )
    }
}

/// `1h02m03s`, `2m03s` or `3s`.
pub(crate) fn format_duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 {
        format!("{}h{:02}m{:02}s", h, m, s)
    } else if m > 0 {
        format!("{}m{:02}s", m, s)
    } else {
        format!("{}s", s)
    }
}

#[derive(Default)]
pub(crate) struct Stats {
    clients: BTreeMap<SocketAddr, Client>,
    /// Since the last status
    interval: Counters,
}

impl Stats {
    pub(crate) fn connect(&mut self, client: SocketAddr) {
        self.clients.insert(
            client,
            Client {
                since: Instant::now(),
                counters: Counters::default(),
            },
        );
    }

    /// Forget `client`, returning a summary of its connection.
    pub(crate) fn disconnect(&mut self, client: SocketAddr) -> Option<String> {
        self.clients.remove(&client).map(|c| c.to_string())
    }

    pub(crate) fn clients(&self) -> usize {
        self.clients.len()
    }

    pub(crate) fn count(&mut self, client: SocketAddr, kind: Kind, failed: bool) {
        self.interval.count(kind, failed);
        if let Some(c) = self.clients.get_mut(&client) {
            c.counters.count(kind, failed);
        }
    }

    /// Summary since the last status (`period` ago): one line for the
    /// device, then one per connected client.
    pub(crate) fn status(&mut self, name: &str, period: Duration) -> Vec<String> {
        let interval = std::mem::take(&mut self.interval);
        let mut lines = vec![format!(
            "{}: {} clients, last {}: {}",
            name,
            self.clients.len(),
            format_duration(period),
            interval
        )];
        for (addr, client) in &self.clients {
            lines.push(format!("{}: client {} {}", name, addr, client));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::peer;

    #[test]
    fn durations() {
        let d = |s| format_duration(Duration::from_secs(s));
        assert_eq!(d(0), "0s");
        assert_eq!(d(59), "59s");
        assert_eq!(d(123), "2m03s");
        assert_eq!(d(3723), "1h02m03s");
        assert_eq!(d(90000), "25h00m00s");
    }

    #[test]
    fn counts_per_client_and_interval() {
        let mut stats = Stats::default();
        let other: SocketAddr = "192.0.2.2:1".parse().unwrap();
        stats.connect(peer());
        stats.count(peer(), Kind::Read, false);
        stats.count(peer(), Kind::Read, false);
        stats.count(peer(), Kind::Write, false);
        stats.count(peer(), Kind::Other, true);
        // requests of unknown clients only count for the device
        stats.count(other, Kind::Read, true);
        assert_eq!(stats.clients(), 1);
        assert_eq!(
            stats.status("dev", Duration::from_secs(120)),
            [
                "dev: 1 clients, last 2m00s: 3 reads, 1 writes, 2 errors",
                "dev: client 192.0.2.1:50000 connected 0s: 2 reads, 1 writes, 1 errors",
            ]
        );
        // interval counters restart, client totals don't
        assert_eq!(
            stats.status("dev", Duration::from_secs(120))[0],
            "dev: 1 clients, last 2m00s: 0 reads, 0 writes, 0 errors"
        );
        assert_eq!(
            stats.disconnect(peer()).unwrap(),
            "connected 0s: 2 reads, 1 writes, 1 errors"
        );
        assert_eq!(stats.disconnect(peer()), None);
        assert_eq!(stats.clients(), 0);
    }
}
