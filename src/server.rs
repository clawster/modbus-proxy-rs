//! Top level: runs one bridge per configured device until shutdown.

use std::sync::Arc;
use std::time::Duration;

use futures::future::join_all;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

use crate::bridge;
use crate::ipfilter::IpFilter;
use crate::registers::RegisterMap;
use crate::settings::{BridgeConfig, Settings};
use crate::Result;

#[derive(Debug)]
pub struct Server {
    bridges: Vec<(BridgeConfig, Option<Arc<RegisterMap>>)>,
    status: Option<Duration>,
    ip_filter: IpFilter,
}

impl Server {
    /// Load the configuration and the register maps it refers to.
    pub fn new(config_file: &str) -> Result<Self> {
        let settings = Settings::load(config_file)?;
        let mut bridges = vec![];
        let status = settings.status();
        for config in settings.devices {
            let registers = match &config.modbus.registers {
                Some(path) => Some(Arc::new(RegisterMap::load(path)?)),
                None => None,
            };
            bridges.push((config, registers));
        }
        Ok(Self {
            bridges,
            status,
            ip_filter: settings.ip_filter,
        })
    }

    pub async fn run(self) {
        let mut tasks = vec![];
        let status = self.status;
        for (config, registers) in self.bridges {
            let ip_filter = self.ip_filter.clone();
            tasks.push(tokio::spawn(async move {
                let Err(err) = bridge::run(&config, &ip_filter, registers, status).await;
                error!("bridge {} stopped: {}", config.listen.bind, err);
            }));
        }

        #[cfg(unix)]
        let mut sigterm = signal(SignalKind::terminate()).unwrap();
        tokio::select! {
            _ = join_all(tasks) => debug!("All tasks finished"),
            _ = tokio::signal::ctrl_c() => debug!("Received Ctrl+C"),
            //#[cfg(unix)]
            _ = sigterm.recv() => debug!("Received SIGTERM"),
        }

        info!("Shutting down");
    }

    pub async fn launch(config_file: &str) -> Result<()> {
        let _: () = Self::new(config_file)?.run().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use std::time::Duration;
    use tokio::time::sleep;

    /// Serializes tests that deliver process-wide signals.
    static SIGNALS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn config_error() {
        assert!(Server::launch("/nonexistent/modbus-proxy.yml")
            .await
            .is_err());
    }

    #[test]
    fn loads_register_maps() {
        let yaml = format!(
            "devices:\n- modbus: {{url: \"127.0.0.1:1\", registers: \"{}\"}}\n  listen: {{bind: \"127.0.0.1:1\"}}\n",
            SIGENERGY_MAP
        );
        let path = write_config("maps", &yaml);
        let server = Server::new(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(server.bridges[0].1.is_some());
    }

    #[test]
    fn register_map_error() {
        let yaml = "devices:\n- modbus: {url: \"127.0.0.1:1\", registers: /nonexistent.json}\n  listen: {bind: \"127.0.0.1:1\"}\n";
        let path = write_config("bad-map", yaml);
        let err = Server::new(&path).unwrap_err();
        std::fs::remove_file(path).unwrap();
        assert!(err.to_string().starts_with("/nonexistent.json: "));
    }

    #[tokio::test]
    async fn stops_when_all_bridges_fail() {
        init();
        let _guard = SIGNALS.lock().await;
        let (_l, addr) = listener().await;
        let path = config_file("fail", &addr);
        Server::launch(&path).await.unwrap();
        std::fs::remove_file(path).unwrap();
    }

    async fn stops_on_signal(name: &str, kind: SignalKind) {
        init();
        let _guard = SIGNALS.lock().await;
        // Install the handler up front so the signal can never hit the default action.
        let _keep = signal(kind).unwrap();
        let path = config_file(name, &closed_addr().await);
        let server = Server::new(&path).unwrap();
        let kill = async {
            sleep(Duration::from_millis(100)).await;
            let status = std::process::Command::new("kill")
                .arg(format!("-{}", name))
                .arg(std::process::id().to_string())
                .status()
                .unwrap();
            assert!(status.success());
        };
        tokio::join!(server.run(), kill);
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn stops_on_sigint() {
        stops_on_signal("INT", SignalKind::interrupt()).await;
    }

    #[tokio::test]
    async fn stops_on_sigterm() {
        stops_on_signal("TERM", SignalKind::terminate()).await;
    }
}
