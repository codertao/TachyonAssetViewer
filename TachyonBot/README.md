# TachyonBot

A headless multiplayer client for Tachyon: The Fringe (proof of concept). It joins a hosted
game, flies its ship in slow circles and prints what it sees: entities appearing and
disappearing, big changes to them, players, scores, messages and deaths.

```
cargo run -- 127.0.0.1                 # a host on this machine, default port 3874
cargo run -- 192.168.1.20:3874 --name Drone --ship GALSPAN_ORION --team galspan
```

Options (`--help`):

| Option | Meaning |
|---|---|
| `--name` | Player name (default `TachyonBot`) |
| `--ship` | Ship alias from `desnames` (default `BORA_CUTLASS`). Deathmatch flies this ship |
| `--team` | `bora`, `galspan` or a `race.cfg` index |
| `--password` | Session password |
| `--game-id` | The host's game ID, or `auto` to read it from a local space.exe (needed for every TCP/IP host) |
| `--radius`, `--period` | Circle size (world units) and seconds per lap |
| `--weapon`, `--fire-every` | Weapon type to fire (default 6) and seconds between shots (default 3; 0 = never) |
| `--muzzle` | Start shots this many units ahead of the ship's centre (default 12), as a real client fires from its mount |
| `--track` | Log every snapshot record for one entity id (hex) |
| `--respawn` | Seconds before asking to respawn after dying (0 = stay dead) |
| `--verbose` | Also log event types the bot doesn't decode |
| `--hexdump` | Print every datagram (after CRC decoding) |

Ctrl-C leaves cleanly: destroy-player, then close.

## Hosting a game it can join
- The host must use the multiplayer menu's **network** (TCP/IP) option. The **LAN** option is
  DirectPlay over IPX (through GoG's ipxwrapper, UDP 54792), which the bot doesn't speak.
- Network hosts use a random game ID and kick joiners with the wrong one (reason 4). The ID is
  shown as "ID: N" on a dedicated server's status screen. For a host on the same Linux machine,
  `--game-id auto` reads it from the space.exe process.

## How it works
The protocol comes from static RE of `space.exe` and `Netsock.dll`. It's written up in
`decomp/notes/multiplayer.md`: "Netsock session protocol", "Engine packet", "Join / startup
handshake", "PLAYERDATA", "GAMEDATA" and the event catalogue.

1. **Netsock** (`netsock.rs`):
   - every datagram carries a CRC-32;
   - enumerate (0x1010) and join (0x1020) using Tachyon's app GUID;
   - create player (0x1030), carrying the 0xc4-byte join blob that says which ship and team.
2. **Engine** (`engine.rs`):
   - every 320 ms (16 ticks at 50 Hz) the bot sends `seq, ack`, a PLAYERDATA block and any
     unacked events;
   - startup stages are acked with 0x103.
3. **Game** (`main.rs`, `proto.rs`):
   - fills the model and descriptor tables from startup stages 0 and 1, and takes its entity
     id and start position from stage 2;
   - sends 0x2012 after "startup complete" and 0x2008 kind 2 after entering the quadrant;
   - acks snapshot pages;
   - respawns with 0x2011.
4. **Flight** (`flight.rs`): position, per-tick velocity and yaw along a circle through the
   spawn point. Engine axes are y up and z forward; angles are 512 units per turn.
5. **World** (`world.rs`):
   - snapshot records, kept with the same lifetimes the game uses;
   - SPAWN / GONE / CHANGE lines. A change is a model, team or flag change, a jump of more than
     500 units from the dead-reckoned position, a turn of 45° or more, or a big change in speed.

The bot fires but never reports hits or collisions:
- After startup it sends a loadout (0x2028) with the weapon in slot 0 and 30000 ammo. The server
  drops shots from an empty slot, and the join-blob path leaves the ammo counter at 0.
- Then it sends a fire event (0x2001) every `--fire-every` seconds, from its position along its
  heading.
- The server relays shots to everyone except the shooter (0x1002), so the bot doesn't see its own
  shots.

## Status
- Works against a GoG space.exe Network host under Wine (2026-11-09):
  - joins, gets through all six startup stages and receives snapshots;
  - sees the host's ship, shots, roster, scores and messages;
  - leaves cleanly on Ctrl-C.
- In-game Network hosts listen on **UDP 2766** by default:
  `cargo run -- 127.0.0.1:2766 --game-id auto`.
- A bot killed without Ctrl-C stays in the game as a ghost until the server's 120 s timeout.
- Not yet confirmed: whether the host *sees* the bot's ship circling. Its throttle and speed are
  reported as 0.
- If the handshake fails, run with `--hexdump`. Also check the host's `server.txt`
  (`Net_DumpPacket`) and the kick reason the bot prints.
