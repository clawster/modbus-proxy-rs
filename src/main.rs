use clap::Parser;

use modbus_proxy_rs::Server;

#[derive(Debug, Parser)]
#[command(
    name = "modbus proxy",
    about = "Connect multiple clients to modbus devices"
)]
struct CmdLine {
    /// configuration file (accepts YAML, TOML, JSON)
    #[arg(short = 'c', long = "config-file")]
    config_file: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    // RUST_LOG overrides the default level
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args = CmdLine::parse();
    if let Err(error) = Server::launch(&args.config_file).await {
        eprintln!("Configuration error: {}", error)
    }
}
