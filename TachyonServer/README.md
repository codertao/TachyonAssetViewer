# tachyon-server

A console dedicated multiplayer server for *Tachyon: The Fringe*, speaking the original Netsock
protocol so stock `space.exe` clients can join. Deathmatch and Base Wars maps, assets from
`Tachyon.pff` or an unpacked directory.

Status: **M1 (join and fly)**. Players join, load, fly, see each other, chat and leave. World
objects, combat, turrets, AI pilots and Base Wars rules come in later milestones (see the plan).
Deliberate differences from the original host: `DEVIATIONS.md`.

## Running

```
cargo run --release -p tachyon-server -- --assets /path/to/Tachyon.pff --list-maps
cargo run --release -p tachyon-server -- --assets /path/to/Tachyon.pff --map DM_01 --log-packets packets.log
```

Options: `--port` (default 2766, the port the game's Network screen uses), `--name`,
`--max-players`, `--password`, `--original-bugs`, `--log-packets <file>`.

Console: `/status`, `/kick <slot>`, `/say <text>` (or just type a line), `/quit`. Players can use
`/status` and `/help` from in-game chat.

## Joining from a stock client

The game joins a specific host only through its Network (NovaWorld) browser: a page whose
`<title>` holds an encoded `host:port` makes the client connect. The server writes that page:

```
tachyon-server --join-page 192.168.1.20 --port 2766 > Startup.htm
```

1. Back up the game's `Startup.htm` (in the game folder) and replace it with the generated one.
2. Start the game, Multiplayer → **Network**. Wait for the NovaWorld ping to time out and press
   **Continue**. The browser opens the page and the game starts joining.

`host:port` must be at most 21 characters (a limit of the game's decoder).

## Testing without the game

`TachyonBot` is a headless client: `TachyonBot 127.0.0.1:2766 --name Bot --ship BORA_CUTLASS
--team 1 --say "/status"`.
