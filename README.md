# boxbox

F1 live timing in your terminal — the pit wall timing screen as a TUI.
Positions, gaps, tires, mini-sectors, race control, and a live track map,
straight from the same feed that powers the official F1 app.

```
 GREEN │ Lap 32/57 │ FL 1:36.167 VER │ A 26.8° · T 31.4° · 1.9m/s · 78% │ ⏵ 3× 0:52:14 / 1:34:20 │ Bahrain GP 2024 · Race
 Timing ──────────────────────────────────────┬ Track ────────────────
P   DRV  GAP        INT        TIRE   LAST    │        ⣰⠒⠒⢦⡀
▸ 1 ▍PIA  LAP 32     —         M 17   1:37.556│        ⡇   ⠙⣄    ⢀⡴⠒⢤
  2 ▍RUS  +7.327    +7.327     M 18   1:38.084│       ⣠⠃    ⠈⠳⢤⡀ ⢐⠗  ⢴
  3 ▍LEC  +8.454    +1.127     M 14   1:38.048│       ⡇        ⠘⡆ ⢻⣆
  4 ▍NOR +10.419    +2.062     M 21   1:37.927│      ⠙⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠒⠉
  5 ▍HAM +19.582    +9.032     M 14   1:37.598├ Driver ───────────────
  6 ▍SAI +38.389   +18.933     M 16   1:39.280│▍ 1 Oscar PIASTRI  #81
  7 ▍TSU +38.974    +0.585     M 19   1:40.754│LAST 1:37.556 BEST 1:37.1
  8 ▍ALB +39.561    +0.587     H 14   1:39.164│S1 31.137 S2 42.532 S3 …
 ...                                          │TIRES S 17 → M 17 · 1 stop
 15:49 L28 FIA STEWARDS: 5 SECOND TIME PENALTY FOR CAR 4 (NOR)   [r] messages
```

## Install

```sh
git clone https://github.com/hung-ng/boxbox
cd boxbox
cargo install --path .
```

Requires Rust 1.88 or newer. Needs a terminal ≥100 columns wide for the side panels (the timing tower alone
works narrower). Team colors use truecolor when the terminal advertises it
(`COLORTERM=truecolor`) and fall back to the xterm-256 palette otherwise.

## Use

```sh
boxbox live                        # connect to the live feed (during sessions)
boxbox replay bahrain race         # replay any archived session
boxbox replay monaco qualifying --year 2024 --speed 2
boxbox replay silverstone race --start-at 01:05:00
boxbox replay                      # no args: browse year → race → session
boxbox replay --list --year 2024   # print the season schedule
```

A query resolves against the full season schedule: a unique match (e.g.
`monaco qualifying`, `silverstone race`) plays immediately, anything ambiguous
drops into the browser pre-filtered by what you typed. Matching covers race
name, circuit, city, and country, plus a few nicknames (`britain`/`uk`, `cota`,
`vegas`, `holland`, …). `replay --list` and the browser share this one source,
so the listing and the picker always agree.

The season is always chosen with `--year`, never as a positional token —
`boxbox replay monaco 2023` is rejected with a hint to use
`boxbox replay monaco --year 2023`.

Replay downloads a session's timing streams once and caches them locally
(`~/Library/Caches/boxbox` on macOS, the XDG cache dir on Linux,
`%LOCALAPPDATA%\boxbox` on Windows), so re-watching is instant and offline.

### Cache

```sh
boxbox cache path               # print the cache directory
boxbox cache info               # per-category disk usage (sessions/schedules/circuits)
boxbox cache clear              # wipe the whole cache
boxbox cache clear --year 2024  # wipe only one season's cached session streams
```

Session streams run tens of MB each and nothing evicts them automatically, so
the cache grows as you watch more sessions — use `boxbox cache info` to check
its size and `boxbox cache clear [--year]` to reclaim space.

### Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | select a driver (detail panel follows them through the field); scroll the RC log while it's open |
| `m` | cycle view: split → map → tower → auto |
| `r` | open/close the full race control log |
| `space` | pause/resume (replay) |
| `+` / `-` | playback speed (0.5× – 120×) |
| `←` / `→` | seek back / forward 1 minute (replay; a paused session scrubs without resuming) |
| `Shift+←` / `Shift+→` | seek back / forward 5 minutes (`,` / `.` work too, for terminals that don't report Shift on arrows) |
| `0` | restart from the very beginning (replay) |
| `g` | restart at the green flag (replay) |
| `?` | toggle the keybindings help overlay |
| `q` / `Esc` | quit (Esc closes the overlay first) |

## The screen

- **Status bar** — track flag chip, session-type chip (`RACE` / `QUALI` /
  `FP1`–`FP3` / `SPRINT` / `SQ`), lap counter or Q-segment + a locally-ticking
  clock (yellow under 2:00, red under 0:30; `Q{p} ended` between segments),
  session fastest lap (purple), weather (`A` air / `T` track temp, wind,
  humidity), replay transport with an `elapsed / total` timeline (`⏵`/`⏸`/`⏹`),
  and the meeting name with its year. Always visible.
- **Timing tower** — the race story: gap, interval, tire, last lap. Position
  changes flash a green ▲ / red ▼; intervals under 1s (DRS range) highlight
  both cars of the battle; pit stops replace the interval (`PIT`/`IN PIT` while
  stopped, then the pit-lane time in cyan, `OUT LAP` on exit) right where your
  eye is. The selected row stays on screen as the field scrolls. In qualifying:
  best lap, gap, a live mini-sector strip per driver, the elimination cutoff
  line in Q1/Q2 (drop-zone drivers show their red gap to the cutoff time), and
  just-completed laps flash in the live-lap column. In practice: a `LAPS` column.
- **Driver panel** — select anyone with `↑↓`: sector times (Q1/Q2/Q3 segment
  bests in qualifying), mini-sectors, full stint history, pit stops and last
  pit-lane time (or lap count in practice), gaps, and a speed-trap line
  (`I1`/`I2`/`FL`/`ST`).
- **Track map** — braille circuit with cars as team-colored dots; active
  yellow-flagged marshal sectors light up yellow on the outline, in-pit cars
  dim, and a dim trace sketches the pit lane as cars use it. The whole outline
  tints with the track flag (yellow / SC-VSC / red).
- **Race control ticker** — the latest message that matters (penalties, safety
  car, flags); routine track-sector/blue-flag spam is filtered into the full
  log behind `r`.

In live mode before a session starts, the body shows a "waiting for a live
session…" message with the next scheduled session.

## Data sources

- `livetiming.formula1.com` — the unofficial SignalR feed used by the official
  web/app timing (live), and its static archive (replay). No account needed.
- `api.multiviewer.app` — circuit outlines for the map.

This project is unofficial and is not associated in any way with Formula 1
companies. F1 and related marks are trademarks of Formula One Licensing B.V.

## License

MIT — see [LICENSE](LICENSE).
