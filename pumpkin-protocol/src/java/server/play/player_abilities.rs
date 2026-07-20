use pumpkin_data::packet::serverbound::PLAY_PLAYER_ABILITIES;
use pumpkin_macros::java_packet;

// The vanilla client sends this packet when the player starts/stops flying. Bitmask 0x02 is set when the player is flying.

use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_PLAYER_ABILITIES)]
pub struct SPlayerAbilities {
    pub flags: i8,
}

impl ServerPacket for SPlayerAbilities {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            flags: bytebuf.get_i8()?,
        })
    }
}
