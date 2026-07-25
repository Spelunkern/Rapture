-- world.db: live/dynamic state -- accounts, characters, inventory, gift box.
-- Everything a real server actually mutates at runtime. This is the ONLY
-- file an end user needs to back up. Created automatically on first boot if
-- missing; safe to run repeatedly (IF NOT EXISTS everywhere).
--
-- Column names are kept identical to the original SQL Server schema on
-- purpose (same names this project spent a long session cross-checking
-- against a real server capture-by-capture) even though the PS_GameData /
-- PS_UserData / PS_Billing "database" prefixes are gone -- SQLite has one
-- flat namespace per file, so those three now live together in one file.

PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS Users_Master (
    UserUID  INTEGER PRIMARY KEY AUTOINCREMENT,
    UserID   TEXT NOT NULL UNIQUE,
    Pw       TEXT NOT NULL,
    Admin    INTEGER NOT NULL DEFAULT 0,
    AdminLevel INTEGER NOT NULL DEFAULT 0,
    Point    INTEGER NOT NULL DEFAULT 0,
    Leave    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS UserMaxGrow (
    ServerID INTEGER NOT NULL,
    UserUID  INTEGER NOT NULL REFERENCES Users_Master(UserUID),
    Country  INTEGER NOT NULL DEFAULT 2,
    MaxGrow  INTEGER NOT NULL DEFAULT 3,
    Del      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (ServerID, UserUID)
);

CREATE TABLE IF NOT EXISTS Chars (
    CharID     INTEGER PRIMARY KEY AUTOINCREMENT,
    ServerID   INTEGER NOT NULL,
    UserID     TEXT NOT NULL,
    UserUID    INTEGER NOT NULL REFERENCES Users_Master(UserUID),
    CharName   TEXT NOT NULL UNIQUE,
    Slot       INTEGER NOT NULL,
    Family     INTEGER NOT NULL DEFAULT 0,
    Grow       INTEGER NOT NULL DEFAULT 0,
    Hair       INTEGER NOT NULL DEFAULT 0,
    Face       INTEGER NOT NULL DEFAULT 0,
    Size       INTEGER NOT NULL DEFAULT 0,
    Job        INTEGER NOT NULL DEFAULT 0,
    Sex        INTEGER NOT NULL DEFAULT 0,
    Level      INTEGER NOT NULL DEFAULT 1,
    StatPoint  INTEGER NOT NULL DEFAULT 0,
    SkillPoint INTEGER NOT NULL DEFAULT 0,
    Str        INTEGER NOT NULL DEFAULT 10,
    Dex        INTEGER NOT NULL DEFAULT 10,
    Rec        INTEGER NOT NULL DEFAULT 10,
    Int        INTEGER NOT NULL DEFAULT 10,
    Wis        INTEGER NOT NULL DEFAULT 10,
    Luc        INTEGER NOT NULL DEFAULT 10,
    HP         INTEGER NOT NULL DEFAULT 100,
    MP         INTEGER NOT NULL DEFAULT 100,
    SP         INTEGER NOT NULL DEFAULT 100,
    Map        INTEGER NOT NULL DEFAULT 1,
    Dir        INTEGER NOT NULL DEFAULT 0,
    Exp        INTEGER NOT NULL DEFAULT 0,
    Money      INTEGER NOT NULL DEFAULT 0,
    PosX       REAL NOT NULL DEFAULT 0,
    PosY       REAL NOT NULL DEFAULT 0,
    Posz       REAL NOT NULL DEFAULT 0,
    Hg         INTEGER NOT NULL DEFAULT 0,
    Vg         INTEGER NOT NULL DEFAULT 0,
    Cg         INTEGER NOT NULL DEFAULT 0,
    Og         INTEGER NOT NULL DEFAULT 0,
    Ig         INTEGER NOT NULL DEFAULT 0,
    K1         INTEGER NOT NULL DEFAULT 0,
    K2         INTEGER NOT NULL DEFAULT 0,
    K3         INTEGER NOT NULL DEFAULT 0,
    K4         INTEGER NOT NULL DEFAULT 0,
    KillLevel  INTEGER NOT NULL DEFAULT 0,
    DeadLevel  INTEGER NOT NULL DEFAULT 0,
    RegDate    TEXT NOT NULL DEFAULT (datetime('now')),
    RenameCnt  INTEGER NOT NULL DEFAULT 0,
    RemainTime INTEGER NOT NULL DEFAULT 0,
    LoginStatus INTEGER NOT NULL DEFAULT 0,
    Del        INTEGER NOT NULL DEFAULT 0,
    DeleteDate TEXT
);
CREATE INDEX IF NOT EXISTS idx_chars_useruid ON Chars(UserUID);

CREATE TABLE IF NOT EXISTS CharItems (
    RowID     INTEGER PRIMARY KEY AUTOINCREMENT,
    CharID    INTEGER NOT NULL REFERENCES Chars(CharID),
    ItemID    INTEGER NOT NULL DEFAULT 0,
    ItemUID   INTEGER NOT NULL DEFAULT 0,
    Type      INTEGER NOT NULL,
    TypeID    INTEGER NOT NULL,
    Bag       INTEGER NOT NULL,
    Slot      INTEGER NOT NULL,
    Quality   INTEGER NOT NULL DEFAULT 0,
    Gem1      INTEGER NOT NULL DEFAULT 0,
    Gem2      INTEGER NOT NULL DEFAULT 0,
    Gem3      INTEGER NOT NULL DEFAULT 0,
    Gem4      INTEGER NOT NULL DEFAULT 0,
    Gem5      INTEGER NOT NULL DEFAULT 0,
    Gem6      INTEGER NOT NULL DEFAULT 0,
    Craftname TEXT NOT NULL DEFAULT '',
    Count     INTEGER NOT NULL DEFAULT 1,
    Maketime  TEXT NOT NULL DEFAULT (datetime('now')),
    Maketype  TEXT NOT NULL DEFAULT 'S',
    Del       INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_charitems_char ON CharItems(CharID, Del);

CREATE TABLE IF NOT EXISTS UserStoredItems (
    RowID    INTEGER PRIMARY KEY AUTOINCREMENT,
    ServerID INTEGER NOT NULL,
    UserUID  INTEGER NOT NULL REFERENCES Users_Master(UserUID),
    ItemID   INTEGER NOT NULL DEFAULT 0,
    Type     INTEGER NOT NULL,
    TypeID   INTEGER NOT NULL,
    ItemUID  INTEGER NOT NULL DEFAULT 0,
    Slot     INTEGER NOT NULL,
    Quality  INTEGER NOT NULL DEFAULT 0,
    Gem1     INTEGER NOT NULL DEFAULT 0,
    Gem2     INTEGER NOT NULL DEFAULT 0,
    Gem3     INTEGER NOT NULL DEFAULT 0,
    Gem4     INTEGER NOT NULL DEFAULT 0,
    Gem5     INTEGER NOT NULL DEFAULT 0,
    Gem6     INTEGER NOT NULL DEFAULT 0,
    Craftname TEXT NOT NULL DEFAULT '',
    Count    INTEGER NOT NULL DEFAULT 1,
    Maketime TEXT NOT NULL DEFAULT (datetime('now')),
    Maketype TEXT NOT NULL DEFAULT 'S',
    Del      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_storeditems_user ON UserStoredItems(UserUID, Del);

CREATE TABLE IF NOT EXISTS CharQuickSlots (
    CharID    INTEGER NOT NULL REFERENCES Chars(CharID),
    QuickBar  INTEGER NOT NULL,
    QuickSlot INTEGER NOT NULL,
    Bag       INTEGER NOT NULL,
    Number    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_quickslots_char ON CharQuickSlots(CharID);

CREATE TABLE IF NOT EXISTS CharSkills (
    CharID     INTEGER NOT NULL REFERENCES Chars(CharID),
    SkillID    INTEGER NOT NULL,
    SkillLevel INTEGER NOT NULL,
    Number     INTEGER NOT NULL,
    Delay      INTEGER NOT NULL DEFAULT 0,
    CreateTime TEXT NOT NULL DEFAULT (datetime('now')),
    Del        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_charskills_char ON CharSkills(CharID, Del);

CREATE TABLE IF NOT EXISTS CharApplySkills (
    CharID        INTEGER NOT NULL REFERENCES Chars(CharID),
    SkillID       INTEGER NOT NULL,
    SkillLevel    INTEGER NOT NULL,
    LeftResetTime INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_applyskills_char ON CharApplySkills(CharID);

CREATE TABLE IF NOT EXISTS Users_Product3 (
    RowID       INTEGER PRIMARY KEY AUTOINCREMENT,
    UserUID     INTEGER NOT NULL REFERENCES Users_Master(UserUID),
    Slot        INTEGER NOT NULL,
    ItemID      INTEGER NOT NULL,
    ItemCount   INTEGER NOT NULL DEFAULT 1,
    ProductCode TEXT NOT NULL,
    BuyDate     TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_giftbox_user ON Users_Product3(UserUID, Slot);
