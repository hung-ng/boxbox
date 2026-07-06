# boxbox demo recording — regenerate from the repo root with: vhs docs/demo.tape
#
# Requires `vhs` (https://github.com/charmbracelet/vhs): brew install vhs
#
# vhs runs a minimal shell that does NOT inherit your interactive PATH, so
# `boxbox` won't be found unless it's on PATH here. This tape builds and uses
# the local release binary — build it first (and cache the session once so the
# GIF records instantly and offline):
#   cargo build --release
#   ./target/release/boxbox replay australia race --year 2024   # then press q

# All Set/Output directives must come before any Env or command.
Set FontSize 13
Set Width 1400
Set Height 720
Set Padding 20
Set Framerate 24

Output docs/demo.gif

# Put the locally-built binary on PATH (vhs runs from the repo root).
Env PATH "./target/release:/opt/homebrew/bin:/usr/bin:/bin"

# Launch (and skip the green-flag pre-session by default). Keep the typed
# command out of the recording: Hide until the TUI is up and settled.
#
# Note: --start-at is an ABSOLUTE offset from the start of the recording and
# replaces the green-flag default, so a small value lands in the pre-race.
# We omit it (defaulting to the green flag) and nudge forward with → instead.
Hide
Type "boxbox replay australia race --year 2024 --speed 30"
Enter
Sleep 6s              # let the field populate and settle at the green flag
Right                 # seek +1 min, a little past the green flag
Sleep 2s
Show

Down@600ms 4          # scroll drivers; the detail panel follows the selection
Sleep 2s

Type "r"              # open the full race control log
Sleep 3s
Type "r"              # close it

Type "m"              # cycle view: split → map
Sleep 4s
Type "m"              # map → tower
Sleep 3s
Type "m"              # tower → auto (back to split)
Sleep 2s

Type "+"              # bump the speed up
Sleep 3s

Type "q"              # quit
