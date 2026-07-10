use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::{DbCharacter, DbCharacterItem, DbConsumableEffect, DbEquipmentStats};
use rapture_common::packet::{Opcode, PacketWriter};
use std::net::TcpStream;

use crate::session::*;

pub(crate) const CHARACTER_ID: u32 = 1;
pub(crate) const MAX_HP: u32 = 100;
pub(crate) const MAX_MP: u32 = 100;
pub(crate) const MAX_SP: u32 = 100;
pub(crate) const BASE_STAT: u32 = 10;
pub(crate) const BASE_MIN_ATTACK: u32 = 15;
pub(crate) const BASE_MAX_ATTACK: u32 = 15;
pub(crate) const BASE_MIN_MAGIC: u32 = 15;
pub(crate) const BASE_MAX_MAGIC: u32 = 15;
pub(crate) const BASE_DEFENSE: u32 = 10;
pub(crate) const BASE_RESISTANCE: u32 = 10;
pub(crate) const HP_PER_REACTION: u32 = 5;
pub(crate) const SP_PER_DEXTERITY: u32 = 5;
pub(crate) const MP_PER_WISDOM: u32 = 5;
pub(crate) const DEFENSE_PER_REACTION: u32 = 1;
pub(crate) const RESISTANCE_PER_WISDOM: u32 = 1;
/// Item Type values eligible for use as a consumable (potions etc.).
/// 25 = standard potion type; 100/101/102 currently added at the user's
/// request without further per-type distinction.
pub(crate) const CONSUMABLE_ITEM_TYPES: [u8; 4] = [25, 100, 101, 102];
pub(crate) const SPECIAL_NONE: u8 = 0;
pub(crate) const SPECIAL_PERCENT_HEALING_POTION: u8 = 17;
pub(crate) const INVENTORY_FIRST_BAG: u8 = 1;
pub(crate) const INVENTORY_LAST_BAG: u8 = 5;
pub(crate) const INVENTORY_SLOT_COUNT: u8 = 24;
pub(crate) const LOG_RAW_PACKETS: bool = true;
pub(crate) const LOG_MOVEMENT_PACKETS: bool = false;
pub(crate) const FACTION_LIGHT: u8 = 0;
pub(crate) const FACTION_DARK: u8 = 1;
pub(crate) const FACTION_NONE: u8 = 2;
pub(crate) const MAX_MODE_ULTIMATE: u8 = 3;
/// Number of character slots available per account (0..MAX_CHARACTER_SLOTS).
pub(crate) const MAX_CHARACTER_SLOTS: u8 = 5;
pub(crate) const START_MAP_ID: u16 = 1;
pub(crate) const SPAWN_X: f32 = 500.0;
pub(crate) const SPAWN_Y: f32 = 50.0;
pub(crate) const SPAWN_Z: f32 = 500.0;
pub(crate) const ENABLE_REAL_INVENTORY_PACKETS: bool = true;

#[derive(Clone, Copy, Debug)]
pub(crate) struct CharacterStats {
    pub(crate) strength: u32,
    pub(crate) reaction: u32,
    pub(crate) intelligence: u32,
    pub(crate) wisdom: u32,
    pub(crate) dexterity: u32,
    pub(crate) luck: u32,
    pub(crate) max_hp: u32,
    pub(crate) max_mp: u32,
    pub(crate) max_sp: u32,
    pub(crate) current_hp: u32,
    pub(crate) current_mp: u32,
    pub(crate) current_sp: u32,
    pub(crate) min_attack: u32,
    pub(crate) max_attack: u32,
    pub(crate) min_magic: u32,
    pub(crate) max_magic: u32,
    pub(crate) defense: u32,
    pub(crate) resistance: u32,
}

