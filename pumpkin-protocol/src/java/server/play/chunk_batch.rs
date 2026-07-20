use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_CHUNK_BATCH_RECEIVED;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_CHUNK_BATCH_RECEIVED)]
pub struct SChunkBatch {
    pub chunks_per_tick: f32,
}

impl ServerPacket for SChunkBatch {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            chunks_per_tick: bytebuf.get_f32_be()?,
        })
    }
}
