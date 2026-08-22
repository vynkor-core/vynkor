use crate::utils::errors::VynkorError;
use tokio::io::{AsyncRead, AsyncWrite};

pub use vynkor_wire::framing::{
    parse_frag_header, serialize_header, target_as_str, FragmentHeader, Frame, COMPRESS_THRESHOLD,
    FLAG_COMPRESSED, FLAG_FRAGMENTED, FLAG_MAC_PRESENT, FLAG_RAW_BINARY, FRAG_HEADER_SIZE,
    MAX_PAYLOAD_SIZE,
};

pub async fn write_frame<W>(
    stream: &mut W,
    target: &str,
    flags: u16,
    payload: &[u8],
) -> Result<(), VynkorError>
where
    W: AsyncWrite + Unpin,
{
    vynkor_wire::framing::write_frame(stream, target, flags, payload)
        .await
        .map_err(Into::into)
}

pub async fn write_frame_raw<W>(stream: &mut W, frame: &Frame) -> Result<(), VynkorError>
where
    W: AsyncWrite + Unpin,
{
    vynkor_wire::framing::write_frame_raw(stream, frame)
        .await
        .map_err(Into::into)
}

pub async fn read_frame<R>(stream: &mut R) -> Result<Frame, VynkorError>
where
    R: AsyncRead + Unpin,
{
    vynkor_wire::framing::read_frame(stream)
        .await
        .map_err(Into::into)
}

pub async fn read_frame_with_timeout<R>(
    stream: &mut R,
    timeout: std::time::Duration,
) -> Result<Frame, VynkorError>
where
    R: AsyncRead + Unpin,
{
    vynkor_wire::framing::read_frame_with_timeout(stream, timeout)
        .await
        .map_err(Into::into)
}
