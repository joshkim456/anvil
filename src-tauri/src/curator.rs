//! Conversational modpack curator: Anthropic Messages API streaming + a
//! tool-use loop over the Modrinth client and the pack engine.
//!
//! CONTRACT: preserve these public signatures so `lib.rs` compiles.
//!
//! Style: FREE-FORM chat. Claude probes naturally until it knows at least
//! theme/genre, MC version + loader, single vs multiplayer, and performance
//! target, then proposes a pack with per-mod rationale. Never silently
//! substitute (state substitutions). MUST call `validate_pack` and have it
//! pass before declaring a pack assembled (spec). Tools (client tools, run
//! here against `crate::modrinth` + `crate::pack`): search_mods, get_mod,
//! validate_pack, assemble_pack (writes the instance via `crate::instance`
//! plus a `.mrpack` via `crate::pack::write_mrpack`).

use anyhow::{anyhow, Context};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::instance::{instance_dir, save_instance, Instance, PinnedMod};
use crate::modrinth::{Modrinth, Version};
use crate::pack::{validate_pack, write_mrpack, ModEntry, PackMeta};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMsg {
    /// "user" | "assistant"
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CuratorEvent {
    /// Streamed assistant text delta.
    Text(String),
    /// A tool call started/finished (for the "Searching Modrinth…" chips).
    Tool { name: String, status: String },
    /// An instance was assembled by the assemble_pack tool.
    Assembled { instance_id: String, name: String },
    Done,
    Error(String),
}

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 4096;
/// Safety cap so a misbehaving model cannot spin the tool loop forever.
const MAX_TOOL_ROUNDS: usize = 8;

const SYSTEM_PROMPT: &str = r#"You are Anvil, a warm and friendly Minecraft modpack curator. You hold a free-form conversation with the player and assemble a real, coherent modpack for them.

How you work:
- Talk naturally. Do not interrogate the player with a form. Through normal conversation, make sure that before you propose a pack you know: the theme or genre they want (tech, magic, exploration, kitchen-sink, performance-only, etc.); the Minecraft version and the mod loader (fabric, forge, neoforge, quilt); whether they play single-player or multiplayer; and their performance target (a low-end machine versus a powerful one). If something is missing, ask for it in a light, conversational way, ideally folding the question into your reply rather than stacking questions.
- Use the tools to find real mods on Modrinth. Never invent or guess a mod name, slug, or id. If you are unsure a mod exists or fits, search for it.
- When you propose a pack, give a short one-line reason for each mod so the player understands why it is there. Keep it tight and useful, not flowery.
- If a concept the player asked for is not available on Modrinth for their version and loader, say so plainly and tell them the substitute you are using and why. Never swap something silently.
- Always call validate_pack on a candidate pack before you present it as ready. Only call assemble_pack after validate_pack comes back clean. If validation fails, fix the pack (swap or drop the offending mods, adjust versions) and validate again.
- After a pack is assembled, briefly confirm what was built and where it lives.

Voice: concise and warm. No purple prose. No em dashes. Plain sentences, short paragraphs."#;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run one assistant turn (with the internal tool loop). Returns the updated
/// history including the assistant's final message.
pub async fn run_turn(
    api_key: &str,
    model: &str,
    history: Vec<ChatMsg>,
    user_message: String,
    tx: UnboundedSender<CuratorEvent>,
) -> anyhow::Result<Vec<ChatMsg>> {
    let http = reqwest::Client::builder()
        .build()
        .context("building HTTP client for Anthropic")?;
    let modrinth = Modrinth::new();

    // `messages` is the internal Anthropic conversation, which may grow with
    // assistant tool_use turns and user tool_result turns. The user-facing
    // `history` only ever gains the user's message and the assistant's final
    // text reply.
    let mut messages: Vec<Value> = Vec::with_capacity(history.len() + 2);
    for m in &history {
        messages.push(json!({ "role": m.role, "content": m.content }));
    }
    messages.push(json!({ "role": "user", "content": user_message.clone() }));

    let tools = tool_specs();
    let mut final_text = String::new();

    for round in 0..MAX_TOOL_ROUNDS {
        let body = json!({
            "model": model,
            "max_tokens": MAX_TOKENS,
            "system": SYSTEM_PROMPT,
            "messages": messages,
            "tools": tools,
            "stream": true,
        });

        let resp = http
            .post(ANTHROPIC_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("sending request to Anthropic")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let detail = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(anyhow!(
                "Anthropic API returned {status}: {}",
                detail.trim()
            ));
        }

        let (blocks, stop_reason) = parse_sse_stream(resp, &tx)
            .await
            .context("reading Anthropic SSE stream")?;

        // Accumulate the assistant text from this round for the final reply.
        for b in &blocks {
            if let ContentBlock::Text { text } = b {
                final_text.push_str(text);
            }
        }

        let tool_uses: Vec<(&String, &String, &Value)> = blocks
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolUse { id, name, input } => Some((id, name, input)),
                _ => None,
            })
            .collect();

        if stop_reason.as_deref() != Some("tool_use") || tool_uses.is_empty() {
            // Terminal: end_turn / stop_sequence / max_tokens / no tools.
            break;
        }

        // Append the assistant message (verbatim block list) so the API has
        // the tool_use ids it expects in the matching tool_result.
        messages.push(json!({
            "role": "assistant",
            "content": blocks_to_api_content(&blocks),
        }));

        // Execute each tool and build the tool_result user message.
        let mut results: Vec<Value> = Vec::with_capacity(tool_uses.len());
        for (id, name, input) in &tool_uses {
            let output = match execute_tool(&modrinth, name, input, &tx).await {
                Ok(s) => s,
                Err(e) => format!("tool error: {e:#}"),
            };
            results.push(json!({
                "type": "tool_result",
                "tool_use_id": id,
                "content": output,
            }));
        }
        messages.push(json!({ "role": "user", "content": results }));

        if round + 1 == MAX_TOOL_ROUNDS {
            let _ = tx.send(CuratorEvent::Error(
                "Tool loop exceeded its round limit; stopping.".to_string(),
            ));
            break;
        }
    }

    let _ = tx.send(CuratorEvent::Done);

    let mut updated = history;
    updated.push(ChatMsg {
        role: "user".to_string(),
        content: user_message,
    });
    updated.push(ChatMsg {
        role: "assistant".to_string(),
        content: final_text,
    });
    Ok(updated)
}

