//! Anvil Tauri application: command surface + event bridging. Heavy logic
//! lives in the modules; this file owns wiring so the modules stay decoupled.

// The curator builds large `serde_json::json!` tool schemas; the default
// macro recursion limit (128) is not enough to expand them.
#![recursion_limit = "1024"]

mod auth;
mod cache;
mod chat;
mod curator;
mod icons;
mod model3d;
#[allow(dead_code)]
mod instance;
mod keybinds;
mod launch;
mod modrinth;
pub mod pack;
#[allow(dead_code)]
mod content;
mod origins;
#[allow(dead_code)]
mod quest;
#[allow(dead_code)]
mod recipe;
#[allow(dead_code)]
mod registry;
mod settings;

use curator::{ChatMsg, CuratorEvent};
use instance::{Instance, PinnedMod};
use launch::LaunchEvent;
use modrinth::{Modrinth, Project, SearchResponse, Version};
use quest::QuestGraph;
use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent,
};

const CURATOR_MODEL: &str = "claude-sonnet-4-6";

/// Append a timestamped line to ~/.anvil/anvil.log. Best-effort; used so a
/// crash or curator error is recoverable from disk without a terminal open.
fn log_event(tag: &str, body: &str) {
    let dir = settings::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("anvil.log"))
    {
        use std::io::Write;
        let _ = writeln!(
            f,
            "\n=== {tag} {} ===\n{body}",
            chrono::Utc::now().to_rfc3339()
        );
    }
}

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
    let off = offset.unwrap_or(0);
    let key = format!("search:{ptype}:{sort}:{off}:{facets}:{query}");

    // Fresh-enough cache hit (5 min) -> skip the network entirely.
    if let Some(cached) = cache::get(&key, 300) {
        if let Ok(r) = serde_json::from_str::<SearchResponse>(&cached) {
            return Ok(r);
        }
    }

    match state.search(&query, Some(&facets), &sort, 40, off).await {
        Ok(resp) => {
            if let Ok(j) = serde_json::to_string(&resp) {
                cache::put(&key, &j);
            }
            Ok(resp)
        }
        // Offline / Modrinth down: serve any stale copy rather than fail.
        Err(e) => match cache::get_stale(&key) {
            Some(stale) => serde_json::from_str::<SearchResponse>(&stale)
                .map_err(|_| e.to_string()),
            None => Err(e.to_string()),
        },
    }
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
    theme: String,
}

#[tauri::command]
fn get_settings() -> SettingsView {
    SettingsView {
        has_anthropic_key: settings::anthropic_key().is_some(),
        // effective id (built-in default unless overridden) so the UI shows
        // sign-in is already configured
        ms_client_id: Some(settings::ms_client_id()),
        theme: settings::theme(),
    }
}

#[tauri::command]
fn set_settings(
    anthropic_api_key: Option<String>,
    ms_client_id: Option<String>,
    theme: Option<String>,
) -> Result<(), String> {
    let mut s = settings::load();
    if let Some(k) = anthropic_api_key {
        s.anthropic_api_key = if k.is_empty() { None } else { Some(k) };
    }
    if let Some(c) = ms_client_id {
        s.ms_client_id = if c.is_empty() { None } else { Some(c) };
    }
    if let Some(t) = theme {
        s.theme = match t.as_str() {
            "light" | "dark" | "system" => Some(t),
            _ => s.theme,
        };
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

const MS_WINDOW: &str = "ms-login";

/// Open the Microsoft sign-in webview, capture the redirect, finish the Xbox
/// chain, persist the account, and emit `auth-event`
/// ({status:signed_in,username} | {error}). Returns immediately; the webview
/// and the rest of the flow run in the background.
#[tauri::command]
async fn auth_start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let result = run_ms_signin(&app).await;
        if let Some(w) = app.get_webview_window(MS_WINDOW) {
            let _ = w.close();
        }
        match result {
            Ok(acct) => {
                let _ = auth::save_account(&acct);
                let _ = app.emit(
                    "auth-event",
                    serde_json::json!({"status":"signed_in","username":acct.username}),
                );
            }
            Err(e) => {
                let _ = app.emit("auth-event", serde_json::json!({ "error": e }));
            }
        }
    });
}

