#[macro_use]
extern crate log;

use std::convert::Infallible;
use std::time::Duration;

use futures::future::{join_all, BoxFuture};
use futures::FutureExt;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::{TcpListener, TcpStream};
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout};

/// Max time to connect to, write to or read from the modbus device. An
/// unresponsive device must not wedge the device task (and with it every
/// client task and socket).
#[cfg(not(test))]
const DEVICE_IO_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(test)]
const DEVICE_IO_TIMEOUT: Duration = Duration::from_millis(200);

/// Max time a client request may wait to be queued and answered.
#[cfg(not(test))]
const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(test)]
const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_millis(300);

/// Pause after a failed accept (e.g. out of file descriptors) before retrying.
#[cfg(not(test))]
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(100);
#[cfg(test)]
const ACCEPT_RETRY_DELAY: Duration = Duration::from_millis(10);

// Use Jemalloc only for musl-64 bits platforms
//#[cfg(all(target_env = "musl", target_pointer_width = "64"))]
//#[global_allocator]
//static ALLOC: jemallocator::Jemalloc = jemallocator::Jemalloc;

type Frame = Vec<u8>;
type ReplySender = oneshot::Sender<Frame>;

#[derive(Debug)]
enum Message {
    Connection,
    Disconnection,
    Packet(Frame, ReplySender),
}

type ChannelRx = mpsc::Receiver<Message>;
type ChannelTx = mpsc::Sender<Message>;

type Error = Box<dyn std::error::Error + Send + Sync + 'static>;
type Result<T> = std::result::Result<T, Error>;

type TcpReader = BufReader<tokio::net::tcp::OwnedReadHalf>;
type TcpWriter = BufWriter<tokio::net::tcp::OwnedWriteHalf>;

fn frame_size(header: &[u8; 6]) -> usize {
    u16::from_be_bytes([header[4], header[5]]) as usize
}

/// Disable Nagle for lower latency. Best effort: it can only fail on an
/// invalid socket, and the proxy still works without it.
fn set_nodelay(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
}

fn split_connection(stream: TcpStream) -> (TcpReader, TcpWriter) {
    let (reader, writer) = stream.into_split();
    (BufReader::new(reader), BufWriter::new(writer))
}

async fn create_connection(url: &str) -> Result<(TcpReader, TcpWriter)> {
    let stream = timeout(DEVICE_IO_TIMEOUT, TcpStream::connect(url)).await??;
    set_nodelay(&stream);
    Ok(split_connection(stream))
}

async fn read_frame(stream: &mut TcpReader) -> Result<Frame> {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header).await?;
    let mut buf = header.to_vec();
    buf.resize(6 + frame_size(&header), 0);
    stream.read_exact(&mut buf[6..]).await?;
    Ok(buf)
}

#[derive(Debug, Deserialize)]
struct Listen {
    bind: String,
}

#[derive(Debug, Deserialize)]
struct Modbus {
    url: String,
}

struct Device {
    url: String,
    stream: Option<(TcpReader, TcpWriter)>,
}

impl Device {
    pub fn new(url: &str) -> Device {
        Device {
            url: url.to_string(),
            stream: None,
        }
    }

    async fn connect(&mut self) -> Result<()> {
        match create_connection(&self.url).await {
            Ok(connection) => {
                info!("modbus connection to {} sucessfull", self.url);
                self.stream = Some(connection);
                Ok(())
            }
            Err(error) => {
                self.stream = None;
                info!("modbus connection to {} error: {} ", self.url, error);
                Err(error)
            }
        }
    }

    fn disconnect(&mut self) {
        self.stream = None;
    }

    fn is_connected(&self) -> bool {
        self.stream.is_some()
    }

    async fn raw_write_read(&mut self, frame: &Frame) -> Result<Frame> {
        let (reader, writer) = self.stream.as_mut().ok_or("no modbus connection")?;
        // A stall is turned into an error so the device connection is
        // dropped instead of wedging the device task.
        timeout(DEVICE_IO_TIMEOUT, async {
            writer.write_all(frame).await?;
            writer.flush().await
        })
        .await??;
        timeout(DEVICE_IO_TIMEOUT, read_frame(reader)).await?
    }

