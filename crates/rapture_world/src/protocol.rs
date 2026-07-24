use anyhow::Result;
use rapture_common::packet::{Opcode, PacketWriter};

use crate::character::*;

pub(crate) fn build_world_handshake() -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::GameHandshake);
    packet.write_u8(0);
    packet.write_u8(2);
    packet.write_bytes(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    packet.into_bytes()
}

pub(crate) fn build_account_faction(faction: u8, max_mode: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::AccountFaction);
    packet.write_u8(faction);
    packet.write_u8(max_mode);
    packet.into_bytes()
}

pub(crate) fn is_valid_account_faction(faction: u8) -> bool {
    matches!(faction, FACTION_LIGHT | FACTION_DARK | FACTION_NONE)
}

pub(crate) fn build_create_character_result(ok: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CreateCharacter);
    // Single result byte (0 = ok, 1 = failure), confirmed via capture: a
    // duplicate-name create replied `02 01 01` (opcode + one byte), which the
    // client renders as a "name already in use" notice. Previously written as
    // a u32; the client accepted that (it reads one byte and ignores the
    // trailing padding), but one byte matches the real server exactly.
    packet.write_u8(if ok { 0 } else { 1 });
    packet.into_bytes()
}

pub(crate) fn build_select_character_result(ok: bool, character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::SelectCharacter);
    packet.write_u8(if ok { 0 } else { 1 });
    packet.write_u32(character_id);
    packet.into_bytes()
}

pub(crate) fn build_delete_character_result(ok: bool, character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::DeleteCharacter);
    packet.write_u8(if ok { 0 } else { 1 });
    packet.write_u32(character_id);
    packet.into_bytes()
}

pub(crate) fn build_restore_character_result(ok: bool, character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::RestoreCharacter);
    packet.write_u8(if ok { 0 } else { 1 });
    packet.write_u32(character_id);
    packet.into_bytes()
}

/// Rename result -- a single byte, and note the INVERTED convention vs
/// delete/restore: here **1 = ok, 0 = fail** (confirmed via capture `0E 01 01`
/// on success, and matching Imgeneus's SendRenamedCharacter). No charId in
/// this client's response.
pub(crate) fn build_rename_character_result(ok: bool) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::RenameCharacter);
    packet.write_u8(if ok { 1 } else { 0 });
    packet.into_bytes()
}

pub(crate) fn shaiya_time_2020_01_01_12_30() -> i32 {
    let year = 2020;
    let month = 1;
    let day = 1;
    let hour = 12;
    let minute = 30;
    let second = 0;

    let num1 = 16 * (year - 16);
    let num2 = 32 * (month + num1);
    second + ((minute + ((hour + 32 * (day + num2)) << 6)) << 6)
}

pub(crate) fn build_world_day() -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::WorldDay);
    packet.write_i32(shaiya_time_2020_01_01_12_30());
    packet.into_bytes()
}

pub(crate) fn build_kill_count_update(index: u8, value: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::UserKillCountUpdate);
    packet.write_u8(index);
    packet.write_u32(value);
    packet.into_bytes()
}

pub(crate) fn build_zero_count_packet(opcode: Opcode) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(opcode);
    packet.write_u8(0);
    packet.into_bytes()
}

pub(crate) fn build_bless_init(faction: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::BlessInit);
    packet.write_u8(faction);
    packet.write_i32(0);
    packet.write_u32(0);
    packet.into_bytes()
}

/// `AccountPoints` (0x2601): a server-push notification of the account's
/// Item Mall point balance, sent once at character select (confirmed via
/// Imgeneus, which comments this as required for item-mall/points display
/// -- not documented in Shaiya-Aries's own headers at all, so the trailing
/// zero byte's purpose is unconfirmed but harmless).
pub(crate) fn build_account_points(points: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::AccountPoints);
    packet.write_u32(points);
    packet.write_u8(0);
    packet.into_bytes()
}

/// `CashPoint` (0x2605): response to the client's (empty-body) request to
/// refresh its displayed point balance, e.g. on opening the cash shop UI.
/// Confirmed against Shaiya-Aries's `network/game/outgoing/2600.h`
/// (`GamePointOutgoing`): a bare `u32`, no trailing byte (unlike 0x2601).
pub(crate) fn build_cash_points(points: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CashPoint);
    packet.write_u32(points);
    packet.into_bytes()
}

/// GamePingOutgoing (0xA303): echoes the client's keep-alive ping.
pub(crate) fn build_ping() -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::Ping);
    packet.write_u16(0);
    packet.into_bytes()
}

