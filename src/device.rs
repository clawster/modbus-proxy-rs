//! The modbus device side: a single task owns the device connection and
//! serves requests from all clients, one at a time, through a channel.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use crate::frame::{read_frame, set_nodelay, split_connection, Frame, TcpReader, TcpWriter};
use crate::registers::RegisterMap;
use crate::settings::Modbus;
use crate::timeouts::DEVICE_IO_TIMEOUT;
use crate::Result;

pub(crate) type ReplySender = oneshot::Sender<Frame>;

/// Requests from client tasks to the device task, with the client address.
#[derive(Debug)]
pub(crate) enum Message {
    Connection(SocketAddr),
    Disconnection(SocketAddr),
    Packet(Frame, SocketAddr, ReplySender),
}

pub(crate) type ChannelRx = mpsc::Receiver<Message>;
pub(crate) type ChannelTx = mpsc::Sender<Message>;

async fn create_connection(url: &str) -> Result<(TcpReader, TcpWriter)> {
    let stream = timeout(DEVICE_IO_TIMEOUT, TcpStream::connect(url)).await??;
    set_nodelay(&stream);
    Ok(split_connection(stream))
}

pub(crate) struct Device {
    url: String,
    /// Name in the register log
    name: String,
    registers: Option<Arc<RegisterMap>>,
    stream: Option<(TcpReader, TcpWriter)>,
}

