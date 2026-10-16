//! Modbus TCP wire format and socket helpers shared by both proxy sides.

use tokio::io::{AsyncReadExt, BufReader, BufWriter};
use tokio::net::TcpStream;

use crate::Result;

/// A Modbus TCP frame: 6 byte MBAP header (payload length in bytes 4..6)
/// followed by the payload.
pub(crate) type Frame = Vec<u8>;

pub(crate) type TcpReader = BufReader<tokio::net::tcp::OwnedReadHalf>;
pub(crate) type TcpWriter = BufWriter<tokio::net::tcp::OwnedWriteHalf>;

fn frame_size(header: &[u8; 6]) -> usize {
    u16::from_be_bytes([header[4], header[5]]) as usize
}

/// Disable Nagle for lower latency. Best effort: it can only fail on an
/// invalid socket, and the proxy still works without it.
pub(crate) fn set_nodelay(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
}

pub(crate) fn split_connection(stream: TcpStream) -> (TcpReader, TcpWriter) {
    let (reader, writer) = stream.into_split();
    (BufReader::new(reader), BufWriter::new(writer))
}

pub(crate) async fn read_frame(stream: &mut TcpReader) -> Result<Frame> {
    let mut header = [0u8; 6];
    stream.read_exact(&mut header).await?;
    let mut buf = header.to_vec();
    buf.resize(6 + frame_size(&header), 0);
    stream.read_exact(&mut buf[6..]).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::*;
    use tokio::io::AsyncWriteExt;

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
}
