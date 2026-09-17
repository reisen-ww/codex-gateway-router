use ai_toolbox_lib::coding::proxy_gateway::{
    aggregate_naming::AggregateNamingMode,
    cli_proxy::manifest::{AggregateGroup, CliProxyManifest},
    paths::ProxyGatewayPaths,
    types::{GatewayCliKey, GatewayProxyMode, ProxyGatewaySettings},
    ProxyGatewayState,
};
use ai_toolbox_lib::db::{helpers::db_put, schema::DbTable, SqliteDbState};
use ai_toolbox_lib::http_client;
use serde_json::{json, Value};
use std::{fs, time::Duration};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

struct RunningAggregateGateway {
    state: ProxyGatewayState,
    url: String,
    _directory: TempDir,
}

impl RunningAggregateGateway {
    fn new(
        providers: &[(&str, &str, &str, &[&str])],
        mode: GatewayProxyMode,
        selected_sites: &[&str],
    ) -> Self {
        Self::new_with_groups(providers, mode, selected_sites, &[])
    }

    fn new_with_groups(
        providers: &[(&str, &str, &str, &[&str])],
        mode: GatewayProxyMode,
        selected_sites: &[&str],
        groups: &[(&str, &[&str])],
    ) -> Self {
        Self::new_with_groups_and_model_rewrites(providers, mode, selected_sites, groups, &[])
    }

    fn new_with_groups_and_model_rewrites(
        providers: &[(&str, &str, &str, &[&str])],
        mode: GatewayProxyMode,
        selected_sites: &[&str],
        groups: &[(&str, &[&str])],
        model_rewrites: &[(&str, &str, &str)],
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let db = SqliteDbState::in_memory_for_test().unwrap();
        db.with_conn(|connection| {
            db_put(
                connection,
                DbTable::Settings,
                "app",
                &json!({"proxy_mode": "direct"}),
            )?;
            for (index, (id, name, upstream_url, models)) in providers.iter().enumerate() {
                let mut record = codex_provider_record(name, upstream_url, models, index as i64);
                let rewrites = model_rewrites
                    .iter()
                    .filter(|(provider_id, _, _)| *provider_id == *id)
                    .map(|(_, from, to)| json!({"from": from, "to": to}))
                    .collect::<Vec<_>>();
                if !rewrites.is_empty() {
                    record["meta"]["modelRewrites"] = json!(rewrites);
                }
                db_put(connection, DbTable::CodexProvider, id, &record)?;
            }
            Ok::<_, String>(())
        })
        .unwrap();

        let probe = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let paths = ProxyGatewayPaths::new(directory.path());
        let mut manifest = CliProxyManifest::new(
            GatewayCliKey::Codex,
            format!("http://127.0.0.1:{port}"),
            "2026-09-15T00:00:00Z".to_string(),
            mode,
            selected_sites
                .first()
                .copied()
                .unwrap_or("siteA")
                .to_string(),
        );
        if mode == GatewayProxyMode::Aggregate {
            let provider_ids = selected_sites
                .iter()
                .map(|site| (*site).to_string())
                .collect();
            let aliases = std::collections::BTreeMap::new();
            let naming = AggregateNamingMode::default();
            manifest = if groups.is_empty() {
                manifest.with_aggregate(provider_ids, ".".to_string(), aliases, naming)
            } else {
                manifest.with_aggregate_groups(
                    provider_ids,
                    ".".to_string(),
                    aliases,
                    naming,
                    groups
                        .iter()
                        .map(|(id, provider_ids)| AggregateGroup {
                            id: (*id).to_string(),
                            provider_ids: provider_ids
                                .iter()
                                .map(|provider_id| (*provider_id).to_string())
                                .collect(),
                        })
                        .collect(),
                )
            };
        }
        fs::create_dir_all(paths.manifest_path(GatewayCliKey::Codex).parent().unwrap()).unwrap();
        fs::write(
            paths.manifest_path(GatewayCliKey::Codex),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();

        let settings = ProxyGatewaySettings {
            listen_port: port,
            port_auto_select: true,
            max_retry_count: 1,
            per_provider_retry_count: 0,
            retry_interval_secs: 0,
            non_streaming_timeout_secs: 1,
            streaming_first_byte_timeout_secs: 1,
            streaming_idle_timeout_secs: 1,
            request_log_enabled: false,
            metrics_enabled: false,
            store_request_body: false,
            store_headers: false,
            store_response_body: false,
            ..ProxyGatewaySettings::default()
        };
        let state = ProxyGatewayState::default();
        let status = state
            .manager
            .lock()
            .unwrap()
            .start_with_context(settings, db, paths)
            .unwrap();
        Self {
            state,
            url: format!(
                "http://127.0.0.1:{}/openai/v1/responses",
                status.listen_port.expect("bound gateway port")
            ),
            _directory: directory,
        }
    }
}

impl Drop for RunningAggregateGateway {
    fn drop(&mut self) {
        let _ = self.state.manager.lock().unwrap().stop();
    }
}

fn codex_provider_record(
    name: &str,
    upstream_url: &str,
    models: &[&str],
    sort_index: i64,
) -> Value {
    let config = format!(
        "model = \"fixture-model\"\nmodel_provider = \"fixture\"\n\
         [model_providers.fixture]\nbase_url = \"{upstream_url}/v1\"\nwire_api = \"responses\"\n"
    );
    json!({
        "name": name,
        "category": "custom",
        "settings_config": serde_json::to_string(&json!({
            "config": config,
            "auth": {"OPENAI_API_KEY": "fixture-key"},
            "modelCatalog": {
                "models": models.iter().map(|model| json!({"model": model})).collect::<Vec<_>>()
            }
        })).unwrap(),
        "meta": {"apiFormat": "openai_responses", "providerType": "custom"},
        "sort_index": sort_index,
        "is_applied": true,
        "is_disabled": false,
    })
}

fn responses_request_for_model(model: &str) -> Value {
    json!({
        "model": model,
        "input": [{"role": "user", "content": "say hello"}],
    })
}

fn responses_success(model: &str, text: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "id": format!("resp_{model}"),
        "object": "response",
        "created_at": 1,
        "model": model,
        "output": [{
            "type": "message",
            "id": "msg_fixture",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": text, "annotations": []}],
        }],
        "status": "completed",
        "usage": {"input_tokens": 3, "output_tokens": 2, "total_tokens": 5},
    }))
    .unwrap()
}

