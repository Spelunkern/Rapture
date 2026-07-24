use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Opcode {
    CharacterList = 0x0101,
    CreateCharacter = 0x0102,
    /// Client -> server: delete (soft) the character in the selection screen.
    /// Confirmed via capture: request body `charId:u32`; response
    /// `result:u8 (0=ok), charId:u32`. The character enters a "pending
    /// deletion" state (still shown in the list, flagged) and is restorable
    /// for 2 hours before it's purged.
    DeleteCharacter = 0x0103,
    SelectCharacter = 0x0104,
    /// Client -> server: restore a character that's pending deletion (within
    /// the 2h window). Confirmed via capture: same shape as DeleteCharacter --
    /// request `charId:u32`, response `result:u8 (0=ok), charId:u32`.
    RestoreCharacter = 0x010F,
    /// Client -> server: commit a pending rename. The client forces this on
    /// login for a character flagged with `Chars.RenameCnt >= 1` (set by using
    /// a name-change stone, `Items.Special == 51`). Confirmed via capture:
    /// request `charId:u32, newName` (null-padded fixed string); response is a
    /// single `result:u8` -- and note the INVERTED convention vs delete/
    /// restore: here **1 = ok, 0 = fail** (matches Imgeneus's
    /// SendRenamedCharacter). No charId in the response.
    RenameCharacter = 0x010E,
    CharacterDetails = 0x0105,
    CharacterItems = 0x0106,
    Logout = 0x0107,
    CharacterSkills = 0x0108,
    AccountFaction = 0x0109,
    CharacterActiveBuffs = 0x010A,
    CharacterSkillBar = 0x010B,
    CharacterEnteredMap = 0x0201,
    AutoAttackStop = 0x0212,
    SetMoney = 0x0213,
    CharacterOverlay = 0x0240,
    TargetMobGetState = 0x0304,
    TargetMobHpUpdate = 0x0305,
    TargetClear = 0x030A,
    /// Mount toggle, confirmed via a real capture: empty-body request from
    /// the client, `ok:u8, isOnVehicle:u8` (2 bytes) response. Unlike
    /// Imgeneus's reference implementation (which has a separate
    /// `USE_VEHICLE_READY`/summon-cast-time step), this project deliberately
    /// makes mounting instant per the user's explicit design requirement --
    /// no cast-time opcode is implemented.
    UseVehicle = 0x0216,
    /// Shows a debuff icon/highlight on a mob's nameplate. Confirmed via a
    /// real capture of a basic-damage-plus-DoT skill hitting a mob:
    /// `debuffType:u8, mobId:u32, skillId:u16, skillLevel:u8` (8 bytes),
    /// broadcast to everyone near the mob (not just the caster). Only
    /// `debuffType == 0x02` has been observed so far (this skill's only
    /// debuff category); other values presumably exist for stun/silence/etc,
    /// and eventually for player-targeted debuffs (not modeled yet).
    MobDebuffAdd = 0x030C,
    /// Clears the icon added by `MobDebuffAdd` -- identical shape, sent
    /// when the underlying DoT/debuff expires (confirmed via the same
    /// capture: this arrived just before the DoT's final damage tick).
    MobDebuffRemove = 0x030D,
    ItemBagToBag = 0x0204,
    AddItem = 0x0205,
    RemoveItem = 0x0206,
    SetStatPoint = 0x0208,
    /// Stat-reset (respec) result, sent when a reset-stats stone
    /// (`Items.Special == 34`) is used. Confirmed shape (via capture +
    /// Imgeneus's SendResetStats): `success:u8, statPoint:u16, str:u16,
    /// rec:u16, int:u16, wis:u16, dex:u16, luc:u16` -- the six base stats
    /// reset to 10 and the refunded point pool. Followed by 0x050B max-energy
    /// updates and a 0x0521 current-hitpoints refresh (REC/WIS/DEX feed the
    /// derived max hp/mp/sp, which drop on reset).
    StatsReset = 0x0214,
    LearnNewSkill = 0x0209,
    CharacterEnteredPortal = 0x020A,
    CharacterMapTeleport = 0x020B,
    UserKillCountUpdate = 0x020E,
    CharacterShape = 0x0303,
    /// Change-appearance / change-sex consumable result. This client sends no
    /// packet when the item is *used* (it just opens the styling UI); on
    /// confirm it sends this with the chosen look. Confirmed via capture:
    /// request `bag:u8, slot:u8, hair:u8, face:u8, height:u8, sex:u8`;
    /// broadcast reply `charId:u32, hair:u8, face:u8, height:u8, sex:u8`.
    /// The item's `Items.Special` decides scope: 44 = appearance only
    /// (hair/face/height), 45 = also sex. Sex: 0 = male, 1 = female.
    ChangeAppearance = 0x0226,
    WorldDay = 0x0404,
    CharacterMove = 0x0501,
    /// Restores a character's current HP/MP/SP as part of the resurrection
    /// sequence (confirmed via a real revive capture): `charId:u32, hp:u32,
    /// mp:u32, sp:u32` (18 bytes). Distinct from `CharacterCurrentHitpoints`
    /// (0x0521, which carries no charId) and `CharacterMaxHpMpSp` (0x051F);
    /// this is the revive-specific full-restore.
    CharacterReviveHitpoints = 0x0505,
    CharacterUserAutoAttack = 0x0502,
    CharacterMobAutoAttack = 0x0503,
    /// A character's death, broadcast when they're killed. Confirmed via a
    /// real capture of a player killed by a mob: `charId:u32, deathType:u8,
    /// killerId:u32` (9 bytes). `deathType == 0x02` was the observed value
    /// for a mob kill; other values presumably exist for PvP/other causes.
    CharacterDeath = 0x0504,
    CharacterMotion = 0x0506,
    CharacterEquipment = 0x0507,
    MapAddItem = 0x0401,
    MapRemoveItem = 0x0402,
    /// First packet the server sends in the resurrection sequence (confirmed
    /// via a real capture of clicking "revive" while dead): `charId:u32`
    /// (6 bytes). Exact purpose unconfirmed -- likely clears the client's
    /// death overlay before the respawn teleport/stat-restore that follow.
    CharacterReviveInit = 0x0406,
    MobEnter = 0x0601,
    MobLeave = 0x0602,
    MobMove = 0x0603,
    /// A mob's basic auto-attack against a player -- the mob->player mirror
    /// of `CharacterMobAutoAttack` (0x0503). Confirmed via a real capture of
    /// a mob fighting back: `result:u8, mobId:u32, charId:u32, damageHp:u16,
    /// damageSp:u16, damageMp:u16` (13 bytes), same field layout as 0x0503
    /// but with the mob as attacker and the character as victim (mobId
    /// before charId). `result == 0` is a normal hit; `result == 1` is the
    /// lethal blow (the capture's killing hit carried `01`).
    MobAttackCharacter = 0x0605,
    MobDeath = 0x0606,
    /// One periodic-damage (DoT) tick against a mob, broadcast to everyone
    /// nearby -- confirmed via capture: `mobId:u32, skillId:u16,
    /// skillLevel:u8, damageHp:u16, damageSp:u16, damageMp:u16` (13 bytes).
    /// Distinct from `CharacterMobAutoAttack` (0x0503, a live swing) since a
    /// DoT tick isn't tied to an attacker's swing and needs to carry which
    /// skill is ticking. Mirrors this project's own
    /// `DbSkillDefinition::time_damage_hp/sp/mp` split; only `damageHp` is
    /// ever nonzero today (mobs have no SP/MP), but the wire shape has all
    /// three slots.
    MobPeriodicDamage = 0x0607,
    /// Broadcasts a mob's current HP to everyone nearby -- confirmed via
    /// capture: `mobId:u32, currentHp:u32` (8 bytes). Keeps onlookers'
    /// health bars in sync without them needing their own target window
    /// open on that mob (unlike `TargetMobHpUpdate`/0x0305).
    MobCurrentHitpoints = 0x0610,
    MapNpcEnter = 0x0E01,
    MapNpcLeave = 0x0E02,
    MapNpcMove = 0x0E03,
    UseItem = 0x050A,
    CharacterMaxEnergy = 0x050B,
    /// "USED_SP_MP" -- notifies the client of a skill's own MP/SP cast
    /// cost, separate from the skill-use ack (which never carries it).
    /// Confirmed against real captures cross-referenced with `Skills.MP`.
    SkillUsedSpMp = 0x050C,
    /// "CHARACTER_SKILL_CASTING" -- sent once when a skill with a nonzero
    /// `ReadyTime` (cast time) begins casting, before the delayed result.
    /// Confirmed against a real capture and Imgeneus's `PacketType.
    /// CHARACTER_SKILL_CASTING`.
    SkillCastStarted = 0x0510,
    /// "BUFF_ADD" per Imgeneus / "TP_PC_SKILL_SET" per Aries -- confirmed
    /// against a real capture (a live "Protective Aura" buff decoded at
    /// login) to match Imgeneus's shape exactly: `id:u32, skillId:u16,
    /// skillLevel:u8, countdownSeconds:u32` (11 bytes), not Aries's
    /// shorter 7-byte guess.
    BuffAdd = 0x050D,
    /// "BUFF_REMOVE" per Imgeneus / "TP_PC_SKILL_REMOVE" per Aries -- both
    /// agree on a bare `objectId:u32` payload for this one.
    BuffRemove = 0x050E,
    /// "TP_SKILL_KEEP" (Aries) / "CHARACTER_SKILL_KEEP" (Imgeneus) --
    /// notifies the client of one periodic tick of an ALREADY-ACTIVE
    /// effect (e.g. a heal-over-time buff), as opposed to a fresh skill
    /// use. Confirmed necessary: re-sending the skill-use ack (0x0511/
    /// 0x0517) on every tick instead of this dedicated opcode made the
    /// client visibly treat each tick as the skill being automatically
    /// re-cast. Shape per Aries's `GameUserSkillKeepOutgoing`: objectId:
    /// u32, skillId:u16, skillLevel:u8, health:u16, mana:u16, stamina:u16
    /// (Imgeneus's own `SendSkillKeep` writes HP/SP/MP in that order
    /// instead of HP/MP/SP -- Aries's order used here per this project's
    /// existing preference for Aries when the two disagree).
    SkillKeep = 0x050F,
    CharacterMaxHpMpSp = 0x051F,
    CharacterCurrentHitpoints = 0x0521,
    CharacterAdditionalStats = 0x0526,
    TeleportSavePositionList = 0x0225,
    BlessInit = 0x0211,
    QuestList = 0x0901,
    QuestFinishedList = 0x0906,
    ChatNormal = 0x1101,
    ChatWorld = 0x1103,
    ChatError = 0x1106,
    ChatMap = 0x1111,
    WarehouseItemList = 0x0711,
    ShopSell = 0x0703,
    SkillUseNoTarget = 0x0511,
    /// Client -> server: the player clicked "revive" while dead. Confirmed
    /// via a real capture: 1-byte body (`0x02` observed -- likely the
    /// respawn-location choice, e.g. revive in town). The server replies
    /// with the resurrection sequence (0x0406, area reload, 0x0551, 0x0505).
    ResurrectRequest = 0x0553,
    /// Server -> client: repositions a resurrected character at their respawn
    /// point (confirmed via a real revive capture): `charId:u32`, then a
    /// 5-byte blob `04 46 AF 37 01` whose meaning isn't confirmed (replicated
    /// verbatim -- possibly a motion/state id plus a timestamp), then
    /// `x:f32, y:f32, z:f32`.
    CharacterResurrect = 0x0551,
    SkillUse = 0x0517,
    SkillExtraDamage = 0x051A,
    /// Broadcasts a character's mount shape, confirmed via a real capture:
    /// `characterId:u32, shape:u8` (5 bytes) -- notably NO model/mount-type
    /// fields; the user confirmed the client resolves the actual 3D model
    /// from the character's own equipped mount item (slot 13), not from
    /// this packet. Distinct from `CharacterShape`/0x0303 (the full player
    /// appearance packet, already confirmed player-specific and unrelated).
    CharacterMountShape = 0x051D,
    /// Sent alongside `CharacterDeath` (0x0504) when a player dies -- exact
    /// meaning of the trailing bytes not yet confirmed. Captured payload from
    /// a real mob kill: `charId:u32, 0x07, 0x02` (6 bytes). The two trailing
    /// bytes are replicated verbatim until a capture pins down what they
    /// encode (likely a death/animation state).
    CharacterDeathState = 0x051C,
    AccountPoints = 0x2601,
    PointBuy = 0x2602,
    /// Item-mall gift ("TPC/TP_POINT_GIFT_SEND" per Aries 2600.h): buy a
    /// product for another character, delivered to their gift box
    /// (PS_Billing.dbo.UsersProduct3) -- they need not be online. Request:
    /// `productCode:String<21>, targetName:String<21>` (character name, not
    /// account). Response: `result:u8, targetName:String<21>,
    /// remainingPoints:u32, log{productCode:String<21>, purchaseDate:u32,
    /// purchasePoints:u32}`. Result codes per Aries's GamePointGiftSendResult:
    /// 0=Success, 1=CharNameDoesNotExist, 2=SameAccount, 3=Full,
    /// 4=NotEnoughSlots, 5=InsertError, 6=ProductCodeDoesNotExist,
    /// 7=PointUpdateError, 8=LogError, 9=ProcessBusy, 10=NotEnoughPoints.
    PointGiftSend = 0x2603,
    /// Gift-box contents push ("TP_POINT_STORED_ITEMLIST" per Aries 2600.h):
    /// `itemCount:u8` + up to 240 x `{npcSlot:u8, type:u8, typeId:u8,
    /// count:u8}`. Server -> client only (no request opcode exists); sent with
    /// the initial item-mall state and re-sent after a gift is claimed.
    PointStoredItemList = 0x2606,
    /// Claim one gift from the box ("TPC/TP_POINT_STORED_ITEMGET" per Aries
    /// 2600.h): request `npcSlot:u8`; response `npcSlot:u8, bag:u8, slot:u8`
    /// -- where the claimed item landed in the character's inventory
    /// (confirmed via capture `07 26 00 01 0F` = slot 0 claimed into bag 1
    /// slot 15; the client already knows the item type/count from the 0x2606
    /// list, so no AddItem packet is involved).
    PointStoredItemGet = 0x2607,
    CashPoint = 0x2605,
    BankItemList = 0xB101,
    LoginHandshake = 0xA101,
    LoginRequest = 0xA102,
    ServerList = 0xA201,
    SelectServer = 0xA202,
    GameHandshake = 0xA301,
    Ping = 0xA303,
    ChatNormalAdmin = 0xF101,
    ChatWorldAdmin = 0xF103,
    GmCommandError = 0xF501,
    GmCommandGetItem = 0xF702,
    /// Item recreation ("TPC/TP_ITEM_COMPOSE" per Aries 0800.h, EP5 variant):
    /// a recreation rune (`Items.Special == 62`) rerolls the orange
    /// (craftname) bonus stats of an eligible item. Request: `runeBag:u8,
    /// runeSlot:u8, itemBag:u8, itemSlot:u8`. Response: `result:u8
    /// (0=Success, 1=Failure), bag:u8, slot:u8, craftName:char[21]`.
    /// Eligibility/limits come from the item's own row in Items: `ReqWis` =
    /// max value of each rolled bonus, `Server` = how many bonuses get
    /// rolled. Weapons (Type 1-15, 45-65) and jewelry (Type 22/23/40) never
    /// roll HP/MP/SP, only the six stats -- armor and capes roll the full
    /// 9-stat pool (see rapture_world::compose, confirmed against retail).
    /// Enchant/lapisia operation ("TPC/TP_LAPISIAN_ADD" per Aries 0800.h, EP5
    /// variant): burns one lapisia + one enchant material to raise an
    /// eligible weapon/armor's enchant level (the craftname's last pair, 0-20
    /// max) by 1, 100% success rate (per project design -- no failure/
    /// downgrade chance modeled). Eligibility: the target's `Items.ReqLuc` =
    /// the required material's item id (`Type*1000+TypeID`), 0 = not
    /// enchantable (jewelry/capes). The lapisia must be Type 95, TypeID 1-5
    /// for a weapon target or 6-10 for an armor target (see
    /// rapture_world::enchant). Both the lapisia and the material are
    /// consumed by exactly 1, regardless of how many the client places
    /// (confirmed via a real capture, whose response's own remaining-count
    /// fields for both items each dropped by exactly 1).
    ///
    /// Request: `lapisiaBag:u8, lapisiaSlot:u8, materialBag:u8,
    /// materialSlot:u8, materialCount:u8 (ignored -- always consumes 1),
    /// itemBag:u8, itemSlot:u8`.
    ///
    /// Response: `result:u8 (0=Failure, 1=Success -- INVERTED vs
    /// ItemCompose's 0=Success), lapisiaBag:u8, lapisiaSlot:u8,
    /// lapisiaCount:u8 (remaining), materialBag:u8, materialSlot:u8,
    /// materialCount:u8 (remaining), itemBag:u8, itemSlot:u8,
    /// remainingMoney:u32, craftName:char[21]`.
    EnchantAdd = 0x0805,
    ItemCompose = 0x0806,
    /// Gem/lapis link ("GEM_ADD" per Imgeneus's numbering, undocumented in
    /// Aries's own headers -- decoded purely from two real captures, one
    /// success and one failure, cross-checked against each other). Request:
    /// `gemBag:u8, gemSlot:u8, itemBag:u8, itemSlot:u8, hammerBag:u8,
    /// hammerSlot:u8` (hammer unused by this project -- always 0,0).
    /// Response: `result:u8 (0=Failure, 1=Success -- same inverted
    /// convention as EnchantAdd), gemBag:u8, gemSlot:u8,
    /// gemRemainingCount:u8, itemBag:u8, itemSlot:u8, gemPosition:u8 (0-5,
    /// which socket), gemTypeId:u8, reserved:u32(=0), remainingGold:u32,
    /// reserved:u16(=0)` (18 bytes).
    GemAdd = 0x0801,
    /// Gem/lapis extraction ("GEM_REMOVE"). This project only implements the
    /// hammerless "general" extraction (removes EVERY socketed gem at once);
    /// extracting a single specific gem needs a Linking Hammer item, not yet
    /// modeled. Request: `itemBag:u8, itemSlot:u8, shouldRemoveSpecific:u8
    /// (always 0 here), gemPosition:u8 (unused, 0), hammerBag:u8,
    /// hammerSlot:u8` (matches Imgeneus's own `RemoveGem` signature).
    /// Response (32 bytes, decoded by diffing a 1-gem success against a
    /// 3-gem failure): `result:u8 (0=Failure, 1=Success), itemBag:u8,
    /// itemSlot:u8, sentinel:u8(=0xFF, constant), removed:[u8;6]` (1 per
    /// socket 0-5), `gemTypeId:[u8;6]`, `newItemBag:[u8;6]`,
    /// `newItemSlot:[u8;6]` (last three arrays only meaningful where
    /// `removed[i]==1`), `remainingGold:u32`.
    GemRemove = 0x0802,
    /// Pre-link rate/cost query ("GEM_ADD_POSSIBILITY"): same request shape
    /// as `GemAdd`. Response (13 bytes) decoded by diffing TWO real
    /// captures against different gem/item pairs (rate=0%/gold=4075 vs
    /// rate=31%/gold=22945): `result:u8(=1), reserved:[u8;5](=0),
    /// flags:u8` (varied per pairing, exact meaning unconfirmed -- always
    /// observed with bit7 set), `ratePercentEncoded:u8` (confirmed exact:
    /// `percent = byte >> 1` -- 0x3F=63 -> 31 matched precisely),
    /// `reserved2:u8(=0x40 constant in both captures -- NOT zero, an
    /// earlier all-zero response here likely caused a real client crash)`,
    /// `costGold:u32`. This project always forces 100% / free (no chance
    /// roll, no item-break, per explicit design) by writing
    /// `ratePercentEncoded = 100 << 1` and `costGold = 0`, but otherwise
    /// replicates the confirmed-constant bytes (flags, reserved2) as
    /// observed, rather than zeroing them.
    GemAddPossibility = 0x0809,
    /// Pre-extraction rate/cost query ("GEM_REMOVE_POSSIBILITY"), same
    /// request shape as `GemRemove` and same 13-byte response shape/
    /// treatment as `GemAddPossibility` (confirmed identical length via
    /// capture; field semantics assumed shared, not independently diffed
    /// for extraction specifically).
    GemRemovePossibility = 0x080A,
    ExchangeRequest = 0x0A01,
    ExchangeAnswer = 0x0A02,
    ExchangeStart = 0x0A03,
    ExchangeEnd = 0x0A04,
    ExchangeReady = 0x0A05,
    ExchangeAdd = 0x0A06,
    ExchangeRemove = 0x0A07,
    ExchangeAddMoney = 0x0A08,
    ExchangeAddDest = 0x0A09,
    ExchangeConfirm = 0x0A0A,
}