// ---------------------------------------------------------------------------
// SSE streaming
// ---------------------------------------------------------------------------

/// An assistant content block, reconstructed from the SSE stream.
#[derive(Debug, Clone)]
enum ContentBlock {
    Text { text: String },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

/// Re-encode reconstructed blocks into the Anthropic message-content shape.
fn blocks_to_api_content(blocks: &[ContentBlock]) -> Vec<Value> {
    blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::ToolUse { id, name, input } => json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": input,
            }),
        })
        .collect()
}

/// While streaming, a tool_use block's JSON input arrives as `partial_json`
/// fragments, so it is buffered as a string until `content_block_stop`.
enum BlockBuilder {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        json_buf: String,
    },
}

/// Drive the SSE response: emit text deltas via `tx`, accumulate tool_use
/// inputs, and return the reconstructed blocks plus the final stop_reason.
async fn parse_sse_stream(
    resp: reqwest::Response,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<(Vec<ContentBlock>, Option<String>)> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    // Blocks are keyed by their stream `index`.
    let mut builders: std::collections::BTreeMap<u64, BlockBuilder> =
        std::collections::BTreeMap::new();
    let mut order: Vec<u64> = Vec::new();
    let mut stop_reason: Option<String> = None;
    // The event name from the most recent `event:` line.
    let mut cur_event: Option<String> = None;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading bytes from Anthropic stream")?;
        buf.extend_from_slice(&chunk);

        // Drain every complete '\n'-terminated line; keep the partial tail.
        loop {
            let Some(nl) = buf.iter().position(|&b| b == b'\n') else {
                break;
            };
            let line_bytes: Vec<u8> = buf.drain(..=nl).collect();
            // Trim the trailing '\n' (and a '\r' if present).
            let mut end = line_bytes.len();
            if end > 0 && line_bytes[end - 1] == b'\n' {
                end -= 1;
            }
            if end > 0 && line_bytes[end - 1] == b'\r' {
                end -= 1;
            }
            let line = String::from_utf8_lossy(&line_bytes[..end]);

            if line.is_empty() {
                // Blank line ends an SSE event; reset the event name.
                cur_event = None;
                continue;
            }
            if let Some(name) = line.strip_prefix("event:") {
                cur_event = Some(name.trim().to_string());
                continue;
            }
            let Some(data) = line.strip_prefix("data:") else {
                continue; // comments / unknown fields
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }

            let val: Value = match serde_json::from_str(data) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("skipping malformed SSE data line: {e}");
                    continue;
                }
            };

            handle_sse_event(
                cur_event.as_deref(),
                &val,
                &mut builders,
                &mut order,
                &mut stop_reason,
                tx,
            );
        }
    }

    // Finalize any blocks that never got an explicit content_block_stop.
    let mut blocks: Vec<ContentBlock> = Vec::new();
    for idx in &order {
        if let Some(b) = builders.remove(idx) {
            if let Some(block) = finalize_block(b) {
                blocks.push(block);
            }
        }
    }

    Ok((blocks, stop_reason))
}

