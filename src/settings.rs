//! Configuration file format.

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

#[derive(Debug, Deserialize)]
pub(crate) struct Settings {
    pub(crate) devices: Vec<BridgeConfig>,
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
    }

    #[test]
    fn load_missing_file() {
        assert!(Settings::load("/nonexistent/modbus-proxy.yml").is_err());
    }
}
