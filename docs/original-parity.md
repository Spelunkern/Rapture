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
| Attack, defense, accuracy, evasion, critical | `CUser::SetAttack` 0x00454280, `CUser::SetSkillAbility` 0x00485e00, `CUser::AddSkillPassive` 0x00489a80 | done: buff abilities 1-9, 13-18, 20-31; passives detail 1 (weapon mastery, with the original running-total quirk), 4, 8, 9 (cross road), 10 (shield); gem reach/speed; armour (slot 1) element. Abilities 10-12, 19, 32-46 and passive details 2, 5, 6 not modeled |
| Level-up | stat/skill points by mode, +1 main stat | done |
| Exp per kill | `CMob::GetExp` 0x00491690 | done (no event multiplier) |
| Recovery | 5 s tick, 5% sitting, none in combat | partial: no equipment/skill recovery bonuses |
| Lapisia enchant | `ItemEnchant.ini` | partial: gold cost to be configured |
| Hit chance | `CUser::AttackSuccess` 0x0044d010 / `CMob::AttackSuccess` 0x004a23f0 | done: skills check the mob's ResistState/ResistSkill and SuccessType 1-3; the magic term reads WIS on both sides (CUser +0x122c, stMOBInfo +0x3a), SuccessType 3 reads LUC (Eternity labels these DEX). Defender blocks (buff 85 by count, 86 by chance, 92 all) and mirror reflection (buff 83, `CUser::ChkMirror` 0x0044e080: SuccessType 1 SuccessValue%, 2 LUC x 10 / value, 3 WIS x 10 / value; 0x051B back to the mob) |
| Damage | `CUser::AttackDamage` 0x0044d540, `CMob::AttackDamage` 0x004a2920 / 0x004a3070, element table `CGameData::m_AttackAttr` 0x00542260 | done: skill DamageType 0-5, skill AttrType element, detail 24 ignores the defender element, bow x0.7 within 3 units unless cross road, mob normal attacks use element row 0 against the armour element; the random terms and the mob critical read LUC (+0x1234 / stMOBInfo +0x3c, Eternity labels them WIS) |
| Mob stats and attacks | `Mobs` Dex/Wis/Luc/Defense/Magic/Attrib/Size, `CMob::AttackAI` 0x004a1ab0 | done: slots 3 to 1 by Attackok (3: HP 20-40%, 4: HP <= 5%, 5: HP < 20%), per-slot cooldowns, AttackTypeN is a mob skill (Skills level 100): attack (0x060D first hit, 0x060B area/repeat, 0x060C drain, 0x060E AddDamage), self heal; areas 6/7 by ApplyRange. Their debuff skills stay on the player (below). Attackok 6 targeting not modeled |
| Mob movement and targeting | `CMob::UpdateIdle` 0x004a7e60, `FindTarget` 0x0049fd20, `Chase` 0x004a6de0, `ReturnHome`, `ReTargetByAggro` 0x00490ab0, `UpdateRecover` 0x004a8210 | done: wander within 10 of the spawn point (NormalTime/NormalStep, speed offsets); only AI 1/3 look for targets, 5 s after spawning, the first living user of another country (Attackattrib3) within ChaseRange over the 3x3 cells; aggro ledger by damage (+ skill Arrow), the attack turns to its top; leash 45 from home (walk home) or from the mob (idle); chase ChaseStep every ChaseTime; return completes within 10 of home and clears effects and aggro without refilling HP; recovery Attack3 (nRecoverValue) every 5 s. No pathfinding around walls (blocked lines walk home) |
| Mob respawn | `CZone::UpdateMobDeath` 0x00421a80, `CZone::UpdateMobRespawn` 0x0041d240, `CGameData::LoadConstMob` respawn table | done: one group per svmap area entry; the first death of a full group starts its timer (Attackspecial3: 5 s, 30 s, 1, 3, 5, 10, 15, 30 min, 1, 4, 12, 24 h, 3 days, 2 h, immediate, 7 days; +-60 min from selector 9); each expiry brings one mob back at a random walkable spot of the area and restarts the timer |
| Drops | `MobItems` orders 1..9 (1-6 per cent, 7-9 per million), grade pick with `Drop` limits, `SequenceItemCreate.ini`, 30% gold `Money1 + rand(Money2)`, `CMob::GetDropPosRand` 0x00490c70 | done: positions within half a unit with Y 0, as the original sends them (the client places them on the ground); no party split |
| Skills | `CGameData::LoadConstSkill` 0x004bffe0, `CUser::SkillAttackSelect` / `SkillAttackRange` / `SkillAttackRangeSelect`, `CUser::AttackMob(skill)` 0x0044fd90, `CUser::HealUser` 0x00450670 | done: load-time normalization (kind = clamp(TypeAttack,1,3)-1, AttrType, StateType, MultiAttack, FixRange hours, detail 93/122), dispatch by TypeEffect, details 21-27 (22 multi-hit, 23 sacrifice, 27 drain with 0x0518), area announce 0x0511, AddDamage 0x051A, cost reductions (cost 1 spends the whole pool), heal + 2 x WIS, buffs 82/91/94/95, ClearApplySkillByDeath. Item skills (`CUser::UseItemSkill`: Range/AttackTime name the skill; candies, remedies, charms) |
| Skills on mobs | `CMob::AddApplySkill` 0x004a0aa0, `RemApplySkill`, `ChkKeepSkill` (every 3 s), `ChkEndTimeSkill` | done: 61 damage over time (TimeDamageType 0/3/4/5 x elapsed/3000), 62/67 ability loss, 63 whole-HP hit on expiry, 64 stun, 65 stop, 66 sleep (woken by damage), 68 no element, 72 target fixed on the caster; speed changes as 0x0609, damage as 0x0607. The original sends no packet for the application (Rapture's old 0x030C/0x030D had no client reader) |
| Skills on players | `CUser::AddApplySkill` 0x00484850, `AddApplySkillDebuff` 0x004855b0, `ChkKeepSkill` 0x00487190, `TimeDamage` 0x00486290 | done: 0x050D (id, skill, level: 9 bytes), 0x050E, 61 damage over time (0x050F), 62/67, 63, 64/66 block actions and 65 movement (0x051C speeds 255), 66 woken by damage, 68, abilities 32-34 disable a category; level rules (same level refreshes, lower does not replace) |
| Death | `CUser::Death` 0x004586a0, `CUser::KillSoul` 0x004599e0, `CUser::ExpDrop` 0x0045ad50 | done: buffs cleared, revive at the bind point with half HP/MP/SP, 5% of the level's exp lost above level 10 (1% with the Blue Dragon or White Tiger charm, 0% with Red Phoenix or an exp-protection ability 38, decided at death) |
| Attack range and speed | `CUser::EnableAttackRange`, `g_ConstUserAttackSpeed` 0x005424e0 | done: weapon reach + mob size + 2.3 + movement allowance (taken as its 2-unit floor); swing index moved by gems, passives and buffs |
| Visibility | `CZone::EnterUser` / `MoveUserLocal` 0x00416370 / `MoveMob` 0x004169d0 / `SendMoveUser` 0x0041f350 / `SendMoveMob` 0x0041f860 / `SendCellInfoView`, `CCell::SendCellInfoEnter/Leave`, `CZone::PSendView` | done: 40-unit cells kept while inside the 60-unit view window (no flip-flop on borders), 5x5 cell view for users, mobs, NPCs, map items and other users; enter/leave only on cell changes, for both sides (0x0201/0x0202 users, 0x0601/0x0602, 0x0E01/0x0E02, 0x0401/0x0402); moves to the 5x5 view; combat and effects to 60 units; mob corpses leave 5 s after death (`CZone::UpdateMobDeath`). Before this Rapture used circles, re-sent mobs at every border crossing, never sent 0x0202 and sent every map item on entry |
| NPC patrol | `CNpc::UpdateMove` 0x004bbeb0, `NpcQuest.SData` MoveDistance/MoveSpeed | done: steps of MoveDistance every MoveSpeed ms (16 units / 8 s in EP4.5 data), points walked forward then back, rest rand(10 s) + MoveSpeed + 10 s at each point |
| Skill learning | `CUser::LearnSkill` 0x004842e0, `EnableLearnSkill` 0x00484080 | done: levels 1-3, new skills at level 1 and known ones to any higher level, errors 1 points / 2 level / 3 otherwise, Country by family, job, growth mode (easy only Grow 0, normal up to 1, hard all but 3), TypeShow 0 hidden; no prerequisite skill (Rapture's PrevSkillID check was invented) |
| Mounts | `vertical_combat_vehicle_prefix`, `CUser::CancleActionVehicle`, `StatusCastingStop` | partial: slot-13 mount, shape from the item's Range (r*2 below 2, else r+7), refusal 0x0216 (0, 1), shown to users entering the view (0x051D), a hit dismounts. The summon delay (item byte +0x32) and the mounted speed are not modeled |
| Equip requirements | `CUser::EnableEquipment` 0x0045b450 | done |
| NPC shop buy | `CUser::PacketShop` 0x0046af10, `NpcQuest.SData` | done (no guild-house discounts) |
| Gatekeepers | `CUser::PacketMaininterface` case 0x020C, `NpcQuest.SData` | done (no guild-house rates; teleport is immediate) |
| Party | `party_runtime*`, `mob_death` (solo exp ×1.2, party count factor, level share, 100-unit range) | done: invites, roster, leader, expel, chat (0x1105), pools/level updates, exp and gold split, shared loot |
| Raids | `party_runtime_raid*`, raid exp table | done: convert, 30 wire slots, leader/sub-leader, slot swaps, invites, auto-join, expel, disband, chunked roster; item-division modes are stored but loot still goes to the killer |
| Quests | `NpcQuest.SData` (1720 quests, item links), `CUser::QuestEnableStart` 0x0047f080, `QuestStartRequest` 0x0047f220, `QuestFindItem` 0x0047f8e0, end/reward/progress routines, `CharQuests` procedures | done: start by NPC or item, level/race/job/mode/prerequisite checks, start items, item/mob/player-kill objectives, timed and minimum-time quests, reward variants and player choice, repeatable quests, abandon, party kill credit, quest item drops (`Mobs.QuestItemID`); no player-kill source yet (no PvP), item-use starts (type 4) not wired |
| NPC talk | `0x0E04` | done (the original only records the NPC in use) |
| Guilds | `0x0Dxx` | missing |

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

Other scripts in `tools/ghidra`: `ListFuncs.java <substring>` lists functions
with their address and size, `Fields.java <struct>` prints a PDB structure's
fields, and `Insns.java <address>... <filter>` prints a function's
instructions containing the filter text (for example `"0x12"` to see which
`CUser` fields it reads). The PDB's `CUser` field names are shifted against
the shipped binary; field meanings above come from the instruction offsets
(Str 0x1220, Dex 0x1224, Int 0x1228, Wis 0x122c, Rec 0x1230, Luc 0x1234,
HP 0x1238, MP 0x123c, SP 0x1240), cross-checked with `CUser::SetAttack`.
