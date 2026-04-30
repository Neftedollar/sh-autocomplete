use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use shac_ml_train::data::{read_jsonl, write_jsonl, SyntheticEvent};
use shac_ml_train::scrub::scrub_text;

#[derive(Parser, Debug)]
#[command(about = "Apply PII scrubbing to a JSONL of SyntheticEvent records")]
struct Args {
    #[arg(long)]
    input: PathBuf,

    #[arg(long)]
    output: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let mut events: Vec<SyntheticEvent> =
        read_jsonl(&args.input).with_context(|| format!("read {}", args.input.display()))?;
    for ev in &mut events {
        ev.cwd = scrub_text(&ev.cwd);
        ev.command = scrub_text(&ev.command);
        ev.prev_command = ev.prev_command.as_ref().map(|c| scrub_text(c));
    }
    write_jsonl(&args.output, &events)?;
    eprintln!(
        "scrubbed {} events: {} → {}",
        events.len(),
        args.input.display(),
        args.output.display()
    );
    Ok(())
}
