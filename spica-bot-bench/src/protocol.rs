//! Low-level Minecraft 1.21.4 (Protocol 769) TCP packet framing, VarInt codec,
//! and zlib threshold compression.

use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::io::{self, Read, Write};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION_1_21_4: i32 = 769;
const MAX_PACKET_SIZE: usize = 8 * 1024 * 1024;

/// Encodes a signed 32-bit Minecraft VarInt into `out`.
pub fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut v = value as u32;
    loop {
        let mut byte = (v & 0x7F) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            break;
        }
    }
}

/// Decodes a VarInt from a byte slice, returning `(value, bytes_consumed)`.
pub fn read_varint_slice(buf: &[u8]) -> io::Result<(i32, usize)> {
    let mut num_read = 0usize;
    let mut result = 0u32;
    loop {
        if num_read >= 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "VarInt too big (>5 bytes)",
            ));
        }
        let &byte = buf
            .get(num_read)
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "Truncated VarInt"))?;
        result |= ((byte & 0x7F) as u32) << (7 * num_read);
        num_read += 1;
        if (byte & 0x80) == 0 {
            return Ok((result as i32, num_read));
        }
    }
}

/// Reads a VarInt asynchronously from a stream, returning `(value, wire_bytes_read)`.
pub async fn read_varint_async<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<(i32, usize)> {
    let mut num_read = 0usize;
    let mut result = 0u32;
    let mut byte_buf = [0u8; 1];
    loop {
        if num_read >= 5 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "VarInt too big (>5 bytes)",
            ));
        }
        reader.read_exact(&mut byte_buf).await?;
        let byte = byte_buf[0];
        result |= ((byte & 0x7F) as u32) << (7 * num_read);
        num_read += 1;
        if (byte & 0x80) == 0 {
            return Ok((result as i32, num_read));
        }
    }
}

pub fn write_mc_string(out: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    write_varint(out, bytes.len() as i32);
    out.extend_from_slice(bytes);
}

pub fn write_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn write_i32(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn write_i64(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn write_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn read_i32_be(buf: &[u8]) -> io::Result<i32> {
    if buf.len() < 4 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short i32"));
    }
    Ok(i32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]))
}

pub fn read_i64_be(buf: &[u8]) -> io::Result<i64> {
    if buf.len() < 8 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "short i64"));
    }
    Ok(i64::from_be_bytes([
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7],
    ]))
}

/// Deterministic 16-byte offline-mode UUID derived from username.
pub fn offline_uuid(username: &str) -> [u8; 16] {
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x9e37_79b9_7f4a_7c15;
    for &b in format!("OfflinePlayer:{}", username).as_bytes() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 = h2.wrapping_mul(0xbf58_476d_1ce4_e5b9) ^ (b as u64);
    }
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&h1.to_be_bytes());
    bytes[8..].copy_from_slice(&h2.to_be_bytes());
    // Set UUID v3 & RFC 4122 variant bits
    bytes[6] = (bytes[6] & 0x0F) | 0x30;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    bytes
}

/// Decoded Minecraft packet along with raw TCP wire byte count.
#[derive(Debug, Clone)]
pub struct Packet {
    pub id: i32,
    pub payload: Vec<u8>,
    pub wire_bytes: usize,
}

/// Encodes a packet into wire format (supporting both uncompressed and zlib threshold framing).
pub fn encode_packet(
    packet_id: i32,
    payload: &[u8],
    compression_threshold: Option<usize>,
) -> io::Result<Vec<u8>> {
    let mut inner = Vec::with_capacity(5 + payload.len());
    write_varint(&mut inner, packet_id);
    inner.extend_from_slice(payload);

    let mut frame = Vec::with_capacity(inner.len() + 10);
    match compression_threshold {
        None => {
            write_varint(&mut frame, inner.len() as i32);
            frame.extend_from_slice(&inner);
        }
        Some(threshold) => {
            if inner.len() >= threshold {
                let mut compressed = Vec::with_capacity(inner.len());
                let mut enc = ZlibEncoder::new(&mut compressed, Compression::fast());
                enc.write_all(&inner)?;
                enc.finish()?;

                let mut body = Vec::with_capacity(5 + compressed.len());
                write_varint(&mut body, inner.len() as i32);
                body.extend_from_slice(&compressed);

                write_varint(&mut frame, body.len() as i32);
                frame.extend_from_slice(&body);
            } else {
                // Data Length = 0 means uncompressed
                let mut body = Vec::with_capacity(1 + inner.len());
                write_varint(&mut body, 0);
                body.extend_from_slice(&inner);

                write_varint(&mut frame, body.len() as i32);
                frame.extend_from_slice(&body);
            }
        }
    }

    Ok(frame)
}

/// Writes a framed packet asynchronously to a stream.
pub async fn send_packet<W: AsyncWrite + Unpin>(
    writer: &mut W,
    packet_id: i32,
    payload: &[u8],
    compression_threshold: Option<usize>,
) -> io::Result<()> {
    let frame = encode_packet(packet_id, payload, compression_threshold)?;
    writer.write_all(&frame).await?;
    writer.flush().await
}

