use anyhow::{Result, bail};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum Opcode {
    CharacterList = 0x0101,
    CreateCharacter = 0x0102,
    SelectCharacter = 0x0104,
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
    ItemBagToBag = 0x0204,
    AddItem = 0x0205,
    RemoveItem = 0x0206,
    LearnNewSkill = 0x0209,
    CharacterEnteredPortal = 0x020A,
    CharacterMapTeleport = 0x020B,
    UserKillCountUpdate = 0x020E,
    CharacterShape = 0x0303,
    WorldDay = 0x0404,
    CharacterMove = 0x0501,
    CharacterUserAutoAttack = 0x0502,
    CharacterMobAutoAttack = 0x0503,
    CharacterMotion = 0x0506,
    CharacterEquipment = 0x0507,
    MapAddItem = 0x0401,
    MapRemoveItem = 0x0402,
    MobEnter = 0x0601,
    MobLeave = 0x0602,
    MobMove = 0x0603,
    MobDeath = 0x0606,
    MapNpcEnter = 0x0E01,
    MapNpcLeave = 0x0E02,
    MapNpcMove = 0x0E03,
    UseItem = 0x050A,
    CharacterMaxEnergy = 0x050B,
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
    AccountPoints = 0x2601,
    PointBuy = 0x2602,
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