/// Apply one decoded SSE event to the in-progress block state.
fn handle_sse_event(
    event: Option<&str>,
    val: &Value,
    builders: &mut std::collections::BTreeMap<u64, BlockBuilder>,
    order: &mut Vec<u64>,
    stop_reason: &mut Option<String>,
    tx: &UnboundedSender<CuratorEvent>,
) {
    // Prefer the JSON "type"; fall back to the SSE event name.
    let kind = val
        .get("type")
        .and_then(Value::as_str)
        .or(event)
        .unwrap_or("");

    match kind {
        "content_block_start" => {
            let Some(index) = val.get("index").and_then(Value::as_u64) else {
                return;
            };
            let block = &val["content_block"];
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    builders.insert(
                        index,
                        BlockBuilder::Text {
                            text: block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                        },
                    );
                    order.push(index);
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    builders.insert(
                        index,
                        BlockBuilder::ToolUse {
                            id,
                            name,
                            json_buf: String::new(),
                        },
                    );
                    order.push(index);
                }
                _ => {}
            }
        }
        "content_block_delta" => {
            let Some(index) = val.get("index").and_then(Value::as_u64) else {
                return;
            };
            let delta = &val["delta"];
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => {
                    if let Some(t) = delta.get("text").and_then(Value::as_str) {
                        if let Some(BlockBuilder::Text { text }) = builders.get_mut(&index) {
                            text.push_str(t);
                        }
                        if !t.is_empty() {
                            let _ = tx.send(CuratorEvent::Text(t.to_string()));
                        }
                    }
                }
                Some("input_json_delta") => {
                    if let Some(pj) = delta.get("partial_json").and_then(Value::as_str) {
                        if let Some(BlockBuilder::ToolUse { json_buf, .. }) =
                            builders.get_mut(&index)
                        {
                            json_buf.push_str(pj);
                        }
                    }
                }
                _ => {}
            }
        }
        "content_block_stop" => {
            // Block stays in the map; finalized after the stream ends so the
            // emission order (via `order`) is preserved.
        }
        "message_delta" => {
            if let Some(sr) = val
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(Value::as_str)
            {
                *stop_reason = Some(sr.to_string());
            }
        }
        "message_stop" | "message_start" | "ping" => {}
        "error" => {
            let msg = val
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown streaming error");
            let _ = tx.send(CuratorEvent::Error(format!("Anthropic stream error: {msg}")));
        }
        _ => {}
    }
}

