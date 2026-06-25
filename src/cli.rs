//! Command-line entrypoint for the Notion -> git backup.

use std::io::Write;

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use clap::Parser;
use log::{error, info, LevelFilter};

use crate::client::NotionClient;
use crate::config::Config;
use crate::dump::JsonDumper;
use crate::git_sync;
use crate::render::MarkdownRenderer;

/// Back up an accessible Notion workspace to a git repository.
#[derive(Parser, Debug)]
#[command(name = "notion-backup", about, version)]
pub struct Args {
    /// Override the BACKUP_DIR target directory.
    #[arg(long)]
    backup_dir: Option<String>,

    /// Render Markdown/CSV after the JSON dump (overrides RENDER_MARKDOWN).
    #[arg(long, conflicts_with_all = ["no_render", "render_only"])]
    render: bool,

    /// Skip the Markdown/CSV render even if RENDER_MARKDOWN is set.
    #[arg(long, conflicts_with_all = ["render", "render_only"])]
    no_render: bool,

    /// Render Markdown/CSV from the existing JSON tree; no API calls, no fetch.
    #[arg(long, conflicts_with_all = ["render", "no_render"])]
    render_only: bool,

    /// Force a full re-fetch instead of an incremental dump (overrides FULL_SYNC).
    #[arg(long)]
    full: bool,

    /// Skip the git commit.
    #[arg(long)]
    no_commit: bool,

    /// Commit but do not push to the remote.
    #[arg(long)]
    no_push: bool,

    /// Enable debug logging.
    #[arg(short, long)]
    verbose: bool,
}

impl Args {
    fn should_render(&self, config: &Config) -> bool {
        if self.render || self.render_only {
            return true;
        }
        if self.no_render {
            return false;
        }
        config.render_markdown
    }
}

pub fn run(args: Args) -> i32 {
    init_logging(args.verbose);

    let config = match Config::from_env(args.backup_dir.as_deref(), !args.render_only) {
        Ok(config) => config,
        Err(err) => {
            error!("{err}");
            return 2;
        }
    };

    match backup(&args, &config) {
        Ok(()) => 0,
        Err(err) => {
            error!("{err:#}");
            1
        }
    }
}

fn backup(args: &Args, config: &Config) -> Result<()> {
    // Stage 1: dump raw JSON (skipped for --render-only).
    if !args.render_only {
        let incremental = !(args.full || config.full_sync);
        let client = NotionClient::new(config.notion_token.clone(), config.api_version.clone());
        info!("Dumping workspace JSON to {}", config.json_dir().display());
        JsonDumper::new(&client, config.json_dir(), incremental).run()?;
    }

    // Stage 2: optionally render Markdown/CSV from the JSON tree.
    if args.should_render(config) {
        info!(
            "Rendering Markdown/CSV to {}",
            config.markdown_dir().display()
        );
        MarkdownRenderer::new(config.json_dir(), config.markdown_dir()).run()?;
    } else {
        info!("Skipping Markdown/CSV render (JSON only).");
    }

    if args.no_commit {
        info!("Backup complete; skipping commit (--no-commit).");
        return Ok(());
    }

    git_sync::ensure_repo(&config.backup_dir)?;
    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false);
    let committed = git_sync::commit_all(
        &config.backup_dir,
        &format!("chore: notion backup {timestamp}"),
    )?;

    if committed {
        if let Some(remote) = &config.git_remote {
            if !args.no_push {
                git_sync::push(&config.backup_dir, remote)?;
            }
        }
    }

    info!("Done.");
    Ok(())
}

fn init_logging(verbose: bool) {
    let level = if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Info
    };
    let mut builder = env_logger::Builder::new();
    builder.filter_level(level).format(|buf, record| {
        writeln!(
            buf,
            "{} {} {}: {}",
            Utc::now().format("%Y-%m-%d %H:%M:%S"),
            record.level(),
            record.target(),
            record.args()
        )
    });
    let _ = builder.try_init();
}
