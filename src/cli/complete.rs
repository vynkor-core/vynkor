use clap::CommandFactory;
use clap_complete::{generate, Shell};

use crate::marketplace::registry::fetch_registry;

use super::Cli;

pub fn generate_completions(shell: Shell) {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "vyn", &mut std::io::stdout());
}

pub async fn complete_slugs() -> anyhow::Result<()> {
    let entries = fetch_registry(false).await?;
    for e in entries {
        println!("{}", e.slug);
    }
    Ok(())
}