fn model_not_found_response(model: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "error": {
            "type": "invalid_request_error",
            "code": "model_not_found",
            "message": format!("model {model} not found"),
        }
    }))
    .unwrap()
}

async fn read_http_json(socket: &mut TcpStream) -> Value {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let size = socket.read(&mut buffer).await.unwrap();
        assert!(size > 0, "upstream request ended before headers");
        bytes.extend_from_slice(&buffer[..size]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).unwrap();
    let content_length = headers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .expect("upstream request must have Content-Length")
        .1
        .trim()
        .parse::<usize>()
        .unwrap();
    while bytes.len() < header_end + content_length {
        let size = socket.read(&mut buffer).await.unwrap();
        assert!(size > 0, "upstream request ended before body");
        bytes.extend_from_slice(&buffer[..size]);
    }
    serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
}

async fn write_http_json(socket: &mut TcpStream, status: u16, body: &[u8]) {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    socket
        .write_all(
            format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    socket.write_all(body).await.unwrap();
    socket.shutdown().await.unwrap();
}

/// Capture at most one real upstream request. Returning `None` means the
/// listener saw no request before the caller aborted the task.
fn spawn_upstream_capture(
    listener: TcpListener,
    status: u16,
    response_body: Vec<u8>,
) -> JoinHandle<Option<Value>> {
    tokio::spawn(async move {
        let Ok(Ok((mut socket, _))) =
            tokio::time::timeout(REQUEST_TIMEOUT, listener.accept()).await
        else {
            return None;
        };
        let body = read_http_json(&mut socket).await;
        write_http_json(&mut socket, status, &response_body).await;
        Some(body)
    })
}

async fn abort_if_idle(mut task: JoinHandle<Option<Value>>) -> Option<Value> {
    match tokio::time::timeout(Duration::from_millis(250), &mut task).await {
        Ok(Ok(captured)) => captured,
        Ok(Err(error)) => panic!("upstream capture task failed: {error}"),
        Err(_) => {
            task.abort();
            let _ = task.await;
            None
        }
    }
}

async fn send_request(gateway_url: &str, model: &str) -> (reqwest::StatusCode, Vec<u8>) {
    let client = http_client::create_client_no_proxy(10).unwrap();
    let response = client
        .post(gateway_url)
        .json(&responses_request_for_model(model))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.bytes().await.unwrap().to_vec();
    (status, bytes)
}

#[tokio::test]
async fn aggregate_site_model_prefix_routes_only_to_site_a_and_strips_prefix() {
    let site_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_b = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_a_url = format!("http://{}", site_a.local_addr().unwrap());
    let site_b_url = format!("http://{}", site_b.local_addr().unwrap());
    let site_a_task = spawn_upstream_capture(site_a, 200, responses_success("modelX", "from A"));
    let site_b_task =
        spawn_upstream_capture(site_b, 500, responses_success("modelX", "unexpected B"));

    let gateway = RunningAggregateGateway::new(
        &[
            ("siteA", "Site A", &site_a_url, &["modelX"]),
            ("siteB", "Site B", &site_b_url, &["modelX"]),
        ],
        GatewayProxyMode::Aggregate,
        &["siteA", "siteB"],
    );
    let (status, bytes) = send_request(&gateway.url, "siteA.modelX").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["model"], "modelX");
    assert_eq!(response["output"][0]["content"][0]["text"], "from A");

    let captured_a = tokio::time::timeout(REQUEST_TIMEOUT, site_a_task)
        .await
        .expect("site A request should arrive")
        .unwrap()
        .expect("site A request should be captured");
    assert_eq!(captured_a["model"], "modelX");
    assert_eq!(abort_if_idle(site_b_task).await, None);
}

