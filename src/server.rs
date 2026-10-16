//! Top level: runs one bridge per configured device until shutdown.

use futures::future::join_all;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

use crate::bridge;
use crate::settings::Settings;

#[derive(Debug)]
pub struct Server {
    settings: Settings,
}

impl Server {
    pub fn new(config_file: &str) -> Result<Self, config::ConfigError> {
        Ok(Self {
            settings: Settings::load(config_file)?,
        })
    }

    pub async fn run(self) {
        let mut tasks = vec![];
        for config in self.settings.devices {
            tasks.push(tokio::spawn(async move {
                let Err(err) = bridge::run(&config).await;
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

    pub async fn launch(config_file: &str) -> Result<(), config::ConfigError> {
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