/// Build the sign-in window, poll its URL for the `oauth20_desktop.srf?code=`
/// redirect (callbacks are unreliable for the final OAuth hop), then exchange.
async fn run_ms_signin(app: &AppHandle) -> Result<auth::MinecraftAccount, String> {
    use std::time::Duration;

    let flow = auth::begin_login().await.map_err(|e| e.to_string())?;
    let url = flow
        .auth_url
        .parse()
        .map_err(|_| "Microsoft returned an invalid sign-in URL.".to_string())?;

    if let Some(w) = app.get_webview_window(MS_WINDOW) {
        let _ = w.close();
    }
    let win = WebviewWindowBuilder::new(app, MS_WINDOW, WebviewUrl::External(url))
        .title("Sign in with Microsoft")
        .inner_size(520.0, 720.0)
        .resizable(true)
        .focused(true)
        .build()
        .map_err(|e| format!("Could not open the sign-in window: {e}"))?;

    // Window closed by the user = cancellation.
    let (cancel_tx, mut cancel_rx) = tokio::sync::mpsc::channel::<()>(1);
    win.on_window_event(move |ev| {
        if matches!(
            ev,
            WindowEvent::CloseRequested { .. } | WindowEvent::Destroyed
        ) {
            let _ = cancel_tx.try_send(());
        }
    });

    let code = tokio::time::timeout(Duration::from_secs(600), async {
        loop {
            tokio::select! {
                _ = cancel_rx.recv() => {
                    return Err("Sign-in window was closed before completing.".to_string());
                }
                _ = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
            let Ok(cur) = win.url() else { continue };
            if !cur.as_str().starts_with(auth::REDIRECT_PREFIX) {
                continue;
            }
            if let Some((_, v)) = cur.query_pairs().find(|(k, _)| k == "code") {
                return Ok(v.into_owned());
            }
            if let Some((_, v)) =
                cur.query_pairs().find(|(k, _)| k == "error_description")
            {
                return Err(format!("Microsoft sign-in failed: {v}"));
            }
            return Err(
                "Microsoft sign-in did not return an authorization code.".to_string(),
            );
        }
    })
    .await
    .map_err(|_| "Sign-in timed out. Please try again.".to_string())??;

    let _ = win.close();
    auth::finish_login(&code, flow)
        .await
        .map_err(|e| e.to_string())
}

// ---- Curator (streaming tool-loop) ----

#[tauri::command]
async fn curator_send(
    app: AppHandle,
    history: Vec<ChatMsg>,
    message: String,
    phase: Option<String>,
    thread_id: Option<String>,
) -> Result<(), String> {
    // Default to the curating phase if an older frontend omits it.
    let phase = phase.unwrap_or_else(|| "curating".to_string());
    let key = settings::anthropic_key().ok_or_else(|| {
        "No Anthropic API key. Add one in Settings, or set ANTHROPIC_API_KEY in your environment."
            .to_string()
    })?;

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CuratorEvent>();
    let app2 = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            let _ = app2.emit("curator-event", ev);
        }
    });
    tauri::async_runtime::spawn(async move {
        use futures_util::FutureExt;
        // A panic anywhere in the curator pipeline (SSE parsing, a tool, JSON
        // handling) would otherwise abort this task silently and the UI would
        // just hang — the "it crashes" symptom. Catch it and surface a real
        // error so the failure is visible and diagnosable instead of dark.
        let run = std::panic::AssertUnwindSafe(curator::run_turn(
            &key,
            CURATOR_MODEL,
            &phase,
            thread_id.as_deref(),
            history,
            message,
            tx.clone(),
        ));
        match run.catch_unwind().await {
            Ok(Ok(_history)) => {}
            Ok(Err(e)) => {
                log_event("CURATOR ERROR", &format!("{e:#}"));
                let _ = tx.send(CuratorEvent::Error(format!("{e:#}")));
            }
            Err(panic) => {
                let what = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_string());
                // The panic hook already logged location + backtrace; record
                // that the curator task was the origin too.
                log_event("CURATOR PANIC", &what);
                let _ = tx.send(CuratorEvent::Error(format!(
                    "The curator hit a bug and stopped: {what}. \
                     Your progress so far is saved; please report this."
                )));
            }
        }
    });
    Ok(())
}

