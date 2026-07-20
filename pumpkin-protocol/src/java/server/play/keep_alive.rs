use pumpkin_data::packet::serverbound::PLAY_KEEP_ALIVE;
use pumpkin_macros::java_packet;

use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_KEEP_ALIVE)]
pub struct SKeepAlive {
    pub keep_alive_id: i64,
}

impl ServerPacket for SKeepAlive {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            keep_alive_id: bytebuf.get_i64_be()?,
        })
    }
}
