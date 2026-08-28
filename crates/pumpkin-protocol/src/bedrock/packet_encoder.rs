use std::{
    io::{Error, Write},
    sync::Mutex,
};

use flate2::{Compression, write::DeflateEncoder};

use crate::{
    CompressionLevel, CompressionThreshold,
    bedrock::{BEDROCK_GAME_PACKET, SubClient},
    codec::var_uint::VarUInt,
    serial::PacketWrite,
};

/// Encoder: Server -> Client
/// Supports Zlib compression.
pub struct BedrockBatchEncoder {
    // compression and compression threshold
    compression: Option<(CompressionThreshold, CompressionLevel)>,
    packet_scratch: Mutex<Vec<u8>>,
}

impl Default for BedrockBatchEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl BedrockBatchEncoder {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            compression: None,
            packet_scratch: Mutex::new(Vec::new()),
        }
    }

    pub const fn set_compression(
        &mut self,
        compression_info: (CompressionThreshold, CompressionLevel),
    ) {
        self.compression = Some(compression_info);
    }

    pub fn write_game_packet(
        &self,
        packet_id: u16,
        sub_client_sender: SubClient,
        sub_client_target: SubClient,
        packet_payload: &[u8],
        mut writer: impl Write,
    ) -> Result<(), Error> {
        // Gamepacket ID Header (14 bits)
        let header_value: u32 = u32::from(packet_id)
            | ((sub_client_sender as u32) << 10)
            | ((sub_client_target as u32) << 12);
        let fourteen_bit_header = header_value & 0x3FFF;

        let header_varint = VarUInt(fourteen_bit_header);
        let total_content_length = (header_varint.written_size() + packet_payload.len()) as u32;

        let framed_length = header_varint
            .written_size()
            .saturating_add(packet_payload.len())
            .saturating_add(VarUInt(total_content_length).written_size());

        // Handle Outer Container
        writer.write_all(&[BEDROCK_GAME_PACKET])?; // Bedrock Game Packet Header

        if let Some((threshold, level)) = self.compression
            && framed_length >= threshold
        {
            writer.write_all(&[0x00])?;
            let mut encoder = DeflateEncoder::new(writer, Compression::new(level));
            VarUInt(total_content_length).write(&mut encoder)?;
            header_varint.write(&mut encoder)?;
            encoder.write_all(packet_payload)?;
            let _ = encoder.finish()?;
            return Ok(());
        }

        if self.compression.is_some() {
            writer.write_all(&[0xFF])?;
        }
        VarUInt(total_content_length).write(&mut writer)?;
        header_varint.write(&mut writer)?;
        writer.write_all(packet_payload)?;

        Ok(())
    }

    pub fn write_packet<P: crate::BClientPacket + ?Sized>(
        &self,
        packet: &P,
        writer: impl Write,
    ) -> Result<(), Error> {
        let mut packet_payload = self
            .packet_scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        packet_payload.clear();
        packet.write_packet(&mut *packet_payload)?;
        self.write_game_packet(
            P::PACKET_ID as u16,
            SubClient::Main,
            SubClient::Main,
            &packet_payload,
            writer,
        )
    }

    pub fn serialize_packet<P: crate::BClientPacket + ?Sized>(
        &self,
        packet: &P,
    ) -> Result<bytes::Bytes, Error> {
        let mut buf = Vec::new();
        self.write_packet(packet, &mut buf)?;
        Ok(buf.into())
    }
}

pub fn write_packet<P: crate::BClientPacket + ?Sized>(
    packet: &P,
    writer: impl Write,
) -> Result<(), Error> {
    BedrockBatchEncoder::new().write_packet(packet, writer)
}

