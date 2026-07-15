# boxbox architecture

## Overview

```
live source (SignalR Core WS) ─┐
                               ├─→ SourceEvent channel → SessionState (delta merge) → ViewModel → ratatui UI
replay source (archive files) ─┘        (Tokio MPSC, capacity 4,096)
```

Two data sources produce the same `FeedMessage { topic, data: serde_json::Value, ts }`
stream; everything downstream is source-agnostic. Network/playback runs on a tokio
runtime; the UI runs a synchronous crossterm/ratatui loop on the main thread,
draining the bounded channel with `try_recv` each frame (~30 fps). Replay controls
travel in the opposite direction through a separate Tokio MPSC channel with
capacity 64.

## Data and storage model

There is no database or persistent application schema. Runtime state is a set of
per-topic `serde_json::Value` trees in `SessionState`, plus typed derived view
models. Persistence is a filesystem cache under the platform cache directory:
`sessions/<cache_id>/<topic>.jsonStream`, `schedules/<year>.json`, and
`circuits/<key>-<year>.json`. Cache files are validated before use and published
with a unique sibling temporary file plus atomic rename, so an interrupted write
is never treated as complete.

## Modules

- `src/main.rs` — clap CLI (`live`, `replay <query> [--year --speed --start-at
  --list]`, `cache <path|info|clear [--year]>`), session resolution, wiring
  channels/runtime to the UI. There are three verbs: `live`, `replay`, and
  `cache`. `default_year()` is `chrono::Utc::now().year()`. Everything replay
  does is backed by one source, the Jolpica schedule (`schedule.rs`):
  - `resolve_session` — with a query, builds the match set (widened haystack +
    nickname aliases); exactly one *available* session → plays it directly, else
    hands off to the browser pre-filtered by the query. Empty query → full browser.
    A year-shaped positional token (`year_token`, `^(19|20)\d{2}$`) is rejected
    up front with a hint to use `--year` (seasons are chosen with the flag, never
    positionally). `--speed` is parsed as a finite multiplier in the inclusive
    range 0.5–120. `--start-at` uses checked arithmetic, rejects invalid minute/
    second fields and overflow, and a value past the recording end bails with a
    hint. The live next-session hint rolls over to `year + 1` once a season ends.
  - `browse` — interactive race → session picker; future sessions
    (`session.date > today`) are shown dimmed and can't be selected.
  - `print_schedule` — `replay --list`, prints the schedule (replaces the old
    `sessions` verb). Same source as the browser, so list and picker agree.
    The live `Index.json` is no longer used for discovery; Jolpica is unified.
  - Pre-TUI output (picker lists, `--list` schedule, loading progress) is
    styled via `tint(tty, code, s)` — ANSI bold/dim gated on the destination
    stream being a terminal, so piped output stays plain. Headers bold, dates
    and per-topic download sizes dim, unavailable sessions dimmed whole.
  - `run_cache` — `cache path/info/clear`. `info` reports per-category disk
    usage (sessions/schedules/circuits) via `Archive::cache_usage`; `clear`
    wipes everything, or with `--year` only that season's session streams
    (their `cache_id`s are `{year}-`-prefixed). `fmt_bytes` humanizes sizes.
- `src/message.rs` — `FeedMessage`, `SourceEvent`
  (Message/Info/Clock/Circuit/Reset/NextSession/Ended), `PlaybackControl`, and
  the queue capacities (`EVENT_CHANNEL_CAPACITY = 4096`,
  `CONTROL_CHANNEL_CAPACITY = 64`). Circuit events carry `{ key, year, data }`
  rather than untagged JSON, so the UI can reject a response from an earlier
  session. `PlaybackControl`
  (SetSpeed/TogglePause/Jump/JumpBack/SeekTo). `Reset` is emitted
  by `live::run` on every reconnect after the first *and* by `replay::play` on
  a rewind, telling the UI to drop stale merged state before the fresh backlog
  arrives. `JumpBack(Duration)` seeks the replay backward; `SeekTo(Duration)`
  is the absolute seek both restart keys use — `0` sends `SeekTo(0)` and `g`
  sends `SeekTo(green)`, with the green-flag offset computed once in `main.rs`
  and handed to the UI alongside the timeline total (0:00 for recordings that
  never report a start, same as the `--start-at` default). `NextSession(String)`
  carries the next upcoming session line for the live empty state.

