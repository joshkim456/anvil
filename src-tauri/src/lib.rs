//! Anvil Tauri application: command surface + event bridging. Heavy logic
//! lives in the modules; this file owns wiring so the modules stay decoupled.

mod auth;
mod curator;
#[allow(dead_code)]
mod instance;
mod launch;
mod modrinth;
pub mod pack;
mod settings;

use auth::DeviceCodeStart;
use curator::{ChatMsg, CuratorEvent};
use instance::Instance;
use launch::LaunchEvent;
use modrinth::{Modrinth, Project, SearchResponse, Version};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

const CURATOR_MODEL: &str = "claude-sonnet-4-6";

// ---- Modrinth browse ----

fn build_facets(
    mc_version: &Option<String>,
    loader: &Option<String>,
    categories: &[String],
    project_type: &str,
) -> String {
    let mut groups: Vec<String> = vec![format!("[\"project_type:{project_type}\"]")];
    if let Some(v) = mc_version {
        if !v.is_empty() {
            groups.push(format!("[\"versions:{v}\"]"));
        }
    }
    if let Some(l) = loader {
        if !l.is_empty() {
            groups.push(format!("[\"categories:{l}\"]"));
        }
    }
    // Each selected category is its own AND group (narrowing filter).
    for c in categories.iter().filter(|c| !c.is_empty()) {
        groups.push(format!("[\"categories:{c}\"]"));
    }
    format!("[{}]", groups.join(","))
}

#[tauri::command]
async fn search_mods(
    state: tauri::State<'_, Modrinth>,
    query: String,
    mc_version: Option<String>,
    loader: Option<String>,
    project_type: Option<String>,
    categories: Option<Vec<String>>,
    index: Option<String>,
    offset: Option<u32>,
) -> Result<SearchResponse, String> {
    let ptype = project_type.unwrap_or_else(|| "mod".to_string());
    let cats = categories.unwrap_or_default();
    let facets = build_facets(&mc_version, &loader, &cats, &ptype);
    // Empty query + popularity sort = the default "browse everything" list.
    let sort = index.filter(|s| !s.is_empty()).unwrap_or_else(|| {
        if query.trim().is_empty() {
            "downloads".to_string()
        } else {
            "relevance".to_string()
        }
    });
    state
        .search(&query, Some(&facets), &sort, 40, offset.unwrap_or(0))
        .await
        .map_err(|e| e.to_string())
}

#[derive(Serialize)]
struct ModDetail {
    project: Project,
    versions: Vec<Version>,
}

#[tauri::command]
async fn get_mod(
    state: tauri::State<'_, Modrinth>,
    id_or_slug: String,
) -> Result<ModDetail, String> {
    let project = state.project(&id_or_slug).await.map_err(|e| e.to_string())?;
    let versions = state
        .versions(&project.id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ModDetail { project, versions })
}

// ---- Settings ----

#[derive(Serialize)]
struct SettingsView {
    has_anthropic_key: bool,
    ms_client_id: Option<String>,
}

#[tauri::command]
fn get_settings() -> SettingsView {
    let s = settings::load();
    SettingsView {
        has_anthropic_key: s.anthropic_api_key.as_deref().is_some_and(|k| !k.is_empty()),
        // effective id (built-in default unless overridden) so the UI shows
        // sign-in is already configured
        ms_client_id: Some(settings::ms_client_id()),
    }
}

#[tauri::command]
fn set_settings(anthropic_api_key: Option<String>, ms_client_id: Option<String>) -> Result<(), String> {
    let mut s = settings::load();
    if let Some(k) = anthropic_api_key {
        s.anthropic_api_key = if k.is_empty() { None } else { Some(k) };
    }
    if let Some(c) = ms_client_id {
        s.ms_client_id = if c.is_empty() { None } else { Some(c) };
    }
    settings::save(&s).map_err(|e| e.to_string())
}

// ---- Microsoft sign-in ----

#[derive(Serialize, Clone)]
struct AccountView {
    username: String,
    uuid: String,
}

#[tauri::command]
fn auth_status() -> Option<AccountView> {
    auth::load_account().map(|a| AccountView {
        username: a.username,
        uuid: a.uuid,
    })
}

#[tauri::command]
fn auth_signout() {
    auth::clear_account();
}

