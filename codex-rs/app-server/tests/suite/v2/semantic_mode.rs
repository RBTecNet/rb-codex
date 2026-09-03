use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_app_server::in_process;
use codex_app_server::in_process::InProcessClientHandle;
use codex_app_server::in_process::InProcessServerEvent;
use codex_app_server::in_process::InProcessStartArgs;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::InitializeParams;
use codex_app_server_protocol::ModelRerouteReason;
use codex_app_server_protocol::ModelReroutedNotification;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::SemanticCompletion;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadItem;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput as V2UserInput;
use codex_arg0::Arg0DispatchPaths;
use codex_config::CloudConfigBundleLoader;
use codex_config::LoaderOverrides;
use codex_config::NoopThreadConfigLoader;
use codex_config::types::AuthCredentialsStoreMode;
use codex_core::config::ConfigBuilder;
use codex_exec_server::EnvironmentManager;
use codex_feedback::CodexFeedback;
use codex_protocol::protocol::SessionSource;
use codex_utils_absolute_path::AbsolutePathBuf;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::Digest;
use sha2::Sha256;
use tempfile::TempDir;
use tokio::time::timeout;

const TEST_TIMEOUT: Duration = Duration::from_secs(30);
const MODEL: &str = "rb-semantic-fixture-model";
const PROVIDER: &str = "openai";
const EMPTY_TOOL_MANIFEST_SHA256: &str =
    "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945";
const HOSTILE_SENTINELS: &[&str] = &[
    "RB_SENTINEL_GLOBAL_AGENTS",
    "RB_SENTINEL_GLOBAL_AGENTS_OVERRIDE",
    "RB_SENTINEL_ANCESTOR_AGENTS",
    "RB_SENTINEL_ANCESTOR_AGENTS_OVERRIDE",
    "RB_SENTINEL_PROJECT_AGENTS",
    "RB_SENTINEL_PROJECT_AGENTS_OVERRIDE",
    "RB_SENTINEL_USER_CONFIG",
    "RB_SENTINEL_PROJECT_CONFIG",
    "RB_SENTINEL_PROFILE_INSTRUCTIONS",
    "RB_SENTINEL_MODEL_INSTRUCTIONS_FILE",
    "RB_SENTINEL_SKILL",
    "RB_SENTINEL_PLUGIN",
    "RB_SENTINEL_MCP_APP",
    "RB_SENTINEL_HOOK_SESSION_START",
    "RB_SENTINEL_HOOK_USER_PROMPT",
    "RB_SENTINEL_HOOK_CONTINUATION",
    "RB_SENTINEL_MEMORY",
    "RB_SENTINEL_HISTORY",
    "RB_SENTINEL_ADDITIONAL_CONTEXT",
    "RB_SENTINEL_GOAL",
    "RB_SENTINEL_ENVIRONMENT",
    "RB_SENTINEL_COLLABORATION",
];

struct SemanticFixture {
    client: InProcessClientHandle,
    codex_home: TempDir,
    auth_home: TempDir,
}