fn finalize_block(b: BlockBuilder) -> Option<ContentBlock> {
    match b {
        BlockBuilder::Text { text } => Some(ContentBlock::Text { text }),
        BlockBuilder::ToolUse { id, name, json_buf } => {
            let input: Value = if json_buf.trim().is_empty() {
                json!({})
            } else {
                serde_json::from_str(&json_buf).unwrap_or_else(|e| {
                    tracing::warn!("tool_use {name} had unparseable input json: {e}");
                    json!({})
                })
            };
            Some(ContentBlock::ToolUse { id, name, input })
        }
    }
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

fn tool_specs() -> Value {
    json!([
        {
            "name": "search_mods",
            "description": "Search real mods on Modrinth. Returns up to 15 matching mods with id, slug, title, description, categories, side support, and download count. Always use this before naming any mod; never invent mods.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms, e.g. 'create automation' or 'sodium'." },
                    "mc_version": { "type": "string", "description": "Minecraft version filter, e.g. '1.21.1'." },
                    "loader": { "type": "string", "description": "Loader filter: fabric, forge, neoforge, or quilt." }
                },
                "required": ["query"]
            }
        },
        {
            "name": "get_mod",
            "description": "Fetch full detail for one mod by id or slug: its project summary and the newest version that matches the given Minecraft version and loader (with that version's id, version_number, and dependencies). Use this to confirm a mod exists and supports the target before adding it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "id_or_slug": { "type": "string", "description": "Modrinth project id or slug." },
                    "mc_version": { "type": "string", "description": "Target Minecraft version, e.g. '1.21.1'." },
                    "loader": { "type": "string", "description": "Target loader: fabric, forge, neoforge, or quilt." }
                },
                "required": ["id_or_slug"]
            }
        },
        {
            "name": "validate_pack",
            "description": "Validate a candidate pack against the chosen Minecraft version and loader. Returns a JSON array of issues; an empty array means the pack is coherent. You must call this and get an empty result before calling assemble_pack.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "mc_version": { "type": "string" },
                    "loader": { "type": "string", "description": "fabric, forge, neoforge, or quilt." },
                    "mods": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "project_id": { "type": "string" },
                                "version_id": { "type": "string" }
                            },
                            "required": ["project_id", "version_id"]
                        }
                    }
                },
                "required": ["mc_version", "loader", "mods"]
            }
        },
        {
            "name": "assemble_pack",
            "description": "Assemble the final pack: creates a local instance and writes a standard .mrpack. Runs validation first and refuses to assemble an invalid pack. Only call this after validate_pack returned an empty issue list.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Human-readable pack name." },
                    "mc_version": { "type": "string" },
                    "loader": { "type": "string", "description": "fabric, forge, neoforge, or quilt." },
                    "loader_version": { "type": "string", "description": "The loader version string, e.g. '0.16.0'." },
                    "mods": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "project_id": { "type": "string" },
                                "version_id": { "type": "string" }
                            },
                            "required": ["project_id", "version_id"]
                        }
                    }
                },
                "required": ["name", "mc_version", "loader", "loader_version", "mods"]
            }
        }
    ])
}

// ---------------------------------------------------------------------------
// Tool dispatch + implementations
// ---------------------------------------------------------------------------