    async fn write_read(&mut self, frame: &Frame) -> Result<Frame> {
        if self.is_connected() {
            let result = self.raw_write_read(frame).await;
            match result {
                Ok(reply) => Ok(reply),
                Err(error) => {
                    warn!("modbus error: {}. Retrying...", error);
                    self.connect().await?;
                    self.raw_write_read(frame).await
                }
            }
        } else {
            self.connect().await?;
            self.raw_write_read(frame).await
        }
    }

    async fn handle_packet(&mut self, frame: Frame, channel: ReplySender) -> Result<()> {
        info!("modbus request {}: {} bytes", self.url, frame.len());
        debug!("modbus request {}: {:?}", self.url, &frame[..]);
        let reply = self.write_read(&frame).await?;
        info!("modbus reply {}: {} bytes", self.url, reply.len());
        debug!("modbus reply {}: {:?}", self.url, &reply[..]);
        channel
            .send(reply)
            .map_err(|error| format!("error sending reply to client: {:?}", error).into())
    }

    async fn run(&mut self, channel: &mut ChannelRx) {
        let mut nb_clients = 0;

        while let Some(message) = channel.recv().await {
            match message {
                Message::Connection => {
                    nb_clients += 1;
                    info!("new client connection (active = {})", nb_clients);
                }
                Message::Disconnection => {
                    nb_clients -= 1;
                    info!("client disconnection (active = {})", nb_clients);
                    if nb_clients == 0 {
                        info!("disconnecting from modbus at {} (no clients)", self.url);
                        self.disconnect();
                    }
                }
                Message::Packet(frame, channel) => {
                    if self.handle_packet(frame, channel).await.is_err() {
                        self.disconnect();
                    }
                }
            }
        }
    }

    async fn launch(url: &str, channel: &mut ChannelRx) {
        let mut modbus = Self::new(url);
        modbus.run(channel).await;
    }
}

type Accept<'a> = dyn FnMut() -> BoxFuture<'a, std::io::Result<TcpStream>> + Send + 'a;

#[derive(Debug, Deserialize)]
struct Bridge {
    listen: Listen,
    modbus: Modbus,
}

impl Bridge {
    pub async fn run(&mut self) -> Result<Infallible> {
        let listener = TcpListener::bind(&self.listen.bind).await?;
        let modbus_url = self.modbus.url.clone();
        let (tx, mut rx) = mpsc::channel::<Message>(32);
        tokio::spawn(async move {
            Device::launch(&modbus_url, &mut rx).await;
        });
        info!(
            "Ready to accept requests on {} to {}",
            self.listen.bind, self.modbus.url
        );
        let listener = &listener;
        let mut accept =
            move || async move { listener.accept().await.map(|(client, _)| client) }.boxed();
        self.serve(&mut accept, tx).await
    }