#[tokio::test]
async fn aggregate_site_model_prefix_routes_only_to_site_b_and_strips_prefix() {
    let site_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_b = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_a_url = format!("http://{}", site_a.local_addr().unwrap());
    let site_b_url = format!("http://{}", site_b.local_addr().unwrap());
    let site_a_task =
        spawn_upstream_capture(site_a, 500, responses_success("modelY", "unexpected A"));
    let site_b_task = spawn_upstream_capture(site_b, 200, responses_success("modelY", "from B"));

    let gateway = RunningAggregateGateway::new(
        &[
            ("siteA", "Site A", &site_a_url, &["modelX"]),
            ("siteB", "Site B", &site_b_url, &["modelY"]),
        ],
        GatewayProxyMode::Aggregate,
        &["siteA", "siteB"],
    );
    let (status, bytes) = send_request(&gateway.url, "siteB.modelY").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["model"], "modelY");
    assert_eq!(response["output"][0]["content"][0]["text"], "from B");

    assert_eq!(abort_if_idle(site_a_task).await, None);
    let captured_b = tokio::time::timeout(REQUEST_TIMEOUT, site_b_task)
        .await
        .expect("site B request should arrive")
        .unwrap()
        .expect("site B request should be captured");
    assert_eq!(captured_b["model"], "modelY");
}

#[tokio::test]
async fn aggregate_model_not_found_on_site_a_fails_over_to_site_b_with_same_model() {
    let site_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_b = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_a_url = format!("http://{}", site_a.local_addr().unwrap());
    let site_b_url = format!("http://{}", site_b.local_addr().unwrap());
    let site_a_task = spawn_upstream_capture(site_a, 404, model_not_found_response("modelX"));
    let site_b_task =
        spawn_upstream_capture(site_b, 200, responses_success("modelX", "fallback B"));

    let gateway = RunningAggregateGateway::new(
        &[
            ("siteA", "Site A", &site_a_url, &["modelX"]),
            ("siteB", "Site B", &site_b_url, &["modelX"]),
        ],
        GatewayProxyMode::Aggregate,
        &["siteA", "siteB"],
    );
    let (status, bytes) = send_request(&gateway.url, "siteA.modelX").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["model"], "modelX");
    assert_eq!(response["output"][0]["content"][0]["text"], "fallback B");

    let captured_a = tokio::time::timeout(REQUEST_TIMEOUT, site_a_task)
        .await
        .expect("site A request should arrive")
        .unwrap()
        .expect("site A request should be captured");
    assert_eq!(captured_a["model"], "modelX");
    let captured_b = tokio::time::timeout(REQUEST_TIMEOUT, site_b_task)
        .await
        .expect("site B fallback request should arrive")
        .unwrap()
        .expect("site B request should be captured");
    assert_eq!(captured_b["model"], "modelX");
}

