mod message;
mod source;
mod state;
mod ui;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use source::archive::Archive;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "boxbox", about = "F1 live timing in your terminal", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Connect to the live timing feed (only active during sessions)
    Live,
    /// Replay an archived session, e.g. `boxbox replay monaco race` or
    /// `boxbox replay sao-paulo qualifying`. No args opens an interactive
    /// browser; `--list` prints the schedule. Pick a season with `--year`.
    Replay {
        /// Race/session tokens, e.g. "monaco qualifying" or "sao-paulo race".
        /// Dash multi-word names (las-vegas, red-bull-ring); tokens are matched
        /// whole, so a partial word like "silvers" won't hit "silverstone".
        /// A year goes in `--year`, not here.
        query: Vec<String>,
        /// Season year, e.g. `--year 2023`. The season is always chosen with
        /// this flag; a year in the positional query is rejected with a hint.
        #[arg(long, default_value_t = default_year())]
        year: u16,
        /// Playback speed multiplier
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// Start offset into the recording (HH:MM:SS or MM:SS). Default: the
        /// green flag, skipping the pre-session; pass 0:00 for the full grid.
        #[arg(long)]
        start_at: Option<String>,
        /// Print the season schedule and exit
        #[arg(long)]
        list: bool,
    },
    /// Inspect or clear the on-disk download cache (session streams, schedules,
    /// circuit outlines).
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
}

#[derive(Subcommand)]
enum CacheAction {
    /// Print the cache directory path
    Path,
    /// Show cache size on disk, broken down by category
    Info,
    /// Delete cached data. Without `--year`, wipes everything; with `--year`,
    /// removes only that season's cached session streams.
    Clear {
        /// Limit the clear to one season's session streams
        #[arg(long)]
        year: Option<u16>,
    },
}

fn default_year() -> u16 {
    use chrono::Datelike;
    chrono::Utc::now().year() as u16
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;

    match cli.command {
        Command::Live => {
            let (tx, rx) = std::sync::mpsc::channel();
            rt.spawn(source::live::run(tx.clone()));
            // Look up the next upcoming session for the empty-state hint (3.10).
            let hint_tx = tx.clone();
            rt.spawn(async move {
                let year = default_year();
                // Once the season's over, next_session_line yields None; roll
                // over to next year (Jolpica publishes it early) so a December
                // `boxbox live` still shows a hint (6.2).
                let line = match next_session_line(year).await {
                    Some(l) => Some(l),
                    None => next_session_line(year + 1).await,
                };
                if let Some(line) = line {
                    let _ = hint_tx.send(message::SourceEvent::NextSession(line));
                }
            });
            ui::run(rx, tx, None, rt.handle().clone(), 1.0, None, None)
        }
        Command::Replay {
            year, list: true, ..
        } => rt.block_on(print_schedule(year)),
        Command::Replay {
            query,
            year,
            speed,
            start_at,
            list: false,
        } => {
            // Parse an explicit --start-at up front so a bad value fails before
            // we download anything.
            let explicit_start = match &start_at {
                Some(s) => Some(parse_start(s)?),
                None => None,
            };
            let archive = Archive::new()?;
            let session = rt.block_on(resolve_session(&archive, year, &query))?;
            let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
            println!(
                "Loading {} — {} {}",
                session.meeting,
                session.session.name,
                tint(tty, "2", &format!("({})", session.session.start_date))
            );
            // Per-topic download sizes are progress noise: dimmed.
            let entries = rt.block_on(source::replay::load_session(
                &archive,
                &session.session,
                move |topic, bytes| {
                    if bytes > 0 {
                        println!(
                            "{}",
                            tint(
                                tty,
                                "2",
                                &format!("  {topic}: {:.1} MB", bytes as f64 / 1e6)
                            )
                        );
                    }
                },
            ))?;
            if entries.is_empty() {
                bail!(
                    "no timing data on the archive for {} — {} ({}); \
                     the session may not have been recorded",
                    session.meeting,
                    session.session.name,
                    session.session.start_date
                );
            }
            // Total recording length (last entry's timestamp), for the timeline
            // and the --start-at bounds check.
            let total = entries.last().map(|e| e.ts).unwrap_or(Duration::ZERO);
            // An explicit --start-at past the end would start a replay that
            // instantly ends — bail helpfully instead (6.3).
            if let Some(s) = explicit_start {
                if s >= total {
                    bail!(
                        "--start-at {} is past the end of this recording ({})",
                        fmt_hms(s),
                        fmt_hms(total)
                    );
                }
            }
            // Default the seek to the green flag so we land on racing, not the
            // long pre-session grid. An explicit --start-at always wins; the
            // pre-session stays in the timeline and is reachable by rewinding.
            // The green offset also feeds the UI's `g` restart key.
            let green = source::replay::green_flag(&entries).unwrap_or(Duration::ZERO);
            let start = explicit_start.unwrap_or(green);
            if explicit_start.is_none() && !start.is_zero() {
                println!("Starting at the green flag ({}).", fmt_hms(start));
            }
            println!("{} messages — starting replay", entries.len());

            let (tx, rx) = std::sync::mpsc::channel();
            let (ctrl_tx, ctrl_rx) = tokio::sync::mpsc::unbounded_channel();
            rt.spawn(source::replay::play(
                entries,
                start,
                speed,
                tx.clone(),
                ctrl_rx,
            ));
            ui::run(
                rx,
                tx,
                Some(ctrl_tx),
                rt.handle().clone(),
                speed,
                Some(total),
                Some(green),
            )
        }
        Command::Cache { action } => run_cache(action),
    }
}

