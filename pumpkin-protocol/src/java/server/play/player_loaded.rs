use crate::{ServerPacket, ser::ReadingError};
use pumpkin_data::packet::serverbound::PLAY_PLAYER_LOADED;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_PLAYER_LOADED)]
pub struct SPlayerLoaded;

impl ServerPacket for SPlayerLoaded {
    fn read(
        _bytebuf: impl Read,
        _protocol_version: &JavaMinecraftVersion,
    ) -> Result<Self, ReadingError> {
        Ok(Self)
    }
}
