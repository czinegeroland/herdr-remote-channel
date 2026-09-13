//! Length-prefixed JSON framing.
//!
//! A stream socket gives no message boundaries, so something has to supply
//! them. A four-byte big-endian length does, and it makes a short read
//! unambiguous: either the length is complete or it is not, and either the
//! body is complete or it is not.
//!
//! The length is checked against [`MAX_FRAME_BYTES`] *before* the buffer is
//! allocated. Reading the length and then calling `with_capacity` on it would
//! let any local process make the daemon reserve as much memory as it liked,
//! which is a denial of service that costs the attacker four bytes.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

use crate::{IpcError, Result};

/// The largest frame either side will read or write.
///
/// Local RPC carries requests and metadata, not message payloads: a pending
/// body reaches the trusted interface, and one megabyte is well above the
/// largest of those while staying far below anything worth worrying about.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Writes one value as a length-prefixed JSON frame.
pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize + ?Sized,
{
    let body = serde_json::to_vec(value).map_err(|error| IpcError::Malformed(error.to_string()))?;

    if body.len() > MAX_FRAME_BYTES {
        return Err(IpcError::FrameTooLarge {
            declared: body.len(),
            limit: MAX_FRAME_BYTES,
        });
    }

    // One write for the header and one for the body would let a reader
    // observe a header with no body if the writer died between them. It is
    // still a partial frame either way, but a single buffer keeps the
    // common case atomic at the socket layer.
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);

    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads one length-prefixed JSON frame.
///
/// Returns [`IpcError::Disconnected`] when the peer closed cleanly between
/// frames, which is an ordinary end of conversation rather than a failure.
pub async fn read_frame<R, T>(reader: &mut R) -> Result<T>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::BrokenPipe
            ) =>
        {
            return Err(IpcError::Disconnected);
        }
        Err(error) => return Err(error.into()),
    }

    let declared = u32::from_be_bytes(header) as usize;
    if declared > MAX_FRAME_BYTES {
        // Before the allocation, deliberately.
        return Err(IpcError::FrameTooLarge {
            declared,
            limit: MAX_FRAME_BYTES,
        });
    }

    let mut body = vec![0u8; declared];
    match reader.read_exact(&mut body).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            // A truncated body is a broken frame, not a clean close: the
            // peer promised bytes it did not send.
            return Err(IpcError::Malformed(format!(
                "frame declared {declared} bytes and ended early"
            )));
        }
        Err(error) => return Err(error.into()),
    }

    serde_json::from_slice(&body).map_err(|error| IpcError::Malformed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde::Deserialize;

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct Ping {
        method: String,
        size: u32,
    }

    fn ping() -> Ping {
        Ping {
            method: "status".into(),
            size: 7,
        }
    }

    #[tokio::test]
    async fn a_frame_round_trips() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &ping()).await.unwrap();

        let mut reader = buffer.as_slice();
        assert_eq!(read_frame::<_, Ping>(&mut reader).await.unwrap(), ping());
    }

    #[tokio::test]
    async fn frames_do_not_run_together() {
        // The property length prefixing exists for: two frames in one buffer
        // must read back as two values, not as one concatenated parse error.
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &ping()).await.unwrap();
        write_frame(&mut buffer, &ping()).await.unwrap();

        let mut reader = buffer.as_slice();
        read_frame::<_, Ping>(&mut reader).await.unwrap();
        read_frame::<_, Ping>(&mut reader).await.unwrap();
        assert!(matches!(
            read_frame::<_, Ping>(&mut reader).await.unwrap_err(),
            IpcError::Disconnected
        ));
    }

    #[tokio::test]
    async fn an_oversized_declared_length_is_refused_before_allocating() {
        // Four bytes of header claiming four gigabytes. The reader must not
        // try to satisfy it.
        let mut frame = (u32::MAX).to_be_bytes().to_vec();
        frame.extend_from_slice(b"{}");

        let mut reader = frame.as_slice();
        let error = read_frame::<_, Ping>(&mut reader).await.unwrap_err();

        assert!(matches!(
            error,
            IpcError::FrameTooLarge {
                declared,
                limit: MAX_FRAME_BYTES
            } if declared == u32::MAX as usize
        ));
    }

    #[tokio::test]
    async fn a_truncated_body_is_a_broken_frame_not_a_clean_close() {
        // Distinguishing these matters: a clean close ends the loop, and a
        // truncated frame means something is wrong with the peer.
        let mut frame = 64u32.to_be_bytes().to_vec();
        frame.extend_from_slice(b"{\"method\":\"status\"");

        let mut reader = frame.as_slice();
        assert!(matches!(
            read_frame::<_, Ping>(&mut reader).await.unwrap_err(),
            IpcError::Malformed(_)
        ));
    }

    #[tokio::test]
    async fn a_truncated_header_is_a_clean_close() {
        let mut reader = [0u8, 0u8].as_slice();
        assert!(matches!(
            read_frame::<_, Ping>(&mut reader).await.unwrap_err(),
            IpcError::Disconnected
        ));
    }

    #[tokio::test]
    async fn a_body_that_is_not_the_expected_shape_is_malformed() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &serde_json::json!({ "unrelated": true }))
            .await
            .unwrap();

        let mut reader = buffer.as_slice();
        assert!(matches!(
            read_frame::<_, Ping>(&mut reader).await.unwrap_err(),
            IpcError::Malformed(_)
        ));
    }

    #[tokio::test]
    async fn writing_an_oversized_value_is_refused() {
        let huge = Ping {
            method: "x".repeat(MAX_FRAME_BYTES + 1),
            size: 0,
        };

        let mut buffer = Vec::new();
        assert!(matches!(
            write_frame(&mut buffer, &huge).await.unwrap_err(),
            IpcError::FrameTooLarge { .. }
        ));
        assert!(buffer.is_empty(), "a refused frame must write nothing");
    }
}