impl CharacterStats {
    pub(crate) fn from_character(
        character: Option<&DbCharacter>,
        equipment: DbEquipmentStats,
    ) -> Self {
        let base_strength = character.map_or(BASE_STAT, |character| character.strength as u32);
        let base_reaction = character.map_or(BASE_STAT, |character| character.reaction as u32);
        let base_intelligence =
            character.map_or(BASE_STAT, |character| character.intelligence as u32);
        let base_wisdom = character.map_or(BASE_STAT, |character| character.wisdom as u32);
        let base_dexterity = character.map_or(BASE_STAT, |character| character.dexterity as u32);
        let base_luck = character.map_or(BASE_STAT, |character| character.luck as u32);
        let base_max_hp = character.map_or(MAX_HP, |character| character.max_hp);
        let base_max_mp = character.map_or(MAX_MP, |character| character.max_mp);
        let base_max_sp = character.map_or(MAX_SP, |character| character.max_sp);
        let strength = base_strength.saturating_add(equipment.strength);
        let reaction = base_reaction.saturating_add(equipment.reaction);
        let intelligence = base_intelligence.saturating_add(equipment.intelligence);
        let wisdom = base_wisdom.saturating_add(equipment.wisdom);
        let dexterity = base_dexterity.saturating_add(equipment.dexterity);
        let luck = base_luck.saturating_add(equipment.luck);

        let derived_max_hp = reaction.saturating_mul(HP_PER_REACTION);
        let derived_max_mp = wisdom.saturating_mul(MP_PER_WISDOM);
        let derived_max_sp = dexterity.saturating_mul(SP_PER_DEXTERITY);
        let max_hp = base_max_hp
            .saturating_add(equipment.max_hp)
            .saturating_add(derived_max_hp);
        let max_mp = base_max_mp
            .saturating_add(equipment.max_mp)
            .saturating_add(derived_max_mp);
        let max_sp = base_max_sp
            .saturating_add(equipment.max_sp)
            .saturating_add(derived_max_sp);
        let current_hp = character
            .map_or(MAX_HP, |character| character.current_hp)
            .saturating_add(equipment.max_hp)
            .saturating_add(derived_max_hp)
            .min(max_hp);
        let current_mp = character
            .map_or(MAX_MP, |character| character.current_mp)
            .saturating_add(equipment.max_mp)
            .saturating_add(derived_max_mp)
            .min(max_mp);
        let current_sp = character
            .map_or(MAX_SP, |character| character.current_sp)
            .saturating_add(equipment.max_sp)
            .saturating_add(derived_max_sp)
            .min(max_sp);
        let derived_min_attack = strength
            .saturating_mul(13)
            .saturating_add(dexterity.saturating_mul(2))
            / 10;
        let derived_min_magic = intelligence
            .saturating_mul(13)
            .saturating_add(wisdom.saturating_mul(2))
            / 10;
        let min_attack = BASE_MIN_ATTACK
            .saturating_add(equipment.min_attack)
            .saturating_add(derived_min_attack);
        let max_attack = BASE_MAX_ATTACK
            .saturating_add(equipment.max_attack)
            .saturating_add(derived_min_attack)
            .max(min_attack);
        let min_magic = BASE_MIN_MAGIC
            .saturating_add(equipment.min_magic)
            .saturating_add(derived_min_magic);
        let max_magic = BASE_MAX_MAGIC
            .saturating_add(equipment.max_magic)
            .saturating_add(derived_min_magic)
            .max(min_magic);

        Self {
            strength,
            reaction,
            intelligence,
            wisdom,
            dexterity,
            luck,
            max_hp,
            max_mp,
            max_sp,
            current_hp,
            current_mp,
            current_sp,
            min_attack,
            max_attack,
            min_magic,
            max_magic,
            defense: BASE_DEFENSE
                .saturating_add(equipment.defense)
                .saturating_add(reaction.saturating_mul(DEFENSE_PER_REACTION)),
            resistance: BASE_RESISTANCE
                .saturating_add(equipment.resistance)
                .saturating_add(wisdom.saturating_mul(RESISTANCE_PER_WISDOM)),
        }
    }

    pub(crate) fn with_recovery(mut self, effect: &DbConsumableEffect) -> Self {
        let hp = recovery_amount(effect.hp, self.max_hp, effect.special);
        let mp = recovery_amount(effect.mp, self.max_mp, effect.special);
        let sp = recovery_amount(effect.sp, self.max_sp, effect.special);
        self.current_hp = self.current_hp.saturating_add(hp).min(self.max_hp);
        self.current_mp = self.current_mp.saturating_add(mp).min(self.max_mp);
        self.current_sp = self.current_sp.saturating_add(sp).min(self.max_sp);
        self
    }
}

