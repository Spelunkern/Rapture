-- Creates the three logical databases this project's DB layer expects
-- (mirroring PS_UserData/PS_GameData/PS_GameDefs from SQL Server), and an
-- application user scoped to just those three -- the emulator connects as
-- this user, never as root.
CREATE DATABASE IF NOT EXISTS PS_UserData CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS PS_GameData CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
CREATE DATABASE IF NOT EXISTS PS_GameDefs CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
