use std::io::Cursor;

use async_compression::tokio::bufread::DeflateDecoder;
use bytes::Bytes;
use tokio::io::BufReader;

use crate::{
    CompressionThreshold, MAX_PACKET_DATA_SIZE, PacketDecodeError, RawPacket,
    bedrock::BEDROCK_GAME_PACKET, codec::var_uint::VarUInt, ser::ReadingError,
};

/// Decoder: Client -> Server
/// Supports Zlib decompression.
pub struct BedrockBatchDecoder {
    compression: Option<CompressionThreshold>,
}

impl Default for BedrockBatchDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl BedrockBatchDecoder {
    #[must_use]
    pub const fn new() -> Self {
        Self { compression: None }
    }

    pub const fn set_compression(&mut self, threshold: CompressionThreshold) {
        self.compression = Some(threshold);
    }

    pub async fn get_packet_payload(
        &mut self,
        full_packet: Vec<u8>,
    ) -> Result<Bytes, PacketDecodeError> {
        if full_packet.is_empty() {
            return Err(PacketDecodeError::MalformedLength("Empty packet".into()));
        }

        // NetherNet carries the batch without the Bedrock game-packet marker.
        // The transport adapter restores it before decoding.
        if full_packet[0] != BEDROCK_GAME_PACKET {
            return Err(PacketDecodeError::MalformedLength(format!(
                "Missing 0xfe header (found 0x{:02x})",
                full_packet[0]
            )));
        }

        let full_packet_payload = Bytes::from(full_packet).slice(1..);

        // If compression is NOT enabled yet, the payload starts at index 0 of full_packet_payload
        if self.compression.is_none() {
            let payload = full_packet_payload;
            if payload.len() > MAX_PACKET_DATA_SIZE {
                return Err(PacketDecodeError::TooLong);
            }
            return Ok(payload);
        }

        // If compression IS enabled, Bedrock expects a compression method byte at index 0 of full_packet_payload.
        let compression_method = full_packet_payload.first().ok_or_else(|| {
            PacketDecodeError::MalformedLength("Missing Bedrock compression method".into())
        })?;
        let data_start = 1;

        match compression_method {
            0x00 => {
                use tokio::io::AsyncReadExt;
                let compressed_payload = &full_packet_payload[data_start..];
                let mut decoder = DeflateDecoder::new(BufReader::new(compressed_payload))
                    .take(MAX_PACKET_DATA_SIZE as u64 + 1);
                let mut decompressed = Vec::new();
                decoder
                    .read_to_end(&mut decompressed)
                    .await
                    .map_err(|e| PacketDecodeError::FailedDecompression(e.to_string()))?;
                if decompressed.len() > MAX_PACKET_DATA_SIZE {
                    return Err(PacketDecodeError::TooLong);
                }
                Ok(decompressed.into())
            }
            0xff => {
                // None (Compression enabled but this specific packet is raw)
                let payload = full_packet_payload.slice(data_start..);
                if payload.len() > MAX_PACKET_DATA_SIZE {
                    return Err(PacketDecodeError::TooLong);
                }
                Ok(payload)
            }
            _ => Err(PacketDecodeError::FailedDecompression(format!(
                "Unsupported compression method: 0x{compression_method:02x}"
            ))),
        }
    }

    pub fn get_game_packet(
        &mut self,
        decompressed_reader: &mut Cursor<Bytes>,
    ) -> Result<RawPacket, PacketDecodeError> {
        let packet_len = VarUInt::decode(decompressed_reader).map_err(|err| match err {
            ReadingError::CleanEOF(_) => PacketDecodeError::ConnectionClosed,
            err => PacketDecodeError::MalformedLength(err.to_string()),
        })?;
        let packet_len = packet_len.0 as usize;
        if packet_len == 0 {
            return Err(PacketDecodeError::MalformedLength(
                "Bedrock game packet length is zero".into(),
            ));
        }
        if packet_len > MAX_PACKET_DATA_SIZE {
            return Err(PacketDecodeError::TooLong);
        }

        let var_header = VarUInt::decode(decompressed_reader)?;
        let header = var_header.0 & 0x3FFF;
        let gamepacket_id = (header & 0x3FF) as u16;

        let header_size = var_header.written_size();
        if packet_len < header_size {
            return Err(PacketDecodeError::MalformedLength(format!(
                "Bedrock game packet length {packet_len} is smaller than header size {header_size}"
            )));
        }

        let payload_len = packet_len - header_size;
        let remaining = decompressed_reader
            .get_ref()
            .len()
            .saturating_sub(decompressed_reader.position() as usize);
        if payload_len > remaining {
            return Err(PacketDecodeError::MalformedLength(format!(
                "Bedrock game packet payload length {payload_len} exceeds remaining batch bytes {remaining}"
            )));
        }

        let payload_start = decompressed_reader.position() as usize;
        let payload_end = payload_start + payload_len;
        let payload = decompressed_reader
            .get_ref()
            .slice(payload_start..payload_end);
        decompressed_reader.set_position(payload_end as u64);

        Ok(RawPacket {
            id: i32::from(gamepacket_id),
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::bedrock::{SubClient, packet_encoder::BedrockBatchEncoder};

    use super::*;

    #[test]
    fn decodes_payload_larger_than_two_mib() {
        const PAYLOAD_LEN: usize = 2 * 1024 * 1024 + 1;
        let payload = vec![0x2a; PAYLOAD_LEN];
        let mut wire_buf = Vec::new();
        let network_encoder = BedrockBatchEncoder::new();
        network_encoder
            .write_game_packet(
                0x01,
                SubClient::Main,
                SubClient::Main,
                &payload,
                &mut wire_buf,
            )
            .expect("encode Bedrock game packet");

        let mut cursor = Cursor::new(Bytes::from(wire_buf).slice(1..));
        let mut decoder = BedrockBatchDecoder::new();

        let packet = decoder
            .get_game_packet(&mut cursor)
            .expect("decode Bedrock game packet");

        assert_eq!(packet.id, 0x01);
        assert_eq!(packet.payload.len(), PAYLOAD_LEN);
        assert_eq!(packet.payload.as_ref(), payload.as_slice());
    }

    #[tokio::test]
    async fn raw_batch_and_inner_packet_share_backing_storage() {
        let payload = vec![0x3c; 64];
        let mut wire_buf = Vec::new();
        BedrockBatchEncoder::new()
            .write_game_packet(
                0x01,
                SubClient::Main,
                SubClient::Main,
                &payload,
                &mut wire_buf,
            )
            .expect("encode Bedrock game packet");
        let expected_batch_pointer = wire_buf.as_ptr().wrapping_add(1);

        let mut decoder = BedrockBatchDecoder::new();
        let batch = decoder
            .get_packet_payload(wire_buf)
            .await
            .expect("decode raw Bedrock batch");
        assert_eq!(batch.as_ptr(), expected_batch_pointer);

        let mut cursor = Cursor::new(batch.clone());
        let packet = decoder
            .get_game_packet(&mut cursor)
            .expect("decode inner Bedrock packet");
        let expected_payload_pointer = batch.as_ptr().wrapping_add(batch.len() - payload.len());
        assert_eq!(packet.payload.as_ptr(), expected_payload_pointer);
        assert_eq!(packet.payload.as_ref(), payload);
    }
}
