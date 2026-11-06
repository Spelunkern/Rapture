# Original parity

Rapture follows the original EP4.5 server. The rules come from
`ps_game.exe` (shipped with its PDB, so functions keep their names), read
through the [Eternity](https://github.com/Spelunkern/Eternity) reconstruction
or directly in Ghidra. Packet layouts come from the 5.4 client's readers
(shaiya-client-decomp), because Eternity targets the EP4 client and some
fields are narrower there.

Status: **done** matches the original; **partial** follows it with the noted
gaps; **invented** is still a Rapture rule to replace.

| System | Original | Status |
|---|---|---|
| Start position, rebirth points | `Map.ini`, `CMap::LoadGameMapConfig` 0x004270a0 | done |
| Character creation | `CUser::ProcessCharacterCreate` 0x00480e90, `CGameData::LoadBaseItem` 0x004bf800 | done |
| Base HP/MP/SP | `CUser::InitEquipment` 0x00454560, `StatusDefs` | done |
| Attack, defense | `CUser::SetAttack` 0x00454280 | partial: no accuracy, evasion, critical or ranged formula yet |
| Level-up | stat/skill points by mode, +1 main stat | done |
| Exp per kill | `CMob::GetExp` 0x00491690 | done (no event multiplier) |
| Recovery | 5 s tick, 5% sitting, none in combat | partial: no equipment/skill recovery bonuses |
| Lapisia enchant | `ItemEnchant.ini` | partial: gold cost to be configured |
| Hit chance | `CUser::AttackSuccess` / `CMob::AttackSuccess` | invented: every hit lands |
| Damage | `CUser::AttackDamage` 0x0044d540, `CMob::AttackDamage` 0x004a2920, element table `CGameData::m_AttackAttr` 0x00542260 | invented: average attack, mob attack minus defense |
| Mob stats | `Mobs` Dex/Wis/Luc/Defense/Magic/Attrib, three attack slots | invented: only Attack1 |
| Mob movement | `NormalTime/NormalStep/ChaseTime/ChaseStep` | invented: fixed steps |
| Mob respawn | `Attackspecial3`, random ±60 min for selector ≥ 9 | partial |
| Drops | `MobItems` orders 1..9 (1-6 per cent, 7-9 per million), grade pick with `Drop` limits, `SequenceItemCreate.ini`, 30% gold `Money1 + rand(Money2)` | invented: order 0 counted, 25% gold |
| Quest item drops | `Mobs.QuestItemID` | missing |
| Equip requirements | level, class, stats, country | missing |
| NPC shop buy | `NpcQuest.SData` | missing |
| Party, quests, guilds | `0x0Bxx`, `0x09xx`, `0x0Dxx` | missing |

## Element table

`CGameData::m_AttackAttr` (attacker attribute × defender attribute):

```
1.0 0.8 0.8 0.8 0.8 0.7 0.7 0.7 0.7
1.2 1.0 0.5 1.0 1.4 1.0 0.4 1.0 1.3
1.2 1.4 1.0 0.5 1.0 1.3 1.0 0.4 1.0
1.2 1.0 1.4 1.0 0.5 1.0 1.3 1.0 0.4
1.2 0.5 1.0 1.4 1.0 0.4 1.0 1.3 1.0
1.3 1.0 0.5 1.0 1.6 1.0 0.5 1.0 1.4
1.3 1.6 1.0 0.5 1.0 1.4 1.0 0.5 1.0
1.3 1.0 1.6 1.0 0.5 1.0 1.4 1.0 0.5
1.3 0.5 1.0 1.6 1.0 0.5 1.0 1.4 1.0
```

## Reading the original

Ghidra 12.1.3 opens Eternity's project read-only:

```sh
export JAVA_HOME=~/.cache/phoenix-reverse/ghidra/jdk-21.0.12.1+1
~/.cache/phoenix-reverse/ghidra/ghidra_12.1.3_PUBLIC/support/analyzeHeadless \
  ~/Projects/Eternity/work/ghidra-12.1.3 Eternity -process ps_game.exe \
  -readOnly -noanalysis -scriptPath tools/ghidra -postScript Decomp.java "CUser::AttackDamage"
```