fn write_hostile_fixtures(codex_home: &Path, project: &Path) -> Result<std::path::PathBuf> {
    let ancestor = project.parent().context("project needs ancestor")?;
    std::fs::create_dir_all(ancestor.join(".git"))?;
    std::fs::write(ancestor.join("AGENTS.md"), "RB_SENTINEL_ANCESTOR_AGENTS")?;
    std::fs::write(
        ancestor.join("AGENTS.override.md"),
        "RB_SENTINEL_ANCESTOR_AGENTS_OVERRIDE",
    )?;
    std::fs::create_dir_all(project)?;
    std::fs::write(project.join("AGENTS.md"), "RB_SENTINEL_PROJECT_AGENTS")?;
    std::fs::write(
        project.join("AGENTS.override.md"),
        "RB_SENTINEL_PROJECT_AGENTS_OVERRIDE",
    )?;
    std::fs::create_dir_all(project.join(".codex"))?;
    std::fs::write(
        project.join(".codex/config.toml"),
        "developer_instructions = \"RB_SENTINEL_PROJECT_CONFIG\"\n",
    )?;

    std::fs::write(codex_home.join("AGENTS.md"), "RB_SENTINEL_GLOBAL_AGENTS")?;
    std::fs::write(
        codex_home.join("AGENTS.override.md"),
        "RB_SENTINEL_GLOBAL_AGENTS_OVERRIDE",
    )?;
    let model_instructions = codex_home.join("hostile-model-instructions.md");
    std::fs::write(&model_instructions, "RB_SENTINEL_MODEL_INSTRUCTIONS_FILE")?;
    let profile_instructions = codex_home.join("hostile-profile-instructions.md");
    std::fs::write(&profile_instructions, "RB_SENTINEL_PROFILE_INSTRUCTIONS")?;

    let skill_dir = codex_home.join("skills/hostile");
    std::fs::create_dir_all(&skill_dir)?;
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: hostile\ndescription: hostile fixture\n---\nRB_SENTINEL_SKILL",
    )?;
    let plugin_dir = codex_home.join("plugins/cache/test/hostile/local");
    std::fs::create_dir_all(plugin_dir.join(".codex-plugin"))?;
    std::fs::create_dir_all(plugin_dir.join("skills/hostile-plugin"))?;
    std::fs::write(
        plugin_dir.join(".codex-plugin/plugin.json"),
        r#"{"name":"hostile","version":"1.0.0"}"#,
    )?;
    std::fs::write(
        plugin_dir.join("skills/hostile-plugin/SKILL.md"),
        "---\nname: hostile-plugin\ndescription: hostile plugin\n---\nRB_SENTINEL_PLUGIN",
    )?;
    std::fs::write(
        plugin_dir.join(".mcp.json"),
        r#"{"mcpServers":{"hostile":{"command":"RB_SENTINEL_MCP_APP"}}}"#,
    )?;

    for (relative, sentinel) in [
        ("memories/memory.md", "RB_SENTINEL_MEMORY"),
        ("history.jsonl", "RB_SENTINEL_HISTORY"),
        ("context/additional.md", "RB_SENTINEL_ADDITIONAL_CONTEXT"),
        ("goals/active.md", "RB_SENTINEL_GOAL"),
        ("environments/selected.md", "RB_SENTINEL_ENVIRONMENT"),
        ("agents/collaboration.md", "RB_SENTINEL_COLLABORATION"),
    ] {
        let path = codex_home.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, sentinel)?;
    }

    Ok(profile_instructions)
}