### State (`src/state/`)

- `merge.rs` — the F1 feed sends full snapshots then delta patches. `merge()`
  implements the convention: objects merge recursively; arrays are patched by
  objects keyed with numeric index strings (`{"3": {...}}` updates/appends
  element 3); scalars replace. It returns a validity flag and rejects array
  indices above 65,535 before mutation, preventing sparse patches from forcing
  unbounded allocation. Unit-tested.
- `mod.rs` — `SessionState`: one merged JSON tree per topic in a `HashMap`,
  plus a specially-maintained `positions: HashMap<car#, CarPosition>` (Position
  batches are telemetry, not state, so they bypass the merge). `.z` topics
  (e.g. `Position.z`) are base64 + raw-deflate JSON, inflated in `inflate_topic`;
  decompressed output is capped at 32 MiB before UTF-8/JSON parsing. Inflate and
  invalid-merge failures bump `dropped` (surfaced in the footer) instead of
  vanishing. `dirty` gates view rebuilds. `reset()` clears topics/positions
  (keeping `dropped`) on live reconnect.
- `view.rs` — pure extraction from the JSON trees into typed structs the UI
  renders: `ViewModel { rows, race_control, cars, weather, lap, track_flag, … }`.
  Handles both race fields (`GapToLeader`, `IntervalToPositionAhead`) and quali
  fields (`Stats[]` array — one entry per Q1/Q2/Q3 segment). Mini-sector codes:
  2049 = personal best (green), 2051 = overall best (purple), 2064 = pit,
  other nonzero = set (yellow). Also derives: session fastest lap (min of
  parsed `BestLapTime`s), `interval_secs` for battle highlighting,
  `session_part` (Q1/Q2/Q3 from `TimingData.SessionPart`), full stint history,
  per-driver `laps` (practice LAPS column), and an `important` flag on race
  control messages (track-sector yellows/clears, blue flags, and rain-risk
  chatter are noise). Per-row enrichment: `speeds` (I1/I2/FL/ST speed trap with
  PB/OB flags), `segment_bests` (Q1/Q2/Q3 bests from `BestLapTimes`), and
  `pit_time` (pit-lane time from `PitLaneTimeCollection.PitTimes[num].Duration`).
  Team-color parsing validates the complete six-byte ASCII hexadecimal string
  before slicing, so malformed UTF-8 cannot panic.
  `yellow_sectors: HashSet<i64>` is folded over the ordered RaceControlMessages
  log — `(DOUBLE) YELLOW / CLEAR IN TRACK SECTOR n` add/remove n, a track-scope
  `GREEN` flag clears all — and is forced empty under SC/VSC/red (the whole-track
  tint subsumes per-sector yellows). ViewModel helpers: `is_practice()`, `part_label()`
  (`Q{p}`/`SQ{p}`), `quali_cutoff()` (Q3 always keeps 10, so the Q1 cutoff is
  `n - (n-10)/2`, the Q2 cutoff is 10), and `cutoff_time()` (the drop-zone
  target best-lap seconds).

### Sources (`src/source/`)

- `archive.rs` — static archive client for `livetiming.formula1.com/static`:
  per-topic `.jsonStream` files (`TOPICS`, incl. `PitLaneTimeCollection` for pit
  times), on-disk cache under the platform cache dir. Responses are BOM-prefixed
  — always `strip_bom`. Only 403 and 404 mean "not available"; authentication,
  throttling, and server errors propagate. The HTTP client has a 15-second
  connect timeout and 120-second request timeout. Bodies are read incrementally
  with limits: 256 MiB per topic and 8 MiB for schedules/circuits. Cached and
  downloaded streams must contain at least one valid timestamped JSON record;
  circuit coordinates must be matching finite numeric arrays. Corrupt caches
  are removed and refetched, and validated data is atomically published. Also
  fetches circuit outlines from `api.multiviewer.app` (cached). `Session` is
  schedule-reconstructed (no index):
  `Session::reconstructed(...)` builds one from a path, with a `cache_id` naming
  its on-disk stream cache. (Discovery no longer reads `Index.json`.) Cache
  management: `cache_dir()`, `cache_usage() -> CacheUsage` (recursive `dir_size`
  over `sessions/`, `schedules/`, `circuits/`), and `clear_cache(Option<year>)`.
