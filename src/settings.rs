//! Configuration file format.

use std::time::Duration;

use serde::Deserialize;

use crate::ipfilter::IpFilter;

#[derive(Debug, Deserialize)]
pub(crate) struct Listen {
    pub(crate) bind: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct Modbus {
    pub(crate) url: String,
    /// Name of the device in the register log, defaults to the url.
    pub(crate) name: Option<String>,
    /// Register map (JSON) used to describe requests in the log.
    pub(crate) registers: Option<String>,
}

/// One proxied device: clients connecting to `listen` are forwarded to `modbus`.
#[derive(Debug, Deserialize)]
pub(crate) struct BridgeConfig {
    pub(crate) listen: Listen,
    pub(crate) modbus: Modbus,
    /// Clients allowed to connect, on top of the global filter.
    #[serde(default)]
    pub(crate) ip_filter: IpFilter,
}

fn default_status_interval() -> u64 {
    120
}

#[derive(Debug, Deserialize)]
pub(crate) struct Settings {
    /// Seconds between status logs of each device, 0 disables them.
    #[serde(default = "default_status_interval")]
    pub(crate) status_interval: u64,
    /// Clients allowed to connect to any device.
    #[serde(default)]
    pub(crate) ip_filter: IpFilter,
    pub(crate) devices: Vec<BridgeConfig>,
}

impl Settings {
    pub(crate) fn status(&self) -> Option<Duration> {
        (self.status_interval > 0).then(|| Duration::from_secs(self.status_interval))
    }
}

impl Settings {
    /// Load a YAML, TOML or JSON configuration file.
    pub(crate) fn load(path: &str) -> Result<Self, config::ConfigError> {
        config::Config::builder()
            .add_source(config::File::with_name(path))
            .build()?
            .try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::config_file;

    #[test]
    fn load_ok() {
        let path = config_file("load", "127.0.0.1:5020");
        let settings = Settings::load(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(settings.devices.len(), 1);
        assert_eq!(settings.devices[0].listen.bind, "127.0.0.1:5020");
        assert_eq!(settings.devices[0].modbus.url, "127.0.0.1:1");
        assert_eq!(settings.status(), Some(Duration::from_secs(120)));
        assert_eq!(settings.ip_filter, IpFilter::default());
        assert_eq!(settings.devices[0].ip_filter, IpFilter::default());
    }

    #[test]
    fn ip_filters() {
        let yaml = "ip_filter:\n  allow: [10.0.0.0/24]\n  deny: [10.0.0.66]\ndevices:\n- modbus: {url: \"127.0.0.1:1\"}\n  listen: {bind: \"127.0.0.1:1\"}\n  ip_filter: {allow: [\"10.0.0.5\", \"fd00::/8\"]}\n";
        let path = crate::test_util::write_config("ip-filter", yaml);
        let settings = Settings::load(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        let names = |networks: &[_]| networks.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(names(&settings.ip_filter.allow), ["10.0.0.0/24"]);
        assert_eq!(names(&settings.ip_filter.deny), ["10.0.0.66/32"]);
        let device = &settings.devices[0].ip_filter;
        assert_eq!(names(&device.allow), ["10.0.0.5/32", "fd00::/8"]);
        assert!(device.deny.is_empty());
    }

    #[test]
    fn invalid_ip_filter() {
        let path = crate::test_util::write_config(
            "bad-ip-filter",
            "ip_filter: {allow: [10.0.0.0/33]}\ndevices: []\n",
        );
        let err = Settings::load(&path).unwrap_err();
        std::fs::remove_file(path).unwrap();
        assert!(
            err.to_string()
                .contains("invalid IP network \"10.0.0.0/33\""),
            "{}",
            err
        );
    }

    #[test]
    fn status_interval() {
        let settings = |yaml: &str| {
            let path = crate::test_util::write_config("status", yaml);
            let settings = Settings::load(&path).unwrap();
            std::fs::remove_file(path).unwrap();
            settings
        };
        assert_eq!(
            settings("status_interval: 30\ndevices: []\n").status(),
            Some(Duration::from_secs(30))
        );
        assert_eq!(settings("status_interval: 0\ndevices: []\n").status(), None);
    }

    #[test]
    fn load_missing_file() {
        assert!(Settings::load("/nonexistent/modbus-proxy.yml").is_err());
    }
}