async fn start_semantic_fixture(server_uri: &str, cwd: &Path) -> Result<SemanticFixture> {
    let codex_home = TempDir::new()?;
    let auth_home = TempDir::new()?;
    let profile_instructions = write_hostile_fixtures(codex_home.path(), cwd)?;
    let hook_marker = codex_home.path().join("hook-ran");
    std::fs::write(
        codex_home.path().join("config.toml"),
        format!(
            r#"model = "{MODEL}"
model_provider = "openai"
approval_policy = "never"
sandbox_mode = "read-only"
openai_base_url = "{server_uri}/v1"
chatgpt_base_url = "{server_uri}/backend-api"
developer_instructions = "RB_SENTINEL_USER_CONFIG"
model_instructions_file = "{}"

[features]
responses_websockets = false
responses_websockets_v2 = false
plugins = true
hooks = true

[skills]
include_instructions = true

[plugins."hostile@test"]
enabled = true

[mcp_servers.hostile]
command = "RB_SENTINEL_MCP_APP"

[hooks]

[[hooks.SessionStart]]
[[hooks.SessionStart.hooks]]
type = "command"
command = "sh -c 'echo RB_SENTINEL_HOOK_SESSION_START >> {}'"

[[hooks.UserPromptSubmit]]
[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "sh -c 'echo RB_SENTINEL_HOOK_USER_PROMPT >> {}'"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "sh -c 'echo RB_SENTINEL_HOOK_CONTINUATION >> {}'"
"#,
            codex_home
                .path()
                .join("hostile-model-instructions.md")
                .display(),
            hook_marker.display(),
            hook_marker.display(),
            hook_marker.display(),
        ),
    )?;
    let profile_config = codex_home.path().join("hostile.config.toml");
    std::fs::write(
        &profile_config,
        format!(
            "model_instructions_file = \"{}\"\n",
            profile_instructions.display()
        ),
    )?;
    write_chatgpt_auth(
        auth_home.path(),
        ChatGptAuthFixture::new("synthetic-access-token")
            .refresh_token("synthetic-refresh-token")
            .plan_type("plus")
            .chatgpt_account_id("synthetic-account"),
        AuthCredentialsStoreMode::File,
    )?;

    let mut loader_overrides = LoaderOverrides::without_managed_config_for_tests();
    loader_overrides.user_config_profile = Some("hostile".parse()?);
    loader_overrides.user_config_path = Some(AbsolutePathBuf::from_absolute_path(profile_config)?);
    let config = Arc::new(
        ConfigBuilder::default()
            .codex_home(codex_home.path().to_path_buf())
            .fallback_cwd(Some(cwd.to_path_buf()))
            .loader_overrides(loader_overrides.clone())
            .build()
            .await?,
    );
    let client = in_process::start(InProcessStartArgs {
        arg0_paths: Arg0DispatchPaths::default(),
        config,
        cli_overrides: Vec::new(),
        loader_overrides,
        strict_config: false,
        cloud_config_bundle: CloudConfigBundleLoader::default(),
        thread_config_loader: Arc::new(NoopThreadConfigLoader),
        feedback: CodexFeedback::new(),
        log_db: None,
        state_db: None,
        environment_manager: Arc::new(EnvironmentManager::default_for_tests()),
        config_warnings: Vec::new(),
        session_source: SessionSource::Cli,
        enable_codex_api_key_env: false,
        explicit_auth_file: Some(auth_home.path().join("auth.json")),
        rb_semantic_runtime: true,
        initialize: InitializeParams {
            client_info: ClientInfo {
                name: "rb-semantic-mode-test".to_string(),
                title: None,
                version: "1.0.0-test".to_string(),
            },
            capabilities: Some(InitializeCapabilities {
                experimental_api: true,
                ..Default::default()
            }),
        },
        channel_capacity: in_process::DEFAULT_IN_PROCESS_CHANNEL_CAPACITY,
    })
    .await?;

    Ok(SemanticFixture {
        client,
        codex_home,
        auth_home,
    })
}

async fn request_ok<T: DeserializeOwned>(
    client: &InProcessClientHandle,
    request: ClientRequest,
) -> Result<T> {
    let result = client
        .request(request)
        .await?
        .map_err(|error| anyhow::anyhow!("app-server request failed: {}", error.message))?;
    serde_json::from_value(result).context("decode app-server result")
}

async fn wait_for_completion(
    client: &mut InProcessClientHandle,
    thread_id: &str,
) -> Result<(TurnCompletedNotification, usize)> {
    timeout(TEST_TIMEOUT, async {
        let mut final_messages = 0;
        loop {
            let event = client
                .next_event()
                .await
                .context("app-server stopped before turn/completed")?;
            let InProcessServerEvent::ServerNotification(notification) = event else {
                continue;
            };
            match notification.as_ref() {
                ServerNotification::ItemCompleted(item)
                    if item.thread_id == thread_id
                        && matches!(item.item, ThreadItem::AgentMessage { .. }) =>
                {
                    final_messages += 1;
                }
                ServerNotification::TurnCompleted(completed)
                    if completed.thread_id == thread_id =>
                {
                    return Ok((completed.clone(), final_messages));
                }
                _ => {}
            }
        }
    })
    .await?
}

async fn wait_for_completion_and_reroute(
    client: &mut InProcessClientHandle,
    thread_id: &str,
) -> Result<(TurnCompletedNotification, ModelReroutedNotification)> {
    timeout(TEST_TIMEOUT, async {
        let mut reroute = None;
        loop {
            let event = client
                .next_event()
                .await
                .context("app-server stopped before rerouted turn completed")?;
            let InProcessServerEvent::ServerNotification(notification) = event else {
                continue;
            };
            match notification.as_ref() {
                ServerNotification::ModelRerouted(event) if event.thread_id == thread_id => {
                    reroute = Some(event.clone());
                }
                ServerNotification::TurnCompleted(completed)
                    if completed.thread_id == thread_id =>
                {
                    return Ok((
                        completed.clone(),
                        reroute.context("missing structured model/rerouted evidence")?,
                    ));
                }
                _ => {}
            }
        }
    })
    .await?
}

fn rb_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" },
            "choice": {
                "oneOf": [
                    { "type": "string" },
                    { "type": "number" }
                ]
            },
            "optionalNote": { "type": "string" },
            "items": { "type": "array", "items": {} }
        },
        "required": ["answer"],
        "additionalProperties": true
    })
}

