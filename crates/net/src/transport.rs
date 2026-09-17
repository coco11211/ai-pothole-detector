//! TCP framing.
//!
//! The wire is a stream of length-prefixed frames: a four-byte big-endian
//! length followed by that many bytes of message. Nothing more — no
//! compression, no encryption, no multiplexing.
//!
//! The length prefix is the whole security surface of this module. A peer
//! controls it, so it is checked against [`MAX_FRAME_BYTES`] *before* a buffer
//! is allocated. Reading the length and then allocating it is the classic
//! way to let a peer exhaust memory with four bytes of effort.
//!
//! This module deliberately does not know what a message means. It moves
//! bytes; [`crate::sync::DagSync`] decides what they are and what to do about
//! them.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Largest frame accepted, in bytes.
///
/// Sixteen megabytes. Above the largest legitimate block (a full body at the
/// 30M gas limit is well under this) and far below anything that would let a
/// peer exhaust memory. A peer that exceeds it is disconnected rather than
/// served.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Bytes in the length prefix.
const LENGTH_PREFIX_BYTES: usize = 4;

/// Writes one length-prefixed frame.
pub async fn write_frame<W>(writer: &mut W, payload: &[u8]) -> Result<(), FrameError>
where
    W: AsyncWriteExt + Unpin,
{
    let length = u32::try_from(payload.len())
        .map_err(|_| FrameError::TooLarge { length: u64::MAX, limit: MAX_FRAME_BYTES })?;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { length: u64::from(length), limit: MAX_FRAME_BYTES });
    }

    // One write call for the whole frame. Writing the prefix separately would
    // let a cancelled task leave a header with no body on the wire, which the
    // peer cannot distinguish from a stall.
    let mut framed = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    framed.extend_from_slice(&length.to_be_bytes());
    framed.extend_from_slice(payload);

    writer.write_all(&framed).await.map_err(FrameError::Io)?;
    writer.flush().await.map_err(FrameError::Io)?;
    Ok(())
}

/// Reads one length-prefixed frame.
///
/// Returns `Ok(None)` on a clean end of stream, which is a peer disconnecting
/// rather than an error.
pub async fn read_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>, FrameError>
where
    R: AsyncReadExt + Unpin,
{
    let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
    match reader.read_exact(&mut prefix).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(FrameError::Io(e)),
    }

    let length = u32::from_be_bytes(prefix);
    // Checked BEFORE allocating. This is the whole point of the module.
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { length: u64::from(length), limit: MAX_FRAME_BYTES });
    }
    if length == 0 {
        return Err(FrameError::Empty);
    }

    let mut payload = vec![0u8; length as usize];
    match reader.read_exact(&mut payload).await {
        Ok(_) => Ok(Some(payload)),
        // A truncated body is a broken peer, not a clean disconnect: it
        // promised bytes it did not send.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            Err(FrameError::Truncated { expected: length })
        }
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// Reasons a frame could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// Underlying socket error.
    #[error("transport io error: {0}")]
    Io(#[source] std::io::Error),
    /// The frame exceeds the protocol limit.
    #[error("frame of {length} bytes exceeds the {limit} byte limit")]
    TooLarge {
        /// Length the peer declared.
        length: u64,
        /// The limit.
        limit: u32,
    },
    /// A zero-length frame. There is no message with no bytes, so this is
    /// either a bug or an attempt to spin the read loop for free.
    #[error("zero-length frame")]
    Empty,
    /// The peer declared more bytes than it sent.
    #[error("truncated frame: {expected} bytes promised, stream ended early")]
    Truncated {
        /// Length the peer declared.
        expected: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_frame_roundtrips() {
        let payload = b"hello chainname".to_vec();
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &payload).await.unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        let read = read_frame(&mut cursor).await.unwrap();
        assert_eq!(read, Some(payload));
    }

    #[tokio::test]
    async fn several_frames_roundtrip_in_order() {
        let mut buffer = Vec::new();
        for payload in [&b"one"[..], &b"two"[..], &b"three"[..]] {
            write_frame(&mut buffer, payload).await.unwrap();
        }

        let mut cursor = std::io::Cursor::new(buffer);
        assert_eq!(read_frame(&mut cursor).await.unwrap().unwrap(), b"one");
        assert_eq!(read_frame(&mut cursor).await.unwrap().unwrap(), b"two");
        assert_eq!(read_frame(&mut cursor).await.unwrap().unwrap(), b"three");
        assert_eq!(read_frame(&mut cursor).await.unwrap(), None, "clean end of stream");
    }

    #[tokio::test]
    async fn an_empty_stream_is_a_clean_disconnect() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        assert_eq!(read_frame(&mut cursor).await.unwrap(), None);
    }

    #[tokio::test]
    async fn an_oversized_length_prefix_is_refused_without_allocating() {
        // Four bytes claiming four gigabytes. Allocating before checking is
        // how a peer exhausts memory for free.
        let mut bytes = u32::MAX.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"nothing like that much follows");

        let mut cursor = std::io::Cursor::new(bytes);
        assert!(matches!(read_frame(&mut cursor).await, Err(FrameError::TooLarge { .. })));
    }

    #[tokio::test]
    async fn a_zero_length_frame_is_refused() {
        let mut cursor = std::io::Cursor::new(0u32.to_be_bytes().to_vec());
        assert!(matches!(read_frame(&mut cursor).await, Err(FrameError::Empty)));
    }

    #[tokio::test]
    async fn a_truncated_body_is_an_error_not_a_disconnect() {
        // The peer promised more than it sent. Treating this as a clean
        // disconnect would silently drop a message.
        let mut bytes = 100u32.to_be_bytes().to_vec();
        bytes.extend_from_slice(b"only a few bytes");

        let mut cursor = std::io::Cursor::new(bytes);
        assert!(matches!(
            read_frame(&mut cursor).await,
            Err(FrameError::Truncated { expected: 100 })
        ));
    }

    #[tokio::test]
    async fn writing_an_oversized_payload_is_refused() {
        let mut buffer = Vec::new();
        let huge = vec![0u8; MAX_FRAME_BYTES as usize + 1];
        assert!(matches!(write_frame(&mut buffer, &huge).await, Err(FrameError::TooLarge { .. })));
        assert!(buffer.is_empty(), "nothing may be written when the frame is refused");
    }

    #[tokio::test]
    async fn a_maximum_size_frame_is_accepted() {
        let mut buffer = Vec::new();
        let payload = vec![7u8; MAX_FRAME_BYTES as usize];
        write_frame(&mut buffer, &payload).await.unwrap();

        let mut cursor = std::io::Cursor::new(buffer);
        assert_eq!(read_frame(&mut cursor).await.unwrap().unwrap().len(), payload.len());
    }
}
