# Deliberate differences from the original host

The server ports the original host's rules, combat and AI as closely as is reasonable, but fixes
obvious bugs. Every such difference is listed here. `--original-bugs` restores the original
behaviour where that is practical.

| Area | Original | This server | `--original-bugs` |
|---|---|---|---|
| Shield facing | Front/rear shield choice ignores the victim's orientation | Uses the hit position relative to the victim's facing | restores |
| Near-entity cap | The 8 near snapshot records are the first 8 in quadrant-list order | The 8 nearest | restores |
| Hit reports | Accepted from shooters who are already dead | Dropped | restores |
| NovaWorld | Heartbeats to the master server; checks the join blob's game ID | Neither | n/a |
| Model / descriptor slots | Filled as ships first appear, so a ship type that joins later gets slots the earlier clients never received in their startup tables | The 11 `vars.cfg` player ships are registered at startup, so every client gets the same table | no |
| Chat commands | Command hook is a stub; every line is relayed | Lines starting with `/` (after the client's `NAME: ` prefix) are answered by the server and not relayed to other players | no |
| Spawn facing | `Vec_DirAngles(pos, origin)` (exact convention not yet confirmed) | Nose aimed at the arena centre, no pitch | no |

Further fixes are proposed to the user before they are added.
