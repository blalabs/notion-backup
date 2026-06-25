//! Back up an accessible Notion workspace to a git repository.

use clap::Parser;
use notion_backup::cli;

fn main() {
    let args = cli::Args::parse();
    std::process::exit(cli::run(args));
}