pub(crate) fn recovery_amount(value: u32, max_value: u32, special: u8) -> u32 {
    if special == SPECIAL_PERCENT_HEALING_POTION {
        max_value.saturating_mul(value).saturating_div(100)
    } else {
        value
    }
}

pub(crate) fn character_name(character: Option<&DbCharacter>) -> &str {
    character
        .map(|character| character.name.as_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("Rapture")
}

pub(crate) fn build_character_details(
    character: Option<&DbCharacter>,
    stats: &CharacterStats,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterDetails);
    packet.write_u16(stats.strength.min(u16::MAX as u32) as u16);
    packet.write_u16(stats.dexterity.min(u16::MAX as u32) as u16);
    packet.write_u16(stats.reaction.min(u16::MAX as u32) as u16);
    packet.write_u16(stats.intelligence.min(u16::MAX as u32) as u16);
    packet.write_u16(stats.wisdom.min(u16::MAX as u32) as u16);
    packet.write_u16(stats.luck.min(u16::MAX as u32) as u16);

    packet.write_u16(character.map_or(0, |character| character.stat_point));
    packet.write_u16(character.map_or(0, |character| character.skill_point));
    packet.write_i32(0); // max hp is sent by 0x051F/0x050B for this client
    packet.write_i32(0); // max mp is sent by 0x051F/0x050B for this client
    packet.write_i32(0); // max sp is sent by 0x051F/0x050B for this client
    packet.write_u16(0); // angle
    packet.write_u32(0); // start level exp
    packet.write_u32(0); // end level exp
    packet.write_u32(character.map_or(0, |character| character.exp)); // current exp
    packet.write_u32(character.map_or(0, |character| character.money)); // gold
    packet.write_f32(character.map_or(SPAWN_X, |character| character.pos_x));
    packet.write_f32(character.map_or(SPAWN_Y, |character| character.pos_y));
    packet.write_f32(character.map_or(SPAWN_Z, |character| character.pos_z));
    packet.write_u32(character.map_or(0, |character| character.kills));
    packet.write_u32(character.map_or(0, |character| character.deaths));
    packet.write_u32(character.map_or(0, |character| character.victories));
    packet.write_u32(character.map_or(0, |character| character.defeats));
    packet.write_fixed_string("", 25)?;

    packet.into_bytes()
}

pub(crate) fn build_character_additional_stats(stats: &CharacterStats) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterAdditionalStats);
    packet.write_u32(stats.strength);
    packet.write_u32(stats.reaction);
    packet.write_u32(stats.intelligence);
    packet.write_u32(stats.wisdom);
    packet.write_u32(stats.dexterity);
    packet.write_u32(stats.luck);
    packet.write_u32(stats.min_attack);
    packet.write_u32(stats.max_attack);
    packet.write_u32(stats.min_magic);
    packet.write_u32(stats.max_magic);
    packet.write_u32(stats.defense);
    packet.write_u32(stats.resistance);
    packet.into_bytes()
}

pub(crate) fn build_current_hitpoints(stats: &CharacterStats) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterCurrentHitpoints);
    packet.write_u32(stats.current_hp);
    packet.write_u32(stats.current_mp);
    packet.write_u32(stats.current_sp);
    packet.into_bytes()
}

pub(crate) fn build_max_hp_mp_sp(
    character: Option<&DbCharacter>,
    stats: &CharacterStats,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterMaxHpMpSp);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u32(stats.max_hp);
    packet.write_u32(stats.max_mp);
    packet.write_u32(stats.max_sp);
    packet.into_bytes()
}

pub(crate) fn build_max_energy(
    character: Option<&DbCharacter>,
    hitpoint_type: u8,
    value: u32,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterMaxEnergy);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u8(hitpoint_type);
    packet.write_u32(value);
    packet.into_bytes()
}

