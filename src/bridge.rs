//! The client side: accepts clients on one listen address and forwards
//! their requests to the device task of that bridge.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::FutureExt;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout};

use crate::device::{ChannelTx, Device, Message};
use crate::frame::{read_frame, set_nodelay, split_connection};
use crate::ipfilter::IpFilter;
use crate::registers::RegisterMap;
use crate::settings::BridgeConfig;
use crate::timeouts::{ACCEPT_RETRY_DELAY, CLIENT_REQUEST_TIMEOUT};
use crate::Result;

type Accept<'a> =
    dyn FnMut() -> BoxFuture<'a, std::io::Result<(TcpStream, SocketAddr)>> + Send + 'a;

/// Run one bridge: spawn its device task and serve clients forever.
/// Returns only on a setup error (e.g. the listen address is in use).
pub(crate) async fn run(
    config: &BridgeConfig,
    ip_filter: &IpFilter,
    registers: Option<Arc<RegisterMap>>,
    status: Option<Duration>,
) -> Result<Infallible> {
    let listener = TcpListener::bind(&config.listen.bind).await?;
    let modbus = config.modbus.clone();
    let (tx, mut rx) = mpsc::channel::<Message>(32);
    tokio::spawn(async move {
        Device::launch(&modbus, registers, status, &mut rx).await;
    });
    info!(
        "Ready to accept requests on {} to {}",
        config.listen.bind, config.modbus.url
    );
    let listener = &listener;
    let mut accept = move || listener.accept().boxed();
    let filters = [ip_filter, &config.ip_filter];
    serve(&config.listen.bind, &mut accept, &filters, tx).await
}

/// Serve clients accepted by `accept` that pass all `filters`.
async fn serve<'a>(
    bind: &str,
    accept: &mut Accept<'a>,
    filters: &[&IpFilter],
    tx: ChannelTx,
) -> Result<Infallible> {
    loop {
        let (client, peer) = match accept().await {
            Ok(accepted) => accepted,
            Err(err) => {
                // e.g. EMFILE: don't die, back off and let fds free up
                error!("accept error on {}: {}", bind, err);
                sleep(ACCEPT_RETRY_DELAY).await;
                continue;
            }
        };
        if !filters.iter().all(|filter| filter.permits(peer.ip())) {
            warn!("{}: refused client {} (ip_filter)", bind, peer);
            continue;
        }
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(client, peer, tx).await {
                error!("Client {} error: {:?}", peer, err);
            }
        });
    }
}

async fn handle_client(client: TcpStream, peer: SocketAddr, channel: ChannelTx) -> Result<()> {
    set_nodelay(&client);
    timeout(
        CLIENT_REQUEST_TIMEOUT,
        channel.send(Message::Connection(peer)),
    )
    .await??;

    let result = client_loop(client, peer, &channel).await;

    // Best effort: the client socket is already closed at this point.
    let _ = timeout(
        CLIENT_REQUEST_TIMEOUT,
        channel.send(Message::Disconnection(peer)),
    )
    .await;

    result
}

