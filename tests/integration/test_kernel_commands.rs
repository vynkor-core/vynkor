use super::helpers::start_kernel;
use std::time::Duration;
use tokio::time::timeout;
use veyron::proto::veyron::{ActionStatus, CommandStatus, PluginManifest};

fn admin_manifest() -> PluginManifest {
    PluginManifest {
        permissions: vec!["PERMISSION_KERNEL_ADMIN".to_string()],
        ..Default::default()
    }
}
use veyron_sdk::VeyronClient;

#[tokio::test]
async fn health_check_via_ipc_returns_ok_with_json_fields() {
    let (shutdown_tx, _registry, _bus) = start_kernel("/tmp/veyron_integ_cmd_hc.sock", 19210).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_hc.sock")
        .await
        .unwrap();
    client
        .register("cmd-hc-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("cmd-1", "health_check", b"{}"),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandOk);
    assert!(ack.error.is_empty(), "unexpected error: {}", ack.error);
    let json = String::from_utf8(ack.data_json).unwrap();
    assert!(
        json.contains("uptime_secs"),
        "missing uptime_secs in: {json}"
    );
    assert!(
        json.contains("plugin_count"),
        "missing plugin_count in: {json}"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn kernel_command_ack_echoes_command_id() {
    let (shutdown_tx, _registry, _bus) = start_kernel("/tmp/veyron_integ_cmd_id.sock", 19211).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_id.sock")
        .await
        .unwrap();
    client
        .register("cmd-id-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("unique-id-42", "health_check", b""),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.command_id, "unique-id-42");

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn unknown_command_via_ipc_returns_command_unknown() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_cmd_unk.sock", 19212).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_unk.sock")
        .await
        .unwrap();
    client
        .register("cmd-unk-client", admin_manifest())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("x1", "totally_unknown_cmd", b""),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandUnknown);
    assert!(
        ack.error.contains("totally_unknown_cmd"),
        "error should name the command, got: {}",
        ack.error
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn reload_config_without_path_returns_error_via_ipc() {
    // Kernel started without a config_file path → reload must return COMMAND_ERROR
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_cmd_reload.sock", 19213).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_reload.sock")
        .await
        .unwrap();
    client
        .register("cmd-reload-client", admin_manifest())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("r1", "reload_config", b""),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandError);
    assert!(
        ack.error.contains("no config path"),
        "expected 'no config path', got: {}",
        ack.error
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn reload_config_without_admin_permission_is_denied() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_cmd_reload_denied.sock", 19214).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_reload_denied.sock")
        .await
        .unwrap();
    client
        .register("cmd-reload-denied-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("r2", "reload_config", b""),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandPermissionDenied);
    assert!(
        ack.error.contains("PERMISSION_KERNEL_ADMIN"),
        "expected PERMISSION_KERNEL_ADMIN in error, got: {}",
        ack.error
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn health_check_exempt_from_admin_permission() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_cmd_hc_noauth.sock", 19215).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_hc_noauth.sock")
        .await
        .unwrap();
    client
        .register("cmd-hc-noauth-client", PluginManifest::default())
        .await
        .unwrap();

    let ack = timeout(
        Duration::from_secs(2),
        client.send_command("hc-1", "health_check", b"{}"),
    )
    .await
    .expect("timed out")
    .expect("send_command failed");

    assert_eq!(ack.status(), CommandStatus::CommandOk);

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn kernel_targeted_action_request_returns_not_found_not_fake_ok() {
    // R5-07 interim honesty fix: the kernel's ActionRequest handler is a
    // permission-check-only stub — it never routes to a provider or executes
    // anything. Reporting ACTION_OK after a passing permission check would
    // lie to callers about work that never happened (AUDIT H-05).
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_cmd_action_stub.sock", 19216).await;

    let mut client = VeyronClient::connect("/tmp/veyron_integ_cmd_action_stub.sock")
        .await
        .unwrap();
    client
        .register(
            "action-stub-client",
            PluginManifest {
                permissions: vec!["PERMISSION_SYSTEM".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let resp = timeout(
        Duration::from_secs(2),
        client.send_action("get_cpu", b"{}", 2000),
    )
    .await
    .expect("timed out")
    .expect("send_action failed");

    assert_eq!(
        resp.status,
        ActionStatus::ActionNotFound as i32,
        "kernel has no action executor yet — must not claim ACTION_OK"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn kernel_routes_action_to_declared_provider_and_correlates_response() {
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_route.sock", 19217).await;

    // Provider registers first and declares the action.
    let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_route.sock")
        .await
        .unwrap();
    provider
        .register(
            "weather-provider",
            PluginManifest {
                actions: vec!["get_weather".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut requester = VeyronClient::connect("/tmp/veyron_integ_action_route.sock")
        .await
        .unwrap();
    requester
        .register("weather-requester", PluginManifest::default())
        .await
        .unwrap();

    // Requester fires the action at "kernel" (existing SDK API, unaware of routing).
    // Spawned so the request is actually sent now: an `async fn` call produces
    // a lazy future that does nothing until polled, so binding it without
    // driving it would leave the provider waiting on a request that was never
    // written to the socket.
    let request_fut = tokio::spawn(async move {
        requester
            .send_action("get_weather", br#"{"city":"nyc"}"#, 2000)
            .await
    });

    // Provider receives the routed request and answers OK, targeted at "kernel".
    let received = timeout(Duration::from_secs(2), provider.recv())
        .await
        .expect("provider recv timed out")
        .expect("provider recv failed");
    let internal_action_id = match received.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionRequest(req)) => {
            assert_eq!(req.action, "get_weather");
            assert_eq!(req.params_json, br#"{"city":"nyc"}"#);
            req.action_id
        }
        other => panic!("expected ActionRequest, got {other:?}"),
    };

    let resp_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
            veyron::proto::veyron::ActionResponse {
                action_id: internal_action_id,
                status: ActionStatus::ActionOk as i32,
                data_json: br#"{"temp_f":72}"#.to_vec(),
                error: String::new(),
            },
        )),
        ..Default::default()
    };
    provider.send("kernel", resp_env).await.unwrap();

    let resp = timeout(Duration::from_secs(2), request_fut)
        .await
        .expect("timed out")
        .expect("task panicked")
        .expect("send_action failed");

    assert_eq!(resp.status, ActionStatus::ActionOk as i32);
    assert_eq!(resp.data_json, br#"{"temp_f":72}"#);

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn action_response_from_non_provider_plugin_is_rejected_not_proxied() {
    // AUDIT (Critical): the internal correlation id handed to the provider
    // is minted from a global, monotonic, zero-entropy counter
    // (`kact-<n>`), so it's trivially predictable/observable by any other
    // registered plugin. A registered plugin with no declared actions must
    // not be able to answer on behalf of the real provider — either to
    // inject falsified data or to steal/grief the response slot before the
    // legitimate provider replies.
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_spoof.sock", 19218).await;

    let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_spoof.sock")
        .await
        .unwrap();
    provider
        .register(
            "weather-provider-real",
            PluginManifest {
                actions: vec!["get_weather".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // An unrelated plugin: registered, but declares no actions at all, so
    // it was never routed anything by the kernel.
    let mut impostor = VeyronClient::connect("/tmp/veyron_integ_action_spoof.sock")
        .await
        .unwrap();
    impostor
        .register("totally-unrelated-plugin", PluginManifest::default())
        .await
        .unwrap();

    let mut requester = VeyronClient::connect("/tmp/veyron_integ_action_spoof.sock")
        .await
        .unwrap();
    requester
        .register("weather-requester-2", PluginManifest::default())
        .await
        .unwrap();

    let request_fut = tokio::spawn(async move {
        requester
            .send_action("get_weather", br#"{"city":"nyc"}"#, 2000)
            .await
    });

    // Real provider receives the routed request and learns the internal
    // correlation id — in this test we reuse that same id to stand in for
    // an attacker who has guessed/observed the predictable `kact-<n>` id.
    let received = timeout(Duration::from_secs(2), provider.recv())
        .await
        .expect("provider recv timed out")
        .expect("provider recv failed");
    let internal_action_id = match received.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionRequest(req)) => {
            assert_eq!(req.action, "get_weather");
            req.action_id
        }
        other => panic!("expected ActionRequest, got {other:?}"),
    };

    // Impostor races the real provider and sends a spoofed response first.
    let spoofed_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
            veyron::proto::veyron::ActionResponse {
                action_id: internal_action_id.clone(),
                status: ActionStatus::ActionOk as i32,
                data_json: br#"{"temp_f":-999,"spoofed":true}"#.to_vec(),
                error: String::new(),
            },
        )),
        ..Default::default()
    };
    impostor.send("kernel", spoofed_env).await.unwrap();

    // Give the kernel a moment to process the spoofed response before the
    // real provider replies, so a bug would actually manifest as the
    // requester observing the spoofed payload.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Real provider now answers truthfully.
    let real_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
            veyron::proto::veyron::ActionResponse {
                action_id: internal_action_id,
                status: ActionStatus::ActionOk as i32,
                data_json: br#"{"temp_f":72}"#.to_vec(),
                error: String::new(),
            },
        )),
        ..Default::default()
    };
    provider.send("kernel", real_env).await.unwrap();

    let resp = timeout(Duration::from_secs(2), request_fut)
        .await
        .expect("timed out waiting for requester's response")
        .expect("task panicked")
        .expect("send_action failed");

    // The requester must see the REAL provider's answer, proving the
    // impostor's spoofed response did not consume the pending-action slot.
    assert_eq!(resp.status, ActionStatus::ActionOk as i32);
    assert_eq!(
        resp.data_json, br#"{"temp_f":72}"#,
        "requester received spoofed data instead of the real provider's response"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn ambiguous_action_providers_returns_not_found() {
    // Two plugins both declare the same action name — a deploy
    // misconfiguration, not something the kernel should guess its way
    // through. Must refuse to route, same as zero providers.
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_ambiguous.sock", 19219).await;

    let mut provider_a = VeyronClient::connect("/tmp/veyron_integ_action_ambiguous.sock")
        .await
        .unwrap();
    provider_a
        .register(
            "weather-provider-a",
            PluginManifest {
                actions: vec!["get_weather".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut provider_b = VeyronClient::connect("/tmp/veyron_integ_action_ambiguous.sock")
        .await
        .unwrap();
    provider_b
        .register(
            "weather-provider-b",
            PluginManifest {
                actions: vec!["get_weather".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut requester = VeyronClient::connect("/tmp/veyron_integ_action_ambiguous.sock")
        .await
        .unwrap();
    requester
        .register("weather-requester-3", PluginManifest::default())
        .await
        .unwrap();

    let resp = timeout(
        Duration::from_secs(2),
        requester.send_action("get_weather", b"{}", 2000),
    )
    .await
    .expect("timed out")
    .expect("send_action failed");

    assert_eq!(
        resp.status,
        ActionStatus::ActionNotFound as i32,
        "ambiguous provider declaration must not be arbitrarily resolved"
    );

    let _ = shutdown_tx.send(());
}

#[tokio::test]
async fn provider_side_action_failure_proxies_through_unchanged() {
    // A provider that legitimately answers with ACTION_ERROR (or any other
    // non-OK status) must have that status/error relayed to the requester
    // as-is — the kernel is a router here, not a translator.
    let (shutdown_tx, _registry, _bus) =
        start_kernel("/tmp/veyron_integ_action_failure.sock", 19220).await;

    let mut provider = VeyronClient::connect("/tmp/veyron_integ_action_failure.sock")
        .await
        .unwrap();
    provider
        .register(
            "flaky-provider",
            PluginManifest {
                actions: vec!["get_weather".to_string()],
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut requester = VeyronClient::connect("/tmp/veyron_integ_action_failure.sock")
        .await
        .unwrap();
    requester
        .register("weather-requester-4", PluginManifest::default())
        .await
        .unwrap();

    let request_fut =
        tokio::spawn(async move { requester.send_action("get_weather", b"{}", 2000).await });

    let received = timeout(Duration::from_secs(2), provider.recv())
        .await
        .expect("provider recv timed out")
        .expect("provider recv failed");
    let internal_action_id = match received.payload {
        Some(veyron::proto::veyron::envelope::Payload::ActionRequest(req)) => req.action_id,
        other => panic!("expected ActionRequest, got {other:?}"),
    };

    let err_env = veyron::proto::veyron::Envelope {
        payload: Some(veyron::proto::veyron::envelope::Payload::ActionResponse(
            veyron::proto::veyron::ActionResponse {
                action_id: internal_action_id,
                status: ActionStatus::ActionError as i32,
                data_json: vec![],
                error: "upstream weather service unreachable".to_string(),
            },
        )),
        ..Default::default()
    };
    provider.send("kernel", err_env).await.unwrap();

    let resp = timeout(Duration::from_secs(2), request_fut)
        .await
        .expect("timed out")
        .expect("task panicked")
        .expect("send_action failed");

    assert_eq!(resp.status, ActionStatus::ActionError as i32);
    assert_eq!(resp.error, "upstream weather service unreachable");

    let _ = shutdown_tx.send(());
}