- `schedule.rs` — full-season schedule from Jolpica (`api.jolpi.ca/ergast/f1`),
  cached on disk, the **single discovery source** for replay. F1's own
  `Index.json` is **trimmed for past seasons** (early rounds vanish) even though
  their streams stay on the server. Jolpica gives the complete schedule with
  per-session dates, from which we **reconstruct the archive path**:
  `{year}/{race_date}_{Meeting_Name}/{session_date}_{Session_Name}/` — meeting
  folder uses the race (Sunday) date, each session folder its own date, names are
  spaces→underscores with UTF-8 preserved (`São_Paulo_Grand_Prix`). Sprint
  weekends preserve the archive-era name (`Sprint Shootout` in 2023,
  `Sprint Qualifying` from 2024) and sort sessions by `(date, UTC time, name)`.
  Query matching
  lives here too: `ScheduledRace::haystack` (race name + circuit id + locality +
  country), an `ALIASES`/`expand_query` nickname layer, `is_available(today)` for
  the future-session gate, and a shared `matches(expanded)` used by both the
  direct resolver and the browser.
- `stream.rs` — the shared defensive `.jsonStream` line parser. It validates the
  exact `HH:MM:SS.mmm` separators and fields with checked timestamp arithmetic,
  tolerates BOM/CR boundaries, and accepts a record only when its JSON parses.
- `replay.rs` — uses `stream.rs`, reports malformed-line counts per topic,
  merge-sorts all valid records by offset, and plays them back in
  sim time. Playback is driven by an index **cursor** into the retained
  `Vec<ReplayEntry>` (not a consuming iterator) so backward seeks work.
  Pause/speed/jump arrive via a tokio mpsc control channel; a forward `Jump`
  emits the skipped backlog instantly so merged state stays correct (this is
  also how `--start-at` seeking works). A `JumpBack` emits `Reset`, rewinds the
  cursor to 0, and fast-applies the whole prefix up to the new target (O(n) per
  rewind, same as a start seek); `SeekTo` takes the same path with an absolute
  target. Before committing that rewind, `seek_to!` drains any already-queued
  controls into the target and skips zero-movement seeks entirely (`g` right
  after launch would otherwise flash the UI and replay the whole prefix for
  nothing). The catch-up pass thins the backlog: only the
  trailing `CATCHUP_POSITION_KEEP` (50) `Position.z` messages are forwarded —
  Position is cumulative per car, and each skipped message saves the UI a
  base64+inflate — and every `CATCHUP_POLL_EVERY` (4096) sends it polls the
  bounded control channel, so key mashing folds into one replay whether the extra
  seeks land mid-pass or between passes. Jump/JumpBack/SeekTo leave
  the paused flag alone (seek-while-paused scrubs the state, then keeps
  waiting); the UI mirrors this by never touching `app.paused` on seeks. Past
  the last entry, `play` sends `Ended` once then idles on the control channel
  (no sleeps) so the session stays rewindable after it finishes; a backward
  seek re-arms it. A *paused* scrub that overruns the end does **not** mark the
  session ended — the final state holds under ⏸, and resuming at the end is
  what sends `Ended` (otherwise the header would flip to ⏹ mid-scrub). The end
  idle still applies `TogglePause` so the source's pause flag can't desync from
  the UI's optimistic one across an end-then-rewind.
  Emits `Clock` events for the header, gated on a `last_clock` high-water mark
  that must be rewound together with the cursor on a backward seek — a stale
  mark saturates the gate and starves Clock events, freezing the UI timeline
  at the seek target while playback advances underneath (this was the long-
  standing "wrong track flag after seek" bug). `main.rs` passes the last
  entry's `ts` to the UI as the timeline total.