fn run_cache(action: CacheAction) -> Result<()> {
    let archive = Archive::new()?;
    match action {
        CacheAction::Path => println!("{}", archive.cache_dir().display()),
        CacheAction::Info => {
            let u = archive.cache_usage();
            println!("Cache: {}", archive.cache_dir().display());
            println!("  sessions:  {}", fmt_bytes(u.sessions));
            println!("  schedules: {}", fmt_bytes(u.schedules));
            println!("  circuits:  {}", fmt_bytes(u.circuits));
            println!("  total:     {}", fmt_bytes(u.total()));
        }
        CacheAction::Clear { year } => {
            let freed = archive.clear_cache(year)?;
            match year {
                Some(y) => println!("Cleared {y} session cache — freed {}", fmt_bytes(freed)),
                None => println!("Cleared cache — freed {}", fmt_bytes(freed)),
            }
        }
    }
    Ok(())
}

/// A byte count as a human-readable size (B, KB, MB, GB; 1 KB = 1024 B).
fn fmt_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

/// Today's date in UTC as `YYYY-MM-DD`, for the availability gate.
fn today() -> String {
    chrono::Utc::now().date_naive().to_string()
}

/// First session with a date on or after today, as a display line for the live
/// empty state (3.10). Returns None if the schedule can't be loaded or the
/// season is over. Date-only granularity is intentional.
async fn next_session_line(year: u16) -> Option<String> {
    let archive = Archive::new().ok()?;
    let schedule = archive.schedule(year).await.ok()?;
    let today = today();
    schedule
        .iter()
        .flat_map(|r| r.sessions.iter().map(move |s| (r, s)))
        .filter(|(_, s)| s.date.as_str() >= today.as_str())
        .min_by(|a, b| a.1.date.cmp(&b.1.date))
        .map(|(r, s)| format!("{} — {} ({})", r.name, s.name, s.date))
}

/// Resolve a replay target from the unified Jolpica schedule. With a query:
/// exactly one *available* session matches → play it directly; otherwise drop
/// into the browser pre-filtered by the query. Empty query → full browser.
/// A query token shaped like a year (1900–2099). Seasons are chosen with
/// `--year`, never as a positional token, so such a token is almost always a
/// mistake — we fail helpfully rather than searching for a race named "2023".
fn year_token(t: &str) -> bool {
    t.len() == 4
        && (t.starts_with("19") || t.starts_with("20"))
        && t.chars().all(|c| c.is_ascii_digit())
}

async fn resolve_session(
    archive: &Archive,
    year: u16,
    query: &[String],
) -> Result<source::archive::SessionRef> {
    // A year in the positional query is a footgun (6.1): point at --year and
    // echo the user's other tokens so the corrected command is copy-pasteable.
    if let Some(y) = query.iter().find(|t| year_token(t)) {
        let rest: Vec<&str> = query
            .iter()
            .filter(|t| !year_token(t))
            .map(String::as_str)
            .collect();
        bail!(
            "a year goes in --year: boxbox replay {} --year {y}",
            if rest.is_empty() {
                "<race>".to_string()
            } else {
                rest.join(" ")
            }
        );
    }
    let schedule = archive.schedule(year).await.with_context(|| {
        format!(
            "couldn't load the {year} schedule (Jolpica); try `boxbox replay --list --year {year}`"
        )
    })?;
    if schedule.is_empty() {
        bail!("no races found for {year} — Jolpica has schedules from 1950 on; check the year");
    }
    let today = today();

    if !query.is_empty() {
        let expanded = source::schedule::expand_query(query);
        // (race, session) pairs matching every query word, available only.
        let hits: Vec<_> = schedule
            .iter()
            .flat_map(|r| {
                r.matches(&expanded)
                    .into_iter()
                    .filter(|s| s.is_available(&today))
                    .map(move |s| (r, s))
            })
            .collect();
        if let [(race, scheduled)] = hits.as_slice() {
            return Ok(session_ref(archive, year, race, scheduled));
        }
    }

    browse(archive, year, &schedule, query, &today)
}