impl Device {
    pub(crate) fn new(url: &str, registers: Option<Arc<RegisterMap>>) -> Device {
        Device {
            url: url.to_string(),
            name: url.to_string(),
            registers,
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

    async fn handle_packet(
        &mut self,
        frame: Frame,
        client: SocketAddr,
        channel: ReplySender,
    ) -> Result<()> {
        info!("modbus request {}: {} bytes", self.url, frame.len());
        debug!("modbus request {}: {:?}", self.url, &frame[..]);
        let reply = self.write_read(&frame).await?;
        info!("modbus reply {}: {} bytes", self.url, reply.len());
        debug!("modbus reply {}: {:?}", self.url, &reply[..]);
        if let Some(registers) = &self.registers {
            for line in registers.describe(&frame, &reply) {
                info!(target: "modbus_proxy_rs::registers", "{} -> {}: {}", client, self.name, line);
            }
        }
        channel
            .send(reply)
            .map_err(|error| format!("error sending reply to client: {:?}", error).into())
    }

    async fn run(&mut self, channel: &mut ChannelRx) {
        let mut nb_clients = 0;

        while let Some(message) = channel.recv().await {
            match message {
                Message::Connection(client) => {
                    nb_clients += 1;
                    info!(
                        "new client connection from {} (active = {})",
                        client, nb_clients
                    );
                }
                Message::Disconnection(client) => {
                    nb_clients -= 1;
                    info!("client {} disconnected (active = {})", client, nb_clients);
                    if nb_clients == 0 {
                        info!("disconnecting from modbus at {} (no clients)", self.url);
                        self.disconnect();
                    }
                }
                Message::Packet(frame, client, channel) => {
                    if self.handle_packet(frame, client, channel).await.is_err() {
                        self.disconnect();
                    }
                }
            }
        }
    }

    /// Serve requests until every sender of `channel` is dropped.
    pub(crate) async fn launch(
        config: &Modbus,
        registers: Option<Arc<RegisterMap>>,
        channel: &mut ChannelRx,
    ) {
        let mut modbus = Self::new(&config.url, registers);
        if let Some(name) = &config.name {
            modbus.name = name.clone();
        }
        modbus.run(channel).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpSocket};
    use tokio::time::error::Elapsed;
    use tokio::time::sleep;

    async fn connected_device(l: &TcpListener, addr: &str) -> (Device, TcpStream) {
        let mut device = Device::new(addr, None);
        device.connect().await.unwrap();
        let (server, _) = l.accept().await.unwrap();
        (device, server)
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
    async fn connect_refused() {
        init();
        let mut device = Device::new(&closed_addr().await, None);
        assert!(device.connect().await.is_err());
        assert!(!device.is_connected());
        assert!(device.write_read(&REQUEST.to_vec()).await.is_err());
    }

    #[tokio::test]
    async fn connect_timeout() {
        // backlog 0 with one queued connection: further SYNs are dropped
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let l = socket.listen(0).unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let _queued = TcpStream::connect(&addr).await.unwrap();
        let err = Device::new(&addr, None).connect().await.unwrap_err();
        assert!(err.is::<Elapsed>());
    }

    #[tokio::test]
    async fn raw_write_read_not_connected() {
        let mut device = Device::new("unused", None);
        let err = device.raw_write_read(&REQUEST.to_vec()).await.unwrap_err();
        assert_eq!(err.to_string(), "no modbus connection");
    }

    #[tokio::test]
    async fn write_read_connects_and_reuses() {
        init();
        let (l, addr) = listener().await;
        let mut device = Device::new(&addr, None);
        let request = REQUEST.to_vec();
        let (reply, _) = tokio::join!(device.write_read(&request), echo_once(&l));
        assert_eq!(reply.unwrap(), REQUEST);
        assert!(device.is_connected());
        device.disconnect();
        assert!(!device.is_connected());
    }

    #[tokio::test]
    async fn write_read_reconnects_after_error() {
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
    async fn write_read_reconnect_fails() {
        init();
        let (l, addr) = listener().await;
        let (mut device, server) = connected_device(&l, &addr).await;
        drop(server);
        drop(l);
        assert!(device.write_read(&REQUEST.to_vec()).await.is_err());
        assert!(!device.is_connected());
    }

    #[tokio::test]
    async fn read_timeout() {
        init();
        let (l, addr) = listener().await;
        let (mut device, _server) = connected_device(&l, &addr).await;
        let err = device.raw_write_read(&REQUEST.to_vec()).await.unwrap_err();
        assert!(err.is::<Elapsed>());
    }

    #[tokio::test]
    async fn write_timeout() {
        let (l, addr) = listener().await;
        let (mut device, _server) = connected_device(&l, &addr).await;
        // fill the socket buffers: the peer never reads
        let (_, writer) = device.stream.as_mut().unwrap();
        while writer.get_mut().try_write(&[0u8; 65536]).is_ok() {}
        let err = device.raw_write_read(&REQUEST.to_vec()).await.unwrap_err();
        assert!(err.is::<Elapsed>());
    }

    #[tokio::test]
    async fn write_errors() {
        // small frame: buffered, fails on flush
        let mut device = reset_device().await;
        assert!(device.raw_write_read(&REQUEST.to_vec()).await.is_err());
        // frame larger than the write buffer: fails on write
        let mut device = reset_device().await;
        assert!(device.raw_write_read(&frame(20_000)).await.is_err());
    }

    #[tokio::test]
    async fn handle_packet() {
        init();
        let (l, addr) = listener().await;
        let mut device = Device::new(&addr, None);
        // reply delivered
        let (tx, rx) = oneshot::channel();
        let (result, _) = tokio::join!(
            device.handle_packet(REQUEST.to_vec(), peer(), tx),
            echo_once(&l)
        );
        result.unwrap();
        assert_eq!(rx.await.unwrap(), REQUEST);
        // client went away before the reply
        device.disconnect();
        let (tx, rx) = oneshot::channel();
        drop(rx);
        let (result, _) = tokio::join!(
            device.handle_packet(REQUEST.to_vec(), peer(), tx),
            echo_once(&l)
        );
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("error sending reply"));
        // device failure
        drop(l);
        device.disconnect();
        let (tx, _rx) = oneshot::channel();
        assert!(device
            .handle_packet(REQUEST.to_vec(), peer(), tx)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn handle_packet_describes_registers() {
        init();
        let (l, addr) = listener().await;
        let map = RegisterMap::load(SIGENERGY_MAP).unwrap();
        let mut device = Device::new(&addr, Some(Arc::new(map)));
        // write plant_start_stop = 1, echoed back by the device
        let request = vec![0, 1, 0, 0, 0, 6, 247, 6, 0x9c, 0x40, 0, 1];
        let (tx, rx) = oneshot::channel();
        let (result, _) = tokio::join!(
            device.handle_packet(request.clone(), peer(), tx),
            echo_once(&l)
        );
        result.unwrap();
        assert_eq!(rx.await.unwrap(), request);
    }

    #[tokio::test]
    async fn run_counts_clients() {
        init();
        let (l, addr) = listener().await;
        let (tx, mut rx) = mpsc::channel(8);
        let config = modbus(&addr, Some("plant"));
        let run = Device::launch(&config, None, &mut rx);
        let script = async {
            tx.send(Message::Connection(peer())).await.unwrap();
            tx.send(Message::Connection(peer())).await.unwrap();
            let (reply_tx, reply_rx) = oneshot::channel();
            tx.send(Message::Packet(REQUEST.to_vec(), peer(), reply_tx))
                .await
                .unwrap();
            echo_once(&l).await;
            assert_eq!(reply_rx.await.unwrap(), REQUEST);
            // failing packet (reply receiver gone) disconnects the device
            let (reply_tx, reply_rx) = oneshot::channel();
            drop(reply_rx);
            tx.send(Message::Packet(REQUEST.to_vec(), peer(), reply_tx))
                .await
                .unwrap();
            echo_once(&l).await;
            tx.send(Message::Disconnection(peer())).await.unwrap();
            tx.send(Message::Disconnection(peer())).await.unwrap();
            drop(tx);
        };
        tokio::join!(run, script);
    }
}
