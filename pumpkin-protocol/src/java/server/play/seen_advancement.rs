use crate::{
    ServerPacket,
    ser::{NetworkReadExt, ReadingError},
};
use pumpkin_data::packet::serverbound::PLAY_SEEN_ADVANCEMENTS;
use pumpkin_macros::java_packet;
use pumpkin_util::identifier::Identifier;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

#[derive(Debug)]
#[java_packet(PLAY_SEEN_ADVANCEMENTS)]
pub enum SSeenAdvancement {
    OpenTab(Identifier),
    CloseTab,
}

impl ServerPacket for SSeenAdvancement {
    fn read(mut bytebuf: impl Read, _version: &JavaMinecraftVersion) -> Result<Self, ReadingError> {
        let action = bytebuf.get_var_int()?;
        match action.0 {
            0 => {
                let id_str = bytebuf.get_str()?;
                let id =
                    Identifier::parse(&id_str).map_err(|e| ReadingError::Message(e.to_string()))?;
                Ok(Self::OpenTab(id))
            }
            1 => Ok(Self::CloseTab),
            _ => Err(ReadingError::Message(format!(
                "Invalid SeenAdvancement action: {}",
                action.0
            ))),
        }
    }
}
