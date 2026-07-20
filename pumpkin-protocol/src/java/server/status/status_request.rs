use crate::{ServerPacket, ser::ReadingError};
use pumpkin_data::packet::serverbound::STATUS_STATUS_REQUEST;
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;
use std::io::Read;

/// Sent by the client to request the server's current status information.
///
/// This is the first packet sent during the "Status" state.
/// The server should respond with `CStatusResponse`.
#[java_packet(STATUS_STATUS_REQUEST)]
pub struct SStatusRequest;

impl ServerPacket for SStatusRequest {
    fn read(
        _bytebuf: impl Read,
        _protocol_version: &JavaMinecraftVersion,
    ) -> Result<Self, ReadingError> {
        Ok(Self)
    }
}
