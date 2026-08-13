use clap::Subcommand;

use crate::marketplace::installer::{
    install, remove_plugin_config, uninstall, write_plugin_config, InstalledPlugin,
};
use crate::marketplace::registry::{
    fetch_registry, fetch_registry_with_url, RegistryEntry, DEFAULT_REGISTRY_URL,
};

#[derive(Subcommand)]
pub enum PluginCmd {
    List {
        #[arg(long)]
        refresh: bool,
        /// List what's installed from the local state store — works offline,
        /// no registry fetch (R10-02).
        #[arg(long)]
        installed: bool,
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
    Remove {
        target: String,
    },
}

/// `port`/`tls`: derive the kernel API's base URL (scheme + host + port).
/// `token`: presented as `Authorization: Bearer <token>` on every request —
/// required against a secured kernel (R5-06, AUDIT H-02/H-03).
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    cmd: PluginCmd,
    port: u16,
    registry_url: Option<&str>,
    token: Option<&str>,
    tls: bool,
    cache_ttl_secs: u64,
    tmp_dir: &std::path::Path,
    max_archive_bytes: u64,
    max_extracted_bytes: u64,
    max_archive_entries: usize,
    marketplace_public_key: Option<&str>,
    config_path: &str,
    plugins_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let fetch = |refresh: bool| {
        let url = registry_url.unwrap_or("");
        async move {
            if url.is_empty() {
                fetch_registry(refresh, cache_ttl_secs, tmp_dir, marketplace_public_key).await
            } else {
                fetch_registry_with_url(
                    url,
                    refresh,
                    cache_ttl_secs,
                    tmp_dir,
                    marketplace_public_key,
                )
                .await
            }
        }
    };
    let base = base_url(port, tls);

    match cmd {
        PluginCmd::List { refresh, installed } => {
            if installed {
                print_installed(tmp_dir);
            } else {
                let entries = fetch(refresh).await?;
                print_table(&entries);
            }
        }
        PluginCmd::Search { query, refresh } => {
            let entries = fetch(refresh).await?;
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
            api_post(&base, &format!("/plugins/{id}/start"), token).await?;
            println!("Plugin '{id}' started.");
        }
        PluginCmd::Stop { id } => {
            api_post(&base, &format!("/plugins/{id}/stop"), token).await?;
            println!("Plugin '{id}' stopped.");
        }
        PluginCmd::Restart { id } => {
            api_post(&base, &format!("/plugins/{id}/restart"), token).await?;
            println!("Plugin '{id}' restarted.");
        }
        PluginCmd::Logs { id, lines } => {
            let body = api_get(&base, &format!("/plugins/{id}/logs?lines={lines}"), token).await?;
            print!("{body}");
        }
        PluginCmd::Install { target, refresh } => {
            let entries = fetch(refresh).await?;
            let source_url = registry_url.unwrap_or(DEFAULT_REGISTRY_URL);
            let installed: InstalledPlugin = install(
                &entries,
                &target,
                tmp_dir,
                max_archive_bytes,
                max_extracted_bytes,
                max_archive_entries,
                marketplace_public_key,
                source_url,
            )
            .await?;
            if write_plugin_config(plugins_dir, &installed)? {
                println!(
                    "✓ Wrote auto-spawn config to {}/{}.yaml",
                    plugins_dir.display(),
                    installed.slug
                );
            } else {
                println!(
                    "Note: {}/{}.yaml already exists — left untouched.",
                    plugins_dir.display(),
                    installed.slug
                );
            }
        }
        PluginCmd::Remove { target } => {
            uninstall(&target, tmp_dir)?;

            match remove_plugin_config(plugins_dir, &target) {
                Ok(true) => {
                    println!(
                        "✓ Removed auto-spawn config {}/{}.yaml",
                        plugins_dir.display(),
                        target
                    );
                }
                Ok(false) => {}
                Err(e) => return Err(e.into()),
            }

            // the drop-in can be gone while the plugin is still live in
            // `plugins:` (deprecated inline list) or another drop-in — warn
            if let Ok(cfg) = crate::utils::config::load_config(config_path) {
                if cfg.plugins.iter().any(|p| p.id == target) {
                    println!(
                        "Note: '{target}' is still configured in `plugins:` or another drop-in — remove that entry too."
                    );
                }
            }
        }
    }
    Ok(())
}

