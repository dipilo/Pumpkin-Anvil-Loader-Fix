use pumpkin_data::packet::serverbound::PLAY_CHAT_COMMAND;
use pumpkin_macros::java_packet;

use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_CHAT_COMMAND)]
pub struct SChatCommand {
    pub command: String,
}

impl ServerPacket for SChatCommand {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        Ok(Self {
            command: bytebuf.get_str()?.into_string(),
        })
    }
}