// ---- Curator chat threads ----

#[tauri::command]
fn list_chats() -> Vec<chat::ChatThread> {
    chat::load_threads()
}

#[tauri::command]
fn get_chat(thread_id: String) -> Option<chat::ChatThread> {
    chat::load_thread(&thread_id)
}

#[tauri::command]
fn save_chat(thread: chat::ChatThread) -> Result<(), String> {
    chat::save_thread(&thread).map_err(|e| e.to_string())
}

#[tauri::command]
fn delete_chat(thread_id: String) -> Result<(), String> {
    chat::delete_thread(&thread_id).map_err(|e| e.to_string())
}

#[tauri::command]
fn chat_for_instance(instance_id: String) -> Option<chat::ChatThread> {
    chat::thread_for_instance(&instance_id)
}

// ---- Instances ----

#[tauri::command]
fn list_instances() -> Vec<Instance> {
    instance::load_instances()
}

// ---- Keybinds (parsed from the instance's options.txt) ----

#[tauri::command]
fn get_keybinds(instance_id: String) -> keybinds::KeybindReport {
    keybinds::read_keybinds(&instance_id)
}

#[tauri::command]
fn set_keybinds(
    instance_id: String,
    changes: Vec<keybinds::KeyChange>,
) -> Result<(), String> {
    keybinds::write_keybinds(&instance_id, &changes).map_err(|e| {
        format!("Could not write keybinds (launch the pack once first?): {e}")
    })
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
        // Imported packs are a frozen .mrpack snapshot with no Modrinth
        // project ids; leave roots empty (the empty == "all mods are roots"
        // back-compat rule applies and edit_pack is not offered for imports).
        roots: vec![],
    };
    instance::save_instance(&inst).map_err(|e| e.to_string())?;
    Ok(inst)
}