/// Begin device-code sign-in. Returns the code/URL to display, and spawns a
/// background task that polls, completes the Minecraft login, persists the
/// account, and emits `auth-event` ({status, username?} | {error}).
#[tauri::command]
async fn auth_start(app: AppHandle) -> Result<DeviceCodeStart, String> {
    let client_id = settings::ms_client_id();

    let start = auth::begin_device_code(&client_id)
        .await
        .map_err(|e| e.to_string())?;

    let dc = start.device_code.clone();
    let interval = start.interval.max(1);
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            match auth::poll_token(&client_id, &dc).await {
                Ok(None) => continue,
                Ok(Some(ms)) => {
                    match auth::minecraft_login(&ms).await {
                        Ok(acct) => {
                            let _ = auth::save_account(&acct);
                            let _ = app.emit(
                                "auth-event",
                                serde_json::json!({"status":"signed_in","username":acct.username}),
                            );
                        }
                        Err(e) => {
                            let _ = app.emit(
                                "auth-event",
                                serde_json::json!({"error": e.to_string()}),
                            );
                        }
                    }
                    break;
                }
                Err(e) => {
                    let _ = app.emit("auth-event", serde_json::json!({"error": e.to_string()}));
                    break;
                }
            }
        }
    });

    Ok(start)
}

// ---- Curator (streaming tool-loop) ----

#[tauri::command]
async fn curator_send(
    app: AppHandle,
    history: Vec<ChatMsg>,
    message: String,
) -> Result<(), String> {
    let key = settings::load()
        .anthropic_api_key
        .filter(|k| !k.is_empty())
        .ok_or_else(|| "No Anthropic API key set. Add one in Settings.".to_string())?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CuratorEvent>();
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = app2.emit("curator-event", ev);
        }
    });
    tauri::async_runtime::spawn(async move {
        if let Err(e) =
            curator::run_turn(&key, CURATOR_MODEL, history, message, tx.clone()).await
        {
            let _ = tx.send(CuratorEvent::Error(e.to_string()));
        }
    });
    Ok(())
}

// ---- Instances ----

#[tauri::command]
fn list_instances() -> Vec<Instance> {
    instance::load_instances()
}

#[tauri::command]
fn import_mrpack(path: String) -> Result<Instance, String> {
    let imported = pack::read_mrpack(std::path::Path::new(&path)).map_err(|e| e.to_string())?;
    let now = chrono::Utc::now();
    let inst = Instance {
        id: format!("import-{}", now.timestamp_millis()),
        name: imported.name,
        mc_version: imported.mc_version,
        loader: imported.loader,
        loader_version: imported.loader_version,
        created: now.to_rfc3339(),
        last_played: None,
        mods: imported
            .mods
            .into_iter()
            .map(|m| instance::PinnedMod {
                project_id: String::new(),
                version_id: String::new(),
                name: m.name,
                path: m.path,
                sha1: m.sha1,
                sha512: m.sha512,
                download_url: m.download_url,
                file_size: m.file_size,
            })
            .collect(),
    };
    instance::save_instance(&inst).map_err(|e| e.to_string())?;
    Ok(inst)
}

#[tauri::command]
async fn launch_instance(app: AppHandle, instance_id: String) -> Result<(), String> {
    let inst = instance::load_instances()
        .into_iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| "instance not found".to_string())?;
    let account = auth::load_account().ok_or_else(|| {
        "Sign in with Microsoft first (Instances tab).".to_string()
    })?;
    let client_id = settings::ms_client_id();
    let account = auth::ensure_fresh(&client_id, account)
        .await
        .map_err(|e| e.to_string())?;
    let _ = auth::save_account(&account);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<LaunchEvent>();
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = app2.emit("launch-event", ev);
        }
    });
    tauri::async_runtime::spawn(async move {
        if let Err(e) = launch::launch(&inst, &account, None, tx.clone()).await {
            let _ = tx.send(LaunchEvent::Error(e.to_string()));
        }
    });
    Ok(())
}

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "Anvil",
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "anvil=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Modrinth::new())
        .invoke_handler(tauri::generate_handler![
            search_mods,
            get_mod,
            app_info,
            get_settings,
            set_settings,
            auth_status,
            auth_signout,
            auth_start,
            curator_send,
            list_instances,
            import_mrpack,
            launch_instance
        ])
        .run(tauri::generate_context!())
        .expect("error while running Anvil");
}
