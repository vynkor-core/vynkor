use clap::Subcommand;

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
    /// Keep a plugin installed but stop auto-spawning it on boot — renames its
    /// `plugins.d/<slug>.yaml` drop-in to `<slug>.yaml.disabled` (R10-04).
    Disable {
        id: String,
    },
    /// Undo `disable`: restore the `<slug>.yaml` drop-in (R10-04).
    Enable {
        id: String,
    },
}

/// `port`/`tls`: derive the kernel API's base URL (scheme + host + port).
/// `token`: presented as `Authorization: Bearer <token>` on every request —
/// required against a secured kernel (R5-06, AUDIT H-02/H-03).
///
/// V-07 (D4): start/stop/restart/logs stay pure REST calls; the marketplace
/// commands delegate to vynm — the grammar above is kept so existing scripts
/// and muscle memory keep working while the implementation lives in
/// vynkor-manager. Shim removal deferred to stage 3.
pub async fn handle(
    cmd: PluginCmd,
    port: u16,
    token: Option<&str>,
    tls: bool,
    cert_path: Option<&std::path::Path>,
    config_path: &str,
) -> anyhow::Result<()> {
    let base = base_url(port, tls);
    let client = build_client(tls, cert_path)?;

    match cmd {
        PluginCmd::Start { id } => {
            api_post(&client, &base, &format!("/plugins/{id}/start"), token).await?;
            println!("Plugin '{id}' started.");
        }
        PluginCmd::Stop { id } => {
            api_post(&client, &base, &format!("/plugins/{id}/stop"), token).await?;
            println!("Plugin '{id}' stopped.");
        }
        PluginCmd::Restart { id } => {
            api_post(&client, &base, &format!("/plugins/{id}/restart"), token).await?;
            println!("Plugin '{id}' restarted.");
        }
        PluginCmd::Logs { id, lines } => {
            let body = api_get(
                &client,
                &base,
                &format!("/plugins/{id}/logs?lines={lines}"),
                token,
            )
            .await?;
            print_log_lines(&body);
        }
        // ── D4 delegation shims ─────────────────────────────────────────
        // --config is forwarded so vynm resolves the same plugins.d the
        // kernel boots from. Accepted-but-ignored flags (--refresh,
        // --installed) keep the grammar stable.
        PluginCmd::List { .. } => {
            exec_vynm(&["list".into(), "--config".into(), config_path.into()])?
        }
        PluginCmd::Search { query, .. } => exec_vynm(&[
            "search".into(),
            query,
            "--config".into(),
            config_path.into(),
        ])?,
        PluginCmd::Install { target, .. } => exec_vynm(&[
            "install".into(),
            target,
            "--config".into(),
            config_path.into(),
        ])?,
        PluginCmd::Remove { target } => exec_vynm(&[
            "remove".into(),
            target,
            "--config".into(),
            config_path.into(),
        ])?,
        PluginCmd::Disable { id } => {
            exec_vynm(&["disable".into(), id, "--config".into(), config_path.into()])?
        }
        PluginCmd::Enable { id } => {
            exec_vynm(&["enable".into(), id, "--config".into(), config_path.into()])?
        }
    }
    Ok(())
}

/// The logs endpoint returns a JSON array of strings; print one per line.
/// Unparsable bodies print verbatim — never swallowed as empty output.
fn print_log_lines(body: &str) {
    print!("{}", render_log_lines(body));
}

fn render_log_lines(body: &str) -> String {
    match serde_json::from_str::<Vec<String>>(body) {
        Ok(lines) => lines.iter().map(|l| format!("{l}\n")).collect(),
        Err(_) => body.to_string(),
    }
}

/// D4 shim: announce the move, then hand off. A missing binary degrades to an
/// actionable message instead of a bare ENOENT — zero silent breakage.
fn exec_vynm(args: &[String]) -> anyhow::Result<()> {
    println!(
        "note: 'vyn plugin …' moved to vynm — running: vynm {}",
        args.join(" ")
    );
    match std::process::Command::new("vynm").args(args).status() {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => anyhow::bail!("vynm exited with {status}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => anyhow::bail!(
            "'marketplace' commands moved to vynm — install it first \
             (cargo install vynkor-manager), or run manually: vynm {}",
            args.join(" ")
        ),
        Err(e) => Err(e.into()),
    }
}

/// The kernel API's base URL. TLS is on whenever the kernel config declares
/// `tls_cert_path`/`tls_key_path` (see `ApiServer::run`) — never guessed from
/// the port.
fn base_url(port: u16, tls: bool) -> String {
    let scheme = if tls { "https" } else { "http" };
    format!("{scheme}://127.0.0.1:{port}")
}

/// HTTP client for the kernel API. TLS (D-07, on by default) pins the exact
/// certificate the kernel serves — the operator-provided one or the
/// auto-generated self-signed pair — instead of silently accepting anything.
pub(crate) fn build_client(
    tls: bool,
    cert_path: Option<&std::path::Path>,
) -> anyhow::Result<reqwest::Client> {
    if !tls {
        return Ok(reqwest::Client::new());
    }
    let cert_path = cert_path.ok_or_else(|| {
        anyhow::anyhow!("tls enabled but no certificate path known for this config")
    })?;
    let pem = std::fs::read(cert_path).map_err(|_| {
        anyhow::anyhow!(
            "cannot read TLS certificate at {} — start the kernel once so it generates one",
            cert_path.display()
        )
    })?;
    let cert = reqwest::Certificate::from_pem(&pem)?;
    Ok(reqwest::Client::builder()
        .add_root_certificate(cert)
        .build()?)
}

pub(crate) async fn api_get(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: Option<&str>,
) -> anyhow::Result<String> {
    let url = format!("{base}{path}");
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

pub(crate) async fn api_post(
    client: &reqwest::Client,
    base: &str,
    path: &str,
    token: Option<&str>,
) -> anyhow::Result<()> {
    let url = format!("{base}{path}");
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
    fn render_log_lines_prints_json_array_one_per_line() {
        assert_eq!(
            render_log_lines(r#"["line one","line two"]"#),
            "line one\nline two\n"
        );
    }

    #[test]
    fn render_log_lines_empty_array_is_empty_output() {
        assert_eq!(render_log_lines("[]"), "");
    }

    #[test]
    fn render_log_lines_falls_back_on_unparsable_body() {
        assert_eq!(render_log_lines("not json"), "not json");
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

        let client = reqwest::Client::new();
        let body = api_get(&client, &server.url(), "/plugins/x/logs", Some("tok-123"))
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

        let client = reqwest::Client::new();
        api_get(&client, &server.url(), "/plugins/x/logs", None)
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

        let client = reqwest::Client::new();
        api_post(&client, &server.url(), "/plugins/x/start", Some("tok-456"))
            .await
            .unwrap();
        mock.assert_async().await;
    }

    #[test]
    fn build_client_without_tls_is_plain() {
        assert!(build_client(false, None).is_ok());
    }

    #[test]
    fn build_client_tls_requires_a_cert_path() {
        assert!(build_client(true, None).is_err());
    }
}
