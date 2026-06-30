use clap::Subcommand;

use crate::marketplace::registry::{fetch_registry, PluginEntry};

#[derive(Subcommand)]
pub enum PluginCmd {
    List {
        #[arg(long)]
        refresh: bool,
    },
    Search {
        query: String,
        #[arg(long)]
        refresh: bool,
    },
    Start {
        id: String,
    },
    Stop {
        id: String,
    },
    Restart {
        id: String,
    },
    Logs {
        id: String,
        #[arg(long, default_value = "20")]
        lines: usize,
    },
    Install {
        target: String,
        #[arg(long)]
        refresh: bool,
    },
}

pub async fn handle(cmd: PluginCmd, port: u16) -> anyhow::Result<()> {
    match cmd {
        PluginCmd::List { refresh } => {
            let entries = fetch_registry(refresh).await?;
            print_table(&entries);
        }
        PluginCmd::Search { query, refresh } => {
            let entries = fetch_registry(refresh).await?;
            let q = query.to_lowercase();
            let filtered: Vec<_> = entries
                .into_iter()
                .filter(|e| {
                    e.slug.to_lowercase().contains(&q)
                        || e.name.to_lowercase().contains(&q)
                        || e.description.to_lowercase().contains(&q)
                })
                .collect();
            print_table(&filtered);
        }
        PluginCmd::Start { id } => {
            api_post(port, &format!("/plugins/{id}/start")).await?;
            println!("Plugin '{id}' started.");
        }
        PluginCmd::Stop { id } => {
            api_post(port, &format!("/plugins/{id}/stop")).await?;
            println!("Plugin '{id}' stopped.");
        }
        PluginCmd::Restart { id } => {
            api_post(port, &format!("/plugins/{id}/restart")).await?;
            println!("Plugin '{id}' restarted.");
        }
        PluginCmd::Logs { id, lines } => {
            let body = api_get(port, &format!("/plugins/{id}/logs?lines={lines}")).await?;
            print!("{body}");
        }
        PluginCmd::Install { .. } => {
            anyhow::bail!("T-10 not implemented yet");
        }
    }
    Ok(())
}

fn print_table(entries: &[PluginEntry]) {
    const HEADERS: [&str; 7] = [
        "ID",
        "SLUG",
        "VERSION",
        "MIN_KERNEL",
        "MAX_KERNEL",
        "PERMISSIONS",
        "DESCRIPTION",
    ];

    // Compute column widths
    let mut widths = [
        HEADERS[0].len(),
        HEADERS[1].len(),
        HEADERS[2].len(),
        HEADERS[3].len(),
        HEADERS[4].len(),
        HEADERS[5].len(),
        HEADERS[6].len(),
    ];

    for e in entries {
        let perms = e.permissions.join(", ");
        widths[0] = widths[0].max(e.id.len());
        widths[1] = widths[1].max(e.slug.len());
        widths[2] = widths[2].max(e.version.len());
        widths[3] = widths[3].max(e.min_kernel_version.len());
        widths[4] = widths[4].max(e.max_kernel_version.len());
        widths[5] = widths[5].max(perms.len());
        widths[6] = widths[6].max(e.description.len());
    }

    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}  {}",
        HEADERS[0],
        HEADERS[1],
        HEADERS[2],
        HEADERS[3],
        HEADERS[4],
        HEADERS[5],
        HEADERS[6],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
        w3 = widths[3],
        w4 = widths[4],
        w5 = widths[5],
    );

    for e in entries {
        let perms = e.permissions.join(", ");
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {:<w5$}  {}",
            e.id,
            e.slug,
            e.version,
            e.min_kernel_version,
            e.max_kernel_version,
            perms,
            e.description,
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
            w3 = widths[3],
            w4 = widths[4],
            w5 = widths[5],
        );
    }
}

async fn api_get(port: u16, path: &str) -> anyhow::Result<String> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let resp = reqwest::get(&url).await.map_err(|_| {
        anyhow::anyhow!("kernel not running — start it with `vyn start`")
    })?;
    if !resp.status().is_success() {
        anyhow::bail!("API error: HTTP {}", resp.status());
    }
    Ok(resp.text().await?)
}

async fn api_post(port: u16, path: &str) -> anyhow::Result<()> {
    let url = format!("http://127.0.0.1:{port}{path}");
    let client = reqwest::Client::new();
    let resp = client.post(&url).send().await.map_err(|_| {
        anyhow::anyhow!("kernel not running — start it with `vyn start`")
    })?;
    if !resp.status().is_success() {
        anyhow::bail!("API error: HTTP {}", resp.status());
    }
    Ok(())
}