pub(crate) fn build_character_entered_map(character: Option<&DbCharacter>) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterEnteredMap);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u8(2); // admin marker used by Imgeneus; enables client-side GM commands
    packet.write_u16(0);
    packet.write_f32(character.map_or(SPAWN_X, |character| character.pos_x));
    packet.write_f32(character.map_or(SPAWN_Y, |character| character.pos_y));
    packet.write_f32(character.map_or(SPAWN_Z, |character| character.pos_z));
    packet.write_u32(0); // guild id
    packet.write_u32(0); // vehicle id
    packet.into_bytes()
}

pub(crate) fn build_character_map_position(character: Option<&DbCharacter>) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterMapTeleport);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u16(character.map_or(START_MAP_ID, |character| character.map_id));
    packet.write_f32(character.map_or(SPAWN_X, |character| character.pos_x));
    packet.write_f32(character.map_or(SPAWN_Y, |character| character.pos_y));
    packet.write_f32(character.map_or(SPAWN_Z, |character| character.pos_z));
    packet.into_bytes()
}

/// `motion`/`direction` must match Aries's MotionType (shaiya/include/
/// common/UserTypes.h) and the client's own facing encoding, respectively.
/// Callers describing a character that is not actively moving (initial
/// spawn, teleport arrival) should pass motion = 0 (MotionType::None,
/// idle/standing) and direction = 0. Callers relaying another player's
/// real-time movement (see chat.rs's ConnectedPlayer::move_packet and the
/// CharacterMove broadcast in main.rs) MUST pass the mover's actual motion
/// and direction from their incoming 0x0501 packet -- hardcoding these to
/// 0 previously made other players appear to freeze/teleport with the
/// wrong facing instead of animating their real run motion.
pub(crate) fn build_character_move(
    character: Option<&DbCharacter>,
    motion: u8,
    direction: u16,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterMove);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u8(motion);
    packet.write_u16(direction);
    packet.write_f32(character.map_or(SPAWN_X, |character| character.pos_x));
    packet.write_f32(character.map_or(SPAWN_Y, |character| character.pos_y));
    packet.write_f32(character.map_or(SPAWN_Z, |character| character.pos_z));
    packet.into_bytes()
}

/// MotionType values (shaiya/include/common/UserTypes.h) relevant to
/// sit/stand/jump, sent via the dedicated 0x0506 CharacterMotion opcode
/// (GameCharMotionIncoming/Outgoing, game/{incoming,outgoing}/0500.h) --
/// distinct from 0x0501 CharacterMove, which always carries a position.
// Not referenced directly: jump is relayed as a raw motion byte with no
// server-side branching needed, unlike sit/stand. Kept named for reference
// against the MotionType enum.
#[allow(dead_code)]
pub(crate) const MOTION_JUMP: u8 = 8;
pub(crate) const MOTION_SIT_DOWN: u8 = 10;
pub(crate) const MOTION_STAND_UP: u8 = 11;

/// Builds a 0x0506 CharacterMotion packet (objectId + motionType). Used to
/// relay sit/stand/jump (and any other one-shot or toggled motion) to
/// other players; see chat.rs's ConnectedPlayer::sitting for how sit state
/// is cached so a newly arriving viewer sees an already-seated player
/// correctly.
pub(crate) fn build_character_motion(character_id: u32, motion: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterMotion);
    packet.write_u32(character_id);
    packet.write_u8(motion);
    packet.into_bytes()
}

/// GameUserLeaveWorldOutgoing (0x0107): ack for the client's leave-world
/// request (same opcode incoming, GameCharLeaveWorldIncoming). The client's
/// "relog" button sends this same request; without this ack the client had
/// nothing to wait for and crashed instead of returning to the login screen.
pub(crate) fn build_leave_world_ack(character_id: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::Logout);
    packet.write_u32(character_id);
    packet.into_bytes()
}

pub(crate) fn equipped_item(items: &[DbCharacterItem], slot: u8) -> Option<&DbCharacterItem> {
    items.iter().find(|item| item.bag == 0 && item.slot == slot)
}