async fn execute_tool(
    mr: &Modrinth,
    name: &str,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    match name {
        "search_mods" => tool_search_mods(mr, input, tx).await,
        "get_mod" => tool_get_mod(mr, input, tx).await,
        "validate_pack" => tool_validate_pack(mr, input, tx).await,
        "assemble_pack" => tool_assemble_pack(mr, input, tx).await,
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

fn str_field<'a>(input: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required string field '{key}'"))
}

fn opt_str_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Map the friendly loader name to the Modrinth dependency key used in
/// `modrinth.index.json`.
fn loader_key(loader: &str) -> String {
    match loader {
        "fabric" => "fabric-loader",
        "quilt" => "quilt-loader",
        "neoforge" => "neoforge",
        "forge" => "forge",
        other => other,
    }
    .to_string()
}

fn tool_chip(tx: &UnboundedSender<CuratorEvent>, name: &str, status: &str) {
    let _ = tx.send(CuratorEvent::Tool {
        name: name.to_string(),
        status: status.to_string(),
    });
}

async fn tool_search_mods(
    mr: &Modrinth,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let query = str_field(input, "query")?;
    let mc_version = opt_str_field(input, "mc_version");
    let loader = opt_str_field(input, "loader");

    tool_chip(tx, "search_mods", &format!("searching \"{query}\""));

    // Facet groups: project_type AND version AND loader (each group is an OR
    // list of one). Mirrors `lib.rs::build_facets`.
    let mut groups: Vec<String> = vec!["[\"project_type:mod\"]".to_string()];
    if let Some(v) = mc_version {
        groups.push(format!("[\"versions:{v}\"]"));
    }
    if let Some(l) = loader {
        groups.push(format!("[\"categories:{l}\"]"));
    }
    let facets = format!("[{}]", groups.join(","));

    let res = mr
        .search(query, Some(&facets), "relevance", 15, 0)
        .await
        .map_err(|e| anyhow!("Modrinth search failed: {e}"))?;

    let out: Vec<Value> = res
        .hits
        .iter()
        .map(|h| {
            json!({
                "project_id": h.project_id,
                "slug": h.slug,
                "title": h.title,
                "description": h.description,
                "categories": h.categories,
                "client_side": h.client_side,
                "server_side": h.server_side,
                "downloads": h.downloads,
            })
        })
        .collect();

    tool_chip(tx, "search_mods", "done");
    Ok(serde_json::to_string(&out)?)
}

async fn tool_get_mod(
    mr: &Modrinth,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let id_or_slug = str_field(input, "id_or_slug")?;
    let mc_version = opt_str_field(input, "mc_version");
    let loader = opt_str_field(input, "loader");

    tool_chip(tx, "get_mod", &format!("loading {id_or_slug}"));

    let project = mr
        .project(id_or_slug)
        .await
        .map_err(|e| anyhow!("Modrinth project lookup failed for {id_or_slug}: {e}"))?;
    let versions = mr
        .versions(&project.id)
        .await
        .map_err(|e| anyhow!("Modrinth versions lookup failed for {id_or_slug}: {e}"))?;

    // Versions come back newest-first. Pick the newest that matches the
    // requested target; if no target was given, fall back to the newest.
    let chosen = versions.iter().find(|v| {
        let ok_mc = match mc_version {
            Some(mc) => v.game_versions.iter().any(|g| g == mc),
            None => true,
        };
        let ok_loader = match loader {
            Some(l) => v.loaders.iter().any(|ld| ld == l),
            None => true,
        };
        ok_mc && ok_loader
    });

    let version_json = match chosen.or_else(|| versions.first()) {
        Some(v) => {
            let deps: Vec<Value> = v
                .dependencies
                .iter()
                .map(|d| {
                    json!({
                        "project_id": d.project_id,
                        "dependency_type": d.dependency_type,
                    })
                })
                .collect();
            json!({
                "id": v.id,
                "version_number": v.version_number,
                "game_versions": v.game_versions,
                "loaders": v.loaders,
                "dependencies": deps,
            })
        }
        None => Value::Null,
    };

    let summary = json!({
        "project_id": project.id,
        "slug": project.slug,
        "title": project.title,
        "description": project.description,
        "categories": project.categories,
        "client_side": project.client_side,
        "server_side": project.server_side,
        "game_versions": project.game_versions,
        "loaders": project.loaders,
        "matched_version": version_json,
    });

    tool_chip(tx, "get_mod", "done");
    Ok(serde_json::to_string(&summary)?)
}

/// A `{project_id, version_id}` pair as the model passes it in.
#[derive(Debug, Deserialize)]
struct ModRef {
    project_id: String,
    version_id: String,
}

fn parse_mod_refs(input: &Value) -> anyhow::Result<Vec<ModRef>> {
    let arr = input
        .get("mods")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("'mods' must be an array of {{project_id, version_id}}"))?;
    let mut refs = Vec::with_capacity(arr.len());
    for item in arr {
        let r: ModRef = serde_json::from_value(item.clone())
            .map_err(|e| anyhow!("bad mod entry {item}: {e}"))?;
        refs.push(r);
    }
    Ok(refs)
}

