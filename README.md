# boxbox

F1 live timing in your terminal — the pit wall timing screen as a TUI.
Positions, gaps, tires, mini-sectors, race control, and a live track map,
straight from the same feed that powers the official F1 app.

```
 GREEN │ Lap 32/57 │ FL 1:36.167 VER │ 26.8°/31.4° 1.9m/s │ ⏵ 3× │ Bahrain GP · Race
 Timing ──────────────────────────────────────┬ Track ────────────────
P   DRV  GAP        INT        TIRE   LAST    │        ⣰⠒⠒⢦⡀
▸ 1 ▍PIA  LAP 32     —         M 17   1:37.556│        ⡇   ⠙⣄    ⢀⡴⠒⢤
  2 ▍RUS  +7.327    +7.327     M 18   1:38.084│       ⣠⠃    ⠈⠳⢤⡀ ⢐⠗  ⢴
  3 ▍LEC  +8.454    +1.127     M 14   1:38.048│       ⡇        ⠘⡆ ⢻⣆
  4 ▍NOR +10.419    +2.062     M 21   1:37.927│      ⠙⠒⠒⠒⠒⠒⠒S/F⠒⠒⠒⠒⠉
  5 ▍HAM +19.582    +9.032     M 14   1:37.598├ Driver ───────────────
  6 ▍SAI +38.389   +18.933     M 16   1:39.280│▍ 1 Oscar PIASTRI  #81
  7 ▍TSU +38.974    +0.585     M 19   1:40.754│LAST 1:37.556 BEST 1:37.1
  8 ▍ALB +39.561    +0.587     H 14   1:39.164│S1 31.137 S2 42.532 S3 …
 ...                                          │TIRES S 17 → M 17 · 1 stop
 15:49 L28 FIA STEWARDS: 5 SECOND TIME PENALTY FOR CAR 4 (NOR)   [r] messages
```

## Install

```sh
cargo install --path .
```

Needs a terminal ≥100 columns wide for the side panels (the timing tower alone
works narrower) and truecolor support for team colors.

## Use

```sh
boxbox live                        # connect to the live feed (during sessions)
boxbox replay bahrain race         # replay any archived session
boxbox replay monaco qualifying --year 2024 --speed 2
boxbox replay silverstone race --start-at 01:05:00
boxbox sessions                    # list this season's archived sessions
boxbox sessions --year 2023
```

Replay downloads a session's timing streams once and caches them locally
(`~/Library/Caches/boxbox` on macOS, XDG cache dir on Linux), so re-watching
is instant and offline.

### Keys

| Key | Action |
| --- | --- |
| `↑` / `↓` | select a driver (detail panel follows them through the field) |
| `r` | open/close the full race control log |
| `space` | pause/resume (replay) |
| `+` / `-` | playback speed (0.5× – 120×) |
| `f` / `F` | jump forward 1 / 5 minutes (replay) |
| `m` | toggle track map |
| `q` / `Esc` | quit (Esc closes the overlay first) |

## The screen

- **Status bar** — track flag chip, lap counter or Q-segment + clock, session
  fastest lap (purple), weather, replay transport. Always visible.
- **Timing tower** — the race story: gap, interval, tire, last lap. Intervals
  under 1s (DRS range) highlight as battles; pit stops replace the interval
  (`IN PIT` / `OUT LAP`) right where your eye is. In qualifying: best lap, gap,
  a live mini-sector strip per driver, and the elimination cutoff line in Q1/Q2.
- **Driver panel** — select anyone with `↑↓`: sector times, mini-sectors,
  full stint history, pit stops, gaps.
- **Track map** — braille circuit with cars as team-colored dots, start/finish
  line marked `S/F`, in-pit cars dimmed.
- **Race control ticker** — the latest message that matters (penalties, safety
  car, flags); routine track-sector/blue-flag spam is filtered into the full
  log behind `r`.

## Data sources

- `livetiming.formula1.com` — the unofficial SignalR feed used by the official
  web/app timing (live), and its static archive (replay). No account needed.
- `api.multiviewer.app` — circuit outlines for the map.

This project is unofficial and is not associated in any way with Formula 1
companies. F1 and related marks are trademarks of Formula One Licensing B.V.

## License

MIT