#[tokio::test]
async fn aggregate_bare_model_applies_provider_model_rewrite() {
    let site_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_a_url = format!("http://{}", site_a.local_addr().unwrap());
    let site_a_task = spawn_upstream_capture(site_a, 200, responses_success("modelX", "rewritten"));

    let gateway = RunningAggregateGateway::new_with_groups_and_model_rewrites(
        &[("siteA", "Site A", &site_a_url, &["modelX"])],
        GatewayProxyMode::Aggregate,
        &["siteA"],
        &[],
        &[("siteA", "modelX", "rewritten-model")],
    );
    let (status, bytes) = send_request(&gateway.url, "modelX").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));

    let captured = tokio::time::timeout(REQUEST_TIMEOUT, site_a_task)
        .await
        .expect("rewritten aggregate request should arrive")
        .unwrap()
        .expect("rewritten aggregate request should be captured");
    assert_eq!(captured["model"], "rewritten-model");
}

#[tokio::test]
async fn strict_aggregate_group_fails_over_only_within_the_named_group() {
    let group_a_first = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let group_a_second = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let group_b_only = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let group_a_first_url = format!("http://{}", group_a_first.local_addr().unwrap());
    let group_a_second_url = format!("http://{}", group_a_second.local_addr().unwrap());
    let group_b_only_url = format!("http://{}", group_b_only.local_addr().unwrap());
    let group_a_first_task =
        spawn_upstream_capture(group_a_first, 404, model_not_found_response("modelX"));
    let group_a_second_task = spawn_upstream_capture(
        group_a_second,
        200,
        responses_success("modelX", "group A fallback"),
    );
    let group_b_only_task = spawn_upstream_capture(
        group_b_only,
        200,
        responses_success("modelX", "unexpected group B"),
    );

    let gateway = RunningAggregateGateway::new_with_groups(
        &[
            (
                "provider-a-1",
                "Group A first",
                &group_a_first_url,
                &["modelX"],
            ),
            (
                "provider-a-2",
                "Group A second",
                &group_a_second_url,
                &["modelX"],
            ),
            (
                "provider-b-1",
                "Group B only",
                &group_b_only_url,
                &["modelX"],
            ),
        ],
        GatewayProxyMode::Aggregate,
        &["provider-a-1"],
        &[
            ("group-a", &["provider-a-1", "provider-a-2"]),
            ("group-b", &["provider-b-1"]),
        ],
    );

    let (status, bytes) = send_request(&gateway.url, "group-a.modelX").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["model"], "modelX");
    assert_eq!(
        response["output"][0]["content"][0]["text"],
        "group A fallback"
    );

    let captured_first = tokio::time::timeout(REQUEST_TIMEOUT, group_a_first_task)
        .await
        .expect("first provider in group A should receive the request")
        .unwrap()
        .expect("first group A request should be captured");
    assert_eq!(captured_first["model"], "modelX");
    let captured_second = tokio::time::timeout(REQUEST_TIMEOUT, group_a_second_task)
        .await
        .expect("second provider in group A should receive the failover request")
        .unwrap()
        .expect("second group A request should be captured");
    assert_eq!(captured_second["model"], "modelX");
    assert_eq!(abort_if_idle(group_b_only_task).await, None);
}