pub fn serialize_packet<P: crate::BClientPacket + ?Sized>(
    packet: &P,
) -> Result<bytes::Bytes, Error> {
    BedrockBatchEncoder::new().serialize_packet(packet)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bedrock::packet_decoder::BedrockBatchDecoder;
    use std::io::Cursor;

    struct ScratchPacket(Vec<u8>);

    impl crate::Packet for ScratchPacket {
        const PACKET_ID: i32 = 1;
    }

    impl PacketWrite for ScratchPacket {
        fn write<W: Write>(&self, writer: &mut W) -> Result<(), Error> {
            writer.write_all(&self.0)
        }
    }

    #[tokio::test]
    async fn bedrock_compression_cycle() -> Result<(), Box<dyn std::error::Error>> {
        let mut encoder = BedrockBatchEncoder::new();
        encoder.set_compression((256, 6));

        let packet_id = 1;
        let payload = b"Hello Bedrock Compression!";
        let mut encoded_buf = Vec::new();

        encoder.write_game_packet(
            packet_id,
            SubClient::Main,
            SubClient::Main,
            payload,
            &mut encoded_buf,
        )?;

        let mut decoder = BedrockBatchDecoder::new();
        decoder.set_compression(256);

        let decompressed_payload = decoder.get_packet_payload(encoded_buf).await?;
        let mut cursor = Cursor::new(decompressed_payload);
        let raw_packet = decoder.get_game_packet(&mut cursor)?;

        assert_eq!(raw_packet.id, packet_id as i32);
        assert_eq!(raw_packet.payload.as_ref(), payload);
        Ok(())
    }

    #[tokio::test]
    async fn bedrock_compression_threshold_selects_raw_and_deflate_methods()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut encoder = BedrockBatchEncoder::new();
        encoder.set_compression((256, 6));

        let mut raw_packet = Vec::new();
        encoder.write_game_packet(
            1,
            SubClient::Main,
            SubClient::Main,
            b"small",
            &mut raw_packet,
        )?;
        assert_eq!(raw_packet[1], 0xFF);

        let large_payload = vec![0x5A; 1024];
        let mut compressed_packet = Vec::new();
        encoder.write_game_packet(
            1,
            SubClient::Main,
            SubClient::Main,
            &large_payload,
            &mut compressed_packet,
        )?;
        assert_eq!(compressed_packet[1], 0x00);

        let mut decoder = BedrockBatchDecoder::new();
        decoder.set_compression(256);
        let decompressed = decoder.get_packet_payload(raw_packet).await?;
        let mut cursor = Cursor::new(decompressed);
        let decoded_raw_packet = decoder.get_game_packet(&mut cursor)?;
        assert_eq!(decoded_raw_packet.payload.as_ref(), b"small");
        let decompressed = decoder.get_packet_payload(compressed_packet).await?;
        let mut cursor = Cursor::new(decompressed);
        let raw_packet = decoder.get_game_packet(&mut cursor)?;
        assert_eq!(raw_packet.payload.as_ref(), large_payload);
        Ok(())
    }

    #[tokio::test]
    async fn bedrock_packet_payload_scratch_is_reused_and_cleared()
    -> Result<(), Box<dyn std::error::Error>> {
        let encoder = BedrockBatchEncoder::new();
        let mut first = Vec::new();
        encoder.write_packet(&ScratchPacket(vec![0xA5; 4096]), &mut first)?;
        let first_capacity = encoder
            .packet_scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .capacity();

        let expected_payload = vec![0x5A; 32];
        let mut second = Vec::new();
        encoder.write_packet(&ScratchPacket(expected_payload.clone()), &mut second)?;
        let second_capacity = encoder
            .packet_scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .capacity();
        assert_eq!(second_capacity, first_capacity);

        let mut decoder = BedrockBatchDecoder::new();
        let decompressed = decoder.get_packet_payload(second).await?;
        let mut cursor = Cursor::new(decompressed);
        let decoded = decoder.get_game_packet(&mut cursor)?;
        assert_eq!(decoded.payload.as_ref(), expected_payload);
        Ok(())
    }
}