pub(crate) fn read_le_f32(bytes: &[u8]) -> Result<f32> {
    if bytes.len() < 4 {
        anyhow::bail!("not enough bytes for f32");
    }

    Ok(f32::from_le_bytes(bytes[0..4].try_into()?))
}

/// Aries's `String<N> = std::array<char, N>` (shaiya/include/common.h) is a
/// plain single-byte buffer, not UTF-8 or UTF-16. The client's ANSI
/// codepage (Windows-1252 for a Spanish/Western install) round-trips
/// accented Latin letters (ñ, á, é, í, ó, ú, ü, ¿, ¡...) identically to
/// ISO-8859-1/Latin-1 for the whole 0xA0-0xFF range, so a byte<->codepoint
/// 1:1 mapping is used here. This does NOT correctly round-trip the
/// Windows-1252-specific punctuation in 0x80-0x9F (curly quotes, em-dash,
/// euro sign) — those are rare in chat and can be special-cased later if
/// needed. Decoding chat as UTF-8 (the previous behavior) mangled any
/// accented character into mojibake, since the client never sends UTF-8.
fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| byte as char).collect()
}

fn encode_latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| if (c as u32) <= 0xFF { c as u8 } else { b'?' })
        .collect()
}

/// Aries chat structs (game/incoming/1100.h, game/incoming/F100.h) encode
/// the message as a plain byte buffer: `messageLength:u8` (length including
/// the trailing null terminator) followed by exactly that many bytes.
pub(crate) fn read_chat_message(payload: &[u8]) -> Result<String> {
    if payload.len() < 3 {
        anyhow::bail!("chat packet is too short");
    }

    let message_len = payload[2] as usize;
    let message_start = 3;
    if payload.len() < message_start + message_len {
        anyhow::bail!("chat packet message is truncated");
    }

    let bytes = &payload[message_start..message_start + message_len];
    let end = bytes
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(bytes.len());
    Ok(decode_latin1(&bytes[..end]))
}

/// Maximum message bytes that fit in Aries's `String<128>` chat buffer,
/// leaving one byte for the null terminator `read_chat_message` expects.
pub(crate) const MAX_CHAT_MESSAGE_LEN: usize = 127;

fn write_chat_text(packet: &mut PacketWriter, message: &str) {
    let mut bytes = encode_latin1(message);
    bytes.truncate(MAX_CHAT_MESSAGE_LEN);
    bytes.push(0);
    packet.write_u8(bytes.len() as u8);
    packet.write_bytes(&bytes);
}

/// Normal/guild/party/union-shaped chat packets identify the sender by a
/// numeric character id (GameChatNormalOutgoing, 0x1101, and its 0xF101
/// admin mirror share this exact layout: senderId:u32 + messageLength:u8 + message).
pub(crate) fn build_chat_by_id(opcode: Opcode, sender_id: u32, message: &str) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(opcode);
    packet.write_u32(sender_id);
    write_chat_text(&mut packet, message);
    packet.into_bytes()
}

pub(crate) fn build_chat_normal(character_id: u32, message: &str) -> Result<Vec<u8>> {
    build_chat_by_id(Opcode::ChatNormal, character_id, message)
}

/// World/map(zone)/admin-trade-shaped chat packets identify the sender by
/// name instead of id (GameChatWorldOutgoing 0x1103, GameChatZoneOutgoing
/// 0x1111, and GameAdminChatTradeOutgoing 0xF103 all share this layout:
/// senderName:String<21> + messageLength:u8 + message).
pub(crate) fn build_chat_by_name(
    opcode: Opcode,
    sender_name: &str,
    message: &str,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(opcode);
    packet.write_fixed_string(sender_name, 21)?;
    write_chat_text(&mut packet, message);
    packet.into_bytes()
}

/// GameChatErrorOutgoing (0x1106): result:u8, used to reject a chat
/// attempt (spam, not authorized for the channel, message rejected, etc).
pub(crate) fn build_chat_error(result: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ChatError);
    packet.write_u8(result);
    packet.into_bytes()
}

pub(crate) fn is_supported_chat_opcode(opcode: u16) -> bool {
    opcode == Opcode::ChatNormal as u16
        || opcode == Opcode::ChatWorld as u16
        || opcode == Opcode::ChatMap as u16
        || opcode == Opcode::ChatNormalAdmin as u16
        || opcode == Opcode::ChatWorldAdmin as u16
}