#[tokio::test]
async fn semantic_mode_reaches_final_provider_request_with_attested_envelope() -> Result<()> {
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-semantic"),
        responses::ev_assistant_message("msg-semantic", r#"{"answer":"ok"}"#),
        responses::ev_completed("resp-semantic"),
    ]);
    let response_mock = responses::mount_sse_once(&server, body).await;
    let workspace = TempDir::new()?;
    let project = workspace.path().join("repo/project");
    let mut fixture = start_semantic_fixture(&server.uri(), &project).await?;

    let started: ThreadStartResponse = request_ok(
        &fixture.client,
        ClientRequest::ThreadStart {
            request_id: RequestId::Integer(1),
            params: ThreadStartParams {
                semantic_mode: true,
                cwd: Some(project.to_string_lossy().into_owned()),
                ephemeral: Some(true),
                ..Default::default()
            },
        },
    )
    .await?;
    let preflight = started
        .semantic_preflight
        .as_ref()
        .context("semantic preflight missing")?;
    assert!(preflight.semantic_mode);
    assert_eq!(preflight.model, MODEL);
    assert_eq!(preflight.model_provider, PROVIDER);
    assert_eq!(preflight.tool_policy, "none");
    assert_eq!(preflight.effective_tool_count, 0);
    assert_eq!(preflight.tool_manifest_digest, EMPTY_TOOL_MANIFEST_SHA256);
    assert_eq!(preflight.instruction_policy, "isolated");
    assert!(!preflight.output_schema_strict);
    assert_eq!(preflight.auth_mode, "chatgpt");
    assert_eq!(preflight.auth_store_kind, "file");
    assert_eq!(preflight.session_mode, "ephemeral");
    assert_eq!(preflight.requested_codex_turns, 1);
    assert_eq!(preflight.request_accounting, "opaque");

    let supplied_schema = rb_schema();
    let _: Value = request_ok(
        &fixture.client,
        ClientRequest::TurnStart {
            request_id: RequestId::Integer(2),
            params: TurnStartParams {
                thread_id: started.thread.id.clone(),
                input: vec![V2UserInput::Text {
                    text: "Return the semantic fixture JSON.".to_string(),
                    text_elements: Vec::new(),
                }],
                output_schema: Some(supplied_schema.clone()),
                ..Default::default()
            },
        },
    )
    .await?;
    let (completed, final_messages) =
        wait_for_completion(&mut fixture.client, &started.thread.id).await?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    assert_eq!(final_messages, 1);
    assert_eq!(
        completed.semantic_completion,
        Some(SemanticCompletion {
            initial_model: MODEL.to_string(),
            initial_model_provider: PROVIDER.to_string(),
            final_model: MODEL.to_string(),
            final_model_provider: PROVIDER.to_string(),
            rerouted: false,
        })
    );

    let request = response_mock.single_request();
    let payload = request.body_json();
    assert_eq!(
        payload.get("model"),
        Some(&Value::String(MODEL.to_string()))
    );
    assert_eq!(payload.get("tools"), Some(&serde_json::json!([])));
    assert_eq!(
        payload.pointer("/text/format/schema"),
        Some(&supplied_schema)
    );
    assert_eq!(
        payload.pointer("/text/format/strict"),
        Some(&Value::Bool(false))
    );
    assert_eq!(response_mock.requests().len(), 1);
    let all_requests = server
        .received_requests()
        .await
        .context("capture all localhost requests")?;
    let request_paths = all_requests
        .iter()
        .map(|request| request.url.path().to_string())
        .collect::<Vec<_>>();
    let request_summaries = all_requests
        .iter()
        .map(|request| {
            let body = serde_json::from_slice::<Value>(&request.body).unwrap_or(Value::Null);
            let body_digest = format!("{:x}", Sha256::digest(&request.body));
            format!(
                "{} model={} input={} tools={} sha256={}",
                request.url.path(),
                body.get("model").and_then(Value::as_str).unwrap_or("-"),
                body.get("input")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
                body.get("tools")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
                body_digest,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        request_paths,
        vec!["/v1/responses"],
        "semantic test must not perform auxiliary network requests: {request_summaries:?}"
    );
    assert_eq!(all_requests[0].url.path(), "/v1/responses");
    let final_request = serde_json::to_string(&payload)?;
    for sentinel in HOSTILE_SENTINELS {
        assert!(
            !final_request.contains(sentinel),
            "custom instruction source reached final provider request: {sentinel}"
        );
    }

    fixture.client.shutdown().await?;
    assert!(!fixture.codex_home.path().join("auth.json").exists());
    assert!(fixture.auth_home.path().join("auth.json").exists());
    assert!(!fixture.codex_home.path().join("hook-ran").exists());
    assert!(!fixture.codex_home.path().join("sessions").exists());
    assert!(!fixture.codex_home.path().join("archived_sessions").exists());
    Ok(())
}

#[tokio::test]
async fn semantic_mode_reports_initial_and_final_rerouted_identity() -> Result<()> {
    const REROUTED_MODEL: &str = "rb-semantic-rerouted-model";
    let server = responses::start_mock_server().await;
    let body = responses::sse(vec![
        responses::ev_response_created("resp-reroute"),
        responses::ev_assistant_message("msg-reroute", r#"{"answer":"rerouted"}"#),
        responses::ev_completed("resp-reroute"),
    ]);
    let response = responses::sse_response(body).insert_header("OpenAI-Model", REROUTED_MODEL);
    let response_mock = responses::mount_response_once(&server, response).await;
    let workspace = TempDir::new()?;
    let project = workspace.path().join("repo/project");
    let mut fixture = start_semantic_fixture(&server.uri(), &project).await?;

    let started: ThreadStartResponse = request_ok(
        &fixture.client,
        ClientRequest::ThreadStart {
            request_id: RequestId::Integer(10),
            params: ThreadStartParams {
                semantic_mode: true,
                cwd: Some(project.to_string_lossy().into_owned()),
                ephemeral: Some(true),
                ..Default::default()
            },
        },
    )
    .await?;
    let preflight = started
        .semantic_preflight
        .as_ref()
        .context("semantic preflight missing")?;
    assert_eq!(
        (&preflight.model, &preflight.model_provider),
        (&MODEL.to_string(), &PROVIDER.to_string())
    );

    let _: Value = request_ok(
        &fixture.client,
        ClientRequest::TurnStart {
            request_id: RequestId::Integer(11),
            params: TurnStartParams {
                thread_id: started.thread.id.clone(),
                input: vec![V2UserInput::Text {
                    text: "Return reroute fixture JSON.".to_string(),
                    text_elements: Vec::new(),
                }],
                output_schema: Some(rb_schema()),
                ..Default::default()
            },
        },
    )
    .await?;
    let (completed, rerouted) =
        wait_for_completion_and_reroute(&mut fixture.client, &started.thread.id).await?;
    assert_eq!(rerouted.from_model, MODEL);
    assert_eq!(rerouted.to_model, REROUTED_MODEL);
    assert_eq!(rerouted.reason, ModelRerouteReason::HighRiskCyberActivity);
    assert_eq!(
        completed.semantic_completion,
        Some(SemanticCompletion {
            initial_model: MODEL.to_string(),
            initial_model_provider: PROVIDER.to_string(),
            final_model: REROUTED_MODEL.to_string(),
            final_model_provider: PROVIDER.to_string(),
            rerouted: true,
        })
    );
    assert_eq!(response_mock.requests().len(), 1);
    fixture.client.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn semantic_mode_fails_closed_for_every_action_event_category() -> Result<()> {
    let cases = [
        (
            responses::ev_function_call(
                "call-command",
                "exec_command",
                r#"{"cmd":"RB_SECRET_COMMAND_BODY"}"#,
            ),
            "commandExecutionEvents",
            "RB_SECRET_COMMAND_BODY",
        ),
        (
            responses::ev_apply_patch_custom_tool_call(
                "call-file",
                "*** Begin Patch\n*** Add File: RB_SECRET_PATH\n*** End Patch",
            ),
            "fileChangeEvents",
            "RB_SECRET_PATH",
        ),
        (
            responses::ev_function_call_with_namespace(
                "call-mcp",
                "mcp__fixture",
                "invoke",
                r#"{"value":"RB_SECRET_MCP_ARGUMENT"}"#,
            ),
            "mcpToolEvents",
            "RB_SECRET_MCP_ARGUMENT",
        ),
        (
            responses::ev_function_call_with_namespace(
                "call-app",
                "mcp__codex_apps",
                "invoke",
                r#"{"value":"RB_SECRET_APP_ARGUMENT"}"#,
            ),
            "appToolEvents",
            "RB_SECRET_APP_ARGUMENT",
        ),
        (
            responses::ev_web_search_call_done("call-web", "completed", "RB_SECRET_SEARCH_QUERY"),
            "webSearchEvents",
            "RB_SECRET_SEARCH_QUERY",
        ),
        (
            responses::ev_image_generation_call(
                "call-other",
                "completed",
                "RB_SECRET_IMAGE_PROMPT",
                "RB_SECRET_IMAGE_OUTPUT",
            ),
            "otherToolEvents",
            "RB_SECRET_IMAGE_PROMPT",
        ),
    ];
    let server = responses::start_mock_server().await;
    let bodies = cases
        .iter()
        .enumerate()
        .map(|(index, (event, _, _))| {
            let response_id = format!("resp-action-{index}");
            responses::sse(vec![
                responses::ev_response_created(&response_id),
                event.clone(),
                responses::ev_completed(&response_id),
            ])
        })
        .collect();
    let response_mock = responses::mount_sse_sequence(&server, bodies).await;
    let workspace = TempDir::new()?;
    let project = workspace.path().join("repo/project");
    let mut fixture = start_semantic_fixture(&server.uri(), &project).await?;

    for (index, (_, expected_category, secret)) in cases.iter().enumerate() {
        let request_id = i64::try_from(index)? * 2 + 100;
        let started: ThreadStartResponse = request_ok(
            &fixture.client,
            ClientRequest::ThreadStart {
                request_id: RequestId::Integer(request_id),
                params: ThreadStartParams {
                    semantic_mode: true,
                    cwd: Some(project.to_string_lossy().into_owned()),
                    ephemeral: Some(true),
                    ..Default::default()
                },
            },
        )
        .await?;
        let preflight = started
            .semantic_preflight
            .as_ref()
            .context("semantic preflight missing")?;
        assert_eq!(preflight.effective_tool_count, 0);

        let _: Value = request_ok(
            &fixture.client,
            ClientRequest::TurnStart {
                request_id: RequestId::Integer(request_id + 1),
                params: TurnStartParams {
                    thread_id: started.thread.id.clone(),
                    input: vec![V2UserInput::Text {
                        text: format!("action rejection fixture {index}"),
                        text_elements: Vec::new(),
                    }],
                    output_schema: Some(rb_schema()),
                    ..Default::default()
                },
            },
        )
        .await?;
        let (completed, final_messages) =
            wait_for_completion(&mut fixture.client, &started.thread.id).await?;
        assert_eq!(completed.turn.status, TurnStatus::Failed);
        assert_eq!(final_messages, 0);
        let error = completed
            .turn
            .error
            .context("failed semantic turn needs error")?;
        assert_eq!(
            error.message,
            format!("RB semantic mode invalid action: category={expected_category} count=1")
        );
        assert!(!error.message.contains(secret));
    }

    assert_eq!(response_mock.requests().len(), cases.len());
    fixture.client.shutdown().await?;
    Ok(())
}