    async fn serve<'a>(&self, accept: &mut Accept<'a>, tx: ChannelTx) -> Result<Infallible> {
        loop {
            let client = match accept().await {
                Ok(client) => client,
                Err(err) => {
                    // e.g. EMFILE: don't die, back off and let fds free up
                    error!("accept error on {}: {}", self.listen.bind, err);
                    sleep(ACCEPT_RETRY_DELAY).await;
                    continue;
                }
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(err) = Self::handle_client(client, tx).await {
                    error!("Client error: {:?}", err);
                }
            });
        }
    }

    async fn handle_client(client: TcpStream, channel: ChannelTx) -> Result<()> {
        set_nodelay(&client);
        timeout(CLIENT_REQUEST_TIMEOUT, channel.send(Message::Connection)).await??;

        let result = Self::client_loop(client, &channel).await;

        // Best effort: the client socket is already closed at this point.
        let _ = timeout(CLIENT_REQUEST_TIMEOUT, channel.send(Message::Disconnection)).await;

        result
    }

    async fn client_loop(client: TcpStream, channel: &ChannelTx) -> Result<()> {
        let (mut reader, mut writer) = split_connection(client);
        while let Ok(buf) = read_frame(&mut reader).await {
            let (tx, rx) = oneshot::channel();
            timeout(
                CLIENT_REQUEST_TIMEOUT,
                channel.send(Message::Packet(buf, tx)),
            )
            .await??;
            let reply = timeout(CLIENT_REQUEST_TIMEOUT, rx).await??;
            writer.write_all(&reply).await?;
            writer.flush().await?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
pub struct Server {
    devices: Vec<Bridge>,
}

impl Server {
    pub fn new(config_file: &str) -> std::result::Result<Self, config::ConfigError> {
        let settings = config::Config::builder()
            .add_source(config::File::with_name(config_file))
            .build()?;
        settings.try_deserialize()
    }

    pub async fn run(self) {
        let mut tasks = vec![];
        for mut bridge in self.devices {
            tasks.push(tokio::spawn(async move {
                let Err(err) = bridge.run().await;
                error!("bridge {} stopped: {}", bridge.listen.bind, err);
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

    pub async fn launch(config_file: &str) -> std::result::Result<(), config::ConfigError> {
        let _: () = Self::new(config_file)?.run().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpSocket;
    use tokio::time::error::Elapsed;

    const REQUEST: [u8; 12] = [0, 1, 0, 0, 0, 6, 1, 3, 0, 0, 0, 1];

    /// Serializes tests that deliver process-wide signals.
    static SIGNALS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn init() {
        let _ = env_logger::builder()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();
    }

    /// Frame announcing `len` payload bytes.
    fn frame(len: usize) -> Frame {
        let mut f = vec![0, 1, 0, 0];
        f.extend((len as u16).to_be_bytes());
        f.resize(6 + len, 7);
        f
    }

    async fn listener() -> (TcpListener, String) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap().to_string();
        (l, addr)
    }

    /// Address nothing listens on.
    async fn closed_addr() -> String {
        listener().await.1
    }

    /// Connected (client, server) socket pair.
    async fn pair() -> (TcpStream, TcpStream) {
        let (l, addr) = listener().await;
        let client = TcpStream::connect(addr).await.unwrap();
        let (server, _) = l.accept().await.unwrap();
        (client, server)
    }

    /// Device answering every request with an echo; one connection per call.
    async fn echo_once(l: &TcpListener) {
        let (stream, _) = l.accept().await.unwrap();
        let (mut r, mut w) = split_connection(stream);
        let f = read_frame(&mut r).await.unwrap();
        w.write_all(&f).await.unwrap();
        w.flush().await.unwrap();
    }

    async fn connected_device(l: &TcpListener, addr: &str) -> (Device, TcpStream) {
        let mut device = Device::new(addr);
        device.connect().await.unwrap();
        let (server, _) = l.accept().await.unwrap();
        (device, server)
    }

    #[tokio::test]
    async fn read_frame_ok_and_truncated() {
        let (mut client, server) = pair().await;
        let (mut r, _w) = split_connection(server);
        client.write_all(&REQUEST).await.unwrap();
        assert_eq!(read_frame(&mut r).await.unwrap(), REQUEST);
        // truncated payload
        client.write_all(&REQUEST[..8]).await.unwrap();
        client.shutdown().await.unwrap();
        assert!(read_frame(&mut r).await.is_err());
        // truncated header
        assert!(read_frame(&mut r).await.is_err());
    }

    #[tokio::test]
    async fn device_connect_refused() {
        init();
        let mut device = Device::new(&closed_addr().await);
        assert!(device.connect().await.is_err());
        assert!(!device.is_connected());
        assert!(device.write_read(&REQUEST.to_vec()).await.is_err());
    }

    #[tokio::test]
    async fn device_raw_write_read_not_connected() {
        let mut device = Device::new("unused");
        let err = device.raw_write_read(&REQUEST.to_vec()).await.unwrap_err();
        assert_eq!(err.to_string(), "no modbus connection");
    }

    #[tokio::test]
    async fn device_write_read_connects_and_reuses() {
        init();
        let (l, addr) = listener().await;
        let mut device = Device::new(&addr);
        let request = REQUEST.to_vec();
        let (reply, _) = tokio::join!(device.write_read(&request), echo_once(&l));
        assert_eq!(reply.unwrap(), REQUEST);
        assert!(device.is_connected());
        device.disconnect();
        assert!(!device.is_connected());
    }

    #[tokio::test]
    async fn device_write_read_reconnects_after_error() {
        init();
        let (l, addr) = listener().await;
        let (mut device, server) = connected_device(&l, &addr).await;
        // first connection answers then dies
        let (mut r, mut w) = split_connection(server);
        let request = REQUEST.to_vec();
        let (reply, _) = tokio::join!(device.write_read(&request), async {
            let f = read_frame(&mut r).await.unwrap();
            w.write_all(&f).await.unwrap();
            w.flush().await.unwrap();
        });
        assert_eq!(reply.unwrap(), REQUEST);
        drop((r, w));
        // retry on a fresh connection succeeds
        let (reply, _) = tokio::join!(device.write_read(&request), echo_once(&l));
        assert_eq!(reply.unwrap(), REQUEST);
    }

    #[tokio::test]
    async fn device_write_read_reconnect_fails() {
        init();
        let (l, addr) = listener().await;
        let (mut device, server) = connected_device(&l, &addr).await;
        drop(server);
        drop(l);
        assert!(device.write_read(&REQUEST.to_vec()).await.is_err());
        assert!(!device.is_connected());
    }

    #[tokio::test]
    async fn device_read_timeout() {
        init();
        let (l, addr) = listener().await;
        let (mut device, _server) = connected_device(&l, &addr).await;
        let err = device.raw_write_read(&REQUEST.to_vec()).await.unwrap_err();
        assert!(err.is::<Elapsed>());
    }

    #[tokio::test]
    async fn device_write_timeout() {
        let (l, addr) = listener().await;
        let (mut device, _server) = connected_device(&l, &addr).await;
        // fill the socket buffers: the peer never reads
        let (_, writer) = device.stream.as_mut().unwrap();
        while writer.get_mut().try_write(&[0u8; 65536]).is_ok() {}
        let err = device.raw_write_read(&REQUEST.to_vec()).await.unwrap_err();
        assert!(err.is::<Elapsed>());
    }

    #[tokio::test]
    async fn device_connect_timeout() {
        // backlog 0 with one queued connection: further SYNs are dropped
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let l = socket.listen(0).unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let _queued = TcpStream::connect(&addr).await.unwrap();
        let err = Device::new(&addr).connect().await.unwrap_err();
        assert!(err.is::<Elapsed>());
    }

    /// Connected device whose peer has reset the connection.
    async fn reset_device() -> Device {
        let (l, addr) = listener().await;
        let (device, server) = connected_device(&l, &addr).await;
        server.set_zero_linger().unwrap();
        drop(server);
        sleep(Duration::from_millis(50)).await;
        device
    }

    #[tokio::test]
    async fn device_write_errors() {
        // small frame: buffered, fails on flush
        let mut device = reset_device().await;
        assert!(device.raw_write_read(&REQUEST.to_vec()).await.is_err());
        // frame larger than the write buffer: fails on write
        let mut device = reset_device().await;
        assert!(device.raw_write_read(&frame(20_000)).await.is_err());
    }

    #[tokio::test]
    async fn device_handle_packet() {
        init();
        let (l, addr) = listener().await;
        let mut device = Device::new(&addr);
        // reply delivered
        let (tx, rx) = oneshot::channel();
        let (result, _) = tokio::join!(device.handle_packet(REQUEST.to_vec(), tx), echo_once(&l));
        result.unwrap();
        assert_eq!(rx.await.unwrap(), REQUEST);
        // client went away before the reply
        device.disconnect();
        let (tx, rx) = oneshot::channel();
        drop(rx);
        let (result, _) = tokio::join!(device.handle_packet(REQUEST.to_vec(), tx), echo_once(&l));
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("error sending reply"));
        // device failure
        drop(l);
        device.disconnect();
        let (tx, _rx) = oneshot::channel();
        assert!(device.handle_packet(REQUEST.to_vec(), tx).await.is_err());
    }

    #[tokio::test]
    async fn device_run_counts_clients() {
        init();
        let (l, addr) = listener().await;
        let (tx, mut rx) = mpsc::channel(8);
        let run = Device::launch(&addr, &mut rx);
        let script = async {
            tx.send(Message::Connection).await.unwrap();
            tx.send(Message::Connection).await.unwrap();
            let (reply_tx, reply_rx) = oneshot::channel();
            tx.send(Message::Packet(REQUEST.to_vec(), reply_tx))
                .await
                .unwrap();
            echo_once(&l).await;
            assert_eq!(reply_rx.await.unwrap(), REQUEST);
            // failing packet (reply receiver gone) disconnects the device
            let (reply_tx, reply_rx) = oneshot::channel();
            drop(reply_rx);
            tx.send(Message::Packet(REQUEST.to_vec(), reply_tx))
                .await
                .unwrap();
            echo_once(&l).await;
            tx.send(Message::Disconnection).await.unwrap();
            tx.send(Message::Disconnection).await.unwrap();
            drop(tx);
        };
        tokio::join!(run, script);
    }

    fn bridge(bind: &str, url: &str) -> Bridge {
        Bridge {
            listen: Listen {
                bind: bind.to_string(),
            },
            modbus: Modbus {
                url: url.to_string(),
            },
        }
    }

    #[tokio::test]
    async fn bridge_bind_error() {
        init();
        let (_l, addr) = listener().await;
        assert!(bridge(&addr, "unused").run().await.is_err());
    }

    #[tokio::test]
    async fn bridge_proxies_requests() {
        init();
        let (dev, dev_addr) = listener().await;
        let bind = closed_addr().await;
        let mut b = bridge(&bind, &dev_addr);
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
        let (result, _) = tokio::join!(timeout(Duration::from_millis(500), b.run()), clients);
        assert!(result.is_err());
        // the bridge is gone: give its device task time to stop
        sleep(Duration::from_millis(50)).await;
    }

    #[tokio::test]
    async fn bridge_accept_error_is_retried() {
        init();
        let b = bridge("test", "unused");
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
        assert!(
            timeout(Duration::from_millis(100), b.serve(&mut accept, tx))
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn handle_client_channel_closed() {
        init();
        let (_c, server) = pair().await;
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        assert!(Bridge::handle_client(server, tx).await.is_err());
    }

    #[tokio::test]
    async fn handle_client_channel_full() {
        let (_c, server) = pair().await;
        let (tx, _rx) = mpsc::channel(1);
        tx.send(Message::Connection).await.unwrap();
        assert!(Bridge::handle_client(server, tx).await.is_err());
    }

    #[tokio::test]
    async fn client_loop_send_errors() {
        // channel closed
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        assert!(Bridge::client_loop(server, &tx).await.is_err());
        // channel full
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (tx, _rx) = mpsc::channel(1);
        tx.send(Message::Connection).await.unwrap();
        assert!(Bridge::client_loop(server, &tx).await.is_err());
    }

    #[tokio::test]
    async fn client_loop_reply_errors() {
        // reply sender dropped
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        let (result, _) = tokio::join!(Bridge::client_loop(server, &tx), async {
            drop(rx.recv().await);
        });
        assert!(result.unwrap_err().is::<oneshot::error::RecvError>());
        // reply never comes
        let (mut c, server) = pair().await;
        c.write_all(&REQUEST).await.unwrap();
        let (result, _msg) = tokio::join!(Bridge::client_loop(server, &tx), rx.recv());
        assert!(result.unwrap_err().is::<tokio::time::error::Elapsed>());
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
            let result = Bridge::client_loop(server, &tx).await;
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
        let (result, _, _) = tokio::join!(client, Device::launch(&addr, &mut rx), device);
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

    fn config_file(name: &str, bind: &str) -> String {
        let path = std::env::temp_dir().join(format!(
            "modbus-proxy-test-{}-{}.yml",
            name,
            std::process::id()
        ));
        let yaml = format!(
            "devices:\n- modbus: {{url: \"127.0.0.1:1\"}}\n  listen: {{bind: \"{}\"}}\n",
            bind
        );
        std::fs::write(&path, yaml).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[tokio::test]
    async fn server_config_error() {
        assert!(Server::launch("/nonexistent/modbus-proxy.yml")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn server_stops_when_all_bridges_fail() {
        init();
        let _guard = SIGNALS.lock().await;
        let (_l, addr) = listener().await;
        let path = config_file("fail", &addr);
        Server::launch(&path).await.unwrap();
        std::fs::remove_file(path).unwrap();
    }

    async fn server_stops_on_signal(name: &str, kind: SignalKind) {
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
    async fn server_stops_on_sigint() {
        server_stops_on_signal("INT", SignalKind::interrupt()).await;
    }

    #[tokio::test]
    async fn server_stops_on_sigterm() {
        server_stops_on_signal("TERM", SignalKind::terminate()).await;
    }
}