pub(crate) fn write_equipment_type(packet: &mut PacketWriter, item: Option<&DbCharacterItem>) {
    if let Some(item) = item {
        packet.write_bytes(&[item.item_type, item.type_id, 0]);
    } else {
        packet.write_bytes(&[0, 0, 0]);
    }
}

pub(crate) fn build_character_equipment(
    character: &DbCharacter,
    items: &[DbCharacterItem],
    slot: u8,
) -> Result<Vec<u8>> {
    let item = equipped_item(items, slot);
    let mut packet = PacketWriter::new(Opcode::CharacterEquipment);
    packet.write_u32(character.id);
    packet.write_u8(slot);

    if let Some(item) = item {
        packet.write_u8(item.item_type);
        packet.write_u8(item.type_id);
        packet.write_u8(0); // enchant step
        if slot == 7 {
            for gem in item.gems {
                packet.write_u8(gem);
            }
        }
    } else {
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u8(0);
        if slot == 7 {
            for _ in 0..6 {
                packet.write_u8(0);
            }
        }
    }

    packet.into_bytes()
}

/// `motion` is the shape struct's own motion byte, applied at spawn time.
/// Earlier revisions of this project passed MOTION_SIT_DOWN here to show an
/// already-seated character as sitting to a newly arriving viewer -- that
/// turned out to be backwards. A character already sitting when a new
/// viewer's client first learns about them needs plain motion (0) in the
/// shape/move packets, with sit conveyed ONLY by a separate, standalone
/// 0x0506 sent right after (see chat.rs's ConnectedPlayer::shape_packet/
/// move_packet/sit_motion_packet). That matches the one case that always
/// worked -- sitting down live in front of an already-known viewer, which
/// is exactly a standalone 0x0506 against an object the client already has
/// -- instead of fighting it.
pub(crate) fn build_character_shape(
    character: Option<&DbCharacter>,
    selected_faction: u8,
    items: &[DbCharacterItem],
    motion: u8,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterShape);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u8(0); // alive
    packet.write_u8(motion);
    packet.write_u8(selected_faction);

    if let Some(character) = character {
        packet.write_u8(character.race);
        packet.write_u8(character.hair);
        packet.write_u8(character.face);
        packet.write_u8(character.height);
        packet.write_u8(character.character_class);
        packet.write_u8(character.gender);
        packet.write_u8(0); // party marker
        packet.write_u8(character.mode);
    } else {
        packet.write_u8(0); // race
        packet.write_u8(0); // hair
        packet.write_u8(0); // face
        packet.write_u8(0); // height
        packet.write_u8(0); // class
        packet.write_u8(0); // gender
        packet.write_u8(0); // party marker
        packet.write_u8(0); // mode
    }

    packet.write_u32(0); // kills
    for slot in 0..8 {
        write_equipment_type(&mut packet, equipped_item(items, slot));
    }
    packet.write_fixed_string(character_name(character), 21)?;
    if let Some(cloak) = equipped_item(items, 7) {
        for gem in cloak.gems {
            packet.write_u8(gem);
        }
    } else {
        for _ in 0..6 {
            packet.write_u8(0); // cloak info
        }
    }
    packet.write_fixed_string("", 25)?; // guild name
    packet.into_bytes()
}

/// Opaque D3DCOLOR (0xAARRGGBB). The exact shade doesn't matter, only that
/// it's visibly different from normal.
pub(crate) const NAME_COLOR_NORMAL: u32 = 0xFFFFFFFF;
pub(crate) const NAME_COLOR_GM: u32 = 0xFF3B9EFF;

pub(crate) fn name_color_for(is_admin: bool) -> u32 {
    if is_admin {
        NAME_COLOR_GM
    } else {
        NAME_COLOR_NORMAL
    }
}