/// Reads and decodes a single framed packet (uncompressed or zlib-compressed) from a stream.
pub async fn read_packet<R: AsyncRead + Unpin>(
    reader: &mut R,
    compression_threshold: Option<usize>,
) -> io::Result<Packet> {
    let (packet_len_i32, len_hdr_bytes) = read_varint_async(reader).await?;
    if packet_len_i32 <= 0 || (packet_len_i32 as usize) > MAX_PACKET_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Invalid packet length: {}", packet_len_i32),
        ));
    }
    let packet_len = packet_len_i32 as usize;
    let mut raw = vec![0u8; packet_len];
    reader.read_exact(&mut raw).await?;
    let wire_bytes = len_hdr_bytes + packet_len;

    let decoded = match compression_threshold {
        None => raw,
        Some(_) => {
            let (data_len, data_len_hdr) = read_varint_slice(&raw)?;
            let body = &raw[data_len_hdr..];
            if data_len == 0 {
                body.to_vec()
            } else if data_len > 0 && (data_len as usize) <= MAX_PACKET_SIZE {
                let expected = data_len as usize;
                let mut dec = ZlibDecoder::new(body).take((expected + 1) as u64);
                let mut out = Vec::with_capacity(expected.min(64 * 1024));
                dec.read_to_end(&mut out)?;
                if out.len() != expected {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "Decompressed packet length mismatch: header={}, actual={}",
                            expected,
                            out.len()
                        ),
                    ));
                }
                out
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("Invalid uncompressed packet length: {}", data_len),
                ));
            }
        }
    };

    let (id, id_bytes) = read_varint_slice(&decoded)?;
    let payload = decoded[id_bytes..].to_vec();

    Ok(Packet {
        id,
        payload,
        wire_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_varint_roundtrip() {
        for &v in &[0, 1, 127, 128, 255, 769, 25565, 2_097_151, -1] {
            let mut buf = Vec::new();
            write_varint(&mut buf, v);
            let (decoded, used) = read_varint_slice(&buf).expect("decode varint");
            assert_eq!(decoded, v);
            assert_eq!(used, buf.len());
        }
    }

    #[tokio::test]
    async fn test_uncompressed_packet_roundtrip() {
        let mut payload = Vec::new();
        write_mc_string(&mut payload, "SpicaBot01");
        let wire = encode_packet(0x00, &payload, None).expect("encode uncompressed");
        let mut cursor = io::Cursor::new(wire.clone());
        let pkt = read_packet(&mut cursor, None)
            .await
            .expect("decode uncompressed");
        assert_eq!(pkt.id, 0x00);
        assert_eq!(pkt.payload, payload);
        assert_eq!(pkt.wire_bytes, wire.len());
    }

    #[tokio::test]
    async fn test_compressed_packet_roundtrip_above_and_below_threshold() {
        // Below threshold (uncompressed with data_len = 0)
        let small = vec![1u8, 2, 3, 4];
        let wire_small = encode_packet(0x27, &small, Some(64)).expect("encode small");
        let mut cur_small = io::Cursor::new(wire_small);
        let pkt_small = read_packet(&mut cur_small, Some(64))
            .await
            .expect("decode small");
        assert_eq!(pkt_small.id, 0x27);
        assert_eq!(pkt_small.payload, small);

        // Above threshold (zlib compressed)
        let large = vec![0x42u8; 512];
        let wire_large = encode_packet(0x2C, &large, Some(64)).expect("encode large");
        assert!(wire_large.len() < large.len());
        let mut cur_large = io::Cursor::new(wire_large);
        let pkt_large = read_packet(&mut cur_large, Some(64))
            .await
            .expect("decode large");
        assert_eq!(pkt_large.id, 0x2C);
        assert_eq!(pkt_large.payload, large);
    }

    #[tokio::test]
    async fn test_compressed_packet_rejects_decompressed_size_mismatch() {
        // Compress 64 bytes, but lie in the header claiming data_len = 32 (zip-bomb / corrupt guard)
        let inner = vec![0x41u8; 64];
        let mut compressed = Vec::new();
        {
            let mut enc = ZlibEncoder::new(&mut compressed, Compression::fast());
            enc.write_all(&inner).unwrap();
            enc.finish().unwrap();
        }
        let mut body = Vec::new();
        write_varint(&mut body, 32); // wrong declared uncompressed size!
        body.extend_from_slice(&compressed);
        let mut frame = Vec::new();
        write_varint(&mut frame, body.len() as i32);
        frame.extend_from_slice(&body);

        let mut cur = io::Cursor::new(frame);
        let res = read_packet(&mut cur, Some(16)).await;
        assert!(res.is_err(), "Mismatched decompressed length MUST be rejected");
    }

    #[test]
    fn test_offline_uuid_deterministic_and_versioned() {
        let u1 = offline_uuid("SpicaBot001");
        let u2 = offline_uuid("SpicaBot001");
        let u3 = offline_uuid("SpicaBot002");
        assert_eq!(u1, u2);
        assert_ne!(u1, u3);
        assert_eq!(u1[6] >> 4, 3);
        assert_eq!(u1[8] >> 6, 2);
    }
}