- `live.rs` — SignalR **Core** client (F1 migrated; the classic `/signalr`
  endpoint now 401s). Handshake: POST `/signalrcore/negotiate?negotiateVersion=1`
  → `connectionToken`, then `wss://…/signalrcore?id=<token>`, then
  `{"protocol":"json","version":1}` record, then a `Subscribe` invocation with
  the topic list. Records are `\x1e`-separated JSON; type 1 invocations with
  target `feed` carry `[topic, data, utc]`, type 3 completion carries the
  initial snapshot map. Sends `{"type":6}` pings every 15 s; the ping tick
  doubles as the watchdog (bails if `last_data` is >90 s stale), and any error
  triggers a reconnect that first emits `SourceEvent::Reset`. WebSocket setup is
  capped at 30 seconds; frames are limited to 16 MiB and complete messages to
  32 MiB. Awaited sends apply backpressure through the bounded event channel.

### UI (`src/ui/`)

Design principle: **the tower tells the story, the sidebar tells the details.**
Hierarchy over density — gaps/intervals get the space, transient detail
(sector times, stint history) lives in the on-demand driver panel, and race
control noise is filtered into an overlay.

- `mod.rs` — `App` state, event loop (drain channel → rebuild view if dirty →
  draw → poll keys). A `RestoreGuard` owns `ratatui::restore`, guaranteeing
  terminal restoration on normal exit and every propagated draw/input error.
  Layout: status bar (1 row) / body / race-control ticker
  (1 row) / footer (1 row). Side column (map over driver panel) only when
  width ≥ 100. `↑↓` selects a driver; selection is tracked by TLA
  (`selected_tla`) so it follows the driver through position changes. `r`
  toggles the race-control overlay (then `↑↓` scrolls it); `q` and `Esc` both
  close the topmost overlay before quitting. Circuit-outline fetch retries up
  to 3× (15s apart) via `circuit_attempts`/`circuit_last_try` so one failure
  doesn't leave the session mapless. `track_id` and `requested_track_id` bind
  the map to `(circuit key, year)`: a concrete identity change clears the old
  map, stale tagged responses are rejected, and an identity-less replay reset
  retains the same-track map. After each view rebuild the loop diffs
  against the previous frame to drive App-side trackers: `pos_flash` (TLA →
  ▲/▼ direction, 3s), `lap_flash` (just-completed lap for the quali live-lap
  column, 5s), `pit_flash` (final pit-lane time flashed in the INT cell on the
  rising edge of a car's `pit_time`, 5s), and a `clock_base`/`clock_at_*` anchor
  for the locally-ticking header clock (`ticking_clock()` extrapolates from wall
  time live, sim time in replay). `pit_lane: Vec<(f64,f64)>` accumulates raw
  Position samples of in-pit cars (deduped by `PIT_LANE_MIN_DIST`, capped at
  `PIT_LANE_CAP`) to sketch the pit lane on the map. `total: Option<Duration>`
  is the replay timeline length. `table_state` drives scroll-follow so the
  selected tower row stays visible. `Reset` clears all these trackers + state,
  the `pit_lane` trace, the ticking-clock anchors, and the `ended` marker, and
  re-arms the circuit fetch (rewind revives a finished replay); `NextSession`
  feeds the live empty state (`Ended` does not stop the drain — a rewind's
  `Reset` may be queued right behind it, and stopping would flash the ended
  state for a frame). Keys: `↑↓` move the selection (or scroll the RC
  overlay); replay transport is `space` (pause), `+`/`-` (speed), `←`/`→`
  (seek 1 min), `Shift+←/→` (seek 5 min — with `,`/`.` as modifier-free
  aliases, since not every terminal reports Shift on arrows; macOS
  Terminal.app doesn't), `0` (restart from the very beginning via
  `SeekTo(0)`), and `g` (restart at the green flag via `SeekTo(green)`, the
  offset `main.rs` passes in). Seeks do **not**
  touch pause — a paused session scrubs. Pause/speed UI state changes only after
  the corresponding bounded control `try_send` succeeds, preventing producer/
  UI state drift under saturation. `m` cycles the view; `?` toggles the help
  overlay.
  `color(r,g,b)` builds a `Color`, quantizing to the xterm-256 cube when
  `COLORTERM` doesn't advertise truecolor (every `Color::Rgb` routes through it).
- `header.rs` — one-line status bar: flag chip · session-type chip
  (`session_chip`: FP1/FP2/FP3/SPRINT/SQ/RACE/QUALI/PRACTICE) · lap counter or
  `Q2 ⏱ h:mm:ss` (ticking clock, yellow <2:00 / red <0:30, `Q{p} ended` when the
  segment's SessionStatus is Finished/Aborted/Inactive) · fastest-lap chip
  (purple) · self-describing weather (`A` air / `T` track · wind · humidity) ·
  replay transport (`⏵`/`⏸`/`⏹` glyph, speed, `elapsed / total` timeline) · dim
  meeting name with its year · session name.
- `tower.rs` — session-aware columns, rendered as a stateful table (scroll-
  follow). A per-row `Ctx` carries whole-field facts computed once in `draw`
  (battle set, cutoff position/time, practice LAPS column). Race:
  P/DRV/GAP/INT/TIRE/LAST — both cars of a ≤1.0s battle get yellow-bold INT,
  pit activity replaces the interval (`PIT {t}`/`IN PIT` while stopped, then the
  final pit-lane time in cyan for 5s, `OUT LAP` on exit), ▲/▼ position-change
  arrows in the P marker slot. Quali: P/DRV/BEST/GAP/TIRE/live-lap — red `╌` cutoff row after
  the last safe position, drop-zone GAP shows the red `+delta` to the cutoff
  time, pit status moves into the live-lap slot (gap stays visible), and a
  just-completed lap flashes there for 5s. Practice adds a LAPS column.
- `focus.rs` — driver detail panel for the selected row: full name, LAST/BEST,
  sector times (or Q1/Q2/Q3 `SEG` bests in quali, which reuse that fixed-height
  slot), mini-sectors, stint history (`M 11 → S 14 · 1 stop`, plus last pit-lane
  time in a race), gaps, and a `SPD` speed-trap line (I1/I2/FL/ST).
- `racecontrol.rs` — `ticker()`: latest important message on one line;
  `overlay()`: centered popup with the full log (noise dimmed), scrollable.
- `map.rs` — `TrackOutline::parse` reads the MultiViewer response (`x`/`y`
  arrays, `corners`, `marshalSectors`, `rotation` in degrees), rotates
  everything around the bbox center, densifies the outline, and renders on a
  braille Canvas with data-units-per-dot equalized on both axes so the circuit
  keeps its shape. Parsing is all-or-nothing for numeric coordinates/markers,
  rejects non-finite or degenerate geometry, caps source coordinates at 100,000,
  and aborts densification above 200,000 points. `sector_ranges` maps each
  marshal sector to the outline-point
  index range it covers (each sector's marker projected to the nearest point,
  ranges running to the next sector); points inside an active `yellow_sectors`
  entry paint yellow, thickened per-slice so colors don't bleed. A short white
  stretch marks start/finish at point 0. Cars are 5-point clusters in team
  color; in-pit cars dimmed. The derived `app.pit_lane` trace draws as dim,
  unthickened dots beneath the ribbon (no pit geometry in the data source).

## Key patterns & gotchas

- **Replay is the dev loop**: the live feed only exists during race weekends,
  so everything is built and tested against archived sessions.
- One canonical state; the UI never touches raw feed JSON — only `view.rs` does.
- Feed values are pre-formatted strings (`"1:32.807"`, `"+2.502"`, `"LAP 12"`
  for the leader); display them verbatim, don't parse.
- The source-to-UI channel is bounded at 4,096 events and all async producers
  await capacity. The UI drains up to `DRAIN_CAP` (20k) messages per frame so
  seek/catch-up bursts don't freeze rendering; when a frame hits the cap it
  polls input with a zero timeout so the backlog drains at full speed instead
  of one cap per 33 ms frame. Replay controls are independently bounded at 64.
- `view.rs` extraction is unit-tested (`build_rows` race/quali/knocked-out,
  speeds + segment bests, pit time, the `yellow_sectors` fold, `quali_cutoff`
  matrix, `parse_laptime`, `stints_from`, `first_nonempty`, `cutoff_time`); the
  256-color `quantize_256` and the CLI `year_token` detector are too, as are
  merge and schedule matching. CI (`.github/workflows/ci.yml`) grants read-only
  repository access by default, pins third-party actions to immutable commit
  SHAs, runs `cargo fmt --check` + `clippy -D warnings` + `test` on Linux and
  macOS, plus a Windows build-only job, pinned to Rust 1.88. A separate Linux
  RustSec job audits `Cargo.lock`; vulnerabilities fail the check while
  informational advisories remain visible.