/// 0x0240 GameCharacterOverlayOutgoing. Aries's real mechanism for a
/// cosmetic item (ItemEffect::CharacterNameColor) to change a character's
/// name color for other players -- reused here as the vehicle for "GMs
/// show a different name color than everyone else". `flags` is a bitmask
/// (NameColor=2, Title=4, Effect=8); only NameColor is set here.
pub(crate) fn build_character_overlay(character_id: u32, name_color: u32) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterOverlay);
    packet.write_u32(character_id);
    packet.write_u8(1); // visible
    packet.write_u32(2); // flags: NameColor
    packet.write_u32(name_color);
    packet.write_u32(0); // titleColor
    packet.write_i32(0); // effectId
    packet.write_i32(0); // effectSubId
    packet.write_fixed_string("", 32)?; // title
    packet.into_bytes()
}

pub(crate) fn send_character_stat_packets(
    stream: &mut TcpStream,
    aes: &mut AesCtr,
    use_expanded_send_key: bool,
    character: Option<&DbCharacter>,
    stats: &CharacterStats,
    logger: &Logger,
) -> Result<()> {
    let packets = [
        (
            build_character_additional_stats(stats)?,
            "0x0526 additional stats",
        ),
        (build_max_hp_mp_sp(character, stats)?, "0x051F max hp/mp/sp"),
        (
            build_max_energy(character, 0, stats.max_hp)?,
            "0x050B max hp",
        ),
        (
            build_max_energy(character, 1, stats.max_sp)?,
            "0x050B max sp",
        ),
        (
            build_max_energy(character, 2, stats.max_mp)?,
            "0x050B max mp",
        ),
        (build_current_hitpoints(stats)?, "0x0521 current hp/mp/sp"),
    ];

    for (packet, label) in packets {
        send_encrypted(stream, aes, use_expanded_send_key, packet, label, logger)?;
    }

    Ok(())
}

pub(crate) fn build_empty_character_list_slot(slot: u8) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterList);
    packet.write_u8(slot);
    packet.write_u32(0);
    packet.into_bytes()
}

pub(crate) fn build_character_list_slot(
    character: &DbCharacter,
    items: &[DbCharacterItem],
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterList);
    packet.write_u8(character.slot);
    packet.write_u32(character.id);
    packet.write_u32(0);
    packet.write_u16(character.level);
    packet.write_u8(character.race);
    packet.write_u8(character.mode);
    packet.write_u8(character.hair);
    packet.write_u8(character.face);
    packet.write_u8(character.height);
    packet.write_u8(character.character_class);
    packet.write_u8(character.gender);
    packet.write_u16(character.map_id);

    packet.write_u16(character.strength);
    packet.write_u16(character.reaction);
    packet.write_u16(character.intelligence);
    packet.write_u16(character.wisdom);
    packet.write_u16(character.dexterity);
    packet.write_u16(character.luck);

    packet.write_u16(character.max_hp as u16);
    packet.write_u16(character.max_mp as u16);
    packet.write_u16(character.max_sp as u16);

    for slot in 0..8 {
        packet.write_u8(
            equipped_item(items, slot)
                .map(|item| item.item_type)
                .unwrap_or_default(),
        );
    }
    for slot in 0..8 {
        packet.write_u8(
            equipped_item(items, slot)
                .map(|item| item.type_id)
                .unwrap_or_default(),
        );
    }

    packet.write_fixed_string(&character.name, 19)?;
    packet.write_u8(0);
    packet.write_u8(0);

    for _ in 0..6 {
        packet.write_u8(0);
    }

    packet.into_bytes()
}

pub(crate) fn send_character_list(
    stream: &mut TcpStream,
    aes: &mut AesCtr,
    use_expanded_send_key: bool,
    characters: &[(DbCharacter, Vec<DbCharacterItem>)],
    logger: &Logger,
) -> Result<()> {
    for slot in 0..MAX_CHARACTER_SLOTS {
        if let Some((character, items)) = characters.iter().find(|(c, _)| c.slot == slot) {
            send_encrypted(
                stream,
                aes,
                use_expanded_send_key,
                build_character_list_slot(character, items)?,
                "0x0101 character slot",
                logger,
            )?;
            continue;
        }

        send_encrypted(
            stream,
            aes,
            use_expanded_send_key,
            build_empty_character_list_slot(slot)?,
            "0x0101 empty character slot",
            logger,
        )?;
    }

    Ok(())
}
