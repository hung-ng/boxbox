mod message;
mod source;
mod state;
mod ui;

use anyhow::{bail, Result};
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
    /// Replay an archived session, e.g. `boxbox replay bahrain race`
    Replay {
        /// Words matching the meeting/session, e.g. "monaco qualifying"
        query: Vec<String>,
        /// Season year
        #[arg(long, default_value_t = default_year())]
        year: u16,
        /// Playback speed multiplier
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// Start offset into the recording (HH:MM:SS or MM:SS)
        #[arg(long)]
        start_at: Option<String>,
    },
    /// List archived sessions for a season
    Sessions {
        /// Season year
        #[arg(long, default_value_t = default_year())]
        year: u16,
    },
}

fn default_year() -> u16 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    1970 + (secs / 31_557_600) as u16
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;

    match cli.command {
        Command::Sessions { year } => rt.block_on(list_sessions(year)),
        Command::Live => {
            let (tx, rx) = std::sync::mpsc::channel();
            rt.spawn(source::live::run(tx.clone()));
            ui::run(rx, tx, None, rt.handle().clone(), 1.0)
        }
        Command::Replay {
            query,
            year,
            speed,
            start_at,
        } => {
            if query.is_empty() {
                bail!("give me something to find, e.g. `boxbox replay bahrain race`");
            }
            let start = match &start_at {
                Some(s) => parse_start(s)?,
                None => Duration::ZERO,
            };
            let archive = Archive::new()?;
            let session = rt.block_on(resolve_session(&archive, year, &query))?;
            println!(
                "Loading {} — {} ({})",
                session.meeting, session.session.name, session.session.start_date
            );
            let entries = rt.block_on(source::replay::load_session(
                &archive,
                &session.session,
                |topic, bytes| {
                    if bytes > 0 {
                        println!("  {topic}: {:.1} MB", bytes as f64 / 1e6);
                    }
                },
            ))?;
            if entries.is_empty() {
                bail!("no data in the archive for this session");
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
            ui::run(rx, tx, Some(ctrl_tx), rt.handle().clone(), speed)
        }
    }
}

async fn resolve_session(
    archive: &Archive,
    year: u16,
    query: &[String],
) -> Result<source::archive::SessionRef> {
    let mut matches = archive.find_sessions(year, query).await?;
    match matches.len() {
        0 => bail!(
            "no {year} session matches \"{}\" — try `boxbox sessions --year {year}`",
            query.join(" ")
        ),
        1 => Ok(matches.remove(0)),
        _ => {
            eprintln!("Multiple sessions match — be more specific:");
            for m in &matches {
                eprintln!(
                    "  {} — {} ({})",
                    m.meeting, m.session.name, m.session.start_date
                );
            }
            bail!("{} matches", matches.len());
        }
    }
}

async fn list_sessions(year: u16) -> Result<()> {
    let archive = Archive::new()?;
    let index = archive.year_index(year).await?;
    for meeting in &index.meetings {
        println!("{}", meeting.name);
        for s in &meeting.sessions {
            let archived = if s.path.is_some() { "" } else { "  (not archived)" };
            println!("  {} — {}{archived}", s.name, s.start_date);
        }
    }
    Ok(())
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
