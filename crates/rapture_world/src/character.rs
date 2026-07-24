use anyhow::Result;
use rapture_common::crypto::AesCtr;
use rapture_common::db::{self, DbCharacter, DbCharacterItem, DbConsumableEffect, DbEquipmentStats};
use rapture_common::packet::{Opcode, PacketWriter};
use std::net::TcpStream;

use crate::session::*;

pub(crate) const CHARACTER_ID: u32 = 1;
pub(crate) const MAX_HP: u32 = 100;
pub(crate) const MAX_MP: u32 = 100;
pub(crate) const MAX_SP: u32 = 100;
/// Base max HP/MP/SP granted per character level, hardcoded and identical
/// for every class -- replaces the old per-(level, job) PS_GameDefs.dbo.
/// StatusDefs lookup. See `base_pool_for_level`.
pub(crate) const BASE_POOL_PER_LEVEL: u32 = 100;

/// Hardcoded base max HP (= base max MP = base max SP) for a character of the
/// given level: a flat `BASE_POOL_PER_LEVEL` (100) per level, the same for
/// all classes (level 1 = 100, level 60 = 6000). The stat-derived bonus
/// (REC/WIS/DEX) is added on top in `CharacterStats::from_character`. No DB
/// involved -- deliberately hardcoded per the project's design.
pub(crate) fn base_pool_for_level(level: u16) -> u32 {
    u32::from(level.max(1)).saturating_mul(BASE_POOL_PER_LEVEL)
}
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
/// `Items.Special` for the change-appearance consumable: lets the player
/// restyle hair/face/height (see `Opcode::ChangeAppearance`).
pub(crate) const SPECIAL_CHANGE_APPEARANCE: u8 = 44;
/// `Items.Special` for the change-sex consumable: hair/face/height AND sex.
pub(crate) const SPECIAL_CHANGE_SEX: u8 = 45;
/// `Items.Special` for the name-change stone: flags the character for a
/// forced rename on next login (`Chars.RenameCnt = 1`), no immediate effect.
pub(crate) const SPECIAL_CHANGE_NAME: u8 = 51;
/// `Items.Special` for the reset-stats (respec) stone: base STR/REC/INT/WIS/
/// DEX/LUC return to `BASE_STAT` (10) and the available pool is set to a
/// fresh level-based grant (`STAT_POINTS_PER_LEVEL` per level).
pub(crate) const SPECIAL_RESET_STATS: u8 = 34;
/// Stat points granted per character level on a stat reset (level 60 -> 300,
/// level 10 -> 50).
pub(crate) const STAT_POINTS_PER_LEVEL: u16 = 5;
/// `Items.Special` for the simple recreation rune: rerolls an eligible
/// item's orange (craftname) bonus stats (see `Opcode::ItemCompose`).
pub(crate) const SPECIAL_RECREATION_RUNE: u8 = 62;

// is_weapon_item_type / is_armor_item_type moved to rapture_common::db --
// get_equipment_stats needs them too (weapon/armor enchant bonuses), so the
// single canonical definition lives there now. See db::is_weapon_item_type.
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
    /// Flat, universal damage reduction from enchanted armor (see
    /// `db::ARMOR_ENCHANT_ABSORPTION`) -- sourced straight from
    /// `equipment.absorption`, not scaled by any player stat. Applied in
    /// `combat::apply_incoming_mob_damage`'s final damage formula.
    pub(crate) absorption: u32,
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
        // Base max is a hardcoded flat 100/level for every class (see
        // base_pool_for_level), NOT the character's stored Chars.HP/MP/SP.
        let base_max_hp = character.map_or(MAX_HP, |character| base_pool_for_level(character.level));
        let base_max_mp = character.map_or(MAX_MP, |character| base_pool_for_level(character.level));
        let base_max_sp = character.map_or(MAX_SP, |character| base_pool_for_level(character.level));
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
        // IMPORTANT INVARIANT: `character.current_hp/mp/sp` (the raw DB
        // field) is NOT the effective/displayed value -- it's a BASE value
        // that this function re-adds the equipment/derived bonus to on
        // every call, then clamps to the (also bonus-inclusive) max. Any
        // code that changes current HP/MP/SP -- a heal, a cost deduction, a
        // future player-damage system -- MUST modify the raw field by a
        // DELTA (raw += or raw -= some amount), never by assigning an
        // effective/computed value directly to the raw field. Assigning an
        // already-bonus-inclusive value here would double-count the bonus
        // on the next recompute. The one intentional exception is a full
        // heal (level-up, the `cure` GM command): those deliberately set
        // raw = raw max_hp/mp/sp, relying on this same add-then-clamp to
        // land exactly at the true effective max, matching a real full-heal
        // display without needing to know the equipment bonus in advance.
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
            absorption: equipment.absorption,
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