/// Pick the primary file from a resolved Modrinth version (or the first file
/// if none is flagged primary).
fn primary_file(v: &Version) -> anyhow::Result<&crate::modrinth::VersionFile> {
    if v.files.is_empty() {
        return Err(anyhow!("version {} has no downloadable files", v.id));
    }
    Ok(v.files.iter().find(|f| f.primary).unwrap_or(&v.files[0]))
}

/// Resolve one `{project_id, version_id}` into a `pack::ModEntry`, fetching
/// the project (for side support) and the version (for the file + hashes).
async fn build_mod_entry(mr: &Modrinth, r: &ModRef) -> anyhow::Result<ModEntry> {
    let project = mr
        .project(&r.project_id)
        .await
        .map_err(|e| anyhow!("project lookup failed for {}: {e}", r.project_id))?;
    let versions = mr
        .versions(&r.project_id)
        .await
        .map_err(|e| anyhow!("versions lookup failed for {}: {e}", r.project_id))?;

    let version = versions
        .iter()
        .find(|v| v.id == r.version_id)
        .ok_or_else(|| {
            anyhow!(
                "version {} not found for project {}",
                r.version_id,
                r.project_id
            )
        })?;

    let file = primary_file(version)?;

    Ok(ModEntry {
        project_id: r.project_id.clone(),
        version_id: r.version_id.clone(),
        path: format!("mods/{}", file.filename),
        sha1: file.hashes.sha1.clone(),
        sha512: file.hashes.sha512.clone(),
        downloads: vec![file.url.clone()],
        file_size: file.size,
        game_versions: version.game_versions.clone(),
        loaders: version.loaders.clone(),
        client_side: project.client_side.clone(),
        server_side: project.server_side.clone(),
    })
}

async fn build_entries(mr: &Modrinth, refs: &[ModRef]) -> anyhow::Result<Vec<ModEntry>> {
    let mut entries = Vec::with_capacity(refs.len());
    for r in refs {
        entries.push(build_mod_entry(mr, r).await?);
    }
    Ok(entries)
}

async fn tool_validate_pack(
    mr: &Modrinth,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let mc_version = str_field(input, "mc_version")?;
    let loader = str_field(input, "loader")?;
    let refs = parse_mod_refs(input)?;

    tool_chip(tx, "validate_pack", "validating");

    let entries = build_entries(mr, &refs).await?;
    let issues = validate_pack(&entries, mc_version, loader);

    tool_chip(tx, "validate_pack", "done");
    Ok(serde_json::to_string(&issues)?)
}