async fn client_loop(client: TcpStream, peer: SocketAddr, channel: &ChannelTx) -> Result<()> {
    let (mut reader, mut writer) = split_connection(client);
    while let Ok(buf) = read_frame(&mut reader).await {
        let (tx, rx) = oneshot::channel();
        timeout(
            CLIENT_REQUEST_TIMEOUT,
            channel.send(Message::Packet(buf, peer, tx)),
        )
        .await??;
        let reply = timeout(CLIENT_REQUEST_TIMEOUT, rx).await??;
        writer.write_all(&reply).await?;
        writer.flush().await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Listen;
    use crate::test_util::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::time::error::Elapsed;

    fn bridge(bind: &str, url: &str) -> BridgeConfig {
        BridgeConfig {
            listen: Listen {
                bind: bind.to_string(),
            },
            modbus: modbus(url, None),
            ip_filter: IpFilter::default(),
        }
    }

    #[tokio::test]
    async fn bind_error() {
        init();
        let (_l, addr) = listener().await;
        assert!(
            run(&bridge(&addr, "unused"), &IpFilter::default(), None, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn proxies_requests() {
        init();
        let (dev, dev_addr) = listener().await;
        let bind = closed_addr().await;
        let config = bridge(&bind, &dev_addr);
        let ip_filter = IpFilter::default();
        let clients = async {
            sleep(Duration::from_millis(50)).await;
            // client connecting and leaving cleanly
            drop(TcpStream::connect(&bind).await.unwrap());
            // request answered by the device
            let mut c = TcpStream::connect(&bind).await.unwrap();
            c.write_all(&REQUEST).await.unwrap();
            echo_once(&dev).await;
            let mut reply = [0u8; 12];
            c.read_exact(&mut reply).await.unwrap();
            assert_eq!(reply, REQUEST);
            // device gone: the client connection is closed
            drop(dev);
            c.write_all(&REQUEST).await.unwrap();
            assert_eq!(c.read(&mut reply).await.unwrap(), 0);
        };
        let (result, _) = tokio::join!(
            timeout(
                Duration::from_millis(500),
                run(&config, &ip_filter, None, None)
            ),
            clients
        );
        assert!(result.is_err());
        // the bridge is gone: give its device task time to stop
        sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn accept_error_is_retried() {
        init();
        let (tx, _rx) = mpsc::channel(1);
        let calls = AtomicUsize::new(0);
        let mut accept = || {
            let n = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                match n {
                    0 => Err(std::io::Error::other("EMFILE")),
                    _ => std::future::pending().await,
                }
            }
            .boxed()
        };
        assert!(timeout(
            Duration::from_millis(100),
            serve("test", &mut accept, &[], tx)
        )
        .await
        .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn ip_filter() {
        init();
        let (l, addr) = listener().await;
        let (tx, mut rx) = mpsc::channel(1);
        let everyone = IpFilter::default();
        let loopback = IpFilter {
            deny: vec!["127.0.0.0/8".parse().unwrap()],
            ..IpFilter::default()
        };
        let l = &l;
        let mut accept = move || l.accept().boxed();
        let wait = Duration::from_millis(100);
        // the client passes the first filter but not the second: closed
        let filters = [&everyone, &loopback];
        let (result, _) = tokio::join!(
            timeout(wait, serve("test", &mut accept, &filters, tx.clone())),
            async {
                let mut c = TcpStream::connect(&addr).await.unwrap();
                assert_eq!(c.read(&mut [0u8; 1]).await.unwrap(), 0);
            }
        );
        assert!(result.is_err());
        assert!(rx.try_recv().is_err());
        // accepted when all filters let it through
        let filters = [&everyone];
        let (result, c) = tokio::join!(
            timeout(wait, serve("test", &mut accept, &filters, tx)),
            TcpStream::connect(&addr)
        );
        assert!(result.is_err());
        let client = c.unwrap().local_addr().unwrap();
        assert_eq!(
            format!("{:?}", rx.try_recv().unwrap()),
            format!("Connection({:?})", client)
        );
    }

    #[tokio::test]
    async fn handle_client_channel_closed() {
        init();
        let (_c, server) = pair().await;
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        assert!(handle_client(server, peer(), tx).await.is_err());
    }

    #[tokio::test]
    async fn handle_client_channel_full() {
        let (_c, server) = pair().await;
        let (tx, _rx) = mpsc::channel(1);
        tx.send(Message::Connection(peer())).await.unwrap();
        assert!(handle_client(server, peer(), tx).await.is_err());
    }

    #[tokio::test]
    async fn client_loop_send_errors() {
        // channel closed
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        assert!(client_loop(server, peer(), &tx).await.is_err());
        // channel full
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (tx, _rx) = mpsc::channel(1);
        tx.send(Message::Connection(peer())).await.unwrap();
        assert!(client_loop(server, peer(), &tx).await.is_err());
    }

    #[tokio::test]
    async fn client_loop_reply_errors() {
        // reply sender dropped
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let (result, _) = tokio::join!(client_loop(server, peer(), &tx), async {
            drop(rx.recv().await);
        });
        assert!(result.unwrap_err().is::<oneshot::error::RecvError>());
        // reply never comes
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (result, _msg) = tokio::join!(client_loop(server, peer(), &tx), rx.recv());
        assert!(result.unwrap_err().is::<Elapsed>());
    }

    /// Client sends a request and resets the connection while the device
    /// is still answering; the reply of `reply_len` payload bytes can then
    /// not be written back.
    async fn client_loop_write_error(reply_len: usize) {
        let (l, addr) = listener().await;
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        c.set_zero_linger().unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let client = async {
            let result = client_loop(server, peer(), &tx).await;
            drop(tx);
            result
        };
        let device = async {
            let (stream, _) = l.accept().await.unwrap();
            let (mut r, mut w) = split_connection(stream);
            read_frame(&mut r).await.unwrap();
            drop(c);
            sleep(Duration::from_millis(50)).await;
            w.write_all(&frame(reply_len)).await.unwrap();
            w.flush().await.unwrap();
        };
        let config = modbus(&addr, None);
        let (result, _, _) =
            tokio::join!(client, Device::launch(&config, None, None, &mut rx), device);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn client_loop_flush_error() {
        client_loop_write_error(5).await;
    }

    #[tokio::test]
    async fn client_loop_write_all_error() {
        client_loop_write_error(20_000).await;
    }
}
