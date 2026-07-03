# boxbox architecture

(crate/binary named `boxbox`; the repo directory may still be named `f1-tui`)

## Overview

```
live source (SignalR Core WS) ─┐
                               ├─→ SourceEvent channel → SessionState (delta merge) → ViewModel → ratatui UI
replay source (archive files) ─┘        (std::sync::mpsc)
```

Two data sources produce the same `FeedMessage { topic, data: serde_json::Value, ts }`
stream; everything downstream is source-agnostic. Network/playback runs on a tokio
runtime; the UI runs a synchronous crossterm/ratatui loop on the main thread,
draining the channel each frame (~30 fps).

## Modules

- `src/main.rs` — clap CLI (`live`, `replay <query> [--year --speed --start-at]`,
  `sessions [--year]`), session resolution, wiring channels/runtime to the UI.
- `src/message.rs` — `FeedMessage`, `SourceEvent` (Message/Info/Clock/Circuit/Ended),
  `PlaybackControl` (SetSpeed/TogglePause/Jump).

### State (`src/state/`)

- `merge.rs` — the F1 feed sends full snapshots then delta patches. `merge()`
  implements the convention: objects merge recursively; arrays are patched by
  objects keyed with numeric index strings (`{"3": {...}}` updates/appends
  element 3); scalars replace. Unit-tested.
- `mod.rs` — `SessionState`: one merged JSON tree per topic in a `HashMap`,
  plus a specially-maintained `positions: HashMap<car#, CarPosition>` (Position
  batches are telemetry, not state, so they bypass the merge). `.z` topics
  (e.g. `Position.z`) are base64 + raw-deflate JSON, inflated in `inflate_topic`.
  `dirty` flag gates view rebuilds.
- `view.rs` — pure extraction from the JSON trees into typed structs the UI
  renders: `ViewModel { rows, race_control, cars, weather, lap, track_flag, … }`.
  Handles both race fields (`GapToLeader`, `IntervalToPositionAhead`) and quali
  fields (`Stats[]` array — one entry per Q1/Q2/Q3 segment). Mini-sector codes:
  2049 = personal best (green), 2051 = overall best (purple), 2064 = pit,
  other nonzero = set (yellow). Also derives: session fastest lap (min of
  parsed `BestLapTime`s), `interval_secs` for battle highlighting,
  `session_part` (Q1/Q2/Q3 from `TimingData.SessionPart`), `quali_cutoff()`
  (last safe position: `n - (n-10)/2 * part`), full stint history, and an
  `important` flag on race control messages (track-sector yellows/clears,
  blue flags, and rain-risk chatter are noise).

### Sources (`src/source/`)

- `archive.rs` — static archive client for `livetiming.formula1.com/static`:
  year index (`/{year}/Index.json`), per-topic `.jsonStream` files, on-disk
  cache under the platform cache dir. Responses are BOM-prefixed — always
  `strip_bom`. Missing topics return 403 *or* 404; both mean "not available".
  Also fetches circuit outlines from `api.multiviewer.app` (cached).
- `replay.rs` — parses `.jsonStream` lines (`HH:MM:SS.mmm{json}` — 12-char
  timestamp prefix), merge-sorts all topics by offset, and plays them back in
  sim time. Pause/speed/jump arrive via a tokio mpsc control channel; jumps
  emit the skipped backlog instantly so merged state stays correct (this is
  also how `--start-at` seeking works). Emits `Clock` events for the header.
- `live.rs` — SignalR **Core** client (F1 migrated; the classic `/signalr`
  endpoint now 401s). Handshake: POST `/signalrcore/negotiate?negotiateVersion=1`
  → `connectionToken`, then `wss://…/signalrcore?id=<token>`, then
  `{"protocol":"json","version":1}` record, then a `Subscribe` invocation with
  the topic list. Records are `\x1e`-separated JSON; type 1 invocations with
  target `feed` carry `[topic, data, utc]`, type 3 completion carries the
  initial snapshot map. Sends `{"type":6}` pings every 15 s; reconnects after
  90 s of silence or any error.

### UI (`src/ui/`)

Design principle: **the tower tells the story, the sidebar tells the details.**
Hierarchy over density — gaps/intervals get the space, transient detail
(sector times, stint history) lives in the on-demand driver panel, and race
control noise is filtered into an overlay.

- `mod.rs` — `App` state, event loop (drain channel → rebuild view if dirty →
  draw → poll keys). Layout: status bar (1 row) / body / race-control ticker
  (1 row) / footer (1 row). Side column (map over driver panel) only when
  width ≥ 100. `↑↓` selects a driver; selection is tracked by TLA
  (`selected_tla`) so it follows the driver through position changes. `r`
  toggles the race-control overlay (then `↑↓` scrolls it). Spawns the
  circuit-outline fetch once `SessionInfo` reveals the circuit key.
- `header.rs` — one-line status bar: flag chip · lap counter or `Q2 ⏱ mm:ss` ·
  fastest-lap chip (purple) · compact weather · replay transport · dim
  meeting/session name.
- `tower.rs` — session-aware columns. Race: P/DRV/GAP/INT/TIRE/LAST, interval
  ≤1.0s highlighted yellow-bold (battle/DRS range), `IN PIT`/`OUT LAP`
  replace the interval. Quali: P/DRV/BEST/GAP/TIRE/live mini-sector strip,
  red `╌` cutoff row after the last safe position, drop-zone positions in red.
- `focus.rs` — driver detail panel for the selected row: full name, LAST/BEST,
  sector times, mini-sectors, stint history (`M 11 → S 14 · 1 stop`), gaps.
- `racecontrol.rs` — `ticker()`: latest important message on one line;
  `overlay()`: centered popup with the full log (noise dimmed), scrollable.
- `map.rs` — `TrackOutline::parse` reads the MultiViewer response (`x`/`y`
  arrays, `corners`, `rotation` in degrees), rotates everything around the
  bbox center, densifies the outline, and renders on a braille Canvas with
  data-units-per-dot equalized on both axes so the circuit keeps its shape.
  Start/finish line drawn perpendicular to the outline at point 0 with an
  `S/F` label. Cars are 5-point clusters in team color; in-pit cars dimmed.

## Key patterns & gotchas

- **Replay is the dev loop**: the live feed only exists during race weekends,
  so everything is built and tested against archived sessions.
- One canonical state; the UI never touches raw feed JSON — only `view.rs` does.
- Feed values are pre-formatted strings (`"1:32.807"`, `"+2.502"`, `"LAP 12"`
  for the leader); display them verbatim, don't parse.
- The channel from source to UI is unbounded; the UI drains up to 20k messages
  per frame so seek/catch-up bursts don't freeze rendering.
- No tests exist for view extraction yet; merge logic is unit-tested.
