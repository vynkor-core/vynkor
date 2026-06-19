#![allow(dead_code)]

use crate::utils::errors::VeyronError;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_PAYLOAD_SIZE: usize = 1_048_576;
const MAGIC: u16 = 0x5652;
const HEADER_SIZE: usize = 44;

#[derive(Debug, Clone)]
pub struct Frame {
    pub magic: u16,
    pub flags: u16,
    pub length: u32,
    pub target: [u8; 32],
    pub crc32: u32,
    pub payload: Vec<u8>,
}

pub async fn write_frame<W>(
    stream: &mut W,
    target: &str,
    flags: u16,
    payload: &[u8],
) -> Result<(), VeyronError>
where
    W: AsyncWrite + Unpin,
{
    if payload.len() > MAX_PAYLOAD_SIZE {
        return Err(VeyronError::PayloadTooLarge(payload.len()));
    }

    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&MAGIC.to_be_bytes());
    header[2..4].copy_from_slice(&flags.to_be_bytes());
    header[4..8].copy_from_slice(&(payload.len() as u32).to_be_bytes());

    let target_bytes = target.as_bytes();
    let copy_len = target_bytes.len().min(32);
    header[8..8 + copy_len].copy_from_slice(&target_bytes[..copy_len]);

    let checksum = crc32fast::hash(payload);
    header[40..44].copy_from_slice(&checksum.to_be_bytes());

    stream.write_all(&header).await?;
    stream.write_all(payload).await?;
    Ok(())
}

pub async fn write_frame_raw<W>(stream: &mut W, frame: &Frame) -> Result<(), VeyronError>
where
    W: AsyncWrite + Unpin,
{
    let mut header = [0u8; HEADER_SIZE];
    header[0..2].copy_from_slice(&frame.magic.to_be_bytes());
    header[2..4].copy_from_slice(&frame.flags.to_be_bytes());
    header[4..8].copy_from_slice(&frame.length.to_be_bytes());
    header[8..40].copy_from_slice(&frame.target);
    header[40..44].copy_from_slice(&frame.crc32.to_be_bytes());

    stream.write_all(&header).await?;
    stream.write_all(&frame.payload).await?;
    Ok(())
}

pub async fn read_frame<R>(stream: &mut R) -> Result<Frame, VeyronError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; HEADER_SIZE];
    stream.read_exact(&mut header).await?;

    let magic = u16::from_be_bytes([header[0], header[1]]);
    if magic != MAGIC {
        return Err(VeyronError::FrameMagicMismatch);
    }

    let flags = u16::from_be_bytes([header[2], header[3]]);
    let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);

    if length as usize > MAX_PAYLOAD_SIZE {
        return Err(VeyronError::PayloadTooLarge(length as usize));
    }

    let mut target = [0u8; 32];
    target.copy_from_slice(&header[8..40]);

    let crc32 = u32::from_be_bytes([header[40], header[41], header[42], header[43]]);

    let mut payload = vec![0u8; length as usize];
    if length > 0 {
        stream.read_exact(&mut payload).await?;
    }

    let computed = crc32fast::hash(&payload);
    if computed != crc32 {
        return Err(VeyronError::FrameCrcMismatch);
    }

    Ok(Frame {
        magic,
        flags,
        length,
        target,
        crc32,
        payload,
    })
}

pub fn target_as_str(frame: &Frame) -> &str {
    let end = frame.target.iter().position(|&b| b == 0).unwrap_or(32);
    std::str::from_utf8(&frame.target[..end]).unwrap_or("")
}
