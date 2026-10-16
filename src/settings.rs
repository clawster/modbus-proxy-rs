//! Configuration file format.

use std::time::Duration;

use serde::Deserialize;

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
}

fn default_status_interval() -> u64 {
    120
}

#[derive(Debug, Deserialize)]
pub(crate) struct Settings {
    /// Seconds between status logs of each device, 0 disables them.
    #[serde(default = "default_status_interval")]
    pub(crate) status_interval: u64,
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