#[tauri::command]
async fn launch_instance(app: AppHandle, instance_id: String) -> Result<(), String> {
    let mut inst = instance::load_instances()
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

    // Stamp "last played" now that the instance is genuinely being launched
    // (auth succeeded, the game is about to start). Nothing else ever wrote
    // this field, so the UI showed "never" forever. Persist before spawning
    // so it survives even if the game later crashes — you still played it.
    inst.last_played = Some(chrono::Utc::now().to_rfc3339());
    if let Err(e) = instance::save_instance(&inst) {
        // Non-fatal: a stat write must never block actually launching.
        eprintln!("warning: could not persist last_played: {e}");
    }

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

/// Tier 3: boot the pack once and report whether mods initialize cleanly
/// (catches the dependency-reject / entrypoint-crash classes before the user
/// has to discover them by playing). Report-only; never mutates the pack.
#[tauri::command]
async fn smoke_test_instance(
    app: AppHandle,
    instance_id: String,
) -> Result<launch::SmokeVerdict, String> {
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
    launch::smoke_test(&inst, &account, None, tx)
        .await
        .map_err(|e| e.to_string())
}

// ---- Instance management ----

fn find_instance(id: &str) -> Option<Instance> {
    instance::load_instances().into_iter().find(|i| i.id == id)
}

/// Newest project version compatible with `mc`/`loader`, as a PinnedMod.
async fn resolve_pinned(
    mr: &Modrinth,
    project_id: &str,
    mc: &str,
    loader: &str,
) -> Result<PinnedMod, String> {
    let project = mr.project(project_id).await.map_err(|e| e.to_string())?;
    let versions = mr.versions(&project.id).await.map_err(|e| e.to_string())?;
    let v = versions
        .iter()
        .find(|v| {
            v.game_versions.iter().any(|g| g == mc)
                && v.loaders.iter().any(|l| l == loader)
        })
        .ok_or_else(|| {
            format!("no {loader} build of {} for {mc}", project.title)
        })?;
    let f = v
        .files
        .iter()
        .find(|f| f.primary)
        .or_else(|| v.files.first())
        .ok_or_else(|| "version has no downloadable file".to_string())?;
    Ok(PinnedMod {
        project_id: project.id.clone(),
        version_id: v.id.clone(),
        name: project.title.clone(),
        path: format!("mods/{}", f.filename),
        sha1: f.hashes.sha1.clone(),
        sha512: f.hashes.sha512.clone(),
        download_url: f.url.clone(),
        file_size: f.size,
    })
}

#[tauri::command]
fn create_instance(
    name: String,
    mc_version: String,
    loader: String,
    loader_version: String,
) -> Result<Instance, String> {
    let now = chrono::Utc::now();
    let inst = Instance {
        id: format!("inst-{}", now.timestamp_millis()),
        name,
        mc_version,
        loader,
        loader_version,
        created: now.to_rfc3339(),
        last_played: None,
        mods: vec![],
        roots: vec![],
    };
    instance::save_instance(&inst).map_err(|e| e.to_string())?;
    Ok(inst)
}

#[tauri::command]
fn delete_instance(instance_id: String) -> Result<(), String> {
    let _ = std::fs::remove_file(
        settings::instances_dir().join(format!("{instance_id}.json")),
    );
    let _ = std::fs::remove_dir_all(instance::instance_dir(&instance_id));
    // A chat exists to build/iterate one pack; once that pack is gone the
    // thread is dead weight, so it goes with it (plus its candidate sidecar).
    chat::delete_threads_for_instance(&instance_id);
    Ok(())
}

#[tauri::command]
fn duplicate_instance(
    instance_id: String,
    new_name: String,
) -> Result<Instance, String> {
    let mut inst =
        find_instance(&instance_id).ok_or_else(|| "instance not found".to_string())?;
    let now = chrono::Utc::now();
    inst.id = format!("inst-{}", now.timestamp_millis());
    inst.name = new_name;
    inst.created = now.to_rfc3339();
    inst.last_played = None;
    instance::save_instance(&inst).map_err(|e| e.to_string())?;
    Ok(inst)
}

#[tauri::command]
async fn add_mod_to_instance(
    state: tauri::State<'_, Modrinth>,
    instance_id: String,
    project_id: String,
) -> Result<Instance, String> {
    let mut inst =
        find_instance(&instance_id).ok_or_else(|| "instance not found".to_string())?;
    if inst.mods.iter().any(|m| m.project_id == project_id) {
        return Ok(inst);
    }
    let pinned =
        resolve_pinned(&state, &project_id, &inst.mc_version, &inst.loader).await?;
    inst.mods.push(pinned);
    instance::save_instance(&inst).map_err(|e| e.to_string())?;
    Ok(inst)
}

#[tauri::command]
fn remove_mod_from_instance(
    instance_id: String,
    project_id: String,
) -> Result<Instance, String> {
    let mut inst =
        find_instance(&instance_id).ok_or_else(|| "instance not found".to_string())?;
    inst.mods.retain(|m| m.project_id != project_id);
    instance::save_instance(&inst).map_err(|e| e.to_string())?;
    Ok(inst)
}

#[derive(Serialize)]
struct UpdateInfo {
    project_id: String,
    name: String,
    from: String,
    to: String,
}

#[tauri::command]
async fn check_instance_updates(
    state: tauri::State<'_, Modrinth>,
    instance_id: String,
) -> Result<Vec<UpdateInfo>, String> {
    let inst =
        find_instance(&instance_id).ok_or_else(|| "instance not found".to_string())?;
    let mut out = Vec::new();
    for m in &inst.mods {
        if m.project_id.is_empty() {
            continue; // imported-pack entries carry no Modrinth project id
        }
        if let Ok(versions) = state.versions(&m.project_id).await {
            if let Some(v) = versions.iter().find(|v| {
                v.game_versions.iter().any(|g| g == &inst.mc_version)
                    && v.loaders.iter().any(|l| l == &inst.loader)
            }) {
                if v.id != m.version_id {
                    out.push(UpdateInfo {
                        project_id: m.project_id.clone(),
                        name: m.name.clone(),
                        from: m.version_id.clone(),
                        to: v.version_number.clone(),
                    });
                }
            }
        }
    }
    Ok(out)
}

#[tauri::command]
async fn apply_instance_updates(
    state: tauri::State<'_, Modrinth>,
    instance_id: String,
    project_ids: Vec<String>,
) -> Result<Instance, String> {
    let mut inst =
        find_instance(&instance_id).ok_or_else(|| "instance not found".to_string())?;
    for pid in &project_ids {
        if let Ok(p) =
            resolve_pinned(&state, pid, &inst.mc_version, &inst.loader).await
        {
            if let Some(slot) = inst.mods.iter_mut().find(|m| &m.project_id == pid) {
                *slot = p;
            }
        }
    }
    instance::save_instance(&inst).map_err(|e| e.to_string())?;
    Ok(inst)
}

// ---- Quests ----

/// Crude mod-namespace guess from jar filenames, for quest ID grounding.
fn instance_namespaces(instance_id: &str) -> Vec<String> {
    find_instance(instance_id)
        .map(|i| {
            i.mods
                .iter()
                .filter_map(|m| {
                    m.path
                        .rsplit('/')
                        .next()
                        .and_then(|f| f.split(['-', '_', '+', '.']).next())
                        .map(|s| s.to_lowercase())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Recipe-grid item icon for the quest viewer. `Ok(None)` => the UI renders a
/// labeled slot (unresolvable / vanilla-without-assets / not in any jar).
/// Offloaded to a blocking thread: it may open several jars on a cache miss.
#[tauri::command]
async fn get_item_icon(
    instance_id: String,
    item_id: String,
) -> Result<Option<String>, String> {
    tokio::task::spawn_blocking(move || {
        icons::item_icon_data_url(&instance_id, &item_id)
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_quest_graph(instance_id: String) -> Option<QuestGraph> {
    quest::load_graph(&instance::instance_dir(&instance_id))
}

#[tauri::command]
fn save_quest_graph(instance_id: String, graph: QuestGraph) -> Result<(), String> {
    let dir = instance::instance_dir(&instance_id);
    quest::write_quests(&graph, &dir).map_err(|e| e.to_string())?;
    // Mirror the curator: a UI-driven save also emits the v1 Origins datapack
    // iff Origins core + Open Loader are pinned (gated, byte-identical else).
    if let Some(inst) = find_instance(&instance_id) {
        let has_origins_core = inst
            .mods
            .iter()
            .any(|m| origins::is_origins_core(&m.project_id, &m.name));
        let has_open_loader = inst.mods.iter().any(|m| {
            let needle = |s: &str| {
                let s = s.to_lowercase();
                s.contains("open-loader") || s.contains("openloader")
            };
            needle(&m.project_id) || needle(&m.name) || needle(&m.path)
        });
        if has_origins_core && has_open_loader {
            origins::write_origins_datapack(&dir, "anvil")
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
fn validate_quest_graph(
    instance_id: String,
    graph: QuestGraph,
) -> Vec<quest::QuestIssue> {
    let idx = quest::build_index(&instance_namespaces(&instance_id));
    quest::validate_graph(&graph, &idx)
}

/// One power as the Origins viewer shows it. `shipped` distinguishes a power
/// Origins itself ships (label resolved from the faithful table) from a power
/// Anvil emitted a file for (label is that power's own name/description).
#[derive(Serialize)]
struct OriginPowerView {
    name: String,
    description: String,
    shipped: bool,
}

#[derive(Serialize)]
struct OriginEntryView {
    id: String,
    name: String,
    description: String,
    icon: String,
    impact: i64,
    powers: Vec<OriginPowerView>,
}

#[derive(Serialize)]
struct OriginsView {
    origins: Vec<OriginEntryView>,
}

/// Read-back of the instance's on-disk Origins datapack for the in-app viewer.
/// `Ok(None)` => the instance has no `anvil-origins` datapack (the UI shows
/// nothing). Offloaded to a blocking thread: it walks a directory of files.
#[tauri::command]
async fn get_origins(instance_id: String) -> Result<Option<OriginsView>, String> {
    tokio::task::spawn_blocking(move || {
        let Some(set) = origins::read_origins(&instance::instance_dir(&instance_id))
        else {
            return None;
        };
        // Local power ids, for classifying each origin's refs.
        let local: std::collections::BTreeMap<&str, &origins::Power> =
            set.powers.iter().map(|p| (p.id.as_str(), p)).collect();
        let origins_view = set
            .origins
            .iter()
            .map(|o| {
                let powers = o
                    .powers
                    .iter()
                    .map(|r| {
                        // Strip an optional namespace. `anvil:<id>` or a bare
                        // `<id>` that names a local power => that local power.
                        // Anything else (e.g. `origins:<id>`) => shipped label.
                        let bare_local = r.strip_prefix("anvil:").unwrap_or(r);
                        if let Some(p) = local.get(bare_local) {
                            OriginPowerView {
                                name: p.name.clone(),
                                description: p.description.clone(),
                                shipped: false,
                            }
                        } else {
                            // Shipped: take the part after the colon (or the
                            // whole ref if unqualified) for the label table.
                            let pid =
                                r.split_once(':').map(|(_, p)| p).unwrap_or(r);
                            let (name, description) =
                                origins::shipped_power_label(pid);
                            OriginPowerView {
                                name,
                                description,
                                shipped: true,
                            }
                        }
                    })
                    .collect();
                OriginEntryView {
                    id: o.id.clone(),
                    name: o.name.clone(),
                    description: o.description.clone(),
                    icon: o.icon.clone(),
                    impact: o.impact,
                    powers,
                }
            })
            .collect();
        Some(OriginsView {
            origins: origins_view,
        })
    })
    .await
    .map_err(|e| e.to_string())
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
    // Load a project .env if present (dev convenience). `dotenv()` searches the
    // cwd and walks up parents, so a .env at the project root is found even
    // though `tauri dev` runs the binary from `src-tauri/`. The shipped app
    // has no .env and falls back to Settings; this never overrides Settings.
    let dotenv = dotenvy::dotenv();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "anvil=info".into()),
        )
        .init();

    match &dotenv {
        Ok(path) => tracing::info!("loaded .env from {}", path.display()),
        Err(_) => tracing::info!("no .env found (using Settings / shell env)"),
    }

    // Persist panics to ~/.anvil/anvil.log so a crash is recoverable without a
    // terminal open. Chains the default hook (stderr still works). Best-effort:
    // the hook itself never panics.
    {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let loc = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_else(|| "<unknown location>".to_string());
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| {
                    info.payload().downcast_ref::<String>().cloned()
                })
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let bt = std::backtrace::Backtrace::force_capture();
            let when = chrono::Utc::now().to_rfc3339();
            let line = format!(
                "\n=== PANIC {when} ===\nat {loc}\n{msg}\n--- backtrace ---\n{bt}\n",
            );
            let dir = settings::data_dir();
            let _ = std::fs::create_dir_all(&dir);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join("anvil.log"))
            {
                use std::io::Write;
                let _ = f.write_all(line.as_bytes());
            }
            prev(info);
        }));
    }

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
            list_chats,
            get_chat,
            save_chat,
            delete_chat,
            chat_for_instance,
            list_instances,
            import_mrpack,
            launch_instance,
            smoke_test_instance,
            create_instance,
            delete_instance,
            duplicate_instance,
            add_mod_to_instance,
            remove_mod_from_instance,
            check_instance_updates,
            apply_instance_updates,
            get_keybinds,
            set_keybinds,
            get_quest_graph,
            save_quest_graph,
            get_item_icon,
            validate_quest_graph,
            get_origins
        ])
        .run(tauri::generate_context!())
        .expect("error while running Anvil");
}
