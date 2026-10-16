//! Fixtures shared by the unit tests of all modules.

use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

use crate::frame::{read_frame, split_connection, Frame};

/// Read holding register 0 of unit 1.
pub(crate) const REQUEST: [u8; 12] = [0, 1, 0, 0, 0, 6, 1, 3, 0, 0, 0, 1];

/// Log everything, so the arguments of every log statement get evaluated.
pub(crate) fn init() {
    let _ = env_logger::builder()
        .is_test(true)
        .filter_level(log::LevelFilter::Trace)
        .try_init();
}

/// Frame announcing `len` payload bytes.
pub(crate) fn frame(len: usize) -> Frame {
    let mut f = vec![0, 1, 0, 0];
    f.extend((len as u16).to_be_bytes());
    f.resize(6 + len, 7);
    f
}

pub(crate) async fn listener() -> (TcpListener, String) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap().to_string();
    (l, addr)
}

/// Address nothing listens on.
pub(crate) async fn closed_addr() -> String {
    listener().await.1
}

/// Connected (client, server) socket pair.
pub(crate) async fn pair() -> (TcpStream, TcpStream) {
    let (l, addr) = listener().await;
    let client = TcpStream::connect(addr).await.unwrap();
    let (server, _) = l.accept().await.unwrap();
    (client, server)
}

/// Device answering one request with an echo on one accepted connection.
pub(crate) async fn echo_once(l: &TcpListener) {
    let (stream, _) = l.accept().await.unwrap();
    let (mut r, mut w) = split_connection(stream);
    let f = read_frame(&mut r).await.unwrap();
    w.write_all(&f).await.unwrap();
    w.flush().await.unwrap();
}

/// Config file with one device (unreachable modbus url) listening on `bind`.
pub(crate) fn config_file(name: &str, bind: &str) -> String {
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