#[tokio::test]
async fn strict_aggregate_group_does_not_fall_back_to_another_group_after_exhaustion() {
    let group_a_first = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let group_a_second = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let group_b_only = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let group_a_first_url = format!("http://{}", group_a_first.local_addr().unwrap());
    let group_a_second_url = format!("http://{}", group_a_second.local_addr().unwrap());
    let group_b_only_url = format!("http://{}", group_b_only.local_addr().unwrap());
    let group_a_first_task =
        spawn_upstream_capture(group_a_first, 404, model_not_found_response("modelX"));
    let group_a_second_task =
        spawn_upstream_capture(group_a_second, 404, model_not_found_response("modelX"));
    let group_b_only_task = spawn_upstream_capture(
        group_b_only,
        200,
        responses_success("modelX", "must not be used"),
    );

    let gateway = RunningAggregateGateway::new_with_groups(
        &[
            (
                "provider-a-1",
                "Group A first",
                &group_a_first_url,
                &["modelX"],
            ),
            (
                "provider-a-2",
                "Group A second",
                &group_a_second_url,
                &["modelX"],
            ),
            (
                "provider-b-1",
                "Group B only",
                &group_b_only_url,
                &["modelX"],
            ),
        ],
        GatewayProxyMode::Aggregate,
        &["provider-a-1"],
        &[
            ("group-a", &["provider-a-1", "provider-a-2"]),
            ("group-b", &["provider-b-1"]),
        ],
    );

    let (status, bytes) = send_request(&gateway.url, "group-a.modelX").await;
    assert_eq!(status, 404, "{}", String::from_utf8_lossy(&bytes));
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["error"]["code"], "model_not_found");

    assert!(tokio::time::timeout(REQUEST_TIMEOUT, group_a_first_task)
        .await
        .expect("first provider in group A should receive the request")
        .unwrap()
        .is_some());
    assert!(tokio::time::timeout(REQUEST_TIMEOUT, group_a_second_task)
        .await
        .expect("second provider in group A should receive the failover request")
        .unwrap()
        .is_some());
    assert_eq!(abort_if_idle(group_b_only_task).await, None);
}

#[tokio::test]
async fn aggregate_unknown_site_prefix_returns_404_without_calling_any_upstream() {
    let site_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_b = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_a_url = format!("http://{}", site_a.local_addr().unwrap());
    let site_b_url = format!("http://{}", site_b.local_addr().unwrap());
    let site_a_task = spawn_upstream_capture(
        site_a,
        500,
        responses_success("unknown.modelX", "unexpected A"),
    );
    let site_b_task = spawn_upstream_capture(
        site_b,
        500,
        responses_success("unknown.modelX", "unexpected B"),
    );

    let gateway = RunningAggregateGateway::new(
        &[
            ("siteA", "Site A", &site_a_url, &["modelX"]),
            ("siteB", "Site B", &site_b_url, &["modelX"]),
        ],
        GatewayProxyMode::Aggregate,
        &["siteA", "siteB"],
    );
    let (status, bytes) = send_request(&gateway.url, "unknown.modelX").await;
    assert_eq!(status, 404, "{}", String::from_utf8_lossy(&bytes));

    assert_eq!(abort_if_idle(site_a_task).await, None);
    assert_eq!(abort_if_idle(site_b_task).await, None);
}

#[tokio::test]
async fn single_mode_keeps_bare_model_routing_and_does_not_regress() {
    let site_a = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let site_a_url = format!("http://{}", site_a.local_addr().unwrap());
    let site_a_task = spawn_upstream_capture(site_a, 200, responses_success("modelX", "single"));

    let gateway = RunningAggregateGateway::new(
        &[("siteA", "Site A", &site_a_url, &["modelX"])],
        GatewayProxyMode::Single,
        &["siteA"],
    );
    let (status, bytes) = send_request(&gateway.url, "modelX").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&bytes));
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["model"], "modelX");
    assert_eq!(response["output"][0]["content"][0]["text"], "single");

    let captured = tokio::time::timeout(REQUEST_TIMEOUT, site_a_task)
        .await
        .expect("single-mode upstream request should arrive")
        .unwrap()
        .expect("single-mode upstream request should be captured");
    assert_eq!(captured["model"], "modelX");
}