async fn tool_assemble_pack(
    mr: &Modrinth,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let name = str_field(input, "name")?.to_string();
    let mc_version = str_field(input, "mc_version")?.to_string();
    let loader = str_field(input, "loader")?.to_string();
    let loader_version = str_field(input, "loader_version")?.to_string();
    let refs = parse_mod_refs(input)?;

    tool_chip(tx, "assemble_pack", &format!("assembling \"{name}\""));

    let entries = build_entries(mr, &refs).await?;

    // Never assemble an invalid pack: validate again as the final gate.
    let issues = validate_pack(&entries, &mc_version, &loader);
    if !issues.is_empty() {
        tool_chip(tx, "assemble_pack", "blocked: validation failed");
        return Ok(format!(
            "Refusing to assemble: validate_pack reported issues. Fix these and retry:\n{}",
            serde_json::to_string(&issues)?
        ));
    }

    // Instance id: lowercase hex of the current nanos plus a small spread, so
    // ids stay sortable and collision-resistant without a uuid crate.
    let now = chrono::Utc::now();
    let nanos = now.timestamp_nanos_opt().unwrap_or_else(|| now.timestamp_millis());
    let rand = (nanos as u128).wrapping_mul(2_654_435_761) & 0xffff;
    let id = format!("{:x}{:x}", nanos.unsigned_abs(), rand);

    let pinned: Vec<PinnedMod> = entries
        .iter()
        .map(|e| PinnedMod {
            project_id: e.project_id.clone(),
            version_id: e.version_id.clone(),
            name: e
                .path
                .strip_prefix("mods/")
                .unwrap_or(&e.path)
                .to_string(),
            path: e.path.clone(),
            sha1: e.sha1.clone(),
            sha512: e.sha512.clone(),
            download_url: e.downloads.first().cloned().unwrap_or_default(),
            file_size: e.file_size,
        })
        .collect();

    let inst = Instance {
        id: id.clone(),
        name: name.clone(),
        mc_version: mc_version.clone(),
        loader: loader.clone(),
        loader_version: loader_version.clone(),
        created: now.to_rfc3339(),
        last_played: None,
        mods: pinned,
    };

    save_instance(&inst).with_context(|| format!("saving instance {id}"))?;

    let dir = instance_dir(&id);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating instance dir {}", dir.display()))?;
    let mrpack_path = dir.join(format!("{name}.mrpack"));

    let meta = PackMeta {
        name: name.clone(),
        version_id: "1.0.0".to_string(),
        summary: name.clone(),
        mc_version: mc_version.clone(),
        loader_key: loader_key(&loader),
        loader_version: loader_version.clone(),
    };
    write_mrpack(&meta, &entries, &mrpack_path)
        .with_context(|| format!("writing .mrpack to {}", mrpack_path.display()))?;

    // Emit the chip before the success string so the UI shows the assembled
    // pack even while the assistant's wrap-up text is still streaming.
    let _ = tx.send(CuratorEvent::Assembled {
        instance_id: id.clone(),
        name: name.clone(),
    });
    tool_chip(tx, "assemble_pack", "done");

    Ok(format!(
        "Assembled \"{name}\" ({} mods) for Minecraft {mc_version} on {loader} {loader_version}. \
         Instance id {id}; .mrpack written to {}.",
        entries.len(),
        mrpack_path.display()
    ))
}

#[cfg(test)]
mod live_tests {
    use super::*;

    /// Exercises the real Anthropic SSE stream + the run_turn loop.
    /// Run on demand (the user has a key):
    /// `ANTHROPIC_API_KEY=sk-... cargo test --lib live_run_turn -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn live_run_turn_smoke() {
        let key = match std::env::var("ANTHROPIC_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => {
                eprintln!("skip: set ANTHROPIC_API_KEY to run this");
                return;
            }
        };
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CuratorEvent>();
        let handle = tokio::spawn(async move {
            run_turn(
                &key,
                "claude-sonnet-4-6",
                vec![],
                "Reply with exactly the single word: ready. Do not use any tools.".to_string(),
                tx,
            )
            .await
        });

        let mut text = String::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                CuratorEvent::Text(t) => text.push_str(&t),
                CuratorEvent::Error(e) => panic!("curator emitted error: {e}"),
                CuratorEvent::Done => done = true,
                _ => {}
            }
        }

        let hist = handle.await.unwrap().expect("run_turn returned Err");
        assert!(done, "expected a Done event");
        assert!(!text.trim().is_empty(), "expected streamed assistant text");
        assert_eq!(
            hist.last().map(|m| m.role.as_str()),
            Some("assistant"),
            "history should end with the assistant message"
        );
        println!("assistant said: {}", text.trim());
    }
}