/// Wire format confirmed against a real capture (fresh login, no leading
/// str/dex/rec/int/wis/luc fields at all -- those are redundant with
/// 0x0526 CharacterAdditionalStats, which already carries them):
/// `stat_point:u16, skill_point:u16, hp:u32, mp:u32, sp:u32` -- verified
/// value-for-value (stat_point=10, skill_point=39, hp=15484, mp=2074,
/// sp=8995 all matched the real client's displayed values for that
/// character). Previously this packet led with 6 placeholder stat fields
/// that don't belong here and hardcoded hp/mp/sp to 0, which shifted every
/// field after it and was the root cause of the "usable points" display
/// never matching the DB (including at fresh login, not just after a live
/// update).
///
/// The remainder (exp, money, position, kills/deaths/victories/defeats,
/// name) is NOT independently confirmed field-by-field -- only the total
/// packet length (63 bytes for that capture) is confirmed, and this
/// ordering fits it exactly. Treat this tail as a best-effort placeholder
/// pending another capture if something in it still displays wrong.
pub(crate) fn build_character_details(
    character: Option<&DbCharacter>,
    stats: &CharacterStats,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::CharacterDetails);
    packet.write_u16(character.map_or(0, |character| character.stat_point));
    packet.write_u16(character.map_or(0, |character| character.skill_point));
    packet.write_u32(stats.current_hp);
    packet.write_u32(stats.current_mp);
    packet.write_u32(stats.current_sp);
    packet.write_u32(character.map_or(0, |character| character.exp)); // current exp
    packet.write_u32(character.map_or(0, |character| character.money)); // gold
    packet.write_f32(character.map_or(SPAWN_X, |character| character.pos_x));
    packet.write_f32(character.map_or(SPAWN_Y, |character| character.pos_y));
    packet.write_f32(character.map_or(SPAWN_Z, |character| character.pos_z));
    packet.write_u32(character.map_or(0, |character| character.kills));
    packet.write_u32(character.map_or(0, |character| character.deaths));
    packet.write_u32(character.map_or(0, |character| character.victories));
    packet.write_u32(character.map_or(0, |character| character.defeats));
    packet.write_fixed_string("", 11)?;

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

/// Builds the 0x0214 StatsReset result. Carries the refunded stat-point pool
/// and the six base stats (10 each after a reset), matching Imgeneus's
/// SendResetStats field order (StatPoint, Str, Rec, Int, Wis, Dex, Luc) --
/// the same order our own character list uses, so the stat sheet updates
/// correctly.
pub(crate) fn build_stats_reset(
    stat_point: u16,
    strength: u16,
    reaction: u16,
    intelligence: u16,
    wisdom: u16,
    dexterity: u16,
    luck: u16,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::StatsReset);
    packet.write_u8(1); // success
    packet.write_u16(stat_point);
    packet.write_u16(strength);
    packet.write_u16(reaction);
    packet.write_u16(intelligence);
    packet.write_u16(wisdom);
    packet.write_u16(dexterity);
    packet.write_u16(luck);
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

/// This byte doubles as the client's spawn-glow control, not just an admin
/// marker (per a GitHub issue report on the public repo): 0 = the character
/// was already on the map (introducing it to a newly arriving viewer), 1 =
/// the character is loading into the map for the first time (its own fresh
/// entry), 2 = admin, regardless of visibility status. Previously hardcoded
/// to 2 unconditionally, which misapplied the admin case to every player.
const ENTERED_MAP_ALREADY_PRESENT: u8 = 0;
const ENTERED_MAP_FIRST_LOAD: u8 = 1;
const ENTERED_MAP_ADMIN: u8 = 2;

pub(crate) fn build_character_entered_map(
    character: Option<&DbCharacter>,
    is_admin: bool,
    first_load: bool,
) -> Result<Vec<u8>> {
    let spawn_marker = if is_admin {
        ENTERED_MAP_ADMIN
    } else if first_load {
        ENTERED_MAP_FIRST_LOAD
    } else {
        ENTERED_MAP_ALREADY_PRESENT
    };
    let mut packet = PacketWriter::new(Opcode::CharacterEnteredMap);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u8(spawn_marker);
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
        // Enchant step: the last craftname pair (see db::DbCraftStats),
        // confirmed via a real capture of a successful enchant -- the
        // server re-sends this same 0x0507 with the new step right after
        // the enchant result, so the client's weapon glow updates live.
        packet.write_u8(
            db::parse_craft_name(&item.craft_name)
                .map(|craft| craft.enchant_level)
                .unwrap_or(0),
        );
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

/// `motion` here is the shape struct's own PERSISTENT motion byte -- unlike
/// the live, one-shot MotionType values sent over 0x0506 CharacterMotion
/// (jump, sit-down, stand-up, etc.), this field only recognizes two states:
/// Standing = 0, Sitting = 1. Any other MotionType value (e.g.
/// MOTION_SIT_DOWN = 10, MOTION_JUMP = 8) is NOT meaningful here and must be
/// treated as Standing -- passing a raw MotionType through unclamped is how
/// characters end up randomly appearing seated. Clamped defensively below
/// rather than trusted from callers.
///
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
/// Builds the 0x0226 ChangeAppearance broadcast: the character's id plus its
/// new hair/face/height/sex, so nearby clients (and the acting client) update
/// the model live. New viewers arriving later get the same look from the
/// regular 0x0303 CharacterShape, since the change is also persisted and the
/// live snapshot updated.
pub(crate) fn build_change_appearance(
    character_id: u32,
    hair: u8,
    face: u8,
    height: u8,
    sex: u8,
) -> Result<Vec<u8>> {
    let mut packet = PacketWriter::new(Opcode::ChangeAppearance);
    packet.write_u32(character_id);
    packet.write_u8(hair);
    packet.write_u8(face);
    packet.write_u8(height);
    packet.write_u8(sex);
    packet.into_bytes()
}

pub(crate) fn build_character_shape(
    character: Option<&DbCharacter>,
    selected_faction: u8,
    items: &[DbCharacterItem],
    motion: u8,
) -> Result<Vec<u8>> {
    let persistent_motion = if motion == 1 { 1 } else { 0 };
    let mut packet = PacketWriter::new(Opcode::CharacterShape);
    packet.write_u32(character.map_or(CHARACTER_ID, |character| character.id));
    packet.write_u8(0); // alive
    packet.write_u8(persistent_motion);
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
    packet.write_u8(u8::from(character.pending_delete)); // 1 = pending deletion (restorable, greyed out)
    packet.write_u8(u8::from(character.rename_pending)); // 1 = forced rename on login (name-change stone used)

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
