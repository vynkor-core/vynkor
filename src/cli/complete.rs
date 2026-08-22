use clap::CommandFactory;
use clap_complete::{generate, Shell};

use super::Cli;

pub fn generate_completions(shell: Shell) {
    let mut cmd = Cli::command();
    generate(shell, &mut cmd, "vyn", &mut std::io::stdout());
}

/// D4/D6: slug completion moved to vynm. Try exec'ing vynm's own hidden
/// command (native support lands with vynm's completion package); a missing
/// binary degrades to an actionable note — completion hooks must not hard-
/// fail the interactive prompt.
pub async fn complete_slugs() -> anyhow::Result<()> {
    match std::process::Command::new("vynm")
        .arg("__complete-slugs")
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        _ => {
            println!("'__complete-slugs' moved to vynm — use 'vynm search <prefix>'; native shell completion ships with vynm's polish package");
            Ok(())
        }
    }
}
