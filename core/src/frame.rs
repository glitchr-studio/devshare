//! Control messages on a stream: a 4-byte big-endian length, then JSON.

use anyhow::{bail, Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub async fn write<W, T>(writer: &mut W, message: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let body = serde_json::to_vec(message)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    writer
        .write_all(&frame)
        .await
        .context("writing a message")?;
    Ok(())
}

pub async fn read<R, T>(reader: &mut R, max: usize) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut length = [0u8; 4];
    reader
        .read_exact(&mut length)
        .await
        .context("reading a message")?;
    let length = u32::from_be_bytes(length) as usize;
    if length > max {
        bail!("message of {length} bytes exceeds the limit of {max}");
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .await
        .context("reading a message")?;
    serde_json::from_slice(&body).context("decoding a message")
}

#[cfg(test)]
mod tests {
    use devshare_protocol::Open;

    #[tokio::test]
    async fn round_trips_and_enforces_the_limit() {
        let mut wire = Vec::new();
        let open = Open {
            host: "shop.test".into(),
            port: 443,
        };
        super::write(&mut wire, &open).await.unwrap();

        let back: Open = super::read(&mut wire.as_slice(), 4096).await.unwrap();
        assert_eq!((back.host.as_str(), back.port), ("shop.test", 443));

        let refused = super::read::<_, Open>(&mut wire.as_slice(), 8).await;
        assert!(refused.is_err());
    }
}
