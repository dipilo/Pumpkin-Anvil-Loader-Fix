use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_TELEPORT_TO_ENTITY;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[java_packet(PLAY_TELEPORT_TO_ENTITY)]
pub struct STeleportToEntity {
    pub target: uuid::Uuid,
}

impl ServerPacket for STeleportToEntity {
    fn read(
        mut bytebuf: impl Read,
        _protocol_version: &JavaMinecraftVersion,
    ) -> Result<Self, ReadingError> {
        Ok(Self {
            target: bytebuf.get_uuid()?,
        })
    }
}