/// Interactive race → session picker over the schedule. `query` pre-filters the
/// race list (races with at least one matching session), so an ambiguous query
/// lands in a narrowed browser rather than a dead end.
fn browse(
    archive: &Archive,
    year: u16,
    schedule: &[source::schedule::ScheduledRace],
    query: &[String],
    today: &str,
) -> Result<source::archive::SessionRef> {
    let tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let expanded = source::schedule::expand_query(query);
    let filtered: Vec<_> = schedule
        .iter()
        .filter(|r| !query.is_empty() && !r.matches(&expanded).is_empty())
        .collect();
    let candidates: Vec<_> = if filtered.is_empty() {
        if !query.is_empty() {
            eprintln!(
                "nothing in {year} matched {} — showing the full schedule.\n{}",
                query.join(" "),
                tint(
                    tty,
                    "2",
                    "(tokens match whole words; dash multi-word names, e.g. sao-paulo)"
                )
            );
        }
        schedule.iter().collect()
    } else {
        filtered
    };

    let race = if candidates.len() == 1 {
        candidates[0]
    } else {
        eprintln!("\n{}", tint(tty, "1", &format!("{year} races:")));
        for (i, r) in candidates.iter().enumerate() {
            eprintln!(
                "  {:>2}) {} {}",
                i + 1,
                r.name,
                tint(tty, "2", &format!("({})", r.race_date))
            );
        }
        candidates[prompt_index("Pick a race", candidates.len(), |_| true)?]
    };

    eprintln!("\n{}", tint(tty, "1", &format!("{} sessions:", race.name)));
    for (i, s) in race.sessions.iter().enumerate() {
        let base = format!("  {:>2}) {}", i + 1, s.name);
        eprintln!(
            "{}",
            session_line(tty, &base, &format!("({})", s.date), s.is_available(today))
        );
    }
    let sidx = prompt_index("Pick a session", race.sessions.len(), |i| {
        race.sessions[i].is_available(today)
    })?;
    Ok(session_ref(archive, year, race, &race.sessions[sidx]))
}

fn session_ref(
    archive: &Archive,
    year: u16,
    race: &source::schedule::ScheduledRace,
    scheduled: &source::schedule::ScheduledSession,
) -> source::archive::SessionRef {
    source::archive::SessionRef {
        meeting: race.name.clone(),
        session: archive.session_from_schedule(year, race, scheduled),
    }
}

/// Prompt on stderr for a 1-based selection, returning a 0-based index.
/// `available(idx)` gates which rows may be chosen (future sessions can't).
fn prompt_index(label: &str, len: usize, available: impl Fn(usize) -> bool) -> Result<usize> {
    use std::io::Write;
    loop {
        eprint!("{label} [1-{len}]: ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line)? == 0 {
            bail!("no selection (end of input) — pass a race/session query to skip the picker");
        }
        match line.trim().parse::<usize>() {
            Ok(n) if (1..=len).contains(&n) && available(n - 1) => return Ok(n - 1),
            Ok(n) if (1..=len).contains(&n) => eprintln!("that session isn't available yet"),
            _ => eprintln!("enter a number between 1 and {len}"),
        }
    }
}

/// Print the full season schedule from Jolpica, marking unheld sessions.
async fn print_schedule(year: u16) -> Result<()> {
    let archive = Archive::new()?;
    let schedule = archive.schedule(year).await?;
    let today = today();
    let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
    println!("{}", tint(tty, "1", &format!("{year} season schedule")));
    for race in &schedule {
        println!("\n{}", tint(tty, "1", &race.name));
        for s in &race.sessions {
            // Session names top out at "Sprint Qualifying" (17 chars); pad so
            // the dates line up in a column.
            let base = format!("  {:<18}", s.name);
            println!(
                "{}",
                session_line(tty, &base, &s.date, s.is_available(&today))
            );
        }
    }
    Ok(())
}

/// One picker/schedule session line, shared so the list and the picker can't
/// drift: available sessions dim only the date; unavailable (future) ones are
/// dimmed whole with a suffix, matching the browser's rule that they can't be
/// picked.
fn session_line(tty: bool, base: &str, date: &str, available: bool) -> String {
    if available {
        format!("{base} {}", tint(tty, "2", date))
    } else {
        tint(tty, "2", &format!("{base} {date} — not yet available"))
    }
}

/// Wrap `s` in an ANSI SGR style (`code`: "1" bold, "2" dim) when the output
/// stream is a terminal; plain text when piped or redirected.
fn tint(tty: bool, code: &str, s: &str) -> String {
    if tty {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// A `Duration` as `H:MM:SS`, for logging the auto-seek point.
fn fmt_hms(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

fn parse_start(s: &str) -> Result<Duration> {
    let nums: Vec<u64> = s
        .split(':')
        .map(|p| p.parse::<u64>())
        .collect::<Result<_, _>>()
        .map_err(|_| anyhow::anyhow!("bad --start-at, use HH:MM:SS or MM:SS"))?;
    Ok(match nums.as_slice() {
        [m, s] => Duration::from_secs(m * 60 + s),
        [h, m, s] => Duration::from_secs(h * 3600 + m * 60 + s),
        _ => bail!("bad --start-at, use HH:MM:SS or MM:SS"),
    })
}

#[cfg(test)]
mod tests {
    use super::year_token;

    #[test]
    fn year_token_detects_seasons_only() {
        assert!(year_token("2023"));
        assert!(year_token("1999"));
        assert!(year_token("2026"));
        // Not years: race names, partial digits, out-of-range centuries.
        assert!(!year_token("monaco"));
        assert!(!year_token("race"));
        assert!(!year_token("202"));
        assert!(!year_token("20233"));
        assert!(!year_token("1850"));
        assert!(!year_token("r12"));
    }
}