/// The kernel API's base URL. TLS is on whenever the kernel config declares
/// `tls_cert_path`/`tls_key_path` (see `ApiServer::run`) — never guessed from
/// the port.
fn base_url(port: u16, tls: bool) -> String {
    let scheme = if tls { "https" } else { "http" };
    format!("{scheme}://127.0.0.1:{port}")
}

/// Offline installed-plugin listing straight from the state store — no
/// registry fetch (R10-02).
fn print_installed(tmp_dir: &std::path::Path) {
    let state = crate::marketplace::state::load_state(tmp_dir);
    if state.entries.is_empty() {
        println!("No plugins installed. Run 'vyn plugin install <slug>' to install one.");
        return;
    }

    const HEADERS: [&str; 4] = ["SLUG", "VERSION", "INSTALLED_AT", "SOURCE"];
    let mut widths: [usize; 4] = HEADERS.map(str::len);
    let rows: Vec<[String; 4]> = state
        .entries
        .iter()
        .map(|e| {
            [
                e.slug.clone(),
                e.version.clone(),
                crate::marketplace::state::format_ts(e.installed_at),
                e.source_url.clone(),
            ]
        })
        .collect();
    for row in &rows {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.len());
        }
    }

    println!(
        "{:<w0$}  {:<w1$}  {:<w2$}  {}",
        HEADERS[0],
        HEADERS[1],
        HEADERS[2],
        HEADERS[3],
        w0 = widths[0],
        w1 = widths[1],
        w2 = widths[2],
    );
    for row in &rows {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            w0 = widths[0],
            w1 = widths[1],
            w2 = widths[2],
        );
    }
}

fn print_table(entries: &[RegistryEntry]) {
    const HEADERS: [&str; 7] = [
        "ID",
        "SLUG",
        "VERSION",
        "MIN_KERNEL",
        "MAX_KERNEL",
        "PERMISSIONS",
        "DESCRIPTION",
    ];

    // revoked entries stay listed so an operator sees why install fails
    let display_slug = |e: &RegistryEntry| {
        if e.is_revoked() {
            format!("{} [revoked]", e.slug)
        } else {
            e.slug.clone()
        }
    };

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
        let slug = display_slug(e);
        widths[0] = widths[0].max(e.id.len());
        widths[1] = widths[1].max(slug.len());
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
            display_slug(e),
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

async fn api_get(base: &str, path: &str, token: Option<&str>) -> anyhow::Result<String> {
    let url = format!("{base}{path}");
    let client = reqwest::Client::new();
    let mut req = client.get(&url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("kernel not running — start it with `vyn start`"))?;
    if !resp.status().is_success() {
        anyhow::bail!("API error: HTTP {}", resp.status());
    }
    Ok(resp.text().await?)
}

async fn api_post(base: &str, path: &str, token: Option<&str>) -> anyhow::Result<()> {
    let url = format!("{base}{path}");
    let client = reqwest::Client::new();
    let mut req = client.post(&url);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("kernel not running — start it with `vyn start`"))?;
    if !resp.status().is_success() {
        anyhow::bail!("API error: HTTP {}", resp.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_defaults_to_http() {
        assert_eq!(base_url(8080, false), "http://127.0.0.1:8080");
    }

    #[test]
    fn base_url_uses_https_when_tls_configured() {
        assert_eq!(base_url(8443, true), "https://127.0.0.1:8443");
    }

    #[tokio::test]
    async fn api_get_attaches_bearer_token_when_present() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/plugins/x/logs")
            .match_header("authorization", "Bearer tok-123")
            .with_status(200)
            .with_body("log line")
            .create_async()
            .await;

        let body = api_get(&server.url(), "/plugins/x/logs", Some("tok-123"))
            .await
            .unwrap();
        assert_eq!(body, "log line");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn api_get_sends_no_authorization_header_without_token() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/plugins/x/logs")
            .match_header("authorization", mockito::Matcher::Missing)
            .with_status(200)
            .with_body("log line")
            .create_async()
            .await;

        api_get(&server.url(), "/plugins/x/logs", None)
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn api_post_attaches_bearer_token_when_present() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/plugins/x/start")
            .match_header("authorization", "Bearer tok-456")
            .with_status(200)
            .create_async()
            .await;

        api_post(&server.url(), "/plugins/x/start", Some("tok-456"))
            .await
            .unwrap();
        mock.assert_async().await;
    }
}