pub struct PacketWriter {
    data: Vec<u8>,
}

impl PacketWriter {
    pub fn new(opcode: Opcode) -> Self {
        let mut writer = Self { data: Vec::new() };
        writer.write_u16(0);
        writer.write_u16(opcode as u16);
        writer
    }

    pub fn write_u8(&mut self, value: u8) {
        self.data.push(value);
    }

    pub fn write_u16(&mut self, value: u16) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_f32(&mut self, value: f32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_bytes(&mut self, value: &[u8]) {
        self.data.extend_from_slice(value);
    }

    pub fn write_fixed_string(&mut self, value: &str, size: usize) -> Result<()> {
        let bytes = value.as_bytes();
        if bytes.len() > size {
            bail!("fixed string value is too large");
        }

        self.data.extend_from_slice(bytes);
        self.data.resize(self.data.len() + (size - bytes.len()), 0);
        Ok(())
    }

    pub fn into_bytes(mut self) -> Result<Vec<u8>> {
        if self.data.len() > u16::MAX as usize {
            bail!("packet is too large");
        }

        let length = (self.data.len() as u16).to_le_bytes();
        self.data[0] = length[0];
        self.data[1] = length[1];
        Ok(self.data)
    }
}

pub fn read_le_u16(bytes: &[u8]) -> Result<u16> {
    if bytes.len() < 2 {
        bail!("not enough bytes for u16");
    }

    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

pub fn read_le_u32(bytes: &[u8]) -> Result<u32> {
    if bytes.len() < 4 {
        bail!("not enough bytes for u32");
    }

    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

pub fn read_fixed_ascii(bytes: &[u8]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).to_string()
}
