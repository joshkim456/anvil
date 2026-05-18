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

use std::path::Path;

use crate::instance::{
    instance_dir, load_instances, save_instance, Instance, PinnedMod,
};
use crate::modrinth::{Modrinth, Version};
use crate::pack::{self, read_mrpack, validate_pack, write_mrpack, ModEntry, PackMeta};
use crate::quest::{
    build_index_for_instance, load_graph, validate_graph, write_quests,
    QuestGraph, QuestIssue,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMsg {
    /// "user" | "assistant"
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
// Adjacently tagged: serde CANNOT serialize internally-tagged newtype
// variants wrapping a primitive (Text(String)/Error(String)) -> they failed
// silently at emit time. `content = "data"` makes every variant serialize.
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum CuratorEvent {
    /// Streamed assistant text delta.
    Text(String),
    /// A tool call started/finished (for the "Searching Modrinth…" chips).
    Tool { name: String, status: String },
    /// An instance was assembled by the assemble_pack tool.
    Assembled { instance_id: String, name: String },
    /// Pipeline phase advanced ("assembled" | "progression" | "complete");
    /// the frontend persists it on the thread so the next turn is scoped to
    /// it. The "progression" phase exposes generate_quests (recipes are a
    /// quest-node facet of it — no separate recipe tool) + query_registry.
    Phase(String),
    /// Per-round token usage pulled from the Anthropic stream (observability;
    /// lets us see cache hit/miss and cost without changing the request).
    Usage(Usage),
    Done,
    Error(String),
}

/// Token counts for one Anthropic streamed response. `message_start` carries
/// `input_tokens` + the two cache counts; `message_delta` carries the
/// cumulative `output_tokens` (last write wins).
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
}

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Generous: a single generate_quests batch (a few rich chapters) or a long
/// pack rationale must fit without the model hitting `max_tokens` and getting
/// its tool call truncated mid-JSON. Sonnet 4.6 supports far more than this.
const MAX_TOKENS: u32 = 16000;
/// Safety cap so a misbehaving model cannot spin the tool loop forever.
/// Conversational curation of a ~28-mod pack legitimately needs many rounds
/// (propose, refine via search/get, validate, assemble, then quests), so this
/// is generous headroom — still finite so a genuine non-converging loop can't
/// run up unbounded API cost, but high enough that a normal build never trips
/// it. The real fix for "it hit the limit" is making propose_pack actually
/// return a candidate (keyword search below), not just raising this.
const MAX_TOOL_ROUNDS: usize = 80;

const SYSTEM_PROMPT: &str = r#"You are Anvil, a warm and friendly Minecraft modpack curator. You hold a free-form conversation with the player and assemble a real, coherent modpack for them.

How you work:
- Talk naturally. Do not interrogate the player with a form. Through normal conversation, make sure that before you propose a pack you know: the theme or genre they want (tech, magic, exploration, kitchen-sink, performance-only, etc.); the Minecraft version and the mod loader; whether they play single-player or multiplayer; and their performance target (a low-end machine versus a powerful one). ANVIL v1 IS FABRIC ONLY: always build on Fabric. If the player asks for Forge or NeoForge, briefly explain Anvil currently builds Fabric packs only (1.20.1 Fabric is the fully verified path, quests included) and proceed on Fabric — never call a tool with loader forge or neoforge, it is hard-blocked. Default to 1.20.1 Fabric, and require 1.20.1 Fabric whenever quests are wanted. If something is missing, ask for it in a light, conversational way, ideally folding the question into your reply rather than stacking questions.
- Use the tools to find real mods on Modrinth. Never invent or guess a mod name, slug, or id. Once you know the theme, Minecraft version and loader, your FIRST move is propose_pack with a one-line brief: it returns a popular, relevant, dependency-resolved candidate pack in one call, and that exact resolved set is SAVED for this conversation. Review and refine it with the player (use search_mods/get_mod only for specific swaps or to confirm a single mod), rather than building the whole pack by hand search by search. To build the proposed pack, call assemble_pack with the name, Minecraft version, loader and loader_version — you may pass an EMPTY mods list and it assembles the saved proposal, so you never need to re-list every mod or re-run propose_pack to recover the list. Only pass explicit {project_id, version_id} pairs when the player asked for specific swaps. assemble_pack MERGES into an existing same-named pack: to ADD a mod later, call it with ONLY the new mod (existing mods are kept automatically — never re-send the whole list to "preserve" it). To REMOVE a mod or rebuild from scratch, pass replace:true with the full intended list.
- Prioritise quality. Every search result includes a downloads count. Strongly prefer well-established, popular mods (high downloads, clearly maintained for the target version). Do not add obscure, abandoned, or very-low-download mods just to fill a slot. If the only option for a requested feature is low quality, say so plainly and let the player decide rather than padding the pack with weak mods.
- Be honest about numbers. Never announce, promise, or estimate a mod count (for example "150+ mods") that you have not actually assembled. Do not pad a pack with filler mods to reach a target number, and never claim a count to satisfy the player. Only state a mod count after validate_pack has passed, and state the true count of the validated list. If the player asked for more mods than you can fill with genuinely good ones, deliver the smaller quality set and say so plainly; quality and coherence always beat hitting a number.
- Draw inspiration from real packs. For a themed or genre request, proactively call seed_from_pack on a well-known pack in that space (for example a popular Create, skyblock, or adventure pack) and study its actual mod list to see which mods the community relies on for that theme. Treat it as inspiration for your own curated selection: keep the strong, popular building blocks, drop what does not fit this player, and adapt with search_mods. Never copy a pack wholesale.
- When you propose a pack, give a short one-line reason for each mod so the player understands why it is there. Keep it tight and useful, not flowery.
- If a concept the player asked for is not available on Modrinth for their version and loader, say so plainly and tell them the substitute you are using and why. Never swap something silently.
- Always call validate_pack on a candidate pack before you present it as ready. Only call assemble_pack after validate_pack comes back clean. If validation fails, fix the pack (swap or drop the offending mods, adjust versions) and validate again.
- After a pack is assembled and looks final, call verify_pack ONCE before you tell the player it is done: it boots the pack headless (about a minute, say so) and confirms every mod initializes. If it returns VERIFIED, confirm the pack works. If it returns a VERIFICATION FAILED analysis, relay the culprit and fix in plain words and ASK the player before changing anything (surface and wait, never modify the pack unprompted). If they approve, call assemble_pack again with the SAME name minus the culprit mod, then call verify_pack one more time — at most once, never loop verify/assemble. Do not call verify_pack after every small change, only when the pack is final or the player asks.
- After a pack is assembled, briefly confirm what was built and where it lives.
- If the player wants a storyline, quests, or a sense of progression, the quest system is Heracles (published on Modrinth as "Odyssey Quests", slug odyssey-quests). Quests require the pack to be Minecraft 1.20.1 on fabric or forge, and the pack must include both odyssey-quests and its dependency resourceful-lib. So: tell the player quests need a 1.20.1 fabric/forge pack and steer the version/loader there if not set, search_mods for "odyssey-quests" and "resourceful-lib" and include both real mods, build the pack first, then add quests. After assembly, the instance id and full mod list are given to you in an ACTIVE PACK STATE block at the top of the conversation — call generate_quests with THAT instance id directly. Never call assemble_pack again just to rediscover the instance id, and if a pack is already assembled and the player asks for quests, your next tool call is generate_quests, not propose_pack or assemble_pack.
- Custom recipes that knit the pinned mods together (one mod's output feeding another mod's recipe, new crafting/smelting bridges) are also part of progression, and they are quest NODES, not a separate system: a quest node may carry a "recipes" array, which makes it a quest to obtain the bridged item AND injects the recipe into a vanilla 1.20.1 datapack loaded by Open Loader. If the player wants custom recipes or cross-mod crafting, the pack MUST include open-loader (Modrinth slug open-loader; 1.20.1 fabric or forge); search_mods for "open-loader" and include the real mod, build the pack first, then design recipe-bridge nodes inside generate_quests.
- Design quests to the standard of a real, well-loved kitchen-sink pack (think All the Mods, Create: Above and Beyond): a deep, interconnected progression web, NOT a short flat list. Concretely:
  - Structure it in chapters that are progression tiers/themes. Start with a small "Getting Started" hub chapter (basic tools, food, first ores). Then one themed questline chapter per MAJOR mod actually in this pack (study the assembled mod list and build an arc around each big mod: its starter item, its core machine/mechanic, an advanced build, a mastery goal). Finish with a milestone/endgame chapter whose quests depend on several mod chapters converging.
  - Scale to the pack: a kitchen-sink pack should get roughly 6 to 10 chapters and 40 to 80 quests; a focused pack fewer, but still tiered, never trivial. generate_quests will reject a graph that is too sparse, has orphan quests, or has chapters not wired into the rest, so make it genuinely interconnected.
  - Dependencies are the backbone. Almost every quest must have at least one prerequisite. Use convergence (a quest that requires 2+ earlier branches), gating (the reward of one quest is the item the next needs), and cross-chapter edges so mod questlines feed the milestone spine. Aim for at least one chapter that can only be entered after progress in two others. Keep at most one root (no-prereq) quest per chapter. The whole thing must stay a DAG (no cycles); every dep must point at a quest that exists.
  - Per-quest quality: an evocative title (not "Quest 3"), a 1 to 3 sentence description with flavor and concrete how-to, tasks that escalate in difficulty across the chapter, and rewards that are genuinely useful, ideally handing over the item or resource the next quest needs, with occasional xp or a command reward for milestones. Vary task types across the pack: item, kill, advancement, biome, dimension, structure, recipe, and the occasional manual checkmark for roleplay beats. Invent cross-mod "integration" quests that combine items from multiple pinned mods (e.g. power one mod's machine with another mod's energy). DIFFICULTY IS TIERED T1 (first 10 minutes) to T5 (completionist/post-credits) and ENFORCED on EVERY call. Chapter 1 is the onboarding ramp and holds BOTH T1 and T2 quests (mix them freely). Every chapter AFTER chapter 1 is T3 or higher (NEVER T1/T2 again) and the ceiling rises gradually to T5 by the final chapter, scaled to how many chapters there are: chapter 2 sits around T3, the middle chapters ramp T3 toward T4, the final chapter may use T5. Keep hard advancements out of chapter 1 (e.g. `adventuring_time`/visit-all-biomes = T5, `netherite_armor` = T4, any End content = T4+), and never put a trivial T1/T2 task in any chapter past the first. generate_quests REJECTS an over-hard task with `OverdifficultForChapter` AND a too-trivial late task with `UnderdifficultForChapter` (task, its tier, the chapter cap/floor) on EVERY call: when you see either, retier the task or move the quest to the right chapter.
  - Build it in batches, not one giant call. Call generate_quests several times, roughly one to three chapters per call, in progression order (earlier tiers first) so each batch's dependencies point at quests already saved. Calls accumulate: a chapter with the same id replaces its previous version, new chapters are appended. When every chapter is in, make ONE last call with "final": true to run the full quality/interconnection check. Hard errors (unknown ids, missing deps, cycles) are reported on every call; the sparse/orphan/disconnected checks only run on the final call. If a call returns issues, fix them and call again.
  - Lay nodes on a readable grid: x increases with each progression tier, y separates parallel branches, about 2.0 units spacing. Only reference items, entities, and advancements from mods actually in the assembled pack; generate_quests rejects anything else.
- Quality-of-life baseline (always raise this, and heavily suggest it). Early in the conversation, once you know the Minecraft version and loader, ask the player whether to include a standard quality-of-life and performance set, and strongly recommend saying yes (default to including it unless they decline). Fold it into the conversation as one light question, not a checklist. The set is: a recipe/item viewer (MANDATORY — see below), a minimap and a world map (Xaero's Minimap and Xaero's World Map), AppleSkin (food and saturation tooltips), Controlling (searchable keybinds), an inventory sorting mod, Mouse Tweaks, and a loader-appropriate performance stack that is compatible with content mods (on Fabric or Quilt: Sodium, plus Indium whenever the pack needs the Fabric Rendering API, plus Lithium, FerriteCore, and Entity Culling; on Forge or NeoForge the equivalents such as Embeddium or Rubidium, FerriteCore, and an entity-culling mod). Never use OptiFine, which breaks content mods. A recipe/item viewer is NOT optional and is NOT subject to the player declining the QoL set: EVERY pack that contains any crafting or machine content mod MUST include one, because without it the player literally cannot discover recipes (tech mods like Modern Industrialization, Tech Reborn, Create, AE2 are unusable without it). Always include EMI (the most broadly compatible viewer and the one tech mods explicitly recommend); JEI or REI are acceptable only if EMI has no compatible build for this version/loader. Search_mods for it and include it even if the player declined everything else. Mod names change between versions and loaders, so search_mods for the current mod that fills each role for THIS version and loader (for example the modern inventory-sorting mod is not the legacy 1.12 "Inventory Tweaks"); include only ones that actually exist and are compatible, skip any OTHER role with no good option for this version (but never skip the recipe viewer), and tell the player exactly which QoL mods you added. If the player signals weak or low-end hardware or a laptop, include the performance stack regardless and say so plainly.
- Keep seed_from_pack natural. If the player points at an existing pack or asks for something like a known pack, use seed_from_pack to ground the build on a real pack, then adapt it with search_mods rather than copying it wholesale.
- Be efficient with tools and converge decisively. In a single turn, issue several search_mods or get_mod calls together rather than one at a time. Do not search endlessly or narrate every step. Size the pack to the request: a focused pack is roughly 15 to 35 mods; a kitchen-sink or "as many as possible" request can be larger, but it is a curated set of quality mods, not a race to a number. Once you have a coherent set that covers the player's theme well, stop searching, call validate_pack, then assemble_pack. Do not keep searching to inflate the count, and do not re-assemble repeatedly chasing a bigger number; if the player asks for more after seeing a pack, add a few specific quality mods and re-assemble once. You have a hard limit on tool rounds, so converge and assemble well before you reach it; a good assembled pack now beats a perfect one that never finishes.
- Iterating is expected and good. The player may keep talking after a pack is assembled and ask for changes. Treat that as normal. For a focused change to an already-assembled pack — add a mod, remove a mod, swap one for another — call edit_pack with the ACTIVE PACK STATE instance id and only the delta (add:[{project_id}], remove:[project_id]); it pulls required deps, blocks conflicts, refuses an unsafe removal with the requiring mods named (then also remove those if the player insists), and keeps every other mod's exact version. Obey edit_pack's recoverable refusal/conflict messages exactly, then call it again. Use assemble_pack again (SAME pack name) only for a from-scratch rebuild or a large set change. Only use a different name if the player clearly wants a separate, new pack.

Voice: concise and warm. No purple prose. No em dashes. Plain sentences, short paragraphs."#;

/// Progression-phase prompt: no pack-building guidance (the pack already
/// exists), just the quest + recipe-bridge design brief. Smaller prompt + a
/// progression-only tool set is the token/error-surface win the phase machine
/// exists for. This phase exposes generate_quests (recipes are a quest-node
/// FACET of it — there is no separate recipe tool) + query_registry + get_mod.
const PROGRESSION_SYSTEM_PROMPT: &str = r#"You are Anvil, designing the PROGRESSION for an already-assembled Minecraft modpack: one graph of quests, some of which also carry custom recipes that bridge the mods. Be concise and warm; plain sentences, short paragraphs, no em dashes.

GROUNDING. Before you reference ANY concrete id (item, entity, advancement, structure, biome, tag, recipe) in a quest or recipe, call query_registry to confirm it actually exists in THIS pack's real registry, scanned from the resolved mod jars. Do not recall ids from memory and do not invent them: a fabricated id (for example a mod boss entity that mod does not actually register) is rejected by generate_quests. Query the registry first for each mod you build an arc around, then design only against the ids it returns. If query_registry reports a mod's jar is not on disk yet, its ids cannot be verified now and will be accepted only low-confidence (and flagged) — prefer ids the tool confirms.

ORIGINS. If the assembled pack runs Origins, a CUSTOM ORIGINS block appears later in this system prompt (it is present ONLY when the pack has Origins). When it is present, call generate_origins ONCE, after your first generate_quests batch, with a small bespoke set of 2 to 5 origins themed to THIS pack and the player's request — a tech pack gets an Engineer, a magic pack an Arcanist, a combat pack a Berserker; never generic filler. Follow that block's rules and SAFE power list exactly; if generate_origins returns issues, fix exactly those and call it again with the full corrected set. If that block is absent, the pack has no Origins — do not call generate_origins.

QUESTS. The quest system is Heracles (on Modrinth as "Odyssey Quests"); the pack is 1.20.1 fabric/forge with odyssey-quests + resourceful-lib already in it. Design quests to the standard of a real kitchen-sink pack (All the Mods, Create: Above and Beyond): a deep, interconnected progression web, not a flat list.

- Chapters are progression tiers/themes: a small "Getting Started" hub, then one themed questline per MAJOR mod actually in the pack, then a milestone/endgame chapter whose quests converge several mod chapters. Kitchen-sink ~6 to 10 chapters / 40 to 80 quests; smaller for focused packs but still tiered.
- Dependencies are the backbone: almost every quest has a prerequisite; use convergence, gating (a quest's reward is the next quest's needed item), and cross-chapter edges; at most one root quest per chapter; keep it a DAG.
- Per quest: an evocative title, a 1 to 3 sentence description with flavor and concrete how-to, escalating tasks (vary types: item, kill, advancement, biome, dimension, structure, recipe, occasional checkmark), and useful rewards. Invent cross-mod integration quests. Difficulty is tiered T1-T5 and ENFORCED every call. Chapter 1 holds BOTH T1 and T2 (onboarding ramp); every later chapter is T3+ (never T1/T2 again), the ceiling rising gradually to T5 by the final chapter scaled to chapter count (ch2 ~ T3, middle ramps to T4, final may use T5). No hard advancements in ch1 (adventuring_time/all-biomes T5, netherite T4, End T4+); no trivial T1/T2 task past ch1. generate_quests rejects over-hard tasks (OverdifficultForChapter) and too-trivial late tasks (UnderdifficultForChapter); retier the task or move the quest.
- Build it with multiple generate_quests calls, one to three chapters per call in progression order; calls accumulate (same chapter id replaces, new ones append; deps may reference earlier-saved quests). Hard errors (unknown ids, missing deps, cycles) are checked every call; the sparse/orphan/disconnected quality gate runs only when you pass "final": true on the last call. Only reference items/entities/advancements from mods actually in the pack. If a call returns issues, fix and call again. Lay nodes on a grid: x = progression tier, y = parallel branch, about 2.0 units apart.

RECIPE-BRIDGE NODES. Recipes are not a separate system: a quest NODE may carry a "recipes" array. Such a node becomes a quest to OBTAIN the bridged item AND injects that custom 1.20.1 recipe into an Open Loader datapack. This is what makes a kitchen-sink pack feel "expert": the questline narrates the tier gate, the embedded recipe makes the shortcut un-craftable so the player must walk the spine. Use these heavily on tech/skyblock packs and to gate a mod chapter behind another mod's output.

- The pack MUST contain open-loader (Modrinth slug open-loader; 1.20.1 fabric/forge) if ANY node has recipes; if it does not, generate_quests returns a recoverable add-open-loader message — tell the player, add it, re-assemble, retry.
- A recipe-bridge node is just a node with a "recipes" array: give it a title/lore/deps/rewards/x/y and the recipes; do NOT also give it "tasks" (the quest auto-gets an item task on the FIRST recipe's result for you), and NEVER supply a recipe "id" (it is derived and Anvil-authored).
- Every recipe must BRIDGE mods: a modded item on at least one side. Vanilla-input to modded-output is a good bridge; modded-input to vanilla-output is fine; a pure vanilla-to-vanilla recipe is rejected as an orphan. Use deps so the bridge becomes available only after the prerequisite mod's chapter, and reward/result-gate so a downstream chapter's root needs the bridged item.
- Batch exactly like quests (recipes ride inside generate_quests): hard checks (grounding to the real registry, structural validity, no duplicate derived ids) run every call; the orphan / no-modded-output quality gate runs only on the final call. Fix and call again if a call returns issues.

WORKED RECIPE-BRIDGE EXAMPLE. To gate a Thermal Series chapter behind Create progress, add this node to (say) the Thermal chapter — it is the chapter's recipe-bridge root and downstream Thermal quests depend on it:
{"id":"thermal_gate","title":"Forge the Machine Frame","description":"Create's andesite work feeds Thermal's machines. Surround a diamond with eight andesite alloy to forge your first machine frame.","x":6.0,"y":0.0,"deps":["create_andesite_alloy_quest"],"rewards":[{"type":"xp","amount":200}],"recipes":[{"type":"shaped","pattern":["AAA","ADA","AAA"],"key":{"A":{"item":"create:andesite_alloy"},"D":{"item":"minecraft:diamond"}},"result":{"item":"thermal:machine_frame","count":1}}]}
Note: 8x a Create item + 1 vanilla diamond -> a Thermal item, a 3x3 shaped recipe; no "tasks" and no recipe "id"; the node auto-becomes an "obtain thermal:machine_frame" quest gating the rest of the Thermal chapter.

CONTENT BOSS NODES. A climax, a boss fight, a "defeat the <X>" beat, or a chapter-final milestone is NEVER a manual "checkmark" and NEVER a kill task on an entity id you hope exists. It MUST be a content boss node: a quest NODE with a "content" facet of kind "boss". Anvil then provisions a REAL encounter, datapack-only, no mod: it summons a registered entity you picked via query_registry, buffs it (attributes), names it, tracks it with a bossbar, detects its death, and on death grants a unique custom-NBT token item. The node auto-becomes the quest to OBTAIN that token (an item task NBT-matched to the token — a real, auto-detected objective, not a clickable checkbox). A bare "checkmark" is ONLY for true non-mechanical roleplay text (a letter read, a vista seen), never for a boss/defeat.

- Pick the base entity with query_registry (kind:"entity") — any hostile-ish registered entity in the pack works (vanilla or modded); it does NOT need to be a "boss" entity, the provisioning makes it one.
- A content node is just a node with a "content" object: title/lore/deps/rewards/x/y/content; do NOT also give it "tasks" (the token-obtain task is added for you) and do NOT supply any ids beyond the grounded entity / equipment / token_item.
- The pack MUST contain open-loader if ANY node has a content facet (same gate as recipes); if not, generate_quests returns a recoverable add-open-loader message.
- Use it as a chapter capstone: deps point at the chapter's prep quests, and a downstream milestone can depend on this node so the token-gate walls off the endgame.

WORKED CONTENT-BOSS EXAMPLE. A Chapter-8 climax — the chapter's terminal node, with the endgame milestone depending on it:
{"id":"climax_void_sovereign","title":"Eternax, the Void Sovereign","description":"The rift tears open. Assemble the summoning altar, then end the Void Sovereign before it ends you. Tear the Void Heart from its corpse.","x":16.0,"y":0.0,"deps":["forge_void_armor","gather_rift_shards"],"rewards":[{"type":"xp","amount":1000}],"content":{"kind":"boss","entity":"minecraft:wither_skeleton","display_name":"Eternax, the Void Sovereign","attributes":{"max_health":400,"attack_damage":22,"armor":18},"equipment":{"mainhand":"minecraft:netherite_sword","helmet":"minecraft:netherite_helmet"},"bossbar_color":"purple","token_item":"minecraft:nether_star","token_name":"Void Heart","trigger":"totem"}}
Note: a buffed registered entity named "Eternax, the Void Sovereign" with a purple bossbar; killing it drops a "Void Heart" (a nether star carrying a unique token NBT); the node auto-becomes the quest to obtain the Void Heart; no "tasks", no ids beyond the grounded entity/equipment/token_item; "trigger":"totem" makes an ALTAR — the player drops a nether star plus this boss's auto-assigned offering block together to summon it (write the quest lore as assembling/activating an altar or shrine, NOT "craft an item")."#;

/// Complete-phase prompt: progression is done; light touch for tweaks.
const COMPLETE_SYSTEM_PROMPT: &str = r#"You are Anvil. This pack's progression is complete: one quest graph, some nodes also carrying custom recipe bridges or provisioned content bosses. Be concise and warm. If the player wants changes, make focused edits with generate_quests (recipes and content bosses live on quest nodes — no separate tool; a climax/boss/defeat beat is always a content boss node, never a manual checkmark; calls accumulate; pass "final": true after the last edit to re-run the full quality check). Otherwise confirm what is built and where it lives."#;

/// The system prompt for a phase. Curating/Assembled keep the full
/// pack-building prompt (it covers refinement too); Progression/Complete are
/// scoped down.
fn system_prompt_for(phase: &str) -> &'static str {
    match phase {
        "progression" => PROGRESSION_SYSTEM_PROMPT,
        "complete" => COMPLETE_SYSTEM_PROMPT,
        _ => SYSTEM_PROMPT,
    }
}

/// The visible tool set for a phase. Scoping the surface keeps the model
/// focused and makes whole classes of mistake impossible (a quest turn cannot
/// call assemble_pack, a curating turn cannot call generate_quests).
fn tool_specs_for(phase: &str) -> Value {
    let allow: &[&str] = match phase {
        // The progression surface: generate_quests (recipes are a quest-node
        // facet of it — no separate recipe tool) + get_mod + query_registry
        // (the Slice-1 grounded id/label search so design is push-grounded,
        // not post-linted).
        "progression" => {
            &["generate_quests", "generate_origins", "query_registry", "get_mod"]
        }
        "complete" => {
            &["generate_quests", "generate_origins", "query_registry", "get_mod"]
        }
        "assembled" => &[
            "propose_pack",
            "search_mods",
            "get_mod",
            "validate_pack",
            "assemble_pack",
            "edit_pack",
            "verify_pack",
            "generate_quests",
            "generate_origins",
            "query_registry",
        ],
        // curating (default): full pack-building set, no progression tools yet.
        _ => &[
            "propose_pack",
            "search_mods",
            "get_mod",
            "seed_from_pack",
            "validate_pack",
            "assemble_pack",
        ],
    };
    let all = tool_specs();
    let filtered: Vec<Value> = all
        .as_array()
        .map(|specs| {
            specs
                .iter()
                .filter(|s| {
                    s.get("name")
                        .and_then(Value::as_str)
                        .map(|n| allow.contains(&n))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    Value::Array(filtered)
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Run one assistant turn (with the internal tool loop). Returns the updated
/// history including the assistant's final message.
/// Authoritative "this conversation already has a pack" context.
///
/// The internal `messages` array (which holds the assemble_pack tool_result
/// with the instance id, and the propose_pack result with the resolved mod
/// list) is discarded when `run_turn` returns. Only the bound `ChatThread`
/// survives, and the model never sees it — so on the next turn it has no idea
/// a pack exists and re-scouts/re-assembles. This injects the persisted truth
/// back into the model's view so it targets the existing instance instead.
fn active_pack_preamble(thread_id: Option<&str>) -> Option<String> {
    let tid = thread_id?;
    let thread = crate::chat::load_thread(tid)?;

    // Assembled-instance branch: a pack already exists on disk. Unchanged.
    if let Some(inst) = thread
        .instance_id
        .as_deref()
        .and_then(|iid| load_instances().into_iter().find(|i| i.id == iid))
    {
        let iid = inst.id.clone();
        let total = inst.mods.len();
        let mut names: Vec<String> = inst
            .mods
            .iter()
            .map(|m| {
                m.name
                    .strip_suffix(".jar")
                    .unwrap_or(&m.name)
                    .to_string()
            })
            .collect();
        names.truncate(60);
        let mut mod_list = names.join(", ");
        if total > 60 {
            mod_list.push_str(&format!(", and {} more", total - 60));
        }

        return Some(format!(
            "ACTIVE PACK STATE (authoritative — this overrides anything the \
             conversation text implies; trust THIS, not your memory of earlier \
             tool calls, which you can no longer see):\n\
             A modpack is ALREADY ASSEMBLED for this conversation. Do NOT call \
             propose_pack, do NOT call assemble_pack to rebuild it, and do NOT \
             re-scout mods, unless the player explicitly asks for a different or \
             brand-new pack.\n\
             - instance_id: {iid}  (pass this EXACT id to generate_quests and \
             query_registry)\n\
             - name: {name}  (pass this EXACT name to assemble_pack ONLY if the \
             player asks to change the mod list; it updates this instance in \
             place)\n\
             - Minecraft {mc} on {loader}\n\
             - {total} mods: {mod_list}\n\
             To build or extend the questline, call generate_quests with \
             instance_id \"{iid}\" directly — the instance already exists, you \
             do NOT need to assemble anything first and you must NOT re-derive \
             the instance id by re-assembling. If the player asked for quests, \
             your next tool call should be generate_quests, not propose_pack.",
            iid = iid,
            name = inst.name,
            mc = inst.mc_version,
            loader = inst.loader,
            total = total,
            mod_list = mod_list,
        ));
    }

    // Pre-assemble branch (state fix): propose_pack ran and SAVED a fully
    // resolved candidate, but the model's memory of that set is wiped at the
    // turn boundary. Without re-injecting it the model re-scouts the whole
    // pack from scratch on the next message — the exact "type anything and it
    // rebuilds from scratch" loop. assemble_pack clears the candidate on
    // success, so this branch and the assembled branch above are mutually
    // exclusive.
    let c = crate::chat::load_candidate(tid)?;
    let total = c.mods.len();
    let mut names: Vec<String> = c
        .mods
        .iter()
        .map(|m| {
            if m.title.trim().is_empty() {
                m.project_id.clone()
            } else {
                m.title.clone()
            }
        })
        .collect();
    names.truncate(60);
    let mut mod_list = names.join(", ");
    if total > 60 {
        mod_list.push_str(&format!(", and {} more", total - 60));
    }
    Some(format!(
        "ACTIVE PACK STATE (authoritative — trust THIS, not your memory of \
         earlier tool calls, which you can no longer see):\n\
         You ALREADY proposed a pack in this conversation and the fully \
         resolved set is SAVED for this thread. Do NOT call propose_pack \
         again and do NOT re-scout mods — propose_pack is idempotent now and \
         will just hand back this same saved set. To build it, call \
         assemble_pack WITHOUT a mods list (omit `mods`; the saved set is \
         used automatically) — you only need loader_version (name/mc/loader \
         fall back to the saved proposal). For swaps or drops, use \
         search_mods/get_mod then assemble_pack with explicit {{project_id, \
         version_id}} refs. The pack resets only when the player starts a \
         brand-new chat.\n\
         - Minecraft {mc} on {loader}\n\
         - {total} proposed mods: {mod_list}\n\
         Your next tool call should be assemble_pack, not propose_pack.",
        mc = c.mc_version,
        loader = c.loader,
        total = total,
        mod_list = mod_list,
    ))
}

pub async fn run_turn(
    api_key: &str,
    model: &str,
    phase: &str,
    thread_id: Option<&str>,
    history: Vec<ChatMsg>,
    user_message: String,
    tx: UnboundedSender<CuratorEvent>,
) -> anyhow::Result<Vec<ChatMsg>> {
    let system_prompt = system_prompt_for(phase);
    // Static prompt+tools are one cached block; the per-thread ground-truth
    // preamble is a second cached block. Both use a 1h TTL so a human-paced
    // chat (minutes between turns) cache-hits across turns, not just within a
    // single round loop. The preamble is byte-stable while the pack is stable.
    let system_blocks: Vec<Value> = {
        let mut v = vec![json!({
            "type": "text",
            "text": system_prompt,
            "cache_control": { "type": "ephemeral", "ttl": "1h" }
        })];
        if let Some(pre) = active_pack_preamble(thread_id) {
            // The preamble is byte-stable while the pack is stable, so give it
            // its own 1h-durable breakpoint instead of leaving it uncached.
            v.push(json!({
                "type": "text",
                "text": pre,
                "cache_control": { "type": "ephemeral", "ttl": "1h" }
            }));
        }
        // The custom-origins catalog is GENERATED from the same Rust
        // constants the validator gates on (so prompt and gate cannot drift)
        // and is only relevant while authoring progression. Static text →
        // its own durable cache block.
        if matches!(phase, "progression" | "complete") {
            v.push(json!({
                "type": "text",
                "text": crate::origins::safe_catalog_prompt_section(),
                "cache_control": { "type": "ephemeral", "ttl": "1h" }
            }));
        }
        v
    };
    // connect_timeout only (NOT a total timeout: SSE generations are long and
    // a request timeout would kill them mid-stream). A dead connection now
    // fails loudly instead of hanging forever.
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
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

    let tools = tool_specs_for(phase);
    let mut final_text = String::new();
    let mut turn_usage = Usage::default();

    for round in 0..MAX_TOOL_ROUNDS {
        // Prompt caching. Render order is tools -> system -> messages, so the
        // cache_control breakpoints on the two system blocks cache the WHOLE
        // static prefix (tools + system + stable preamble) together. The
        // top-level cache_control auto-places a further breakpoint on the last
        // message block, so the growing conversation prefix is also read from
        // cache each tool round instead of re-billed. The 1h TTL (set above,
        // gated by the extended-cache-ttl beta header) survives the minutes
        // between human turns, so turn 2+ reads the prefix instead of
        // rewriting it. Sonnet 4.6 min cacheable prefix is 2048 tokens; our
        // system + tools schema is far larger, so it engages.
        let body = json!({
            "model": model,
            "max_tokens": MAX_TOKENS,
            "system": system_blocks,
            "messages": messages,
            "tools": tools,
            "stream": true,
            "cache_control": { "type": "ephemeral", "ttl": "1h" },
        });

        let resp = http
            .post(ANTHROPIC_URL)
            .header("x-api-key", api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            // Required for the `"ttl": "1h"` cache_control above; without it
            // the 1h TTL is rejected/ignored and writes fall back to 5m.
            .header("anthropic-beta", "extended-cache-ttl-2025-04-11")
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

        let (blocks, stop_reason, round_usage) = parse_sse_stream(resp, &tx)
            .await
            .context("reading Anthropic SSE stream")?;

        turn_usage.input_tokens += round_usage.input_tokens;
        turn_usage.cache_creation_input_tokens += round_usage.cache_creation_input_tokens;
        turn_usage.cache_read_input_tokens += round_usage.cache_read_input_tokens;
        turn_usage.output_tokens += round_usage.output_tokens;
        tracing::info!(
            round,
            input = round_usage.input_tokens,
            cache_creation = round_usage.cache_creation_input_tokens,
            cache_read = round_usage.cache_read_input_tokens,
            output = round_usage.output_tokens,
            "curator round usage"
        );
        let _ = tx.send(CuratorEvent::Usage(round_usage));

        // Accumulate the assistant text from this round for the final reply.
        let mut round_had_text = false;
        for b in &blocks {
            if let ContentBlock::Text { text } = b {
                if !text.trim().is_empty() {
                    round_had_text = true;
                }
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
            // A `max_tokens` stop means the reply (often a large tool call like
            // generate_quests) was cut off mid-stream and silently dropped, so
            // say so instead of ending as if the work were done.
            if stop_reason.as_deref() == Some("max_tokens") {
                let _ = tx.send(CuratorEvent::Text(
                    "\n\n_(My last reply hit the length limit and was cut off. \
                     Say \"continue\" and I'll pick up where I left off. Your \
                     progress is saved: if a pack was proposed it's kept for \
                     this chat (just say \"assemble it\"), and an assembled \
                     pack is remembered so I won't rebuild it — for a big \
                     questline I build it a few chapters at a time.)_"
                        .to_string(),
                ));
            }
            break;
        }

        // This round produced text and the model is continuing with tools, so
        // more text is coming. Separate the two with a blank line so back-to-
        // back assistant turns don't render as one run-on paragraph.
        if round_had_text {
            let _ = tx.send(CuratorEvent::Text("\n\n".to_string()));
            final_text.push_str("\n\n");
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
            let output = match execute_tool(&modrinth, thread_id, name, input, &tx).await {
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
                "Reached the tool limit before finishing. Your progress is \
                 saved — if I proposed a pack it's kept for this chat, so just \
                 say \"assemble it\" and I'll build that saved set without \
                 re-scouting. If a pack is already assembled, say \"continue\" \
                 and I'll keep going from there."
                    .to_string(),
            ));
            break;
        }
    }

    tracing::info!(
        input = turn_usage.input_tokens,
        cache_creation = turn_usage.cache_creation_input_tokens,
        cache_read = turn_usage.cache_read_input_tokens,
        output = turn_usage.output_tokens,
        "curator turn usage total"
    );
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
) -> anyhow::Result<(Vec<ContentBlock>, Option<String>, Usage)> {
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();

    // Blocks are keyed by their stream `index`.
    let mut builders: std::collections::BTreeMap<u64, BlockBuilder> =
        std::collections::BTreeMap::new();
    let mut order: Vec<u64> = Vec::new();
    let mut stop_reason: Option<String> = None;
    let mut usage = Usage::default();
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
                &mut usage,
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

    Ok((blocks, stop_reason, usage))
}

/// Apply one decoded SSE event to the in-progress block state.
fn handle_sse_event(
    event: Option<&str>,
    val: &Value,
    builders: &mut std::collections::BTreeMap<u64, BlockBuilder>,
    order: &mut Vec<u64>,
    stop_reason: &mut Option<String>,
    usage: &mut Usage,
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
                    // The model now streams this tool's input JSON, which for
                    // generate_quests/propose_pack can take 20-30s. Show it
                    // immediately so the UI is never silently "thinking".
                    if !name.is_empty() {
                        let _ = tx.send(CuratorEvent::Tool {
                            name: name.clone(),
                            status: "composing".to_string(),
                        });
                    }
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
            // Cumulative output token count; last write wins.
            if let Some(o) = val
                .get("usage")
                .and_then(|u| u.get("output_tokens"))
                .and_then(Value::as_u64)
            {
                usage.output_tokens = o;
            }
        }
        "message_start" => {
            // Carries input + the two cache counts (output_tokens here is
            // just the priming 1; the real total arrives via message_delta).
            if let Some(u) = val.get("message").and_then(|m| m.get("usage")) {
                let get = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                usage.input_tokens = get("input_tokens");
                usage.cache_creation_input_tokens = get("cache_creation_input_tokens");
                usage.cache_read_input_tokens = get("cache_read_input_tokens");
            }
        }
        "message_stop" | "ping" => {}
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
            "name": "propose_pack",
            "description": "PRIMARY pack-building tool. Give a one-line brief plus the target Minecraft version and loader and get back, in ONE call, a reviewed candidate pack: popular, relevant mods already pinned to compatible versions WITH their required-dependency libraries auto-resolved, plus any unresolved issues. Prefer this over manual search_mods/get_mod loops. After it returns, summarise the highlights for the player, take swap/drop requests in chat (use search_mods for specific swaps), then call assemble_pack with the {project_id, version_id} pairs.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "brief": { "type": "string", "description": "One line of intent, e.g. 'cozy create-based automation with farming and decoration'." },
                    "mc_version": { "type": "string", "description": "Target Minecraft version, e.g. '1.20.1'." },
                    "loader": { "type": "string", "description": "fabric, forge, neoforge, or quilt." },
                    "count": { "type": "integer", "description": "Approx how many headline mods to propose before dependency resolution (8-45, default 28)." }
                },
                "required": ["brief", "mc_version", "loader"]
            }
        },
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
            "description": "Assemble the final pack: creates a local instance and writes a standard .mrpack. Runs validation first and refuses to assemble an invalid pack. Only call this after validate_pack returned an empty issue list. MERGE SEMANTICS: if a pack with this name already exists, `mods` is MERGED into it (existing mods kept; a mod you list with a new version_id updates that one). So to ADD a mod to an existing pack, pass ONLY the new mod — do NOT re-send the whole list and do NOT fear overwriting. You CANNOT drop a mod by omitting it; removing mods or a clean rebuild requires `replace: true` with the full intended list.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Human-readable pack name." },
                    "mc_version": { "type": "string" },
                    "loader": { "type": "string", "description": "fabric, forge, neoforge, or quilt." },
                    "loader_version": { "type": "string", "description": "Always use \"latest\". Anvil installs the newest stable loader for the MC version; never pin an old loader version." },
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
                    },
                    "replace": { "type": "boolean", "description": "Default false (merge into an existing same-named pack). Set true ONLY for a deliberate rebuild-from-scratch or to remove mods: the instance's mod list becomes EXACTLY `mods` (everything not listed is dropped)." }
                },
                "required": ["name", "mc_version", "loader", "loader_version", "mods"]
            }
        },
        {
            "name": "verify_pack",
            "description": "Boot the assembled pack ONCE, headless (~1 minute), to confirm every mod actually initializes — this catches launch-breaking problems that no metadata reveals (an API-incompatible major, a runtime mixin failure). On success it reports the pack is confirmed working. On failure it returns an automated crash analysis naming the culprit mod and the recommended fix. Call this AFTER assemble_pack succeeds and the pack looks final, BEFORE you tell the player it is done; do NOT call it after every small tweak. If it reports a failure: relay the analysis in plain words and ASK the player before changing anything (surface and wait). If they approve a fix, call assemble_pack again with the SAME name minus the culprit mod, then call verify_pack ONE more time — at most once, never loop.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "instance_id": { "type": "string", "description": "The id of the assembled instance, as returned by assemble_pack." }
                },
                "required": ["instance_id"]
            }
        },
        {
            "name": "seed_from_pack",
            "description": "Ground the build on a real existing modpack. Searches Modrinth modpacks for the query, takes the top hit, and reads its newest .mrpack manifest. Returns the pack name, Minecraft version, loader, loader version, mod count, and a capped list of its mods (name and download url). Use this when the player references an existing pack or wants something like a known pack, then adapt with search_mods. The returned mods are a starting point, not a final pack: still resolve real Modrinth ids with search_mods and get_mod, and still validate before assembling.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Modpack search terms, e.g. 'better minecraft' or 'create above and beyond'." }
                },
                "required": ["query"]
            }
        },
        {
            "name": "generate_quests",
            "description": "Append a batch of progression chapters to an assembled instance (Heracles / Odyssey Quests, config/heracles/quests JSON). The instance should be Minecraft 1.20.1 fabric/forge and contain odyssey-quests + resourceful-lib. A node may ALSO carry a 'recipes' facet (custom 1.20.1 recipes that bridge the pinned mods: such a node becomes a quest to obtain the bridged item) OR a 'content' facet (a provisioned boss: a real summoned, named, bossbar boss built from a registered entity that drops a unique quest token — such a node becomes the encounter quest, auto-tasked to obtain that token). Both facets inject an Open Loader datapack (the pack MUST contain open-loader if ANY node has recipes OR content; if not, this returns a recoverable add-open-loader message). There is no separate recipe/content tool — recipes and bosses ARE quest nodes. A climax / boss / 'defeat the <X>' / chapter-final milestone MUST be a content boss node, NOT a manual checkmark. Build across SEVERAL calls — roughly one to three chapters per call, in progression order — rather than one massive call (a large graph will not fit in one reply). Calls accumulate: a chapter id that already exists is replaced, new chapters are appended, dependencies may reference quests saved by earlier calls. Every call enforces hard correctness: a DAG (deps form no cycles and every dep resolves), tasks/rewards referencing only ids real in the pack, per-recipe grounding + structural validity + no duplicate derived ids, and content TOKEN ATOMICITY (a content boss's base entity/equipment/token grounded, and the full atomic boss set emittable — checked every call). The quality gate (no orphan quests, every chapter wired in, deep All-the-Mods-style web; no orphan recipes; the datapack produces at least one modded output) runs ONLY when you pass \"final\": true on the last call. On failure nothing in that call is written and the issues are returned; fix and call again.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "instance_id": { "type": "string", "description": "The id of the assembled instance, as returned by assemble_pack." },
                    "final": { "type": "boolean", "description": "False (default) for an intermediate batch of chapters. Set true ONLY on the last call, once every chapter is submitted, to run the full quality/interconnection check on the whole accumulated questline." },
                    "graph": {
                        "type": "object",
                        "description": "The quest graph.",
                        "properties": {
                            "title": { "type": "string" },
                            "chapters": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string" },
                                        "title": { "type": "string" },
                                        "quests": {
                                            "type": "array",
                                            "items": {
                                                "type": "object",
                                                "properties": {
                                                    "id": { "type": "string" },
                                                    "title": { "type": "string" },
                                                    "description": { "type": "string" },
                                                    "x": { "type": "number", "description": "Grid x; increases with progression tier." },
                                                    "y": { "type": "number", "description": "Grid y; separates parallel branches. Spacing about 2.0 units." },
                                                    "deps": {
                                                        "type": "array",
                                                        "items": { "type": "string" },
                                                        "description": "Ids of prerequisite quests. Must form a DAG."
                                                    },
                                                    "tasks": {
                                                        "type": "array",
                                                        "items": {
                                                            "type": "object",
                                                            "description": "One task. Shapes: type:'item' {id,count} (have/obtain an item); type:'gather_item' {item,count,nbt?} (collect, nbt optional SNBT discriminator); type:'kill' {entity_type,count,nbt?} (defeat an entity; entity_type is an entity id or #tag; nbt optional SNBT discriminator — do NOT invent nbt, omit it unless you actually know the discriminator); type:'advancement' {id}; type:'biome' {biome}; type:'dimension' {dimension}; type:'structure' {structure}; type:'recipe' {recipe}; type:'composite' {tasks:[...nested tasks]}; type:'stat' {stat,target}; type:'location' {dimension?,biome?,structure?} (be-in a place); type:'checkmark' {} (pure-flavor beat — VERIFIED against Heracles source: a checkmark AUTO-COMPLETES the instant the quest is reached and gates nothing, it is NOT a player action; use it only for non-mechanical lore, never as a gate or to represent something the player must do). Vary types across the pack; do not make every task a collect-item.",
                                                            "properties": {
                                                                "type": { "type": "string", "enum": ["item", "gather_item", "kill", "advancement", "biome", "dimension", "structure", "recipe", "composite", "stat", "location", "checkmark"] },
                                                                "id": { "type": "string" },
                                                                "item": { "type": "string", "description": "Item id or #tag for gather_item, e.g. minecraft:diamond." },
                                                                "entity_type": { "type": "string", "description": "Entity id or #tag for kill, e.g. minecraft:zombie." },
                                                                "nbt": { "type": "string", "description": "Optional SNBT compound discriminator for kill/gather_item. Only set when you genuinely know the mod-internal discriminator; never invent one." },
                                                                "biome": { "type": "string", "description": "Biome id, e.g. minecraft:soul_sand_valley (also used by location)." },
                                                                "dimension": { "type": "string", "description": "Dimension id, e.g. minecraft:the_nether (also used by location)." },
                                                                "structure": { "type": "string", "description": "Structure id, e.g. minecraft:fortress (also used by location)." },
                                                                "recipe": { "type": "string", "description": "Recipe id, e.g. create:crushing/andesite." },
                                                                "stat": { "type": "string", "description": "Statistic id for type:'stat', e.g. minecraft:jump." },
                                                                "target": { "type": "integer", "description": "Threshold for type:'stat'." },
                                                                "tasks": { "type": "array", "description": "Nested tasks for type:'composite' (all must complete)." },
                                                                "count": { "type": "integer" }
                                                            },
                                                            "required": ["type"]
                                                        }
                                                    },
                                                    "rewards": {
                                                        "type": "array",
                                                        "items": {
                                                            "type": "object",
                                                            "description": "type:'item' {id,count}; type:'xp' {amount}; type:'command' {command}.",
                                                            "properties": {
                                                                "type": { "type": "string", "enum": ["item", "xp", "command"] },
                                                                "id": { "type": "string" },
                                                                "count": { "type": "integer" },
                                                                "amount": { "type": "integer" },
                                                                "command": { "type": "string" }
                                                            },
                                                            "required": ["type"]
                                                        }
                                                    },
                                                    "recipes": {
                                                        "type": "array",
                                                        "description": "OPTIONAL recipe facet. Custom 1.20.1 recipes this node injects into the Open Loader datapack (the pack MUST contain open-loader; if not, this tool returns a recoverable add-open-loader message). A node with a non-empty recipes array IS a recipe-bridge quest: it ALWAYS auto-surfaces a Heracles quest with an 'item' task on the FIRST recipe's result (do NOT also supply 'tasks' on a recipe node — only title/lore/deps/rewards/x/y/recipes; the item-on-result task is added for you). NEVER supply a recipe 'id' — the datapack id is DERIVED deterministically from the node and is Anvil-authored. Every recipe should BRIDGE mods: a modded item on at least one side (a pure vanilla-to-vanilla recipe is rejected as an orphan; vanilla-input to modded-output bridges are good). Hard checks (grounding to the real registry, structural validity, no duplicate derived ids) run every call; the orphan / no-modded-output quality gate runs only on the final call.",
                                                        "items": {
                                                            "type": "object",
                                                            "description": "One recipe. Shapes by type: 'shaped' {pattern (1-3 equal-length rows of <=3 chars; space = empty), key (char -> {item:'ns:id'} or {tag:'ns:id'}), result {item:'ns:id', count}}; 'shapeless' {ingredients (1-9 of {item}|{tag}), result {item, count}}; 'smelting' {ingredient ({item}|{tag}), result 'ns:id' (a plain STRING, not an object), experience (number), cookingtime (integer ticks, default 200)}. Ids default to the minecraft namespace if unprefixed. Do NOT include an 'id' field — it is derived.",
                                                            "properties": {
                                                                "type": { "type": "string", "enum": ["shaped", "shapeless", "smelting"] },
                                                                "pattern": {
                                                                    "type": "array",
                                                                    "items": { "type": "string" },
                                                                    "description": "Shaped only. 1-3 rows, each the same length and at most 3 chars. A space means an empty slot."
                                                                },
                                                                "key": {
                                                                    "type": "object",
                                                                    "description": "Shaped only. Maps each non-space pattern char to an ingredient object {\"item\":\"ns:id\"} or {\"tag\":\"ns:id\"}."
                                                                },
                                                                "ingredients": {
                                                                    "type": "array",
                                                                    "items": { "type": "object" },
                                                                    "description": "Shapeless only. 1 to 9 ingredient objects {\"item\":\"ns:id\"} or {\"tag\":\"ns:id\"}."
                                                                },
                                                                "ingredient": {
                                                                    "type": "object",
                                                                    "description": "Smelting only. One ingredient object {\"item\":\"ns:id\"} or {\"tag\":\"ns:id\"}."
                                                                },
                                                                "result": {
                                                                    "description": "Shaped/shapeless: an object {\"item\":\"ns:id\",\"count\":N}. Smelting: a plain namespaced item STRING (1.20.1 form, not an object)."
                                                                },
                                                                "experience": { "type": "number", "description": "Smelting only. XP granted, e.g. 0.7." },
                                                                "cookingtime": { "type": "integer", "description": "Smelting only. Ticks to cook; default 200." }
                                                            },
                                                            "required": ["type"]
                                                        }
                                                    },
                                                    "content": {
                                                        "type": "object",
                                                        "description": "OPTIONAL content facet. Makes this node a PROVISIONED BOSS: a real, summoned, named, bossbar-tracked boss built from a registered entity you picked via query_registry, buffed via attributes, that on death grants a unique quest token. A node with a content facet IS the encounter quest: it ALWAYS auto-surfaces a Heracles quest whose task is to obtain that token (an item task NBT-matched to the boss's unique token — auto-detected, not a manual checkbox). Do NOT also supply 'tasks' on a content node (only title/lore/deps/rewards/x/y/content; the token task is added for you). NO ids are derived/supplied beyond the grounded base entity / equipment / token_item — the summon function, bossbar, kill-detection advancement, reward function and trigger are all Anvil-authored. The pack MUST contain open-loader (same gate as recipes). USE THIS for every climax / boss / 'defeat the <X>' / chapter-final milestone — such a beat MUST be a content boss, NEVER a manual 'checkmark'. A bare 'checkmark' is only for true non-mechanical roleplay text.",
                                                        "properties": {
                                                            "kind": { "type": "string", "enum": ["boss"], "description": "v1 supports 'boss'." },
                                                            "entity": { "type": "string", "description": "The base REGISTERED entity to summon (confirm via query_registry; e.g. minecraft:wither_skeleton). A fabricated id is rejected." },
                                                            "display_name": { "type": "string", "description": "The boss's in-game name (CustomName + bossbar title), e.g. 'Eternax, the Void Sovereign'." },
                                                            "attributes": {
                                                                "type": "object",
                                                                "description": "Optional attribute buffs; sane boss defaults applied for any omitted (max_health 200, attack_damage 12, armor 10, knockback_resistance 0.6, movement_speed 0.28, follow_range 40).",
                                                                "properties": {
                                                                    "max_health": { "type": "number" },
                                                                    "attack_damage": { "type": "number" },
                                                                    "armor": { "type": "number" },
                                                                    "knockback_resistance": { "type": "number" },
                                                                    "movement_speed": { "type": "number" },
                                                                    "follow_range": { "type": "number" }
                                                                }
                                                            },
                                                            "equipment": {
                                                                "type": "object",
                                                                "description": "Optional per-slot equipment item ids (grounded; no enchantments in v1).",
                                                                "properties": {
                                                                    "mainhand": { "type": "string" },
                                                                    "helmet": { "type": "string" },
                                                                    "chestplate": { "type": "string" },
                                                                    "leggings": { "type": "string" },
                                                                    "boots": { "type": "string" }
                                                                }
                                                            },
                                                            "bossbar_color": { "type": "string", "description": "red|blue|green|yellow|pink|purple|white (default red)." },
                                                            "token_item": { "type": "string", "description": "The vanilla item used as the unique token carrier (default minecraft:nether_star). Grounded." },
                                                            "token_name": { "type": "string", "description": "The token's display name (default '<display_name> Token'), e.g. 'Void Heart'." },
                                                            "trigger": { "type": "string", "enum": ["totem", "command"], "description": "How the boss is summoned. 'totem' (default, recommended) = an ALTAR: the player drops a nether star + this boss's auto-assigned offering block together (write lore as an altar/shrine ritual, not crafting an item); 'command' = a /trigger command. Do not use 'region' in v1." }
                                                        },
                                                        "required": ["kind", "entity", "display_name"]
                                                    }
                                                },
                                                "required": ["id", "title", "x", "y"]
                                            }
                                        }
                                    },
                                    "required": ["id", "title", "quests"]
                                }
                            }
                        },
                        "required": ["title", "chapters"]
                    }
                },
                "required": ["instance_id", "graph"]
            }
        },
        {
            "name": "generate_origins",
            "description": "Author the pack's CUSTOM ORIGINS (Origins/Apoli datapack) — only if the assembled instance runs Origins core + Open Loader. Design 2-5 origins themed to THIS pack and the player's request (a tech pack gets an Engineer, a magic pack an Arcanist, etc.). This is a SINGLE authored set: calling again REPLACES the whole set (it does NOT accumulate like generate_quests). The proposal is hard-validated against the in-code Apoli catalog before anything is written; on failure NOTHING is written and a structured list of {kind, where, why, hint} is returned — fix exactly those and call again. Follow the CUSTOM ORIGINS rules and the SAFE power-type list in the system prompt verbatim: name/description are plain strings, impact is an integer 0-3, powers are either ids you define here or shipped origins:<id> references (never redefine a shipped power).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "instance_id": { "type": "string", "description": "The assembled instance id from assemble_pack." },
                    "origins": {
                        "type": "object",
                        "description": "The full authored origin set.",
                        "properties": {
                            "origins": {
                                "type": "array",
                                "description": "2-5 origins.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "description": "lowercase [a-z0-9_.-]+ (becomes the file name)." },
                                        "name": { "type": "string", "description": "PLAIN string display name (never an object)." },
                                        "description": { "type": "string", "description": "PLAIN string." },
                                        "powers": {
                                            "type": "array",
                                            "items": { "type": "string" },
                                            "description": "Each entry is EITHER a power id you define in `powers` below, OR a shipped `origins:<id>` reference."
                                        },
                                        "icon": { "type": "string", "description": "Real vanilla item id, namespace:path, e.g. minecraft:netherite_chestplate." },
                                        "impact": { "type": "integer", "enum": [0, 1, 2, 3], "description": "0 none, 1 low, 2 medium, 3 high." },
                                        "order": { "type": "integer", "description": "Display order; distinct per origin." }
                                    },
                                    "required": ["id", "name", "description", "powers", "icon", "impact", "order"]
                                }
                            },
                            "powers": {
                                "type": "array",
                                "description": "Every power your origins define locally (shipped origins:<id> refs do NOT go here).",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "id": { "type": "string", "description": "lowercase [a-z0-9_.-]+." },
                                        "name": { "type": "string", "description": "PLAIN string." },
                                        "description": { "type": "string", "description": "PLAIN string." },
                                        "type": { "type": "string", "description": "A SAFE power-type id from the system-prompt list (apoli:<id>)." },
                                        "body": { "type": "object", "description": "Type-specific fields per the SAFE list (e.g. apoli:attribute -> {\"modifier\":{...}}; apoli:modify_falling -> {\"velocity\":n,\"take_fall_damage\":false}). Omit/empty for self-contained types." }
                                    },
                                    "required": ["id", "name", "description", "type"]
                                }
                            }
                        },
                        "required": ["origins", "powers"]
                    }
                },
                "required": ["instance_id", "origins"]
            }
        },
        {
            "name": "edit_pack",
            "description": "Safely ADD and/or REMOVE mods on an ALREADY-ASSEMBLED instance (post-assemble refinement: 'add JEI', 'remove sodium', 'swap X for Y'). Prefer this over assemble_pack for a single change — it re-resolves only what is needed and keeps every other mod's exact pinned version. Adds are dependency-complete (required deps are pulled in automatically) and conflict-checked; removes are reverse-dependency safe (a mod still required by a kept mod is REFUSED with the requiring set, never silently broken), and deps that only existed for a removed mod are auto-pruned. On refusal/conflict NOTHING is written and a recoverable explanation is returned — fix exactly that (e.g. also remove the requiring mods, or pick a compatible version) and call again. Do not hand-list the whole pack; pass only the delta. Not for imported (.mrpack) packs — those carry no Modrinth project ids, so there is nothing to resolve against; tell the player to rebuild it as a curated pack instead.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "instance_id": { "type": "string", "description": "The assembled instance id from assemble_pack." },
                    "add": {
                        "type": "array",
                        "description": "Mods to add (their required dependencies are pulled in for you). Omit or [] to only remove.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "project_id": { "type": "string", "description": "Modrinth project id (or slug) to add." },
                                "version_id": { "type": "string", "description": "OPTIONAL exact Modrinth version id. Omit to let the resolver pick the best version compatible with this pack's Minecraft version + loader." }
                            },
                            "required": ["project_id"]
                        }
                    },
                    "remove": {
                        "type": "array",
                        "description": "Modrinth project ids to remove. A mod still required by a kept mod is refused (also remove the requiring mods to force it). Omit or [] to only add.",
                        "items": { "type": "string" }
                    }
                },
                "required": ["instance_id"]
            }
        },
        {
            "name": "query_registry",
            "description": "Search the assembled pack's REAL registry (scanned offline from the resolved mod jars) for concrete ids to design against. Use this BEFORE referencing any item/entity/advancement/structure/biome/tag/recipe id in a quest or recipe, instead of recalling ids from memory: a fabricated id (e.g. an entity a mod does not actually have) is rejected by generate_quests, so query first and design only against ids this returns. Returns up to 50 matches as {id, label, source_mod}; pass an offset to page. If a mod's jar is not on disk yet its ids will not appear here (they are accepted low-confidence at generation time and flagged) — prefer ids this tool confirms.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "instance_id": { "type": "string", "description": "The id of the assembled instance, as returned by assemble_pack." },
                    "kind": {
                        "type": "string",
                        "enum": ["item", "entity", "advancement", "structure", "biome", "tag", "recipe"],
                        "description": "Which registry to search."
                    },
                    "filter": {
                        "type": "object",
                        "description": "Optional narrowing. All fields optional; combine freely.",
                        "properties": {
                            "namespace": { "type": "string", "description": "Only ids in this namespace, e.g. 'create'." },
                            "contains": { "type": "string", "description": "Only ids (or labels) containing this substring, case-insensitive." },
                            "mod": { "type": "string", "description": "Only ids contributed by this mod (by mod id or display name)." }
                        }
                    },
                    "offset": { "type": "integer", "description": "Pagination offset; results are capped at 50 per call. Default 0." }
                },
                "required": ["instance_id", "kind"]
            }
        }
    ])
}

// ---------------------------------------------------------------------------
// Tool dispatch + implementations
// ---------------------------------------------------------------------------

async fn execute_tool(
    mr: &Modrinth,
    thread_id: Option<&str>,
    name: &str,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    // Fabric-only (v1): refuse Forge/NeoForge at the tool boundary. The
    // validator's jar-in-jar / builtin handling is verified for Fabric only;
    // letting the curator build Forge packs produces false-positive validation
    // it cannot get past. Hard server-side floor, not a prompt suggestion.
    if let Some(l) = input.get("loader").and_then(Value::as_str) {
        let l = l.to_lowercase();
        if l == "forge" || l == "neoforge" {
            return Ok(format!(
                "BLOCKED: Anvil v1 supports Fabric only — {l} is not \
                 supported yet. Do NOT retry with {l}. Tell the player Anvil \
                 currently builds Fabric packs only (1.20.1 Fabric is the \
                 fully verified path, quests included) and rebuild this as a \
                 Fabric pack."
            ));
        }
    }
    // Funnel gate: manual search_mods/get_mod is disabled until propose_pack
    // has been attempted in this thread. This forces the reliable path so a
    // recoverable candidate always exists — killing the "type anything and it
    // re-scouts the whole pack from scratch" loop. A propose_pack miss still
    // unlocks these as the honest fallback (it marks-proposed before failing).
    if matches!(name, "search_mods" | "get_mod") {
        if let Some(tid) = thread_id {
            if !crate::chat::proposed(tid) {
                return Ok(
                    "BLOCKED: call propose_pack FIRST. search_mods/get_mod is \
                     disabled until propose_pack has run in this conversation \
                     (it gives you a reviewed, dependency-resolved candidate \
                     that is saved so the pack survives across turns). Call \
                     propose_pack now with the brief + Minecraft version + \
                     loader; after it returns you may use search_mods/get_mod \
                     for specific swaps."
                        .to_string(),
                );
            }
        }
    }
    match name {
        "propose_pack" => tool_propose_pack(mr, thread_id, input, tx).await,
        "search_mods" => tool_search_mods(mr, input, tx).await,
        "get_mod" => tool_get_mod(mr, input, tx).await,
        "validate_pack" => tool_validate_pack(mr, input, tx).await,
        "assemble_pack" => tool_assemble_pack(mr, thread_id, input, tx).await,
        "edit_pack" => tool_edit_pack(mr, thread_id, input, tx).await,
        "seed_from_pack" => tool_seed_from_pack(mr, input, tx).await,
        "generate_quests" => tool_generate_quests(thread_id, input, tx).await,
        "generate_origins" => tool_generate_origins(thread_id, input, tx).await,
        "query_registry" => tool_query_registry(input, tx).await,
        "verify_pack" => tool_verify_pack(input, tx).await,
        other => Err(anyhow!("unknown tool: {other}")),
    }
}

/// Clip an over-long mod blurb so search/get_mod results do not bloat the
/// (cached, but still billed once) conversation history.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let clipped: String = s.chars().take(max).collect();
        format!("{clipped}...")
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

/// Run `fut`, emitting a heartbeat chip every few seconds while it is still
/// pending. Long tool work (resolving 100+ Modrinth dependency chains) emits
/// nothing on its own, so without this the UI's silence watchdog wrongly
/// reports the curator as dead. Cancellation-safe (the sleep is dropped).
async fn pump<F, T>(
    fut: F,
    tx: &UnboundedSender<CuratorEvent>,
    tool: &str,
) -> T
where
    F: std::future::Future<Output = T>,
{
    use std::time::Duration;
    tokio::pin!(fut);
    loop {
        tokio::select! {
            out = &mut fut => return out,
            _ = tokio::time::sleep(Duration::from_secs(7)) => {
                tool_chip(tx, tool, "resolving dependencies");
            }
        }
    }
}

/// Pre-warm the pinned mod jars into `<instance>/mods/` so registry grounding
/// (`scan_instance`) has real jars to read at curation/quest time, not just
/// post-launch. Without this every mod degrades to `unscanned`, the vocab is
/// empty, and the model falls back to fabricated ids — the exact failure
/// grounding exists to prevent.
///
/// - Reuses the launcher's `ensure_mod` (sha512-verified, delete-on-mismatch),
///   so a jar landing here also warms the launch cache — same dir, same path,
///   same verify (no double download at launch).
/// - Bounded concurrency (a pack can be 80+ jars).
/// - Resilient: a failed/again-missing jar is logged and SKIPPED — never
///   returns Err, never blocks. That mod just stays `unscanned` (existing
///   `scan_instance` behavior); grounding degrades, it does not fail.
/// - Heartbeat: emits a chip before starting and a periodic progress chip
///   while downloads are in flight, so the UI silence watchdog does not
///   false-fire on a multi-minute fetch (mirrors `pump`'s select-on-sleep).
/// - Cache correctness: `anvil-registry.json` is keyed only by the pinned mod
///   SET (`mod_set_key`), which is blind to whether the jars are on disk. A
///   cache written by an earlier all-jars-absent grounding run has the SAME
///   key yet an EMPTY vocab; left in place it would be reused and grounding
///   would stay broken forever. So once jars are present we bust any such
///   stale empty-vocab cache, forcing exactly one rescan. A populated cache is
///   never busted (repeat grounding stays instant — idempotent).
pub(crate) async fn ensure_mod_jars(
    client: &reqwest::Client,
    inst: &Instance,
    instance_dir: &Path,
    tx: &UnboundedSender<CuratorEvent>,
    tool: &str,
) {
    // Only jars actually missing or the wrong size need a fetch. (ensure_mod
    // re-verifies sha512 anyway, but checking size first avoids reading every
    // already-present jar off disk on the cheap idempotent repeat call.)
    let needed: Vec<&PinnedMod> = inst
        .mods
        .iter()
        .filter(|m| {
            let dest = instance_dir.join(&m.path);
            match std::fs::metadata(&dest) {
                Ok(meta) => {
                    !meta.is_file()
                        || (m.file_size > 0 && meta.len() != m.file_size)
                }
                Err(_) => true,
            }
        })
        .collect();

    let total = inst.mods.len();
    if !needed.is_empty() {
        // mods dir = exactly where the launcher downloads to (inst/mods/...).
        if let Some(mods_dir) = needed
            .first()
            .map(|m| instance_dir.join(&m.path))
            .and_then(|p| p.parent().map(Path::to_path_buf))
        {
            let _ = tokio::fs::create_dir_all(&mods_dir).await;
        }

        tool_chip(
            tx,
            tool,
            &format!("fetching {} mod(s) for grounding", needed.len()),
        );

        // Bounded concurrency in chunks of 8 with a per-chunk progress chip.
        // Every capture is OWNED (cloned client/PinnedMod/PathBuf) so each
        // future is 'static + Send: a borrowed `&PinnedMod`/`&Client` driven
        // through `buffer_unordered` + `tokio::select!` trips rustc's HRTB
        // "Send is not general enough", so we use owned `join_all` chunks and
        // a chip per chunk (re-arms the UI watchdog) instead of select!.
        let want = needed.len();
        let jobs: Vec<(PinnedMod, std::path::PathBuf)> = needed
            .into_iter()
            .map(|m| (m.clone(), instance_dir.join(&m.path)))
            .collect();
        let mut done = 0usize;
        for chunk in jobs.chunks(8) {
            let futs = chunk.iter().map(|(m, dest)| {
                let client = client.clone();
                let m = m.clone();
                let dest = dest.clone();
                async move {
                    // Resilient: a failed jar is skipped, never propagated.
                    // That mod stays `unscanned` via existing scan behavior.
                    if let Err(e) =
                        crate::launch::ensure_mod(&client, &m, &dest).await
                    {
                        eprintln!(
                            "ensure_mod_jars: skipping {} (grounding will treat \
                             it as unscanned): {e:#}",
                            m.name
                        );
                    }
                }
            });
            futures_util::future::join_all(futs).await;
            done += chunk.len();
            tool_chip(tx, tool, &format!("downloading mods {done}/{want}"));
        }
    }

    // K = jars actually on disk now (some `needed` ones may have failed and
    // been skipped — that's fine, they stay unscanned).
    let present = inst
        .mods
        .iter()
        .filter(|m| {
            std::fs::metadata(instance_dir.join(&m.path))
                .map(|meta| meta.is_file() && meta.len() > 0)
                .unwrap_or(false)
        })
        .count();

    // Cache-rescan fix: bust a stale all-jars-absent cache (matching mod-set
    // key, EMPTY vocab) now that jars exist, so the next
    // `build_index_for_instance` rescans instead of reusing the empty vocab.
    // Independent of whether THIS call fetched anything, so it also self-heals
    // a cache left by an earlier curator run while jars were warmed by launch.
    // A populated cache is left intact (idempotent — repeat grounding instant).
    if present > 0 {
        let cache_path = instance_dir.join("anvil-registry.json");
        let stale = std::fs::read_to_string(&cache_path)
            .ok()
            .and_then(|txt| {
                serde_json::from_str::<crate::registry::ScanResult>(&txt).ok()
            })
            .map(|c| {
                c.mod_set_key == crate::registry::mod_set_key(inst)
                    && c.vocab.is_empty()
            })
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(&cache_path);
        }
    }

    if total > 0 {
        tool_chip(
            tx,
            "query_registry",
            &format!("grounded against {present}/{total} mod jar(s)"),
        );
    }
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
                "description": truncate(&h.description, 160),
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
        "description": truncate(&project.description, 280),
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
#[derive(Debug, Deserialize, PartialEq)]
pub(crate) struct ModRef {
    project_id: String,
    version_id: String,
}

/// Decide which mod set `assemble_pack` builds. Explicit refs always win (the
/// player asked for swaps/drops); empty or omitted refs fall back to the saved
/// proposal — the recovery path for when a turn boundary / tool-round limit
/// wiped the list the model was told to pass. Returns `(refs, used_saved)`.
fn assemble_refs(
    explicit: Vec<ModRef>,
    saved: Option<&crate::chat::CandidatePack>,
) -> (Vec<ModRef>, bool) {
    if !explicit.is_empty() {
        return (explicit, false);
    }
    let recovered = saved
        .map(|c| {
            c.mods
                .iter()
                .map(|m| ModRef {
                    project_id: m.project_id.clone(),
                    version_id: m.version_id.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    (recovered, true)
}

/// Merge-not-replace for a same-named re-assemble. `assemble_pack` used to set
/// the instance's roots to exactly the call's refs, so the model calling it to
/// "add Open Loader" with a one-mod list silently overwrote a 50-mod pack down
/// to one (the Starbound Origins failure: pin set collapsed -> registry empty
/// -> hollow vanilla quests). Now the existing instance's mods are roots too;
/// the call's refs come FIRST so they win a `project_id` collision (a
/// deliberate version bump / swap still applies). `replace == true` opts back
/// into full-replace for an intentional rebuild-from-scratch (and is the only
/// way to DROP a mod via assemble). Empty-`project_id` entries (imported-pack
/// jars) are dropped — `resolve_pack` cannot Modrinth-resolve them anyway.
/// Pure + deterministic so it is unit-tested directly.
fn merge_roots<I: IntoIterator<Item = ModRef>>(
    call_refs: Vec<ModRef>,
    existing: I,
    replace: bool,
) -> Vec<ModRef> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let tail: Vec<ModRef> = if replace {
        Vec::new()
    } else {
        existing.into_iter().collect()
    };
    for r in call_refs.into_iter().chain(tail) {
        if !r.project_id.is_empty() && seen.insert(r.project_id.clone()) {
            out.push(r);
        }
    }
    out
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

// ---------------------------------------------------------------------------
// Transitive dependency resolution driver
// ---------------------------------------------------------------------------
//
// `pack::resolve_dependencies` is pure: it walks the required-dependency graph
// over Modrinth version data it is *given*. This async driver is the part that
// actually talks to Modrinth — it fetches a project's versions on demand,
// caches every fetch (and negative-caches failures so a transient 404 doesn't
// retry forever or abort the pack), and re-runs the pure resolver until the
// closure stops growing. The result is the full transitive pin set plus any
// dependency-level validation issues (unresolved required / incompatible
// present) the pure pass surfaced.

/// Per-resolution cache of `project_id -> versions` (Ok) or the lookup error
/// string (Err, negative-cached). One pass over an 80-mod pack thus issues at
/// most one `project` + one `versions` request per distinct project.
type VersionCache = std::collections::HashMap<String, std::result::Result<Vec<Version>, String>>;

/// Side-support (`client_side`/`server_side`) cache, same dedupe rationale.
type SideCache = std::collections::HashMap<String, (String, String)>;

/// Fetch `project_id`'s versions, memoized. The `Err` arm is cached too so a
/// failing dependency degrades to one reported issue, not a refetch storm.
async fn cached_versions<'a>(
    mr: &Modrinth,
    cache: &'a mut VersionCache,
    project_id: &str,
) -> std::result::Result<&'a Vec<Version>, String> {
    if !cache.contains_key(project_id) {
        let got = mr
            .versions(project_id)
            .await
            .map_err(|e| format!("{e}"));
        cache.insert(project_id.to_string(), got);
    }
    match cache.get(project_id).expect("just inserted") {
        Ok(v) => Ok(v),
        Err(e) => Err(e.clone()),
    }
}

/// Project side-support, memoized. A lookup failure defaults to the Modrinth
/// norm ("required"/"required") so a flaky project page never blocks a pin;
/// the version's own mc/loader data still governs compatibility.
async fn cached_sides(
    mr: &Modrinth,
    cache: &mut SideCache,
    project_id: &str,
) -> (String, String) {
    if let Some(s) = cache.get(project_id) {
        return s.clone();
    }
    let sides = match mr.project(project_id).await {
        Ok(p) => (p.client_side, p.server_side),
        Err(_) => ("required".to_string(), "required".to_string()),
    };
    cache.insert(project_id.to_string(), sides.clone());
    sides
}

fn pick_file(v: &Version) -> Option<&crate::modrinth::VersionFile> {
    if v.files.is_empty() {
        return None;
    }
    Some(v.files.iter().find(|f| f.primary).unwrap_or(&v.files[0]))
}

/// Map a Modrinth `Version` (+ resolved side support) into the pure resolver's
/// `pack::ResolvedVersion` view. `None` if the version has no downloadable
/// file (it cannot be pinned into a `.mrpack`).
fn to_resolved(
    v: &Version,
    client_side: &str,
    server_side: &str,
) -> Option<pack::ResolvedVersion> {
    let file = pick_file(v)?;
    Some(pack::ResolvedVersion {
        project_id: v.project_id.clone(),
        version_id: v.id.clone(),
        path: format!("mods/{}", file.filename),
        sha1: file.hashes.sha1.clone(),
        sha512: file.hashes.sha512.clone(),
        downloads: vec![file.url.clone()],
        file_size: file.size,
        game_versions: v.game_versions.clone(),
        loaders: v.loaders.clone(),
        client_side: client_side.to_string(),
        server_side: server_side.to_string(),
        dependencies: v
            .dependencies
            .iter()
            .map(|d| pack::DepEdge {
                project_id: d.project_id.clone(),
                version_id: d.version_id.clone(),
                dependency_type: d.dependency_type.clone(),
            })
            .collect(),
        version_type: v.version_type.clone(),
        date_published: v.date_published.clone(),
    })
}

/// Ensure a mod jar exists in the shared sha1-keyed cache under
/// `~/.anvil/cache/jars/<sha1>.jar` and return its on-disk path. The launcher
/// downloads these same jars later, so this is amortized, not extra net I/O
/// over a session. Best-effort: any failure yields `None` (the caller simply
/// skips that jar, never a hard error).
///
/// `pub(crate)` so the launcher's registry-dump pass can populate the
/// throwaway `<dump>/mods/` by HARD-LINKING the cached file instead of
/// re-downloading every pinned mod each assemble (see `registry_dump_pass`).
/// Returning a `PathBuf` (not bytes) is what makes that hard-link possible.
pub(crate) async fn ensure_jar_cached(
    client: &reqwest::Client,
    url: &str,
    sha1: &str,
) -> Option<std::path::PathBuf> {
    let dir = crate::settings::data_dir().join("cache").join("jars");
    let _ = std::fs::create_dir_all(&dir);
    // No sha1 → no stable cache key; we cannot dedupe, so bail (callers treat
    // this exactly like a fetch failure: skip, never error).
    if sha1.is_empty() {
        return None;
    }
    let path = dir.join(format!("{sha1}.jar"));
    if let Ok(b) = std::fs::read(&path) {
        if !b.is_empty() {
            return Some(path);
        }
    }
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?.to_vec();
    if bytes.is_empty() {
        return None;
    }
    std::fs::write(&path, &bytes).ok()?;
    Some(path)
}

/// Fetch a mod jar's bytes, sha1-cached under `~/.anvil/cache/jars/`. Thin
/// wrapper over `ensure_jar_cached` (reads the cached file it guarantees);
/// behavior is byte-identical to the previous direct implementation, including
/// the empty-sha1 fallback (no cache key → download uncached, never skip — a
/// jar with no Modrinth sha1 must still be jar-augmentable).
async fn fetch_jar_cached(
    client: &reqwest::Client,
    url: &str,
    sha1: &str,
) -> Option<Vec<u8>> {
    if sha1.is_empty() {
        // Preserve prior behavior: no stable cache key, but still fetch so the
        // jar can be parsed for hidden dependencies.
        let resp = client.get(url).send().await.ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let bytes = resp.bytes().await.ok()?.to_vec();
        return if bytes.is_empty() { None } else { Some(bytes) };
    }
    let path = ensure_jar_cached(client, url, sha1).await?;
    let bytes = std::fs::read(&path).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(bytes)
}

/// Jar-truth augmentation. Modrinth's `version.dependencies[]` is author-
/// curated and systematically incomplete (Spectrum 1.8.13 lists ZERO deps on
/// Modrinth yet its jar requires revelationary, modonomicon, ...). After the
/// Modrinth-metadata closure stabilizes we read every jar's real
/// fabric.mod.json/mods.toml and pull anything the closure does not provide.
///
/// Returns new roots to fold back into the fixpoint, plus issues for jar
/// requirements that resolve to no Modrinth project / no compatible version.
#[allow(clippy::too_many_arguments)]
async fn jar_augment(
    mr: &Modrinth,
    client: &reqwest::Client,
    entries: &[ModEntry],
    scanned: &mut std::collections::HashSet<String>,
    provided: &mut std::collections::HashSet<String>,
    manifests: &mut std::collections::HashMap<String, crate::registry::JarManifest>,
    vcache: &mut VersionCache,
    scache: &mut SideCache,
    mc_version: &str,
    loader: &str,
) -> (Vec<pack::ResolvedVersion>, Vec<pack::ValidationIssue>) {
    // 1. Parse jars we have not parsed yet: union their provided modids,
    //    collect their hard requirements.
    let mut requires: Vec<(String, String)> = Vec::new(); // (needed_by_pid, modid)
    for e in entries {
        if !scanned.insert(e.project_id.clone()) {
            continue;
        }
        let Some(url) = e.downloads.first() else {
            continue;
        };
        let Some(bytes) = fetch_jar_cached(client, url, &e.sha1).await else {
            continue;
        };
        let Some(man) = crate::registry::jar_manifest(&bytes) else {
            continue;
        };
        for (modid, _ver) in &man.provided {
            provided.insert(modid.clone());
        }
        for (modid, _range) in &man.requires {
            requires.push((e.project_id.clone(), modid.clone()));
        }
        // Keep the parsed manifest for Tier 2 (avoids re-reading jars).
        manifests.insert(e.project_id.clone(), man);
    }

    // 2. A required modid the closure does not provide is a real miss Fabric
    //    would reject. Resolve it to a Modrinth project and add it as a root.
    let mut new_roots: Vec<pack::ResolvedVersion> = Vec::new();
    let mut issues: Vec<pack::ValidationIssue> = Vec::new();
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (needed_by, modid) in requires {
        if provided.contains(&modid) || !handled.insert(modid.clone()) {
            continue;
        }
        // slug == modid for the real cases (midnightlib, revelationary,
        // modonomicon); also try the common `_`->`-` slug spelling. A wrong
        // fuzzy match is worse than a clear issue, so no broad search here.
        let proj = match mr.project(&modid).await {
            Ok(p) => Some(p),
            Err(_) => mr.project(&modid.replace('_', "-")).await.ok(),
        };
        let Some(proj) = proj else {
            issues.push(pack::ValidationIssue::UnresolvedRequiredDependency {
                needed_by,
                missing_project_id: modid.clone(),
                reason: format!(
                    "the {modid} jar requires mod '{modid}', which Modrinth has no project for"
                ),
            });
            continue;
        };
        let Ok(versions) = cached_versions(mr, vcache, &proj.id).await.map(|v| v.clone())
        else {
            continue;
        };
        let (cs, ss) = cached_sides(mr, scache, &proj.id).await;
        let best = versions
            .iter()
            .filter(|v| {
                v.game_versions.iter().any(|g| g == mc_version)
                    && (v.loaders.iter().any(|l| l == loader)
                        || (loader == "quilt"
                            && v.loaders.iter().any(|l| l == "fabric")))
            })
            .min_by(|a, b| {
                vt_rank(&a.version_type)
                    .cmp(&vt_rank(&b.version_type))
                    .then(b.date_published.cmp(&a.date_published))
                    .then(a.id.cmp(&b.id))
            });
        match best.and_then(|v| to_resolved(v, &cs, &ss)) {
            Some(rv) => {
                provided.insert(modid.clone());
                new_roots.push(rv);
            }
            None => issues.push(pack::ValidationIssue::UnresolvedRequiredDependency {
                needed_by,
                missing_project_id: proj.id.clone(),
                reason: format!(
                    "the jar requires '{modid}' but {} has no {mc_version}/{loader} version",
                    proj.slug
                ),
            }),
        }
    }
    (new_roots, issues)
}

/// Resolve the user/curator-pinned `refs` AND their full transitive
/// `required`-dependency closure into a complete, launchable pin set.
///
/// Returns `(entries, issues)`:
/// - `entries`: every root plus every transitively-required dependency,
///   deduped by project_id (user pins win), each fully pinned.
/// - `issues`: dependency-level problems the pure resolver found —
///   `UnresolvedRequiredDependency` (a required dep with no mc/loader-
///   compatible version, or that 404'd / was unreachable) and
///   `IncompatibleDependencyPresent`. Combine with `validate_pack` for the
///   complete picture.
///
/// A failed root (the model passed a bad project/version id) is the one hard
/// error — that is a malformed request, not a recoverable dependency gap.
async fn resolve_pack(
    mr: &Modrinth,
    refs: &[ModRef],
    mc_version: &str,
    loader: &str,
) -> anyhow::Result<(Vec<ModEntry>, Vec<pack::ValidationIssue>)> {
    let mut vcache: VersionCache = std::collections::HashMap::new();
    let mut scache: SideCache = std::collections::HashMap::new();
    let mut scanned: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut provided: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut manifests: std::collections::HashMap<
        String,
        crate::registry::JarManifest,
    > = std::collections::HashMap::new();
    resolve_pack_with_state(
        mr,
        refs,
        mc_version,
        loader,
        &mut vcache,
        &mut scache,
        &mut scanned,
        &mut provided,
        &mut manifests,
    )
    .await
}

// ---------------------------------------------------------------------------
// Safe incremental add / remove on an already-assembled instance.
//
// The only safe instance-mod mutation primitive. It NEVER mutates the
// instance; it returns the resolved `EditResult` to persist, or an `EditError`
// the agent can recover from. Safety is delegated to the real resolver rather
// than a hand-rolled second dependency graph (the naive lib.rs append/retain
// commands are the bug class this exists to avoid).
// ---------------------------------------------------------------------------

/// Outcome of a successful `edit_instance_mods`. The diff is computed against
/// the instance's prior closure so the caller can report exactly what changed.
#[derive(Debug)]
pub(crate) struct EditResult {
    /// The new resolved closure (raw resolver output). The caller maps this to
    /// `PinnedMod` for `Instance.mods` and feeds it to `write_mrpack` — same
    /// as `assemble_pack`, so persistence + serialization stay in one place.
    pub entries: Vec<ModEntry>,
    /// The new explicit root project_ids, ready to persist as `Instance.roots`.
    pub roots: Vec<String>,
    /// Mods explicitly added (a new root).
    pub added: Vec<String>,
    /// Dependencies pulled in transitively by the additions.
    pub pulled_deps: Vec<String>,
    /// Mods explicitly removed and now gone.
    pub removed: Vec<String>,
    /// Mods that fell out because the only thing that needed them was removed.
    pub pruned_orphans: Vec<String>,
    /// True iff the resolved closure AND root set are unchanged — the caller
    /// must report "nothing changed" and skip every write (no registry re-dump).
    pub noop: bool,
}

/// One refused removal: still required by other kept mods.
#[derive(Debug)]
pub(crate) struct StillRequired {
    pub label: String,
    /// Kept mods whose real jar manifests still require it. Empty when jar
    /// metadata could not attribute it to a specific mod.
    pub required_by: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum EditError {
    /// One or more removals cannot be honored — still required by kept mods.
    StillRequired(Vec<StillRequired>),
    /// The resolved set has blocking issues (an add introduced an
    /// incompatibility / unresolved dependency). The purely-informational
    /// `IncompatibleAddonDropped` never appears here.
    Conflicts(Vec<pack::ValidationIssue>),
    /// Resolution itself failed (Modrinth unreachable, etc.).
    Resolve(String),
}

/// Safely add and/or remove mods on an already-assembled instance.
///
/// - **Add is dependency-complete + conflict-checked** — required deps are
///   pulled in; any incompatibility blocks (no bare append).
/// - **Remove is reverse-dependency safe** — we re-resolve from the reduced
///   root set; if a removed mod *reappears* in the closure a kept mod still
///   requires it, so the removal is refused with that requiring set.
/// - **Orphans auto-prune** — a dep that only existed because a removed mod
///   needed it is simply not re-added, and is reported.
pub(crate) async fn edit_instance_mods(
    mr: &Modrinth,
    inst: &Instance,
    add: &[ModRef],
    remove: &[String],
) -> Result<EditResult, EditError> {
    let mut vcache: VersionCache = std::collections::HashMap::new();
    let mut scache: SideCache = std::collections::HashMap::new();
    let mut scanned: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut provided: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut manifests: std::collections::HashMap<
        String,
        crate::registry::JarManifest,
    > = std::collections::HashMap::new();
    edit_instance_mods_with_state(
        mr,
        inst,
        add,
        remove,
        &mut vcache,
        &mut scache,
        &mut scanned,
        &mut provided,
        &mut manifests,
    )
    .await
}

/// `edit_instance_mods`' body with the resolver state lifted to parameters —
/// the offline test seam (mirrors `resolve_pack` → `resolve_pack_with_state`):
/// tests pre-seed `vcache`/`manifests` from recorded Modrinth + real jar data
/// so the REAL root math + REAL gates run on production-shaped data with no
/// network. Production callers go through the empty-state wrapper above.
#[allow(clippy::too_many_arguments)]
async fn edit_instance_mods_with_state(
    mr: &Modrinth,
    inst: &Instance,
    add: &[ModRef],
    remove: &[String],
    vcache: &mut VersionCache,
    scache: &mut SideCache,
    scanned: &mut std::collections::HashSet<String>,
    provided: &mut std::collections::HashSet<String>,
    manifests: &mut std::collections::HashMap<
        String,
        crate::registry::JarManifest,
    >,
) -> Result<EditResult, EditError> {
    use std::collections::HashSet;

    let remove_set: HashSet<&str> =
        remove.iter().map(String::as_str).collect();

    // roots0: explicit roots paired with their currently-pinned versions so
    // re-resolving for ONE change does not silently bump every other mod.
    // Back-compat: empty `roots` => treat every pinned mod as a root.
    let roots0: Vec<ModRef> = if inst.roots.is_empty() {
        inst.mods
            .iter()
            .filter(|m| !m.project_id.is_empty())
            .map(|m| ModRef {
                project_id: m.project_id.clone(),
                version_id: m.version_id.clone(),
            })
            .collect()
    } else {
        inst.roots
            .iter()
            .filter(|p| !p.is_empty())
            .map(|pid| ModRef {
                project_id: pid.clone(),
                version_id: inst
                    .mods
                    .iter()
                    .find(|m| &m.project_id == pid)
                    .map(|m| m.version_id.clone())
                    .unwrap_or_default(),
            })
            .collect()
    };

    // An add without an explicit version_id must be pinned to a concrete best
    // compatible version NOW: a ROOT pin needs a version (the resolver only
    // auto-picks for a transitive dep, not a root). Same selection
    // assemble/propose use (release < beta < alpha, then newest, then id).
    // No compatible version => a real "cannot add" conflict, surfaced
    // structured rather than as an opaque resolver error.
    let mut add_resolved: Vec<ModRef> = Vec::with_capacity(add.len());
    let mut unresolvable: Vec<pack::ValidationIssue> = Vec::new();
    for a in add {
        if a.project_id.is_empty() {
            continue;
        }
        if !a.version_id.is_empty() {
            add_resolved.push(ModRef {
                project_id: a.project_id.clone(),
                version_id: a.version_id.clone(),
            });
            continue;
        }
        let versions = match cached_versions(mr, vcache, &a.project_id).await
        {
            Ok(v) => v.clone(),
            Err(e) => return Err(EditError::Resolve(e)),
        };
        let best = versions
            .iter()
            .filter(|v| {
                v.game_versions.iter().any(|g| g == &inst.mc_version)
                    && (v.loaders.iter().any(|l| l == &inst.loader)
                        || (inst.loader == "quilt"
                            && v.loaders.iter().any(|l| l == "fabric")))
            })
            .min_by(|x, y| {
                vt_rank(&x.version_type)
                    .cmp(&vt_rank(&y.version_type))
                    .then(y.date_published.cmp(&x.date_published))
                    .then(x.id.cmp(&y.id))
            });
        match best {
            Some(v) => add_resolved.push(ModRef {
                project_id: a.project_id.clone(),
                version_id: v.id.clone(),
            }),
            None => {
                unresolvable.push(
                    pack::ValidationIssue::IncompatibleGameVersion {
                        project_id: a.project_id.clone(),
                        want: inst.mc_version.clone(),
                    },
                );
            }
        }
    }
    if !unresolvable.is_empty() {
        return Err(EditError::Conflicts(unresolvable));
    }

    // new_roots = (roots0 - remove) ∪ add. Adds come FIRST so an add of an
    // already-present project can re-pin its version (mirrors `merge_roots`:
    // the explicit call wins a project_id collision).
    let mut seen: HashSet<String> = HashSet::new();
    let mut new_roots: Vec<ModRef> = Vec::new();
    for r in &add_resolved {
        if remove_set.contains(r.project_id.as_str()) {
            continue;
        }
        if seen.insert(r.project_id.clone()) {
            new_roots.push(ModRef {
                project_id: r.project_id.clone(),
                version_id: r.version_id.clone(),
            });
        }
    }
    for r in roots0 {
        if remove_set.contains(r.project_id.as_str()) {
            continue;
        }
        if seen.insert(r.project_id.clone()) {
            new_roots.push(r);
        }
    }

    // Resolve the new root set through the real resolver (transitive closure,
    // version floor, conflict + audit). `manifests` is retained (real jar
    // requires/provided) for the reverse-dependency attribution below.
    let (entries, resolve_issues) = resolve_pack_with_state(
        mr,
        &new_roots,
        &inst.mc_version,
        &inst.loader,
        vcache,
        scache,
        scanned,
        provided,
        manifests,
    )
    .await
    .map_err(|e| EditError::Resolve(e.to_string()))?;

    // ModEntry has only a path; PinnedMod carries a name. One pretty form for
    // every diff label so add/pulled (from path) and removed/pruned (from the
    // stored name) read consistently.
    let pretty = |s: &str| -> String {
        s.strip_prefix("mods/")
            .unwrap_or(s)
            .trim_end_matches(".jar")
            .to_string()
    };

    // --- Gate 1: a removal that re-appears is still required ---------------
    let mut still: Vec<StillRequired> = Vec::new();
    for rp in remove {
        let Some(culprit) = entries.iter().find(|e| &e.project_id == rp)
        else {
            continue; // genuinely gone — good
        };
        // Modids the removed project provides (from its real jar manifest).
        let provided_ids: Vec<String> = manifests
            .get(rp)
            .map(|m| m.provided.iter().map(|(modid, _)| modid.clone()).collect())
            .unwrap_or_default();
        // Any kept mod whose jar manifest still requires one of those modids.
        let mut required_by: Vec<String> = Vec::new();
        if !provided_ids.is_empty() {
            for e in &entries {
                if &e.project_id == rp {
                    continue;
                }
                if let Some(man) = manifests.get(&e.project_id) {
                    if man.requires.iter().any(|(modid, _)| {
                        provided_ids.iter().any(|p| p == modid)
                    }) {
                        required_by.push(pretty(&e.path));
                    }
                }
            }
        }
        required_by.sort();
        required_by.dedup();
        still.push(StillRequired {
            label: pretty(&culprit.path),
            required_by,
        });
    }
    if !still.is_empty() {
        return Err(EditError::StillRequired(still));
    }

    // --- Gate 2: blocking validation issues -------------------------------
    // Gate on the SAME combined view assemble_pack/validate_pack use: the
    // resolver's dep-level issues PLUS per-entry validate_pack checks
    // (game-version / loader / side / dup / insecure). Without the combine an
    // added mod incompatible with the pack's MC version would slip the gate.
    // Mirror assemble_pack: everything blocks except the purely-informational
    // IncompatibleAddonDropped (its conflicting addon was already removed).
    let issues = combined_issues(
        &entries,
        resolve_issues,
        &inst.mc_version,
        &inst.loader,
    );
    let blocking: Vec<pack::ValidationIssue> = issues
        .into_iter()
        .filter(|i| {
            !matches!(
                i,
                pack::ValidationIssue::IncompatibleAddonDropped { .. }
            )
        })
        .collect();
    if !blocking.is_empty() {
        return Err(EditError::Conflicts(blocking));
    }

    // --- Build the diff + pinned closure ----------------------------------
    let old: HashSet<&str> =
        inst.mods.iter().map(|m| m.project_id.as_str()).collect();
    let new_pids: HashSet<&str> =
        entries.iter().map(|e| e.project_id.as_str()).collect();
    let root_pids: HashSet<&str> =
        new_roots.iter().map(|r| r.project_id.as_str()).collect();

    let mut added = Vec::new();
    let mut pulled_deps = Vec::new();
    for e in &entries {
        if !old.contains(e.project_id.as_str()) {
            if root_pids.contains(e.project_id.as_str()) {
                added.push(pretty(&e.path));
            } else {
                pulled_deps.push(pretty(&e.path));
            }
        }
    }
    let mut removed = Vec::new();
    let mut pruned_orphans = Vec::new();
    for m in &inst.mods {
        if !new_pids.contains(m.project_id.as_str()) {
            if remove_set.contains(m.project_id.as_str()) {
                removed.push(pretty(&m.name));
            } else {
                pruned_orphans.push(pretty(&m.name));
            }
        }
    }

    // noop: resolved closure (project_id+version_id multiset) AND root set
    // unchanged. Caller skips every write (no needless registry re-dump).
    let same_closure = {
        let mut a: Vec<(&str, &str)> = inst
            .mods
            .iter()
            .map(|m| (m.project_id.as_str(), m.version_id.as_str()))
            .collect();
        let mut b: Vec<(&str, &str)> = entries
            .iter()
            .map(|e| (e.project_id.as_str(), e.version_id.as_str()))
            .collect();
        a.sort();
        b.sort();
        a == b
    };
    let new_root_ids: Vec<String> =
        new_roots.iter().map(|r| r.project_id.clone()).collect();
    let old_root_ids: HashSet<&str> = if inst.roots.is_empty() {
        inst.mods.iter().map(|m| m.project_id.as_str()).collect()
    } else {
        inst.roots.iter().map(String::as_str).collect()
    };
    let same_roots = new_root_ids.len() == old_root_ids.len()
        && new_root_ids
            .iter()
            .all(|p| old_root_ids.contains(p.as_str()));

    Ok(EditResult {
        entries,
        roots: new_root_ids,
        added,
        pulled_deps,
        removed,
        pruned_orphans,
        noop: same_closure && same_roots,
    })
}

/// `resolve_pack`'s body with the per-resolution caches + jar-scan state lifted
/// to parameters. The only reason this seam exists: tests pre-seed `vcache`
/// (from a recorded Modrinth snapshot) so `cached_versions`' `contains_key`
/// short-circuits the network, pre-seed `scanned` so `jar_augment`'s download
/// loop is a no-op, and pre-seed `manifests` with REAL `fabric.mod.json`
/// `depends` so the Tier-2 floor runs on production-shaped data — exercising
/// the *real* fixpoint + floor path offline, with no synthetic dual-candidate
/// `pool`. Production callers go through the `resolve_pack` wrapper above
/// (empty state) and are unchanged.
#[allow(clippy::too_many_arguments)]
async fn resolve_pack_with_state(
    mr: &Modrinth,
    refs: &[ModRef],
    mc_version: &str,
    loader: &str,
    vcache: &mut VersionCache,
    scache: &mut SideCache,
    scanned: &mut std::collections::HashSet<String>,
    provided: &mut std::collections::HashSet<String>,
    manifests: &mut std::collections::HashMap<String, crate::registry::JarManifest>,
) -> anyhow::Result<(Vec<ModEntry>, Vec<pack::ValidationIssue>)> {
    // 1. Materialize the explicitly-pinned roots at their exact versions.
    let mut roots: Vec<pack::ResolvedVersion> = Vec::with_capacity(refs.len());
    for r in refs {
        let versions = cached_versions(mr, vcache, &r.project_id)
            .await
            .map_err(|e| anyhow!("versions lookup failed for {}: {e}", r.project_id))?
            .clone();
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
        let (cs, ss) = cached_sides(mr, scache, &r.project_id).await;
        let resolved = to_resolved(version, &cs, &ss).ok_or_else(|| {
            anyhow!(
                "version {} of {} has no downloadable files",
                r.version_id,
                r.project_id
            )
        })?;
        roots.push(resolved);
    }

    // 2. Fixpoint loop: run the pure resolver, fetch whatever projects it asks
    //    for, repeat until the closure stops growing. `pool` accumulates every
    //    candidate version we've fetched; `failed` carries hard lookup
    //    failures so the resolver turns them into reported issues.
    let mut pool: std::collections::HashMap<String, Vec<pack::ResolvedVersion>> =
        std::collections::HashMap::new();
    for r in &roots {
        pool.entry(r.project_id.clone()).or_default().push(r.clone());
    }
    let mut failed: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();

    // Jar-augmentation state (Tier 1). `scanned` keeps each jar parsed once
    // across outer iterations; `provided` is the running union of modids the
    // closure supplies; `jar_issues` accumulates unresolvable jar requires.
    let jar_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut jar_issues: Vec<pack::ValidationIssue> = Vec::new();
    // Tier 2 state: parsed jar manifests (reused from the Tier 1 scan, no
    // re-read; injected so tests can pre-seed REAL manifests) and the dep
    // projects already floored (each at most once, which also guarantees
    // termination since a floored pin becomes an authoritative root the
    // resolver never replaces). `scanned`/`provided`/`manifests` are now
    // parameters (empty in production via the wrapper).
    let mut tier2_pinned: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // Projects whose FULL Modrinth version list the floor-lookahead pre-pass
    // has fetched into `pool` (vs. only the lone pinned root entry). The
    // discriminator the hard gate uses to tell "no compatible FLK exists"
    // (block) from "FLK's pool was never expanded" (stay silent).
    let mut pool_complete_for: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    // A library published more than this long after the mod that needs it is
    // treated as a likely API-incompatible major bump (Create 6, Kotlin 2.x).
    const TIER2_GRACE_DAYS: i64 = 90;

    // Safety bound: distinct projects in a pack are finite; this only trips on
    // a pathological graph and prevents any chance of a spin.
    for _ in 0..512 {
        let (entries, issues, needed) =
            pack::resolve_dependencies(&roots, &pool, &failed, mc_version, loader);
        if needed.is_empty() {
            // Modrinth-metadata closure is stable. Now reconcile against the
            // jars' real fabric.mod.json/mods.toml (Modrinth deps are
            // systematically incomplete) and fold any discovered libraries
            // back into the fixpoint until that stabilizes too.
            let (new_roots, ji) = jar_augment(
                mr,
                &jar_client,
                &entries,
                scanned,
                provided,
                manifests,
                vcache,
                scache,
                mc_version,
                loader,
            )
            .await;
            jar_issues.extend(ji);
            let mut added = false;
            for nr in new_roots {
                if !roots.iter().any(|r| r.project_id == nr.project_id) {
                    pool.entry(nr.project_id.clone())
                        .or_default()
                        .push(nr.clone());
                    roots.push(nr);
                    added = true;
                }
            }
            if added {
                continue;
            }

            // Floor-lookahead pool expansion (off the hot path). The bug:
            // a ROOT-pinned project enters `pool` with ONLY its single pinned
            // version (it never reaches `needed.projects`, so the fixpoint's
            // full-list `pool.insert` never runs for it). When that root is a
            // floor target (FLK pinned at Kotlin-2.x while a scanned manifest
            // needs Kotlin-1.x; or Create pinned at 6 while a Create-0.5-only
            // addon is present) the Tier-2 floor has no older candidate to
            // pick and silently ships the crash. Fix: GENERALISED by design —
            // for ANY project that some scanned manifest references through an
            // OPEN-ENDED range (the exact predicate the floor itself uses,
            // `is_open_ended_range`, via the same modid→owner lookup the floor
            // builds), ensure that owner's FULL version list is in `pool`
            // before the (pure, sync) floor runs. Reuses the SAME
            // `cached_versions` cache the resolver uses elsewhere; scoped to
            // only floor-relevant owners so a 50-mod pack does not balloon.
            // The hard gate downstream stays FLK-specific for now.
            {
                let mut owner: std::collections::HashMap<&str, &str> =
                    std::collections::HashMap::new();
                for (pid, m) in &*manifests {
                    for (modid, _ver) in &m.provided {
                        owner.insert(modid.as_str(), pid.as_str());
                    }
                }
                let mut floor_relevant: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for m in manifests.values() {
                    for (modid, range) in &m.requires {
                        if !crate::registry::is_open_ended_range(range) {
                            continue;
                        }
                        if let Some(&pid) = owner.get(modid.as_str()) {
                            if !pool_complete_for.contains(pid) {
                                floor_relevant.insert(pid.to_string());
                            }
                        }
                    }
                }
                for pid in floor_relevant {
                    if let Ok(versions) =
                        cached_versions(mr, vcache, &pid).await.map(|v| v.clone())
                    {
                        let (cs, ss) = cached_sides(mr, scache, &pid).await;
                        let mapped: Vec<pack::ResolvedVersion> = versions
                            .iter()
                            .filter_map(|v| to_resolved(v, &cs, &ss))
                            .collect();
                        if !mapped.is_empty() {
                            pool.insert(pid.clone(), mapped);
                        }
                        // Mark complete even on empty: we DID fetch the full
                        // list — an empty result is "no compatible version
                        // exists", which the hard gate should act on.
                        pool_complete_for.insert(pid);
                    }
                }
            }

            // Tier 2: the jar/Modrinth closure is stable; now apply the
            // open-ended-range version floor. A floored dep is re-pinned as an
            // authoritative root and the fixpoint re-runs so the older library
            // propagates consistently.
            let (repins, floor_issues) = pack::version_floor_repins(
                &entries,
                manifests,
                &pool,
                mc_version,
                loader,
                TIER2_GRACE_DAYS,
                &tier2_pinned,
                &pool_complete_for,
            );
            let mut floored = false;
            for (dep_pid, vid) in repins {
                let Some(pinned) = pool
                    .get(&dep_pid)
                    .and_then(|vs| vs.iter().find(|v| v.version_id == vid))
                    .cloned()
                else {
                    continue;
                };
                roots.retain(|r| r.project_id != dep_pid);
                roots.push(pinned);
                tier2_pinned.insert(dep_pid);
                floored = true;
            }
            if floored {
                continue;
            }

            // Post-resolution exact-pin audit: drop incompatible leaf addons
            // (e.g. createairfabric vs Create 6) so a guaranteed launch crash
            // never ships; raise a hard issue for a non-leaf conflict.
            let (entries, audit_issues) =
                pack::audit_version_satisfaction(&entries, manifests);
            let mut all = issues;
            all.extend(jar_issues);
            all.extend(audit_issues);
            all.extend(floor_issues); // hard KotlinMajorUnsatisfiable gate
            // Step 3 (report-only): general depends/breaks range check over
            // the final closure + real manifests. Flows through the existing
            // combined_issues -> assemble/edit gate, so a violation now blocks
            // pre-assemble instead of crashing Fabric at launch. Selection is
            // unchanged here (Step 4 does the re-pin).
            all.extend(pack::check_version_constraints(&entries, manifests));
            return Ok((entries, all));
        }
        for (pid, _pinned_vid) in &needed.projects {
            // `cached_versions` borrows vcache mutably; collect first.
            let fetched = cached_versions(mr, vcache, pid).await;
            match fetched {
                Ok(versions) => {
                    let versions = versions.clone();
                    let (cs, ss) = cached_sides(mr, scache, pid).await;
                    let mapped: Vec<pack::ResolvedVersion> = versions
                        .iter()
                        .filter_map(|v| to_resolved(v, &cs, &ss))
                        .collect();
                    // Insert even if empty so the resolver decides
                    // (NoCompatibleVersion) instead of re-requesting forever.
                    pool.insert(pid.clone(), mapped);
                }
                Err(e) => {
                    failed.insert(pid.clone(), e);
                }
            }
        }
    }

    // Pathological: bail with whatever we have rather than loop.
    let (entries, mut issues, _needed) =
        pack::resolve_dependencies(&roots, &pool, &failed, mc_version, loader);
    issues.extend(jar_issues);
    issues.extend(pack::check_version_constraints(&entries, manifests));
    Ok((entries, issues))
}

// `build_entries` was the old "materialize the exact pinned refs" helper. It
// has been superseded by `resolve_pack`, which does the same root
// materialization AND the transitive required-dependency closure (the missing
// step that caused packs to ship without their libraries). Both tool entry
// points now call `resolve_pack` directly with the pack's mc_version + loader,
// which the closure needs to pick compatible dependency versions.

/// Combine the dependency-resolution issues (unresolved required / present
/// incompatible) with the per-entry `validate_pack` issues over the FULL
/// transitive closure, deduped. The model sees one coherent issue list and is
/// not asked to hand-add libraries the resolver already pinned.
fn combined_issues(
    entries: &[ModEntry],
    dep_issues: Vec<pack::ValidationIssue>,
    mc_version: &str,
    loader: &str,
) -> Vec<pack::ValidationIssue> {
    let mut issues = dep_issues;
    for i in validate_pack(entries, mc_version, loader) {
        if !issues.contains(&i) {
            issues.push(i);
        }
    }
    issues
}

/// Stable-channel rank for picking a root version: release < beta < alpha.
fn vt_rank(version_type: &str) -> u8 {
    match version_type {
        "release" => 0,
        "beta" => 1,
        "alpha" => 2,
        _ => 3,
    }
}

/// Modrinth search is keyword full-text, NOT semantic — a prose brief like
/// "a pack where you play as an archetype with space exploration and tech
/// progression" matches zero projects. Reduce the brief to its distinct
/// content tokens (drop generic modpack/filler words), order-preserving and
/// capped, so each becomes a real query. An empty result means the brief was
/// all filler and the caller falls back to the popular-staples search.
fn brief_keywords(brief: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "you", "your", "that", "this", "these",
        "those", "where", "when", "what", "who", "how", "want", "wants",
        "make", "build", "based", "style", "styled", "themed", "theme",
        "vibe", "experience", "adventure", "feature", "features",
        "featuring", "including", "include", "includes", "etc", "plus",
        "some", "more", "most", "very", "really", "kind", "sort", "like",
        "about", "around", "whole", "everything", "sense", "play", "playing",
        "player", "game", "games", "gaming", "minecraft", "pack", "packs",
        "modpack", "modpacks", "mod", "mods", "thing", "things", "lots",
        "system", "systems", "gameplay", "progression", "stuff", "have",
        "has", "having", "able", "into", "from", "out", "but", "not", "all",
        "any", "can", "get", "let", "set", "are", "was", "will", "one",
        "two", "use", "via", "per", "its", "his", "her", "our", "why",
    ];
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    for raw in brief.split(|c: char| !c.is_ascii_alphanumeric()) {
        let w = raw.trim().to_lowercase();
        if w.len() < 3 || STOP.contains(&w.as_str()) {
            continue;
        }
        if seen.insert(w.clone()) {
            out.push(w);
        }
        if out.len() == 6 {
            break;
        }
    }
    out
}

/// Compound tool: turn a one-line brief into a reviewed candidate pack in a
/// SINGLE round. Searches Modrinth (relevance + popularity), picks the best
/// compatible version per mod (release-then-newest), then runs the existing
/// transitive dependency resolver so required libraries are already included.
/// Collapses the search_mods -> get_mod -> search_mods loop the model used to
/// pay tokens for; it now reviews one list and calls assemble_pack.
async fn tool_propose_pack(
    mr: &Modrinth,
    thread_id: Option<&str>,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    // Mark BEFORE any early return: a propose_pack miss still unlocks the
    // manual-search fallback (funnel gate), and a success additionally writes
    // the recoverable candidate (Part B).
    if let Some(tid) = thread_id {
        crate::chat::mark_proposed(tid);
        // Idempotent (state fix): a resolved candidate already exists for this
        // thread — hand it straight back instead of re-scouting. The model
        // forgets the proposed set every turn and otherwise calls propose_pack
        // again on the next message, throwing away the (expensive) resolved
        // pack and starting from scratch. This runs BEFORE the brief is even
        // read, so a re-scout cannot be triggered by a reworded brief — the
        // only reset is a brand-new chat (delete_thread clears the candidate);
        // an assemble success also clears it so a later "make another pack"
        // scouts fresh.
        if let Some(c) = crate::chat::load_candidate(tid) {
            let candidates: Vec<Value> = c
                .mods
                .iter()
                .map(|m| {
                    json!({
                        "project_id": m.project_id,
                        "version_id": m.version_id,
                        "title": m.title,
                    })
                })
                .collect();
            tool_chip(
                tx,
                "propose_pack",
                &format!("reusing saved {} mods", candidates.len()),
            );
            return Ok(serde_json::to_string(&json!({
                "mc_version": c.mc_version,
                "loader": c.loader,
                "candidates": candidates,
                "note": "This conversation ALREADY has a saved, resolved \
                         candidate pack (shown above). propose_pack did NOT \
                         re-scout — it returned the saved set unchanged. Do \
                         NOT call propose_pack again. To build it, call \
                         assemble_pack with loader_version and omit `mods` \
                         (the saved set is used automatically). For swaps or \
                         drops, use search_mods/get_mod then assemble_pack \
                         with explicit {project_id, version_id} refs. The \
                         pack resets only when the player starts a new chat.",
            }))?);
        }
    }
    let brief = str_field(input, "brief")?;
    let mc = str_field(input, "mc_version")?;
    let loader = str_field(input, "loader")?;
    let count = input
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or(28)
        .clamp(8, 45) as usize;

    tool_chip(tx, "propose_pack", &format!("searching \"{brief}\""));
    let facets =
        format!("[[\"project_type:mod\"],[\"versions:{mc}\"],[\"categories:{loader}\"]]");

    // Modrinth search is keyword full-text, NOT semantic: feeding it the
    // model's prose brief verbatim returns zero hits, so propose_pack used to
    // die before saving a candidate (the entire "propose_pack is having
    // trouble" → manual loop → tool-limit cascade). Reduce the brief to its
    // content keywords and search each, THEN a facets-only popular-staples
    // pass that is guaranteed non-empty for any valid mc/loader — so
    // propose_pack ALWAYS yields a candidate it can save.
    let mut hits: Vec<crate::modrinth::SearchHit> = Vec::new();
    let mut seen: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let keywords = brief_keywords(brief);
    let queries: Vec<String> = if keywords.is_empty() {
        vec![brief.to_string()]
    } else {
        keywords
    };
    let per = (((count as u32) * 2) / queries.len() as u32).max(12);
    for q in &queries {
        if let Ok(res) = mr.search(q, Some(&facets), "relevance", per, 0).await {
            for h in res.hits {
                if seen.insert(h.project_id.clone()) {
                    hits.push(h);
                }
            }
        }
    }
    // Popular staples: empty query + facets = the most-downloaded mods for
    // this mc/loader. The guarantee propose_pack never returns empty unless
    // Modrinth is unreachable or the mc/loader is genuinely barren.
    if let Ok(res) = mr
        .search("", Some(&facets), "downloads", (count as u32) * 2, 0)
        .await
    {
        for h in res.hits {
            if seen.insert(h.project_id.clone()) {
                hits.push(h);
            }
        }
    }
    if hits.is_empty() {
        return Ok(format!(
            "propose_pack: Modrinth returned nothing for {mc}/{loader} — it \
             may be unreachable, or nothing publishes for that exact \
             version+loader. Double-check the Minecraft version and loader."
        ));
    }
    hits.sort_by(|a, b| b.downloads.cmp(&a.downloads));
    hits.truncate(count);

    // Best compatible version per hit — the Rust-side fan-out the model no
    // longer spends tokens looping over.
    tool_chip(tx, "propose_pack", "resolving versions");
    let mut refs: Vec<ModRef> = Vec::new();
    let mut meta: std::collections::HashMap<String, (String, String)> =
        std::collections::HashMap::new();
    for h in &hits {
        let Ok(versions) = mr.versions(&h.project_id).await else {
            continue;
        };
        let best = versions
            .iter()
            .filter(|v| {
                v.game_versions.iter().any(|g| g == mc)
                    && (v.loaders.iter().any(|l| l == loader)
                        || (loader == "quilt"
                            && v.loaders.iter().any(|l| l == "fabric")))
            })
            .min_by(|a, b| {
                vt_rank(&a.version_type)
                    .cmp(&vt_rank(&b.version_type))
                    .then(b.date_published.cmp(&a.date_published))
                    .then(a.id.cmp(&b.id))
            });
        if let Some(v) = best {
            refs.push(ModRef {
                project_id: h.project_id.clone(),
                version_id: v.id.clone(),
            });
            let reason = format!(
                "{} · {} downloads",
                h.categories.first().cloned().unwrap_or_else(|| "mod".into()),
                h.downloads
            );
            meta.insert(h.project_id.clone(), (h.title.clone(), reason));
        }
    }
    if refs.is_empty() {
        return Ok(format!(
            "propose_pack: matched mods but none publish a {mc}/{loader} version. \
             Try a different Minecraft version or loader."
        ));
    }

    // Transitive required-dependency closure via the reviewed resolver.
    let (entries, dep_issues) =
        pump(resolve_pack(mr, &refs, mc, loader), tx, "propose_pack").await?;
    let issues = combined_issues(&entries, dep_issues, mc, loader);

    let candidates: Vec<Value> = entries
        .iter()
        .map(|e| {
            let (title, reason) = meta
                .get(&e.project_id)
                .cloned()
                .unwrap_or_else(|| (String::new(), "required dependency".into()));
            json!({
                "project_id": e.project_id,
                "version_id": e.version_id,
                "title": title,
                "reason": reason,
            })
        })
        .collect();

    // Persist the resolved set so assemble_pack can recover it after the turn
    // boundary / tool-round limit wipes the model's context (Part B). Saved
    // from `entries` (the post-resolution closure assemble would actually use,
    // required libs included), not the model's roots.
    if let Some(tid) = thread_id {
        let saved = crate::chat::CandidatePack {
            mc_version: mc.to_string(),
            loader: loader.to_string(),
            mods: entries
                .iter()
                .map(|e| crate::chat::CandidateMod {
                    project_id: e.project_id.clone(),
                    version_id: e.version_id.clone(),
                    title: meta
                        .get(&e.project_id)
                        .map(|(t, _)| t.clone())
                        .unwrap_or_default(),
                })
                .collect(),
        };
        crate::chat::save_candidate(tid, &saved);
    }

    tool_chip(tx, "propose_pack", &format!("{} mods", candidates.len()));
    Ok(serde_json::to_string(&json!({
        "mc_version": mc,
        "loader": loader,
        "candidates": candidates,
        "issues": issues,
        "note": "Reviewed candidate pack (your roots plus auto-resolved required \
                 libraries). This exact set is SAVED for this conversation. \
                 Tell the player the highlights and take swap/drop requests in \
                 chat. To build it, call assemble_pack with the name + \
                 Minecraft version + loader + loader_version; you may pass an \
                 empty mods list and it will assemble this saved set, or pass \
                 explicit {project_id, version_id} pairs to override after \
                 swaps. Do NOT re-run propose_pack just to get this list back.",
    }))?)
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

    // Resolve the full transitive closure FIRST so the required libraries the
    // model omitted are auto-included; only genuinely unsatisfiable deps (or
    // present incompatibles) are reported back as issues to fix.
    let (entries, dep_issues) =
        pump(resolve_pack(mr, &refs, mc_version, loader), tx, "validate_pack").await?;
    let issues = combined_issues(&entries, dep_issues, mc_version, loader);

    tool_chip(tx, "validate_pack", "done");
    Ok(serde_json::to_string(&issues)?)
}

/// Fire the Slice-1.5 registry-dump pass for `id` in a detached task. Shared
/// by `assemble_pack` and `edit_pack` so the grounding cache
/// (`<instance>/anvil-registry.json`) is refreshed whenever the pinned mod set
/// changes.
///
/// DETACHED on purpose: booting a headless dedicated server is slow and
/// best-effort — it must NEVER delay the caller. If it fails for ANY reason
/// the on-disk registry is left exactly as it was (grounding silently keeps
/// using the static scan); the next assemble/edit re-triggers it.
///
/// GATE: skip only when the cache is already a `DumpReconciled` match for the
/// CURRENT pins (re-dumping that is wasted work). `edit_pack` deletes the
/// cache first, so after an edit this always runs.
pub(crate) fn spawn_registry_dump_detached(
    id: String,
    inst: Instance,
    mr: Modrinth, // `Modrinth: Clone`; a borrow cannot cross the spawn
) {
    tokio::spawn(async move {
        let cache_path = instance_dir(&id).join("anvil-registry.json");
        let key = crate::registry::mod_set_key(&inst);
        // Gate read. Parse failure / absent => run (no usable cache). A
        // matching DumpReconciled cache => skip.
        if let Ok(txt) = std::fs::read_to_string(&cache_path) {
            if let Ok(c) =
                serde_json::from_str::<crate::registry::ScanResult>(&txt)
            {
                if c.mod_set_key == key
                    && c.source == crate::registry::ScanSource::DumpReconciled
                {
                    return; // already reconciled for these pins.
                }
            }
        }

        // No AppHandle here (detached): a throwaway LaunchEvent channel whose
        // receiver is dropped — sends fail silently, which is fine (progress
        // is non-essential for a background pass).
        let (ltx, lrx) = tokio::sync::mpsc::unbounded_channel::<
            crate::launch::LaunchEvent,
        >();
        drop(lrx);

        let dump =
            match crate::launch::registry_dump_pass(&inst, &mr, ltx).await {
                Ok(d) => d,
                // registry_dump_pass already degrades internally; an Err here
                // is unexpected but still must not touch the cache.
                Err(_) => return,
            };
        // Only a real dump rewrites the cache (degrade => leave static).
        let Some(dump_dir) = dump else { return };

        // Reconcile the CURRENT static scan with the live dump, then
        // atomically replace the cache (temp file + rename) so a crash
        // mid-write never leaves a half-written, mis-parsed registry.
        let static_scan =
            crate::registry::scan_instance(&inst, &instance_dir(&id));
        let reconciled =
            crate::registry::reconcile_scan(static_scan, Some(&dump_dir));
        if let Ok(txt) = serde_json::to_string(&reconciled) {
            let tmp = cache_path.with_extension("json.tmp");
            if std::fs::write(&tmp, &txt).is_ok() {
                let _ = std::fs::rename(&tmp, &cache_path);
            }
        }
        // The throwaway server dir is large; reclaim it (non-fatal).
        let _ =
            std::fs::remove_dir_all(instance_dir(&id).join(".anvil-dump"));
    });
}

async fn tool_assemble_pack(
    mr: &Modrinth,
    thread_id: Option<&str>,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    // The saved proposal (Part B): the resolved set propose_pack pinned for
    // this thread. It is the recovery path for the common failure where the
    // model proposed a pack, then a turn boundary / tool-round limit wiped the
    // {project_id, version_id} list it was told to pass here.
    let saved = thread_id.and_then(crate::chat::load_candidate);

    let (refs, _used_saved) =
        assemble_refs(parse_mod_refs(input).unwrap_or_default(), saved.as_ref());
    if refs.is_empty() {
        return Err(anyhow!(
            "assemble_pack: no mods given and no saved proposal for this \
             conversation. Call propose_pack first to build a candidate set."
        ));
    }

    // mc/loader fall back to the saved proposal's; name to the thread title;
    // loader_version must still be supplied (propose_pack does not pin it).
    let mc_version = str_field(input, "mc_version")
        .map(str::to_string)
        .ok()
        .or_else(|| saved.as_ref().map(|c| c.mc_version.clone()))
        .ok_or_else(|| anyhow!("assemble_pack: missing mc_version"))?;
    let loader = str_field(input, "loader")
        .map(str::to_string)
        .ok()
        .or_else(|| saved.as_ref().map(|c| c.loader.clone()))
        .ok_or_else(|| anyhow!("assemble_pack: missing loader"))?;
    let loader_version = str_field(input, "loader_version")?.to_string();
    let name = str_field(input, "name")
        .map(str::to_string)
        .ok()
        .or_else(|| {
            thread_id
                .and_then(crate::chat::load_thread)
                .map(|t| t.title)
                .filter(|t| !t.trim().is_empty())
        })
        .ok_or_else(|| anyhow!("assemble_pack: missing pack name"))?;

    // Resolve the instance this name maps to FIRST: a same-named re-assemble
    // must EXTEND it, not overwrite it (see `merge_roots` — the Starbound
    // Origins failure). `replace: true` is the explicit opt-out: rebuild from
    // scratch, and the only way to DROP a mod via assemble.
    let replace = input
        .get("replace")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let existing = load_instances()
        .into_iter()
        .find(|i| i.name.eq_ignore_ascii_case(&name));
    let merging = existing.is_some() && !replace;
    let refs = merge_roots(
        refs,
        existing.iter().flat_map(|e| {
            e.mods.iter().map(|m| ModRef {
                project_id: m.project_id.clone(),
                version_id: m.version_id.clone(),
            })
        }),
        replace,
    );

    tool_chip(
        tx,
        "assemble_pack",
        &if merging {
            format!("merging into \"{name}\"")
        } else {
            format!("assembling \"{name}\"")
        },
    );

    // Resolve the full transitive required-dependency closure. `entries` now
    // carries every required library automatically; the model never has to
    // hand-add bookshelf / lithostitched / yungsapi etc.
    let (entries, dep_issues) =
        pump(resolve_pack(mr, &refs, &mc_version, &loader), tx, "assemble_pack").await?;

    // Never assemble an invalid pack: validate the WHOLE closure as the final
    // gate (per-entry checks + unresolved/incompatible dependency issues).
    // `IncompatibleAddonDropped` is INFORMATIONAL — the offending leaf addon
    // was already removed by the exact-pin audit, so the pack is valid; report
    // it but do NOT block on it.
    let issues = combined_issues(&entries, dep_issues, &mc_version, &loader);
    let (dropped, blocking): (Vec<_>, Vec<_>) = issues.iter().cloned().partition(|i| {
        matches!(i, pack::ValidationIssue::IncompatibleAddonDropped { .. })
    });
    if !blocking.is_empty() {
        tool_chip(tx, "assemble_pack", "blocked: validation failed");
        return Ok(format!(
            "Refusing to assemble: validate_pack reported issues. Fix these and retry:\n{}",
            serde_json::to_string(&blocking)?
        ));
    }
    if !dropped.is_empty() {
        tool_chip(
            tx,
            "assemble_pack",
            &format!("dropped {} incompatible addon(s)", dropped.len()),
        );
    }

    // Iterating on a pack UPDATES its instance, never spawns a duplicate. The
    // same-named instance found above (its mods already merged into `refs`
    // unless `replace`) is reused: id, dir, created date, play history kept,
    // mod list rewritten from the resolved closure.
    let now = chrono::Utc::now();
    let (id, created, last_played) = match existing {
        Some(e) => (e.id, e.created, e.last_played),
        None => {
            // lowercase-hex of nanos + a small spread: sortable, collision-
            // resistant, no uuid crate.
            let nanos = now
                .timestamp_nanos_opt()
                .unwrap_or_else(|| now.timestamp_millis());
            let rand = (nanos as u128).wrapping_mul(2_654_435_761) & 0xffff;
            (
                format!("{:x}{:x}", nanos.unsigned_abs(), rand),
                now.to_rfc3339(),
                None,
            )
        }
    };

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
        created,
        last_played,
        mods: pinned,
        // The explicitly-chosen roots (post-merge). `mods` above is the
        // resolved closure; this records what `edit_pack` re-resolves from so
        // a single add/remove is dependency-correct. `merge_roots` already
        // dropped empty project_ids, so this is the clean root set.
        roots: refs.iter().map(|r| r.project_id.clone()).collect(),
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

    // Consumed: drop the saved proposal so a later "make another pack" in this
    // thread re-proposes fresh instead of re-assembling this one.
    if let Some(tid) = thread_id {
        crate::chat::clear_candidate(tid);
    }

    // Slice 1.5 registry-dump trigger (detached, best-effort). Shared with
    // edit_pack so the grounding cache is refreshed whenever the pinned mod
    // set changes — see `spawn_registry_dump_detached`.
    spawn_registry_dump_detached(id.clone(), inst.clone(), mr.clone());

    // Emit the chip before the success string so the UI shows the assembled
    // pack even while the assistant's wrap-up text is still streaming.
    let _ = tx.send(CuratorEvent::Assembled {
        instance_id: id.clone(),
        name: name.clone(),
    });
    let _ = tx.send(CuratorEvent::Phase("assembled".to_string()));
    tool_chip(tx, "assemble_pack", "done");

    let dropped_note = if dropped.is_empty() {
        String::new()
    } else {
        format!(
            " Auto-removed {} incompatible addon(s) that exact-pin a different \
             version of a mod in this pack (would crash Fabric at launch): {}. \
             Tell the player these were dropped and why.",
            dropped.len(),
            serde_json::to_string(&dropped).unwrap_or_default()
        )
    };
    Ok(format!(
        "Assembled \"{name}\" ({} mods) for Minecraft {mc_version} on {loader} {loader_version}. \
         Instance id {id}; .mrpack written to {}.{dropped_note}",
        entries.len(),
        mrpack_path.display()
    ))
}

/// Newest version first: Modrinth returns versions newest-first, so the first
/// entry is the latest. Pick its primary file (or the first file).
fn newest_primary_file(versions: &[Version]) -> anyhow::Result<&crate::modrinth::VersionFile> {
    let v = versions
        .first()
        .ok_or_else(|| anyhow!("pack project has no published versions"))?;
    if v.files.is_empty() {
        return Err(anyhow!("newest pack version has no downloadable files"));
    }
    Ok(v.files.iter().find(|f| f.primary).unwrap_or(&v.files[0]))
}

async fn tool_seed_from_pack(
    mr: &Modrinth,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let query = str_field(input, "query")?;
    tool_chip(tx, "seed_from_pack", &format!("loading {query}"));

    // Helper so any failure becomes a recoverable tool_result string rather
    // than aborting the turn.
    async fn inner(mr: &Modrinth, query: &str) -> anyhow::Result<String> {
        let res = mr
            .search(query, Some("[[\"project_type:modpack\"]]"), "relevance", 5, 0)
            .await
            .map_err(|e| anyhow!("Modrinth modpack search failed: {e}"))?;
        let top = res
            .hits
            .first()
            .ok_or_else(|| anyhow!("no modpacks matched \"{query}\""))?;

        let versions = mr
            .versions(&top.project_id)
            .await
            .map_err(|e| anyhow!("versions lookup failed for {}: {e}", top.project_id))?;
        let file = newest_primary_file(&versions)?;

        // Unique temp path; mirrors the nanos id pattern used elsewhere.
        let now = chrono::Utc::now();
        let nanos = now
            .timestamp_nanos_opt()
            .unwrap_or_else(|| now.timestamp_millis());
        let tmp = std::env::temp_dir().join(format!("anvil-seed-{}.mrpack", nanos.unsigned_abs()));

        // Download the .mrpack with a fresh client (the Modrinth client's HTTP
        // handle is private; reqwest is already a dependency).
        let dl = reqwest::Client::new();
        let bytes = dl
            .get(&file.url)
            .send()
            .await
            .map_err(|e| anyhow!("downloading .mrpack failed: {e}"))?
            .error_for_status()
            .map_err(|e| anyhow!("downloading .mrpack failed: {e}"))?
            .bytes()
            .await
            .map_err(|e| anyhow!("reading .mrpack body failed: {e}"))?;
        std::fs::write(&tmp, &bytes)
            .map_err(|e| anyhow!("writing temp .mrpack failed: {e}"))?;

        let parsed = read_mrpack(&tmp);
        // Always clean up the temp file, success or failure.
        let _ = std::fs::remove_file(&tmp);
        let pack = parsed.map_err(|e| anyhow!("parsing .mrpack failed: {e:#}"))?;

        let mods: Vec<Value> = pack
            .mods
            .iter()
            .take(80)
            .map(|m| {
                json!({
                    "name": m.name,
                    "download_url": m.download_url,
                })
            })
            .collect();

        Ok(serde_json::to_string(&json!({
            "name": pack.name,
            "mc_version": pack.mc_version,
            "loader": pack.loader,
            "loader_version": pack.loader_version,
            "mod_count": pack.mods.len(),
            "mods": mods,
        }))?)
    }

    let result = match inner(mr, query).await {
        Ok(s) => s,
        Err(e) => format!(
            "seed_from_pack failed: {e:#}. You can recover: use search_mods to build the pack from scratch instead."
        ),
    };
    tool_chip(tx, "seed_from_pack", "done");
    Ok(result)
}

/// Model-authored custom origins. Mirrors `tool_generate_quests`'s
/// propose -> validate -> (recoverable issues) -> retry shape; the
/// `run_turn` round loop (bounded by MAX_TOOL_ROUNDS) IS the repair loop, so
/// a failed proposal returns structured `{kind,where,why,hint}` issues rather
/// than erroring. A single authored set per pack (REPLACES on re-call).
async fn tool_generate_origins(
    thread_id: Option<&str>,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let instance_id = str_field(input, "instance_id")?.to_string();
    let origins_val = input
        .get("origins")
        .cloned()
        .ok_or_else(|| anyhow!("missing required object field 'origins'"))?;

    let _ = tx.send(CuratorEvent::Phase("progression".to_string()));
    tool_chip(tx, "generate_origins", "validating");

    let set: crate::origins::OriginsSet = match serde_json::from_value(origins_val) {
        Ok(s) => s,
        Err(e) => {
            return Ok(format!(
                "generate_origins: the `origins` JSON did not match the expected shape: {e}. \
                 Each origin needs id/name/description/powers/icon/impact(int 0-3)/order; \
                 each power needs id/name/description/type(+optional body). Fix and call again."
            ));
        }
    };

    let Some(inst) = load_instances().into_iter().find(|i| i.id == instance_id) else {
        return Ok(format!(
            "generate_origins: no instance found with id {instance_id}. Assemble the pack first."
        ));
    };

    // Gate: Origins core + Open Loader (the datapack lives under
    // config/openloader/data and its powers are Apoli powers core registers).
    let has_core = inst
        .mods
        .iter()
        .any(|m| crate::origins::is_origins_core(&m.project_id, &m.name));
    let has_open_loader = inst.mods.iter().any(|m| {
        let needle = |s: &str| {
            let s = s.to_lowercase();
            s.contains("open-loader") || s.contains("openloader")
        };
        needle(&m.project_id) || needle(&m.name) || needle(&m.path)
    });
    if !has_core {
        return Ok(format!(
            "generate_origins: instance {instance_id} does not pin Origins core, so a custom \
             origins datapack would do nothing. Do not call generate_origins for this pack."
        ));
    }
    if !has_open_loader {
        return Ok(format!(
            "generate_origins: instance {instance_id} has Origins but not Open Loader, so the \
             origins datapack (config/openloader/data/...) would never load. Recover: search_mods \
             for \"open-loader\", add it (1.20.1 fabric), validate_pack then assemble_pack again \
             with the SAME pack name, then call generate_origins again."
        ));
    }

    // THE GATE. On failure nothing is written; return the structured repair
    // payload so the next round converges (run_turn bounds the retries).
    let validated = match crate::origins::validate(set) {
        Ok(v) => v,
        Err(errs) => {
            tool_chip(tx, "generate_origins", "blocked: invalid");
            return Ok(format!(
                "generate_origins refused to write: {} issue(s). Fix EXACTLY these and call \
                 generate_origins again with the corrected full set:\n{}",
                errs.len(),
                serde_json::to_string(&crate::origins::errors_to_json(&errs))?
            ));
        }
    };

    crate::origins::write_validated_origins(
        &instance_dir(&instance_id),
        "anvil",
        &validated,
    )
    .with_context(|| format!("writing authored origins for instance {instance_id}"))?;
    if let Some(tid) = thread_id {
        crate::chat::mark_origins_authored(tid);
    }

    let s = validated.get();
    tool_chip(tx, "generate_origins", "done");
    Ok(format!(
        "generate_origins: wrote {} custom origin(s) and {} power file(s) to instance \
         {instance_id}. The Origins datapack is valid and fully replaces any prior set. \
         Do not call generate_origins again unless revising the whole set.",
        s.origins.len(),
        s.powers.len()
    ))
}

/// The dir-scoped on-disk side of a successful edit: (a) prune jars no longer
/// in the closure, (b) invalidate the stale grounding cache, (d) rewrite the
/// `.mrpack`. Returns the updated `Instance` (mods + roots applied) for the
/// caller to `save_instance`. Pure w.r.t. the global instances dir (everything
/// is under the passed `dir`) so it is tempdir-testable.
///
/// (a) matters: the real launch path only ENSURES manifest jars — it never
/// deletes one that left the manifest, so without this prune a removed mod
/// would still load. Anvil fully owns `<inst>/mods/` (the manifest is the only
/// source of truth) and launch re-materializes from cache, so a filename
/// reconcile here is safe and self-healing (also clears a version-bumped mod's
/// stale old jar).
pub(crate) fn apply_edit_writes(
    dir: &std::path::Path,
    base: &Instance,
    result: &EditResult,
) -> std::io::Result<Instance> {
    use std::collections::HashSet;

    // (a) prune.
    let keep: HashSet<String> = result
        .entries
        .iter()
        .filter_map(|e| {
            std::path::Path::new(&e.path)
                .file_name()
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })
        .collect();
    if let Ok(rd) = std::fs::read_dir(dir.join("mods")) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|s| s.to_str()) == Some("jar") {
                if let Some(f) = p.file_name().and_then(|s| s.to_str()) {
                    if !keep.contains(f) {
                        let _ = std::fs::remove_file(&p);
                    }
                }
            }
        }
    }

    // (b) invalidate stale grounding cache.
    let _ = std::fs::remove_file(dir.join("anvil-registry.json"));

    // (c) build the updated instance (caller persists it).
    let mut updated = base.clone();
    updated.mods = result
        .entries
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
    updated.roots = result.roots.clone();

    // (d) rewrite the .mrpack so launch/export stay consistent.
    std::fs::create_dir_all(dir)?;
    let mrpack_path = dir.join(format!("{}.mrpack", updated.name));
    let meta = PackMeta {
        name: updated.name.clone(),
        version_id: "1.0.0".to_string(),
        summary: updated.name.clone(),
        mc_version: updated.mc_version.clone(),
        loader_key: loader_key(&updated.loader),
        loader_version: updated.loader_version.clone(),
    };
    write_mrpack(&meta, &result.entries, &mrpack_path).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
    })?;

    Ok(updated)
}

/// Safely add/remove mods on an already-assembled instance. Mirrors
/// `tool_generate_origins`: parse → find instance → safe core → on failure a
/// RECOVERABLE `Ok(string)` (the run_turn round loop is the repair loop) → on
/// success do only safe local writes in a fixed order, then re-dump grounding.
async fn tool_edit_pack(
    mr: &Modrinth,
    _thread_id: Option<&str>,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let instance_id = str_field(input, "instance_id")?.to_string();

    // add[]: {project_id, version_id?}. Bad shape => recoverable, not a hard
    // error (the model fixes it and retries — same contract as generate_origins).
    let mut add: Vec<ModRef> = Vec::new();
    if let Some(arr) = input.get("add").and_then(Value::as_array) {
        for item in arr {
            let Some(pid) = item.get("project_id").and_then(Value::as_str)
            else {
                return Ok("edit_pack: every `add` entry needs a string \
                     `project_id` (and an optional `version_id`). Fix and \
                     call edit_pack again."
                    .to_string());
            };
            add.push(ModRef {
                project_id: pid.to_string(),
                version_id: item
                    .get("version_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    let remove: Vec<String> = input
        .get("remove")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if add.is_empty() && remove.is_empty() {
        return Ok("edit_pack: nothing to do — give at least one `add` \
             ([{project_id, version_id?}]) or one `remove` ([project_id])."
            .to_string());
    }

    let _ = tx.send(CuratorEvent::Phase("assembled".to_string()));
    tool_chip(tx, "edit_pack", "resolving");

    let Some(inst) =
        load_instances().into_iter().find(|i| i.id == instance_id)
    else {
        return Ok(format!(
            "edit_pack: no instance found with id {instance_id}. Assemble \
             the pack first."
        ));
    };

    let result = match edit_instance_mods(mr, &inst, &add, &remove).await {
        Ok(r) => r,
        Err(EditError::Resolve(e)) => {
            tool_chip(tx, "edit_pack", "blocked: resolve failed");
            return Ok(format!(
                "edit_pack: could not resolve the new mod set ({e}). This is \
                 usually a transient Modrinth/network issue — tell the player \
                 and try again shortly, or pick a different mod. Nothing was \
                 changed."
            ));
        }
        Err(EditError::StillRequired(items)) => {
            tool_chip(tx, "edit_pack", "blocked: still required");
            let lines: Vec<String> = items
                .iter()
                .map(|s| {
                    if s.required_by.is_empty() {
                        format!(
                            "- {} is still pulled in as a dependency of \
                             other mods in this pack (jar metadata could not \
                             name the exact requirer); it cannot be removed \
                             on its own.",
                            s.label
                        )
                    } else {
                        format!(
                            "- {} is still required by: {}. To remove {}, \
                             also remove those mods in the SAME edit_pack \
                             call.",
                            s.label,
                            s.required_by.join(", "),
                            s.label
                        )
                    }
                })
                .collect();
            return Ok(format!(
                "edit_pack refused: {} removal(s) would break the pack \
                 (nothing was changed):\n{}\nEither keep these mods, or call \
                 edit_pack again also removing the requiring mods.",
                items.len(),
                lines.join("\n")
            ));
        }
        Err(EditError::Conflicts(issues)) => {
            tool_chip(tx, "edit_pack", "blocked: conflicts");
            return Ok(format!(
                "edit_pack refused: the requested change introduces {} \
                 blocking conflict(s) (nothing was changed). Choose a \
                 compatible mod/version or drop the conflicting add, then \
                 call edit_pack again:\n{}",
                issues.len(),
                serde_json::to_string(&issues)?
            ));
        }
    };

    if result.noop {
        tool_chip(tx, "edit_pack", "no change");
        return Ok(format!(
            "edit_pack: no change — the resolved mod set for instance \
             {instance_id} is identical to what it already has (the add(s) \
             were already present and/or the remove(s) were not roots). \
             Nothing was written."
        ));
    }

    tool_chip(tx, "edit_pack", "writing");
    let dir = instance_dir(&instance_id);
    let updated = apply_edit_writes(&dir, &inst, &result)
        .with_context(|| format!("applying edit to instance {instance_id}"))?;
    save_instance(&updated)
        .with_context(|| format!("saving instance {instance_id}"))?;

    // (e) Refresh the grounding cache for the NEW pin set (detached).
    spawn_registry_dump_detached(
        instance_id.clone(),
        updated.clone(),
        mr.clone(),
    );

    tool_chip(tx, "edit_pack", "done");

    let mut parts: Vec<String> = Vec::new();
    if !result.added.is_empty() {
        parts.push(format!("added {}", result.added.join(", ")));
    }
    if !result.pulled_deps.is_empty() {
        parts.push(format!(
            "pulled {} required dep(s): {}",
            result.pulled_deps.len(),
            result.pulled_deps.join(", ")
        ));
    }
    if !result.removed.is_empty() {
        parts.push(format!("removed {}", result.removed.join(", ")));
    }
    if !result.pruned_orphans.is_empty() {
        parts.push(format!(
            "auto-pruned {} now-unused dep(s): {}",
            result.pruned_orphans.len(),
            result.pruned_orphans.join(", ")
        ));
    }
    Ok(format!(
        "edit_pack: instance {instance_id} now has {} mods ({}). Instance, \
         .mrpack and grounding cache updated. Tell the player exactly what \
         changed and why — especially any auto-pulled deps or pruned mods.",
        updated.mods.len(),
        if parts.is_empty() {
            "version re-pin".to_string()
        } else {
            parts.join("; ")
        }
    ))
}

// ---------------------------------------------------------------------------
// UI-facing safe edit. `tool_edit_pack` is the agent path (recoverable strings
// the run_turn loop repairs); the Instances screen's add/remove "X" needs the
// SAME safe core but with a STRUCTURED error the frontend can branch on. This
// is that adapter: identical success-path order to `tool_edit_pack`, with the
// `EditError` mapped to a serde-tagged `ApplyEditError` (the cross-team JSON
// contract the parallel frontend is built against — do not change the shape).
// ---------------------------------------------------------------------------

/// One refused removal, view-modelled for the frontend. Mirror of the internal
/// `StillRequired` (kept separate so the wire shape is owned by this contract,
/// not by an internal type's incidental layout).
#[derive(Debug, Serialize)]
pub(crate) struct StillRequiredView {
    pub label: String,
    pub required_by: Vec<String>,
}

/// Structured failure for the UI add/remove commands. Internally-tagged on
/// `kind` (snake_case) — Tauri requires `Err: Serialize`; the frontend matches
/// on `kind`. EXACT shapes (locked cross-team contract):
/// - `{"kind":"still_required","items":[{"label":"..","required_by":[".."]}]}`
/// - `{"kind":"conflicts","issues":[<ValidationIssue>...]}`
/// - `{"kind":"resolve","message":".."}`
/// - `{"kind":"not_found","instance_id":".."}`
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ApplyEditError {
    /// A requested removal is still required by other kept mods.
    StillRequired { items: Vec<StillRequiredView> },
    /// The resolved set has blocking conflicts (bad add / unresolved dep).
    Conflicts { issues: Vec<pack::ValidationIssue> },
    /// Resolution / write / save failed (transient or I/O).
    Resolve { message: String },
    /// No instance with this id.
    NotFound { instance_id: String },
}

/// UI-facing safe pack edit: same safe core + same on-disk write order as
/// `tool_edit_pack`, returning the updated `Instance` or a structured
/// `ApplyEditError`. `add` is `[(project_id, version_id?)]`: a `None`
/// `version_id` lets the resolver pick the best compatible build (an empty
/// `version_id` string is the in-core "auto-pick" sentinel — same as the
/// agent path). The naive lib.rs append/retain commands are the exact bug
/// class this routes around: add is dependency-complete + conflict-gated,
/// remove is reverse-dependency safe + prunes the on-disk jar.
pub(crate) async fn apply_pack_edit(
    mr: &Modrinth,
    instance_id: &str,
    add: &[(String, Option<String>)],
    remove: &[String],
) -> Result<Instance, ApplyEditError> {
    let Some(inst) =
        load_instances().into_iter().find(|i| i.id == instance_id)
    else {
        return Err(ApplyEditError::NotFound {
            instance_id: instance_id.to_string(),
        });
    };

    let add_refs: Vec<ModRef> = add
        .iter()
        .map(|(pid, vid)| ModRef {
            project_id: pid.clone(),
            version_id: vid.clone().unwrap_or_default(),
        })
        .collect();

    let result = match edit_instance_mods(mr, &inst, &add_refs, remove).await {
        Ok(r) => r,
        Err(EditError::StillRequired(v)) => {
            return Err(ApplyEditError::StillRequired {
                items: v
                    .into_iter()
                    .map(|s| StillRequiredView {
                        label: s.label,
                        required_by: s.required_by,
                    })
                    .collect(),
            });
        }
        Err(EditError::Conflicts(issues)) => {
            return Err(ApplyEditError::Conflicts { issues });
        }
        Err(EditError::Resolve(message)) => {
            return Err(ApplyEditError::Resolve { message });
        }
    };

    // noop: the resolved closure + roots are unchanged. Nothing to write,
    // no registry re-dump — return the instance as-is.
    if result.noop {
        return Ok(inst);
    }

    // Same fixed order as `tool_edit_pack`'s success path: dir-scoped writes
    // (jar prune + grounding-cache invalidation + .mrpack rewrite) → persist
    // the instance → detached grounding re-dump for the new pin set.
    let dir = instance_dir(instance_id);
    let updated = apply_edit_writes(&dir, &inst, &result)
        .map_err(|e| ApplyEditError::Resolve { message: e.to_string() })?;
    save_instance(&updated)
        .map_err(|e| ApplyEditError::Resolve { message: e.to_string() })?;
    spawn_registry_dump_detached(
        instance_id.to_string(),
        updated.clone(),
        mr.clone(),
    );

    Ok(updated)
}

async fn tool_generate_quests(
    thread_id: Option<&str>,
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let instance_id = str_field(input, "instance_id")?.to_string();
    let graph_val = input
        .get("graph")
        .cloned()
        .ok_or_else(|| anyhow!("missing required object field 'graph'"))?;
    // Incremental build: the model submits a few chapters per call and we
    // accumulate. The quality/interconnection gate only runs on the final
    // call, so partial progress is never rejected for "looking sparse".
    let is_final = input
        .get("final")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let _ = tx.send(CuratorEvent::Phase("progression".to_string()));
    tool_chip(tx, "generate_quests", "validating");

    let submitted: QuestGraph = match serde_json::from_value(graph_val) {
        Ok(g) => g,
        Err(e) => {
            return Ok(format!(
                "generate_quests: the graph JSON did not match the expected shape: {e}. \
                 Fix the graph and call generate_quests again."
            ));
        }
    };

    // Merge into any questline already built for this instance: a submitted
    // chapter replaces an existing one with the same id, new chapters append,
    // so deps can reference quests added in earlier calls.
    let mut graph = load_graph(&instance_dir(&instance_id)).unwrap_or(QuestGraph {
        title: submitted.title.clone(),
        chapters: Vec::new(),
    });
    if !submitted.title.trim().is_empty() {
        graph.title = submitted.title.clone();
    }
    // Dedupe by id OR title (not just the model-supplied id) so a retry that
    // re-emits the same chapter under a fresh id REPLACES it instead of
    // appending a visible duplicate (gap #11).
    crate::quest::merge_chapters(&mut graph.chapters, submitted.chapters);

    // Find the assembled instance.
    let instances = load_instances();
    let Some(inst) = instances.into_iter().find(|i| i.id == instance_id) else {
        return Ok(format!(
            "generate_quests: no instance found with id {instance_id}. \
             Call assemble_pack first and use the instance id from its result."
        ));
    };

    // Slice 2: recipes are a quest-node FACET. If ANY node in the MERGED graph
    // carries a `recipes` array, the pack MUST ship Open Loader or the
    // datapack would never load (the same presence-gate pattern as
    // quests-require-odyssey-quests). Match by Modrinth project id OR a
    // path/name containing open-loader/openloader (verbatim the old recipe
    // path's detector). Recoverable string (never an aborting error).
    let any_recipes = graph
        .chapters
        .iter()
        .any(|c| c.quests.iter().any(|q| !q.recipes.is_empty()));
    // Slice 3: a content facet ALSO writes into an Open Loader datapack (the
    // sibling `anvil-content` pack), so the SAME presence-gate fires when any
    // node has a content facet (extends the recipe gate, same recovery path).
    let any_content = crate::quest::any_content(&graph);
    if any_recipes || any_content {
        let has_open_loader = inst.mods.iter().any(|m| {
            let needle = |s: &str| {
                let s = s.to_lowercase();
                s.contains("open-loader") || s.contains("openloader")
            };
            needle(&m.project_id) || needle(&m.name) || needle(&m.path)
        });
        if !has_open_loader {
            tool_chip(tx, "generate_quests", "blocked: open-loader missing");
            return Ok(format!(
                "generate_quests: instance {instance_id} has recipe- or content-facet quest \
                 node(s) but does not include Open Loader, so the custom-recipe / provisioned-\
                 content datapack would never load. Recover: search_mods for \"open-loader\", \
                 add it to the pack (1.20.1 fabric/forge), call validate_pack then assemble_pack \
                 again with the SAME pack name to update this instance in place, then call \
                 generate_quests again."
            ));
        }
    }

    // Slice 1: CONCRETE-id grounding against the pack's real registry,
    // scanned (and cached) from the resolved jars. Slice 2: the Anvil-authored
    // allowlist (tier 2) is seeded with EVERY node-recipe's DERIVED
    // `anvil:<hex>` id so the recipe quest's auto item-on-result task and any
    // self-reference ground cleanly even before the datapack is written. The
    // `anvil` namespace is authored-by-construction regardless; this makes the
    // exact ids explicit and is the seam Slice 3 reuses for boss/site/gate ids.
    // Prefetch the pinned jars so grounding scans the pack's REAL registry,
    // not the (pre-launch) empty state that made the model fall back to
    // memory ids. Same idempotent, resilient, cache-busting prefetch
    // query_registry uses. MUST run before build_index_for_instance.
    let dl = reqwest::Client::builder()
        .user_agent("anvil/0.1.0 (registry-prefetch)")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    ensure_mod_jars(&dl, &inst, &instance_dir(&instance_id), tx, "generate_quests").await;

    // Slice 2 + 3: the Anvil-authored allowlist (tier 2) is seeded with EVERY
    // node-recipe's DERIVED `anvil:<hex>` id AND every content facet's derived
    // `anvil:<hex>_*` ids, so the recipe quest's auto item-on-result task, the
    // content quest's auto GatherItem-on-token task, and any internal
    // cross-reference ground cleanly even before the datapacks are written.
    let mut authored: Vec<String> = crate::quest::authored_recipe_ids(&graph);
    authored.extend(crate::quest::authored_content_ids(&graph));
    let idx = build_index_for_instance(
        &inst,
        &instance_dir(&instance_id),
        authored,
    );
    let mut issues = validate_graph(&graph, &idx);
    // Hard correctness (concrete grounding, missing deps, cycles) is enforced
    // on every call. The quality/interconnection gate only applies on the
    // final call. `LowConfidenceId` is NEVER write-blocking (a mod jar not on
    // disk yet, or namespace-only fallback): it is surfaced to the model in
    // the success message, not gated, so jar-absence degrades and never
    // blocks (design §2).
    let low_conf: Vec<QuestIssue> = issues
        .iter()
        .filter(|i| matches!(i, QuestIssue::LowConfidenceId { .. }))
        .cloned()
        .collect();
    issues.retain(|i| !matches!(i, QuestIssue::LowConfidenceId { .. }));
    if !is_final {
        // Quality/interconnection gate is final-only. Slice 2 adds
        // `RecipeQuality` (orphan recipe / no-mod-output) to the same
        // final-only set; `RecipeStructural` is HARD and stays every call.
        issues.retain(|i| {
            !matches!(
                i,
                QuestIssue::OrphanQuest { .. }
                    | QuestIssue::DisconnectedChapter { .. }
                    | QuestIssue::TooSparse { .. }
                    | QuestIssue::RecipeQuality { .. }
            )
        });
    }
    if !issues.is_empty() {
        // GATE: never write an invalid graph. Return the issues so the model
        // can repair the graph and retry.
        tool_chip(tx, "generate_quests", "blocked: validation failed");
        return Ok(format!(
            "generate_quests refused to write: {} issue(s). Fix these and call generate_quests again:\n{}",
            issues.len(),
            serde_json::to_string(&issues)?
        ));
    }

    write_quests(&graph, &instance_dir(&instance_id))
        .with_context(|| format!("writing quests for instance {instance_id}"))?;

    // Emit the DETERMINISTIC rescue Origins datapack IFF this pack runs
    // Origins core + Open Loader — UNLESS the model already authored a
    // bespoke, validated set for this thread via generate_origins (in which
    // case re-running the rescue write would clobber it). The rescue write is
    // the fallback for packs where the model did not author origins; a model-
    // authored set is written by tool_generate_origins itself. Non-Origins
    // packs stay byte-for-byte unchanged.
    let has_origins_core = inst
        .mods
        .iter()
        .any(|m| crate::origins::is_origins_core(&m.project_id, &m.name));
    let has_open_loader = inst.mods.iter().any(|m| {
        let needle = |s: &str| {
            let s = s.to_lowercase();
            s.contains("open-loader") || s.contains("openloader")
        };
        needle(&m.project_id) || needle(&m.name) || needle(&m.path)
    });
    let authored = thread_id.is_some_and(crate::chat::origins_authored);
    if has_origins_core && has_open_loader && !authored {
        crate::origins::write_origins_datapack(
            &instance_dir(&instance_id),
            "anvil",
        )
        .with_context(|| {
            format!("writing origins datapack for instance {instance_id}")
        })?;
        tool_chip(tx, "generate_quests", "origins datapack written");
    }

    let chapter_count = graph.chapters.len();
    let quest_count: usize = graph.chapters.iter().map(|c| c.quests.len()).sum();
    tool_chip(
        tx,
        "generate_quests",
        &format!("{quest_count} quests so far"),
    );

    // Surface (do NOT gate) low-confidence ids: ones accepted because their
    // mod jar was not on disk at scan time, or because the pack is in
    // namespace-only fallback. The model should `query_registry` to confirm
    // these against the real registry rather than trust recall.
    let warn = if low_conf.is_empty() {
        String::new()
    } else {
        let n = low_conf.len();
        let sample = serde_json::to_string(
            &low_conf.iter().take(8).collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        format!(
            " NOTE: {n} id(s) were accepted UNVERIFIED (mod jar absent at scan \
             time or namespace-only fallback) — they may not exist in-game. \
             Use query_registry to confirm real ids: {sample}"
        )
    };

    if is_final {
        let _ = tx.send(CuratorEvent::Phase("complete".to_string()));
        Ok(format!(
            "Questline complete: {quest_count} quests across {chapter_count} chapter(s) written to instance {instance_id}.{warn}"
        ))
    } else {
        Ok(format!(
            "Saved. The questline now has {quest_count} quests across {chapter_count} chapter(s). \
             Continue with more generate_quests calls (a few chapters each, in progression order), \
             then make a final call with \"final\": true to run the full quality check.{warn}"
        ))
    }
}

/// LOCKED signature (design-doc §8): `query_registry(kind, filter:{namespace?,
/// contains?, mod?})` → `[{id, label, source_mod}]`, paginated (the locked
/// spec explicitly says "cap N, offset"). Read-only over the Slice-1
/// `RegistryVocab` + Anvil-authored allowlist. Reuses the same scanned+cached
/// index as `tool_generate_quests` (built lazily, persisted at
/// `<instance>/anvil-registry.json`) so repeated calls are cheap.
///
/// Pagination is the locked `cap N + offset`: a top-level `offset` (int,
/// default 0) over a hard 50-result cap, so the model can walk a large
/// registry without blowing the context window. The locked `filter` shape
/// (`{namespace?, contains?, mod?}`) is exactly as specified — no extra
/// filter fields.
const QUERY_REGISTRY_CAP: usize = 50;

/// Pure filter + paginate over one already-selected vocab set. `set` is a
/// `BTreeSet` so iteration is sorted — paging is deterministic and stable.
/// Returns `(page_rows, total_matched)`; `total` counts ALL matches (so the
/// caller can report "N of M" + whether more pages exist).
#[allow(clippy::too_many_arguments)]
fn query_vocab(
    set: &std::collections::BTreeSet<String>,
    vocab: &crate::registry::RegistryVocab,
    mod_name: &std::collections::HashMap<String, String>,
    f_ns: Option<&str>,
    f_contains: Option<&str>,
    f_mod_ns: Option<&str>,
    offset: usize,
    cap: usize,
) -> (Vec<Value>, usize) {
    let ns_of = |id: &str| -> String {
        id.split_once(':')
            .map(|(n, _)| n.to_string())
            .unwrap_or_else(|| "minecraft".to_string())
    };
    let mut matched: Vec<Value> = Vec::new();
    let mut total = 0usize;
    for id in set.iter() {
        let ns = ns_of(id);
        if let Some(want) = f_ns {
            if ns != want {
                continue;
            }
        }
        if let Some(want) = f_mod_ns {
            if ns != want {
                continue;
            }
        }
        let label = vocab.labels.get(id).cloned().unwrap_or_default();
        if let Some(sub) = f_contains {
            if !id.to_lowercase().contains(sub)
                && !label.to_lowercase().contains(sub)
            {
                continue;
            }
        }
        total += 1;
        if total <= offset || matched.len() >= cap {
            continue;
        }
        let source_mod =
            mod_name.get(&ns).cloned().unwrap_or_else(|| ns.clone());
        matched.push(json!({
            "id": id,
            "label": label,
            "source_mod": source_mod,
        }));
    }
    (matched, total)
}

// ---------------------------------------------------------------------------
// verify_pack: bounded post-assembly smoke + crash analyst
// ---------------------------------------------------------------------------

const ANALYST_MODEL: &str = "claude-sonnet-4-6";

/// One focused, non-streaming Anthropic call: read a crash report + the pack's
/// mod list, name the single culprit and the fix. Returns a short human
/// paragraph (already phrased for the curator to relay). This is the
/// diagnosis layer the brittle regex classifier must NOT be — the classifier
/// only decides *when* the boot failed; this decides *what* and *why*.
async fn analyze_crash(
    api_key: &str,
    crash: &str,
    mods: &[String],
) -> anyhow::Result<String> {
    // Crash reports run long; the cause is in the exception chain near the
    // end. Keep the tail.
    let tail: String = if crash.len() > 12_000 {
        crash[crash.len() - 12_000..].to_string()
    } else {
        crash.to_string()
    };
    let sys = "You are a Minecraft Fabric 1.20.1 crash analyst. Given a crash \
        log and the pack's mod list, identify the SINGLE most likely culprit \
        mod and the root cause. Reply with ONLY a JSON object, no prose: \
        {\"culprit_mod\":\"<id or name from the list, or empty>\",\
        \"root_class\":\"missing_dep|version_break|runtime_mixin|other\",\
        \"one_line\":\"<one sentence, plain>\",\
        \"recommendation\":\"<the concrete fix, e.g. remove <mod>, or pin \
        <lib> older>\"}";
    let user = format!(
        "Mod list ({} mods): {}\n\n---- crash ----\n{}",
        mods.len(),
        mods.join(", "),
        tail
    );
    let http = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(20))
        .build()
        .context("building HTTP client for crash analyst")?;
    let body = json!({
        "model": ANALYST_MODEL,
        "max_tokens": 700,
        "system": sys,
        "messages": [{ "role": "user", "content": user }],
    });
    let resp = http
        .post(ANTHROPIC_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("crash analyst request")?;
    if !resp.status().is_success() {
        return Err(anyhow!("crash analyst HTTP {}", resp.status()));
    }
    let v: Value = resp.json().await.context("crash analyst JSON")?;
    let text = v["content"][0]["text"].as_str().unwrap_or("").to_string();
    Ok(format_analyst(&text))
}

/// Pull the JSON object out of the model's reply (tolerant of stray prose)
/// and render it as the curator-facing, surface-and-wait instruction.
fn format_analyst(text: &str) -> String {
    let parsed = text
        .find('{')
        .and_then(|s| text.rfind('}').map(|e| &text[s..=e]))
        .and_then(|j| serde_json::from_str::<Value>(j).ok());
    let Some(p) = parsed else {
        return format!(
            "Automated diagnosis (unstructured): {}",
            text.trim()
        );
    };
    let culprit = p["culprit_mod"].as_str().unwrap_or("").trim().to_string();
    let class = p["root_class"].as_str().unwrap_or("other");
    let one = p["one_line"].as_str().unwrap_or("").trim();
    let rec = p["recommendation"].as_str().unwrap_or("").trim();
    format!(
        "CRASH ANALYSIS — culprit: {}; class: {}. {} Recommended fix: {}\n\
         SURFACE-AND-WAIT: tell the player this in plain words and ASK whether \
         to apply the fix. Do NOT modify the pack unless they say yes. If they \
         approve removing a mod, call assemble_pack again with the SAME pack \
         name and the mod list minus that mod, then call verify_pack ONE more \
         time. Do this at most once — never loop verify/assemble.",
        if culprit.is_empty() { "unclear" } else { &culprit },
        class,
        one,
        rec
    )
}

async fn tool_verify_pack(
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let instance_id = str_field(input, "instance_id")?.to_string();
    let inst = load_instances()
        .into_iter()
        .find(|i| i.id == instance_id)
        .ok_or_else(|| {
            anyhow!(
                "verify_pack: no assembled instance {instance_id}. \
                 Call assemble_pack first and use the id it returns."
            )
        })?;

    // Mod-init does not need a real session (spike-verified); a dummy offline
    // identity lets verify run before the player has signed in.
    let account =
        crate::auth::load_account().unwrap_or_else(crate::auth::offline_account);

    tool_chip(tx, "verify_pack", "booting the pack once (~1 min)");
    let (ltx, mut lrx) =
        tokio::sync::mpsc::unbounded_channel::<crate::launch::LaunchEvent>();
    let tx2 = tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = lrx.recv().await {
            if let crate::launch::LaunchEvent::Status(s) = ev {
                tool_chip(&tx2, "verify_pack", &s);
            }
        }
    });

    let verdict = crate::launch::smoke_test(&inst, &account, None, ltx)
        .await
        .map_err(|e| anyhow!("verify_pack could not boot the pack: {e:#}"))?;

    match verdict {
        crate::launch::SmokeVerdict::Ok => {
            tool_chip(tx, "verify_pack", "clean");
            Ok("VERIFIED: the pack booted and every mod initialized cleanly. \
                Tell the player the pack is confirmed working."
                .to_string())
        }
        crate::launch::SmokeVerdict::Inconclusive { reason } => {
            tool_chip(tx, "verify_pack", "inconclusive");
            Ok(format!(
                "INCONCLUSIVE: {reason}. Could not confirm in time — tell the \
                 player it is probably fine but they may want to launch once."
            ))
        }
        crate::launch::SmokeVerdict::Failed { mod_name, reason } => {
            tool_chip(tx, "verify_pack", "failed — analyzing");
            // Prefer the full crash report; fall back to the log reason.
            let crash = newest_crash_report(&instance_id).unwrap_or_else(|| {
                format!(
                    "{}{}",
                    mod_name
                        .as_ref()
                        .map(|m| format!("mod: {m}\n"))
                        .unwrap_or_default(),
                    reason
                )
            });
            let mods: Vec<String> =
                inst.mods.iter().map(|m| m.name.clone()).collect();
            match crate::settings::anthropic_key() {
                Some(key) => match analyze_crash(&key, &crash, &mods).await {
                    Ok(diag) => Ok(format!("VERIFICATION FAILED.\n{diag}")),
                    Err(e) => Ok(format!(
                        "VERIFICATION FAILED: {reason}. (Automated analysis \
                         unavailable: {e}.) Tell the player which mod the log \
                         names and ASK before changing anything."
                    )),
                },
                None => Ok(format!(
                    "VERIFICATION FAILED: {reason}. Tell the player and ASK \
                     before changing the pack (no Anthropic key for an \
                     automated diagnosis)."
                )),
            }
        }
    }
}

/// Newest `crash-reports/*.txt` for an instance, if any.
fn newest_crash_report(instance_id: &str) -> Option<String> {
    let dir = instance_dir(instance_id).join("crash-reports");
    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "txt") {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                if newest.as_ref().map(|(t, _)| m > *t).unwrap_or(true) {
                    newest = Some((m, p));
                }
            }
        }
    }
    std::fs::read_to_string(newest?.1).ok()
}

async fn tool_query_registry(
    input: &Value,
    tx: &UnboundedSender<CuratorEvent>,
) -> anyhow::Result<String> {
    let instance_id = str_field(input, "instance_id")?.to_string();
    let kind = str_field(input, "kind")?.to_string();
    let filter = input.get("filter").cloned().unwrap_or(json!({}));
    let f_ns = opt_str_field(&filter, "namespace").map(str::to_lowercase);
    let f_contains = opt_str_field(&filter, "contains").map(str::to_lowercase);
    let f_mod = opt_str_field(&filter, "mod").map(str::to_lowercase);
    let offset = input
        .get("offset")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    tool_chip(tx, "query_registry", "scanning registry");

    let instances = load_instances();
    let Some(inst) = instances.into_iter().find(|i| i.id == instance_id) else {
        return Ok(format!(
            "query_registry: no instance found with id {instance_id}. \
             Call assemble_pack first and use the instance id from its result."
        ));
    };

    // Prefetch the pinned jars so the scan below sees real jars, not just the
    // post-launch state. Idempotent (present jars are skipped) and resilient
    // (a failed jar stays unscanned, never blocks). MUST run before
    // build_index_for_instance so the cache reflects the on-disk reality.
    let inst_dir = instance_dir(&instance_id);
    let dl = reqwest::Client::builder()
        .user_agent("anvil/0.1.0 (registry-prefetch)")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    ensure_mod_jars(&dl, &inst, &inst_dir, tx, "query_registry").await;

    // Same scanned + cached index the grounding path uses (anvil-registry.json).
    let idx = build_index_for_instance(
        &inst,
        &inst_dir,
        Vec::<String>::new(),
    );
    let v = &idx.vocab;

    // mod id (lowercased) -> display name, for the source_mod column.
    let mut mod_name: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for mm in &v.mod_meta {
        mod_name.insert(mm.id.to_lowercase(), mm.name.clone());
    }
    // Resolve the `mod` filter to a namespace if it names a known mod.
    let f_mod_ns: Option<String> = f_mod.as_ref().map(|needle| {
        v.mod_meta
            .iter()
            .find(|mm| {
                mm.id.to_lowercase() == *needle
                    || mm.name.to_lowercase() == *needle
            })
            .map(|mm| mm.id.to_lowercase())
            .unwrap_or_else(|| needle.clone())
    });

    let set: &std::collections::BTreeSet<String> = match kind.as_str() {
        "item" => &v.items,
        "entity" => &v.entities,
        "advancement" => &v.advancements,
        "structure" => &v.structures,
        "biome" => &v.biomes,
        "tag" => &v.tags,
        "recipe" => &v.recipe_ids,
        other => {
            return Ok(format!(
                "query_registry: unknown kind '{other}'. Use one of: item, \
                 entity, advancement, structure, biome, tag, recipe."
            ));
        }
    };

    let (matched, total) = query_vocab(
        set,
        v,
        &mod_name,
        f_ns.as_deref(),
        f_contains.as_deref(),
        f_mod_ns.as_deref(),
        offset,
        QUERY_REGISTRY_CAP,
    );

    tool_chip(
        tx,
        "query_registry",
        &format!("{} of {total} match(es)", matched.len()),
    );

    let scanned_note = if idx.has_vocab {
        ""
    } else {
        " (no mod jars are on disk yet, so the real registry could not be \
         scanned — these results are empty; ids will be accepted \
         low-confidence at generation time)"
    };
    let next = offset + matched.len();
    let more = if next < total {
        format!(
            " More available: call again with offset={next} for the next page."
        )
    } else {
        String::new()
    };
    // Compact line table instead of a JSON array of repeated-key objects:
    // ~40-50% fewer tokens, information-identical, trivially parseable, same
    // order. The leading summary + pagination hint are unchanged so the
    // offset contract still holds.
    let table = matched
        .iter()
        .map(|m| {
            let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("");
            format!("{} | {} | {}", s("id"), s("label"), s("source_mod"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    Ok(format!(
        "query_registry {kind}: {} of {total} match(es){scanned_note}.{more}\n\
         id | label | source_mod\n{table}",
        matched.len(),
    ))
}

#[cfg(test)]
mod verify_tests {
    use super::format_analyst;

    #[test]
    fn well_formed_analyst_json_becomes_surface_and_wait_instruction() {
        let raw = r#"Sure, here is the analysis:
        {"culprit_mod":"create_dd","root_class":"version_break",
         "one_line":"create_dd 0.1d targets Create 0.5.x but the pack has Create 6.",
         "recommendation":"Remove create_dd (or pin Create to 0.5.x)."} thanks"#;
        let out = format_analyst(raw);
        assert!(out.contains("culprit: create_dd"));
        assert!(out.contains("class: version_break"));
        assert!(out.contains("Remove create_dd"));
        // The surface-and-wait contract MUST be in the tool result.
        assert!(out.contains("ASK whether to apply"));
        assert!(out.contains("at most once"));
    }

    #[test]
    fn unparseable_reply_falls_back_not_panics() {
        let out = format_analyst("the model rambled with no json");
        assert!(out.starts_with("Automated diagnosis (unstructured):"));
    }
}

#[cfg(test)]
mod keyword_tests {
    use super::brief_keywords;

    #[test]
    fn prose_brief_reduces_to_searchable_content_tokens() {
        // The exact failure shape: a verbose brief Modrinth scored 0 hits on.
        let kw = brief_keywords(
            "a pack where you play as an archetype with specializations and \
             space exploration tech progression and boss fights gating \
             rocket material",
        );
        // Filler is gone; thematic content survives.
        assert!(!kw.contains(&"pack".to_string()));
        assert!(!kw.contains(&"where".to_string()));
        assert!(!kw.contains(&"progression".to_string()));
        assert!(kw.contains(&"space".to_string()));
        assert!(kw.contains(&"exploration".to_string()));
        assert!(kw.contains(&"tech".to_string()));
        // Capped and deduped, never empty for a themed brief.
        assert!(!kw.is_empty() && kw.len() <= 6);
    }

    #[test]
    fn all_filler_brief_yields_empty_so_caller_falls_back() {
        assert!(brief_keywords("a pack of mods you want to play").is_empty());
    }
}

#[cfg(test)]
mod merge_roots_tests {
    use super::*;

    fn r(p: &str, v: &str) -> ModRef {
        ModRef {
            project_id: p.into(),
            version_id: v.into(),
        }
    }

    /// The Starbound Origins failure: "add Open Loader" to a 50-mod pack must
    /// NOT collapse it to one. Existing mods are preserved; the new mod added.
    #[test]
    fn merge_keeps_existing_and_adds_new() {
        let call = vec![r("openloader", "ol1")];
        let existing = vec![r("heracles", "h1"), r("mi", "mi1"), r("bomd", "b1")];
        let out = merge_roots(call, existing, false);
        let pids: Vec<&str> = out.iter().map(|m| m.project_id.as_str()).collect();
        assert!(pids.contains(&"openloader"));
        assert!(pids.contains(&"heracles"));
        assert!(pids.contains(&"mi"));
        assert!(pids.contains(&"bomd"));
        assert_eq!(out.len(), 4);
    }

    /// A call ref for an already-present project wins the collision (a
    /// deliberate version bump / swap still applies), existing version dropped.
    #[test]
    fn call_ref_wins_collision() {
        let call = vec![r("create", "NEW")];
        let existing = vec![r("create", "OLD"), r("jei", "j1")];
        let out = merge_roots(call, existing, false);
        let create = out.iter().find(|m| m.project_id == "create").unwrap();
        assert_eq!(create.version_id, "NEW");
        assert_eq!(out.len(), 2);
    }

    /// `replace: true` is the explicit opt-out — exactly the call set, the only
    /// way to drop a mod.
    #[test]
    fn replace_true_drops_unlisted() {
        let call = vec![r("only", "o1")];
        let existing = vec![r("only", "old"), r("gone", "g1")];
        let out = merge_roots(call, existing, true);
        assert_eq!(out, vec![r("only", "o1")]);
    }

    /// No existing instance (brand-new pack) → just the deduped call set;
    /// empty-project_id entries (imported jars) are filtered (unresolvable).
    #[test]
    fn no_existing_and_empty_pid_filtered() {
        let call = vec![r("a", "1"), r("a", "2"), r("", "x")];
        let out = merge_roots(call, std::iter::empty(), false);
        assert_eq!(out, vec![r("a", "1")]);
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::*;
    use crate::chat::{CandidateMod, CandidatePack};

    fn saved() -> CandidatePack {
        CandidatePack {
            mc_version: "1.20.1".into(),
            loader: "fabric".into(),
            mods: vec![
                CandidateMod {
                    project_id: "p1".into(),
                    version_id: "v1".into(),
                    title: "Sodium".into(),
                },
                CandidateMod {
                    project_id: "p2".into(),
                    version_id: "v2".into(),
                    title: "Iris".into(),
                },
            ],
        }
    }

    #[test]
    fn empty_refs_recover_the_saved_proposal() {
        // The screenshot-1 case: model proposed, lost the list, said
        // "assemble it" with no mods -> assemble the saved set, not nothing.
        let (refs, used_saved) = assemble_refs(vec![], Some(&saved()));
        assert!(used_saved);
        assert_eq!(
            refs,
            vec![
                ModRef {
                    project_id: "p1".into(),
                    version_id: "v1".into()
                },
                ModRef {
                    project_id: "p2".into(),
                    version_id: "v2".into()
                },
            ]
        );
    }

    #[test]
    fn explicit_refs_override_the_saved_proposal() {
        // A swap/drop turn: the model passed an explicit list -> honor it
        // verbatim, never silently substitute the saved set.
        let explicit = vec![ModRef {
            project_id: "x".into(),
            version_id: "y".into(),
        }];
        let (refs, used_saved) = assemble_refs(explicit, Some(&saved()));
        assert!(!used_saved);
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].project_id, "x");
    }

    #[test]
    fn no_refs_and_no_saved_yields_empty_so_caller_errors() {
        // No proposal ever made: assemble must refuse, not assemble garbage.
        let (refs, used_saved) = assemble_refs(vec![], None);
        assert!(used_saved);
        assert!(refs.is_empty());
    }

    #[test]
    fn candidate_pack_round_trips_through_serde() {
        // Guards the persisted sidecar shape (backend-only file the frontend
        // must never need to know about).
        let c = saved();
        let json = serde_json::to_string(&c).unwrap();
        let back: CandidatePack = serde_json::from_str(&json).unwrap();
        assert_eq!(back.mc_version, "1.20.1");
        assert_eq!(back.loader, "fabric");
        assert_eq!(back.mods.len(), 2);
        assert_eq!(back.mods[1].title, "Iris");
    }
}

#[cfg(test)]
mod query_registry_tests {
    use super::*;
    use crate::registry::{ModMeta, RegistryVocab};
    use std::collections::{BTreeSet, HashMap};

    fn vocab() -> (RegistryVocab, HashMap<String, String>) {
        let mut v = RegistryVocab::default();
        // 60 create items + a couple of vanilla, to exercise the 50-cap.
        for i in 0..60 {
            v.items.insert(format!("create:gizmo_{i:02}"));
        }
        v.items.insert("minecraft:diamond".to_string());
        v.items.insert("create:cogwheel".to_string());
        v.labels
            .insert("create:cogwheel".to_string(), "Cogwheel".to_string());
        v.mod_meta.push(ModMeta {
            id: "create".to_string(),
            name: "Create".to_string(),
            categories: vec![],
        });
        let mut mn = HashMap::new();
        mn.insert("create".to_string(), "Create".to_string());
        (v, mn)
    }

    #[test]
    fn query_vocab_filters_and_paginates() {
        let (v, mn) = vocab();
        let set: &BTreeSet<String> = &v.items;

        // namespace filter: only create:* (62), capped at 50, total reported.
        let (page1, total) =
            query_vocab(set, &v, &mn, Some("create"), None, None, 0, 50);
        assert_eq!(total, 61, "60 gizmos + cogwheel, create-namespace only");
        assert_eq!(page1.len(), 50, "hard 50 cap");
        assert_eq!(page1[0]["source_mod"], "Create");

        // page 2 via offset returns the remainder, deterministically.
        let (page2, total2) =
            query_vocab(set, &v, &mn, Some("create"), None, None, 50, 50);
        assert_eq!(total2, 61);
        assert_eq!(page2.len(), 11);
        // Stable, non-overlapping pages (BTreeSet order).
        let last1 = page1.last().unwrap()["id"].as_str().unwrap().to_string();
        let first2 = page2[0]["id"].as_str().unwrap().to_string();
        assert!(first2 > last1, "pages must not overlap and stay ordered");

        // contains filter (matches id OR label, case-insensitive).
        let (cog, n) =
            query_vocab(set, &v, &mn, None, Some("cogwheel"), None, 0, 50);
        assert_eq!(n, 1);
        assert_eq!(cog[0]["id"], "create:cogwheel");
        assert_eq!(cog[0]["label"], "Cogwheel");

        // contains matching the human label only.
        let (cog2, _) =
            query_vocab(set, &v, &mn, None, Some("cogwh"), None, 0, 50);
        assert_eq!(cog2.len(), 1);

        // mod filter resolved to a namespace excludes vanilla.
        let (only_create, tc) =
            query_vocab(set, &v, &mn, None, None, Some("create"), 0, 100);
        assert_eq!(tc, 61);
        assert!(only_create
            .iter()
            .all(|r| r["id"].as_str().unwrap().starts_with("create:")));

        // Determinism: same args -> byte-identical output.
        let a = query_vocab(set, &v, &mn, Some("create"), None, None, 0, 50);
        let b = query_vocab(set, &v, &mn, Some("create"), None, None, 0, 50);
        assert_eq!(
            serde_json::to_string(&a.0).unwrap(),
            serde_json::to_string(&b.0).unwrap()
        );
        assert_eq!(a.1, b.1);
    }
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
                "curating",
                None,
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

/// REAL recorded-data tests for the FLK Kotlin-major floor in the production
/// `resolve_pack` path. These deliberately do NOT hand-build a dual-candidate
/// `pool` (that synthetic shape is exactly what hid the bug from the existing
/// pure-unit test `pack::floors_flk_to_matching_kotlin_major`). Instead they
/// pre-seed `vcache` from a frozen Modrinth snapshot
/// (`tests/fixtures/real/flk_versions_1.20.1.json`) and the real Stellar
/// Origins instance pin list (`tests/fixtures/real/stellar_origins.instance.json`,
/// the actual crash-causing pack the buggy resolver shipped), then drive the
/// real fixpoint + Tier-2 floor offline through `resolve_pack_with_state`.
#[cfg(test)]
mod flk_floor_real_data_tests {
    use super::*;
    use crate::modrinth::Version;

    const FIXT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real");

    /// FLK's real Modrinth project id (matches both the snapshot's
    /// `project_id` and the Stellar Origins root pin).
    const FLK_PID: &str = "Ha28R6CL";
    /// The Kotlin-2.x build Stellar Origins shipped (the crash pin).
    const FLK_K2_VID: &str = "2i87JpYj";

    fn read_fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{FIXT}/{name}"))
            .unwrap_or_else(|e| panic!("read fixture {name}: {e}"))
    }

    /// Real instance roots → `ModRef`s.
    fn stellar_roots() -> Vec<ModRef> {
        let v: serde_json::Value =
            serde_json::from_str(&read_fixture("stellar_origins.instance.json"))
                .expect("parse stellar instance");
        v["mods"]
            .as_array()
            .expect("mods array")
            .iter()
            .map(|m| ModRef {
                project_id: m["project_id"].as_str().unwrap().to_string(),
                version_id: m["version_id"].as_str().unwrap().to_string(),
            })
            .collect()
    }

    /// The recorded REAL FLK version list for 1.20.1/fabric (33 builds, both
    /// Kotlin major 1 and 2), deserialized into the same `Vec<Version>` the
    /// live API returns.
    fn flk_real_versions() -> Vec<Version> {
        serde_json::from_str(&read_fixture("flk_versions_1.20.1.json"))
            .expect("parse recorded FLK snapshot")
    }

    /// A minimal one-element `Vec<Version>` for a non-FLK root: the exact
    /// pinned id, 1.20.1/fabric-compatible, ONE file, NO dependency edges (so
    /// the pure resolver requests nothing further → fully offline). This is
    /// the cache short-circuit seam, NOT a candidate pool — there is exactly
    /// one version per non-floor project, so nothing here can mask a floor.
    fn stub_versions(pid: &str, vid: &str) -> Vec<Version> {
        let j = serde_json::json!([{
            "id": vid,
            "project_id": pid,
            "name": vid,
            "version_number": "1.0.0",
            "game_versions": ["1.20.1"],
            "version_type": "release",
            "loaders": ["fabric"],
            "downloads": 0,
            "date_published": "2024-01-01T00:00:00Z",
            "files": [{
                "hashes": { "sha1": "0".repeat(40), "sha512": "0".repeat(128) },
                "url": format!("https://cdn.modrinth.com/{pid}/{vid}.jar"),
                "filename": format!("{pid}-{vid}.jar"),
                "primary": true,
                "size": 1
            }],
            "dependencies": []
        }]);
        serde_json::from_value(j).expect("stub Version deserializes")
    }

    /// Build the offline state: `vcache` seeded for every root (real snapshot
    /// for FLK, single-version stub for the rest), `scache` seeded for every
    /// pid, `scanned` seeded for every pid so `jar_augment`'s download loop is
    /// a no-op. `manifests` is supplied by the caller (empty = no floor
    /// trigger; real IPN/libIPN `depends` = floor fires).
    fn offline_state(
        roots: &[ModRef],
    ) -> (
        VersionCache,
        SideCache,
        std::collections::HashSet<String>,
    ) {
        let mut vcache: VersionCache = std::collections::HashMap::new();
        let mut scache: SideCache = std::collections::HashMap::new();
        let mut scanned: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for r in roots {
            if r.project_id == FLK_PID {
                vcache
                    .insert(r.project_id.clone(), Ok(flk_real_versions()));
            } else {
                vcache.insert(
                    r.project_id.clone(),
                    Ok(stub_versions(&r.project_id, &r.version_id)),
                );
            }
            scache.insert(
                r.project_id.clone(),
                ("required".into(), "required".into()),
            );
            scanned.insert(r.project_id.clone());
        }
        (vcache, scache, scanned)
    }

    /// The real IPN/libIPN `fabric.mod.json` `depends` constraint, recorded
    /// from the actual jars: `fabric-language-kotlin >=1.9.2+kotlin.1.8.10`
    /// (Kotlin major 1). Both InventoryProfilesNext and libIPN encode it.
    fn ipn_kotlin1_manifests(
    ) -> std::collections::HashMap<String, crate::registry::JarManifest> {
        let mut m = std::collections::HashMap::new();
        // FLK provides the `fabric-language-kotlin` modid.
        m.insert(
            FLK_PID.to_string(),
            crate::registry::JarManifest {
                provided: vec![("fabric-language-kotlin".into(), String::new())],
                requires: vec![],
                breaks: vec![],
                version: String::new(),
            },
        );
        for ipn in ["O7RBXm3n", "onSQdWhM"] {
            m.insert(
                ipn.to_string(),
                crate::registry::JarManifest {
                    provided: vec![],
                    requires: vec![(
                        "fabric-language-kotlin".into(),
                        ">=1.9.2+kotlin.1.8.10".into(),
                    )],
                    breaks: vec![],
                    version: String::new(),
                },
            );
        }
        m
    }

    /// STEP 0 — invariant gate. Feed the REAL Stellar Origins root pin list
    /// through the SAME path `resolve_pack` uses; with EMPTY manifests the
    /// Tier-2 floor has no trigger, so EVERY root must come back at exactly
    /// its pinned `version_id`. Proves the seam + (later) the pre-pass do not
    /// disturb roots the floor is not legitimately repinning — even though
    /// FLK's pool is the full 33-version real snapshot, the pinned root still
    /// wins. (FLK itself is the one root the *fix* legitimately repins; that
    /// is asserted in `floor_fires_*`, not here, so this gate stays valid both
    /// pre- and post-fix.)
    #[tokio::test]
    async fn step0_real_stellar_roots_all_preserved_when_no_floor_trigger() {
        let roots = stellar_roots();
        assert_eq!(roots.len(), 50, "real instance has 50 roots");
        let (mut vcache, mut scache, mut scanned) = offline_state(&roots);
        let mut provided = std::collections::HashSet::new();
        let mut manifests = std::collections::HashMap::new(); // EMPTY: no floor

        let mr = Modrinth::new();
        let (entries, _issues) = resolve_pack_with_state(
            &mr,
            &roots,
            "1.20.1",
            "fabric",
            &mut vcache,
            &mut scache,
            &mut scanned,
            &mut provided,
            &mut manifests,
        )
        .await
        .expect("offline resolve succeeds");

        let got: std::collections::HashMap<&str, &str> = entries
            .iter()
            .map(|e| (e.project_id.as_str(), e.version_id.as_str()))
            .collect();
        for r in &roots {
            assert_eq!(
                got.get(r.project_id.as_str()),
                Some(&r.version_id.as_str()),
                "root {} must keep its pinned version {} (no floor trigger)",
                r.project_id,
                r.version_id
            );
        }
        assert_eq!(entries.len(), 50, "no roots dropped or added");
    }

    /// FLOOR-FIRES — the production-shaped scenario the old pure-unit test
    /// could not catch. Real Stellar Origins roots (FLK pinned at the real
    /// Kotlin-2.x `1.13.11+kotlin.2.3.21` = version `2i87JpYj`) + the REAL
    /// IPN/libIPN `>=1.9.2+kotlin.1.8.10` manifests + the recorded 33-version
    /// FLK snapshot as the `cached_versions` source. The fix's pre-pass must
    /// expand FLK's pool from the lone pinned 2.x build to the full real list
    /// so the Kotlin-major floor can pick a Kotlin-1.x build.
    #[tokio::test]
    async fn floor_fires_repins_flk_to_kotlin1_on_real_data() {
        let roots = stellar_roots();
        let (mut vcache, mut scache, mut scanned) = offline_state(&roots);
        let mut provided = std::collections::HashSet::new();
        let mut manifests = ipn_kotlin1_manifests();

        let mr = Modrinth::new();
        let (entries, _issues) = resolve_pack_with_state(
            &mr,
            &roots,
            "1.20.1",
            "fabric",
            &mut vcache,
            &mut scache,
            &mut scanned,
            &mut provided,
            &mut manifests,
        )
        .await
        .expect("offline resolve succeeds");

        let flk = entries
            .iter()
            .find(|e| e.project_id == FLK_PID)
            .expect("FLK still in the closure");
        assert_ne!(
            flk.version_id, FLK_K2_VID,
            "FLK must NOT remain the Kotlin-2.x crash pin {FLK_K2_VID}"
        );
        // The repinned build's filename carries `+kotlin.1.x` (the floor
        // picked a Kotlin-major-1 build, as IPN's >=...+kotlin.1.8.10 needs).
        // Read the embedded Kotlin major straight off the recorded filename.
        let kmaj = flk
            .path
            .split("kotlin.")
            .nth(1)
            .and_then(|s| s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok());
        assert_eq!(
            kmaj,
            Some(1),
            "FLK repinned to a Kotlin-1.x build; got path {}",
            flk.path
        );
    }

    /// The REAL FLK snapshot filtered to ONLY Kotlin-2.x builds (a real
    /// subset of recorded data, not a synthetic fixture). Models the genuine
    /// "FLK upstream has dropped all Kotlin-1.x builds for this MC/loader"
    /// state where the IPN constraint is truly unsatisfiable.
    fn flk_kotlin2_only() -> Vec<Version> {
        flk_real_versions()
            .into_iter()
            .filter(|v| v.version_number.contains("kotlin.2"))
            .collect()
    }

    /// HARD-GATE — when the (now-expanded) FLK pool genuinely contains NO
    /// Kotlin-1.x build, the resolver must raise the blocking
    /// `KotlinMajorUnsatisfiable` issue rather than silently ship the crash.
    /// And with a NORMAL snapshot (Kotlin-1.x builds present) it must NOT
    /// raise it — no false positive (the floor just repins instead).
    #[tokio::test]
    async fn hard_gate_raised_only_when_kotlin1_genuinely_absent() {
        let roots = stellar_roots();

        // (a) Kotlin-2.x-only pool -> genuinely unsatisfiable -> HARD gate.
        {
            let (mut vcache, mut scache, mut scanned) = offline_state(&roots);
            // Replace FLK's full real list with the k2-only real subset.
            vcache.insert(FLK_PID.to_string(), Ok(flk_kotlin2_only()));
            let mut provided = std::collections::HashSet::new();
            let mut manifests = ipn_kotlin1_manifests();

            let mr = Modrinth::new();
            let (_entries, issues) = resolve_pack_with_state(
                &mr,
                &roots,
                "1.20.1",
                "fabric",
                &mut vcache,
                &mut scache,
                &mut scanned,
                &mut provided,
                &mut manifests,
            )
            .await
            .expect("offline resolve succeeds");

            let gate = issues.iter().find_map(|i| match i {
                pack::ValidationIssue::KotlinMajorUnsatisfiable {
                    requirer,
                    needs_major,
                    present,
                } => Some((requirer.clone(), *needs_major, *present)),
                _ => None,
            });
            let (requirer, needs, present) = gate.expect(
                "KotlinMajorUnsatisfiable MUST be raised when no Kotlin-1.x \
                 FLK build exists in the expanded pool",
            );
            assert_eq!(needs, 1, "IPN needs Kotlin major 1");
            assert!(present >= 2, "the only FLK builds are Kotlin 2.x");
            assert!(
                requirer == "O7RBXm3n" || requirer == "onSQdWhM",
                "requirer names the real IPN/libIPN project, got {requirer}"
            );

            // It reaches the SAME blocking partition the assemble gate uses:
            // only IncompatibleAddonDropped is non-blocking; everything else
            // (incl. this) blocks. Mirror that partition here.
            let combined =
                combined_issues(&_entries, issues, "1.20.1", "fabric");
            let blocking = combined.iter().any(|i| {
                !matches!(
                    i,
                    pack::ValidationIssue::IncompatibleAddonDropped { .. }
                ) && matches!(
                    i,
                    pack::ValidationIssue::KotlinMajorUnsatisfiable { .. }
                )
            });
            assert!(
                blocking,
                "KotlinMajorUnsatisfiable must land in the blocking set"
            );
        }

        // (b) Normal full real snapshot -> Kotlin-1.x exists -> NO gate
        //     (the floor repins instead; no false positive).
        {
            let (mut vcache, mut scache, mut scanned) = offline_state(&roots);
            let mut provided = std::collections::HashSet::new();
            let mut manifests = ipn_kotlin1_manifests();
            let mr = Modrinth::new();
            let (_entries, issues) = resolve_pack_with_state(
                &mr,
                &roots,
                "1.20.1",
                "fabric",
                &mut vcache,
                &mut scache,
                &mut scanned,
                &mut provided,
                &mut manifests,
            )
            .await
            .expect("offline resolve succeeds");
            assert!(
                !issues.iter().any(|i| matches!(
                    i,
                    pack::ValidationIssue::KotlinMajorUnsatisfiable { .. }
                )),
                "no false positive: a Kotlin-1.x build exists, so the floor \
                 repins and the hard gate must stay silent"
            );
        }
    }
}

/// REAL recorded-data tests for the safe add/remove primitive
/// (`edit_instance_mods`) + the dir-scoped writes (`apply_edit_writes`). Same
/// discipline as `flk_floor_real_data_tests`: drive the offline test seam
/// (`edit_instance_mods_with_state`) on production-shaped `Version`/jar data —
/// the REAL resolver + REAL gates, no network, no synthetic dual-candidate
/// pool. The 50-mod backfill test uses the actual shipped Stellar Origins pin
/// list (`tests/fixtures/real/stellar_origins.instance.json`).
#[cfg(test)]
mod edit_pack_real_data_tests {
    use super::*;
    use crate::instance::{Instance, PinnedMod};
    use crate::modrinth::Version;

    const FIXT: &str =
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/real");

    /// One real-shaped Version: the given pin, 1.20.1/fabric, one primary
    /// file named `<pid>.jar`, optional REQUIRED Modrinth dependency edges
    /// (project_id only → resolver picks best; exactly the production shape).
    fn ver(pid: &str, vid: &str, mc: &str, req: &[&str]) -> Version {
        let deps: Vec<serde_json::Value> = req
            .iter()
            .map(|d| {
                serde_json::json!({"project_id": d, "dependency_type": "required"})
            })
            .collect();
        let j = serde_json::json!([{
            "id": vid, "project_id": pid, "name": vid,
            "version_number": "1.0.0", "game_versions": [mc],
            "version_type": "release", "loaders": ["fabric"],
            "downloads": 0, "date_published": "2024-01-01T00:00:00Z",
            "files": [{
                "hashes": {"sha1": "0".repeat(40), "sha512": "0".repeat(128)},
                "url": format!("https://cdn.modrinth.com/{pid}/{vid}.jar"),
                "filename": format!("{pid}.jar"),
                "primary": true, "size": 1
            }],
            "dependencies": deps
        }]);
        let mut v: Vec<Version> =
            serde_json::from_value(j).expect("stub Version deserializes");
        v.pop().unwrap()
    }

    /// Offline resolver state for an explicit pid→version map: `vcache` (the
    /// network short-circuit), `scache` (sides), `scanned` (jar_augment
    /// no-op). `manifests` is supplied per-test.
    #[allow(clippy::type_complexity)]
    fn offline(
        pv: &[(&str, Version)],
    ) -> (VersionCache, SideCache, std::collections::HashSet<String>) {
        let mut vc: VersionCache = std::collections::HashMap::new();
        let mut sc: SideCache = std::collections::HashMap::new();
        let mut scn: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for (pid, v) in pv {
            vc.entry(pid.to_string())
                .or_insert_with(|| Ok(vec![v.clone()]));
            sc.insert(
                pid.to_string(),
                ("required".into(), "required".into()),
            );
            scn.insert(pid.to_string());
        }
        (vc, sc, scn)
    }

    fn pin(p: &str, v: &str) -> PinnedMod {
        PinnedMod {
            project_id: p.into(),
            version_id: v.into(),
            name: format!("{p}.jar"),
            path: format!("mods/{p}.jar"),
            sha1: String::new(),
            sha512: String::new(),
            download_url: String::new(),
            file_size: 0,
        }
    }

    fn instance(roots: &[&str], mods: &[(&str, &str)]) -> Instance {
        Instance {
            id: "t".into(),
            name: "T".into(),
            mc_version: "1.20.1".into(),
            loader: "fabric".into(),
            loader_version: "0.15.0".into(),
            created: "2024".into(),
            last_played: None,
            mods: mods.iter().map(|(p, v)| pin(p, v)).collect(),
            roots: roots.iter().map(|s| s.to_string()).collect(),
        }
    }

    async fn run(
        inst: &Instance,
        add: &[ModRef],
        remove: &[String],
        pv: &[(&str, Version)],
        man: std::collections::HashMap<String, crate::registry::JarManifest>,
    ) -> Result<EditResult, EditError> {
        let (mut vc, mut sc, mut scn) = offline(pv);
        let mut prov = std::collections::HashSet::new();
        let mut man = man;
        let mr = Modrinth::new();
        edit_instance_mods_with_state(
            &mr, inst, add, remove, &mut vc, &mut sc, &mut scn, &mut prov,
            &mut man,
        )
        .await
    }

    fn add1(pid: &str) -> Vec<ModRef> {
        vec![ModRef { project_id: pid.into(), version_id: String::new() }]
    }

    /// Old-shape instance (no `roots`) → backfill makes every pinned mod a
    /// root; a no add/remove call on a self-consistent 50-mod REAL pack is a
    /// pure noop. Proves the back-compat rule + noop short-circuit on real
    /// shipped pins.
    #[tokio::test]
    async fn backfill_then_noop_on_real_50_mod_pack() {
        let raw = std::fs::read_to_string(format!(
            "{FIXT}/stellar_origins.instance.json"
        ))
        .expect("read stellar fixture");
        let v: serde_json::Value =
            serde_json::from_str(&raw).expect("parse stellar");
        let pins: Vec<(String, String)> = v["mods"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| {
                (
                    m["project_id"].as_str().unwrap().to_string(),
                    m["version_id"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(pins.len(), 50, "real instance has 50 pins");
        let inst = Instance {
            id: "s".into(),
            name: "Stellar".into(),
            mc_version: "1.20.1".into(),
            loader: "fabric".into(),
            loader_version: "0.15.0".into(),
            created: "x".into(),
            last_played: None,
            mods: pins.iter().map(|(p, v)| pin(p, v)).collect(),
            roots: vec![], // OLD SHAPE — must backfill
        };
        let pv: Vec<(&str, Version)> = pins
            .iter()
            .map(|(p, v)| (p.as_str(), ver(p, v, "1.20.1", &[])))
            .collect();
        let r = run(&inst, &[], &[], &pv, Default::default())
            .await
            .expect("ok");
        assert!(r.noop, "self-consistent pack, no delta => noop");
        assert_eq!(r.roots.len(), 50, "empty roots backfilled to all mods");
        assert_eq!(r.entries.len(), 50);
        assert!(
            r.added.is_empty()
                && r.removed.is_empty()
                && r.pruned_orphans.is_empty()
        );
    }

    /// Remove a leaf root: gone from the closure, recorded in `removed`,
    /// dropped from `roots`, not a noop.
    #[tokio::test]
    async fn remove_leaf_root() {
        let inst = instance(&["A", "B"], &[("A", "a1"), ("B", "b1")]);
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &[])),
            ("B", ver("B", "b1", "1.20.1", &[])),
        ];
        let r = run(&inst, &[], &["B".into()], &pv, Default::default())
            .await
            .expect("ok");
        assert!(!r.noop);
        assert!(r.entries.iter().any(|e| e.project_id == "A"));
        assert!(!r.entries.iter().any(|e| e.project_id == "B"));
        assert_eq!(r.removed, vec!["B".to_string()]);
        assert_eq!(r.roots, vec!["A".to_string()]);
    }

    /// Reverse-dependency safety: B requires A (real Modrinth edge), so
    /// removing A re-pulls it via B → refused, and the requirer is named from
    /// B's REAL jar manifest (`requires` modid ∩ A's `provided`).
    #[tokio::test]
    async fn remove_still_required_is_refused_and_attributed() {
        let inst = instance(&["A", "B"], &[("A", "a1"), ("B", "b1")]);
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &[])),
            ("B", ver("B", "b1", "1.20.1", &["A"])),
        ];
        let mut man = std::collections::HashMap::new();
        man.insert(
            "A".to_string(),
            crate::registry::JarManifest {
                provided: vec![("amod".into(), String::new())],
                requires: vec![],
                breaks: vec![],
                version: String::new(),
            },
        );
        man.insert(
            "B".to_string(),
            crate::registry::JarManifest {
                provided: vec![],
                requires: vec![("amod".into(), "*".into())],
                breaks: vec![],
                version: String::new(),
            },
        );
        let e = run(&inst, &[], &["A".into()], &pv, man)
            .await
            .expect_err("must refuse");
        match e {
            EditError::StillRequired(v) => {
                assert_eq!(v.len(), 1);
                assert_eq!(v[0].label, "A");
                assert!(
                    v[0].required_by.iter().any(|r| r == "B"),
                    "attributed to B from jar metadata, got {:?}",
                    v[0].required_by
                );
            }
            other => panic!("expected StillRequired, got {other:?}"),
        }
    }

    /// Add is dependency-complete: NEW requires DEP → both land; NEW is an
    /// `added` root, DEP a non-root `pulled_deps`.
    #[tokio::test]
    async fn add_pulls_required_dependency() {
        let inst = instance(&["A"], &[("A", "a1")]);
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &[])),
            ("NEW", ver("NEW", "n1", "1.20.1", &["DEP"])),
            ("DEP", ver("DEP", "d1", "1.20.1", &[])),
        ];
        let r = run(&inst, &add1("NEW"), &[], &pv, Default::default())
            .await
            .expect("ok");
        assert!(r.entries.iter().any(|e| e.project_id == "NEW"));
        assert!(r.entries.iter().any(|e| e.project_id == "DEP"));
        assert!(r.added.contains(&"NEW".to_string()));
        assert!(r.pulled_deps.contains(&"DEP".to_string()));
        assert!(r.roots.contains(&"NEW".to_string()));
        assert!(!r.roots.contains(&"DEP".to_string()));
    }

    /// The `combined_issues` fix: an added mod with NO version compatible
    /// with the pack's MC must NOT slip the gate silently — it is blocked
    /// (or, at minimum, never silently added).
    #[tokio::test]
    async fn add_incompatible_game_version_is_not_silently_accepted() {
        let inst = instance(&["A"], &[("A", "a1")]);
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &[])),
            ("BAD", ver("BAD", "x1", "1.19.2", &[])), // wrong MC only
        ];
        match run(&inst, &add1("BAD"), &[], &pv, Default::default()).await {
            Err(EditError::Conflicts(v)) => {
                assert!(!v.is_empty(), "blocked with a reported conflict");
            }
            Ok(r) => {
                assert!(
                    !r.entries.iter().any(|e| e.project_id == "BAD"),
                    "an MC-incompatible add must never be silently included"
                );
                assert!(!r.added.contains(&"BAD".to_string()));
            }
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }

    /// Regression guard for the `combined_issues` fix specifically. With an
    /// EXPLICIT version_id the best-version pre-resolve is skipped, so the
    /// incompatible mod is pinned as a root, lands in the resolved closure,
    /// and is caught ONLY by the `validate_pack` half of `combined_issues`
    /// (not the resolver's dep-issues, not the pre-resolve early-exit). If
    /// the gate ever regresses to resolver-issues-only this fails.
    #[tokio::test]
    async fn explicit_incompatible_version_blocked_via_combined_issues() {
        let inst = instance(&["A"], &[("A", "a1")]);
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &[])),
            ("BAD", ver("BAD", "x1", "1.19.2", &[])), // wrong MC
        ];
        let add = vec![ModRef {
            project_id: "BAD".into(),
            version_id: "x1".into(), // EXPLICIT → pre-resolve is skipped
        }];
        match run(&inst, &add, &[], &pv, Default::default()).await {
            Err(EditError::Conflicts(v)) => assert!(
                v.iter().any(|i| matches!(
                    i,
                    pack::ValidationIssue::IncompatibleGameVersion { .. }
                )),
                "combined_issues must surface the per-entry \
                 IncompatibleGameVersion, got {v:?}"
            ),
            Ok(r) => assert!(
                !r.entries.iter().any(|e| e.project_id == "BAD"),
                "an explicit MC-incompatible pin must never land silently"
            ),
            Err(other) => panic!("unexpected error {other:?}"),
        }
    }

    /// Swap in ONE call (atomic): remove B + add C → single resolve, both
    /// reflected, roots = {A, C}.
    #[tokio::test]
    async fn swap_remove_and_add_in_one_call() {
        let inst = instance(&["A", "B"], &[("A", "a1"), ("B", "b1")]);
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &[])),
            ("B", ver("B", "b1", "1.20.1", &[])),
            ("C", ver("C", "c1", "1.20.1", &[])),
        ];
        let r = run(&inst, &add1("C"), &["B".into()], &pv, Default::default())
            .await
            .expect("ok");
        let pids: std::collections::HashSet<&str> =
            r.entries.iter().map(|e| e.project_id.as_str()).collect();
        assert!(pids.contains("A") && pids.contains("C"));
        assert!(!pids.contains("B"));
        assert!(r.added.contains(&"C".to_string()));
        assert!(r.removed.contains(&"B".to_string()));
        let roots: std::collections::HashSet<String> =
            r.roots.into_iter().collect();
        assert_eq!(
            roots,
            ["A".to_string(), "C".to_string()].into_iter().collect()
        );
    }

    /// Explicit roots: removing a leaf root must NOT prune a transitive dep
    /// another kept root still needs (A requires D; remove B; D survives, is
    /// never a root, is not an orphan).
    #[tokio::test]
    async fn explicit_roots_keep_needed_transitive_dep() {
        let inst = instance(
            &["A", "B"],
            &[("A", "a1"), ("B", "b1"), ("D", "d1")],
        );
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &["D"])),
            ("B", ver("B", "b1", "1.20.1", &[])),
            ("D", ver("D", "d1", "1.20.1", &[])),
        ];
        let r = run(&inst, &[], &["B".into()], &pv, Default::default())
            .await
            .expect("ok");
        let pids: std::collections::HashSet<&str> =
            r.entries.iter().map(|e| e.project_id.as_str()).collect();
        assert!(pids.contains("A") && pids.contains("D"));
        assert!(!pids.contains("B"));
        assert_eq!(r.roots, vec!["A".to_string()]);
        assert!(
            r.pruned_orphans.is_empty(),
            "D still needed by A => not an orphan, got {:?}",
            r.pruned_orphans
        );
        assert!(r.removed.contains(&"B".to_string()));
    }

    /// Blocker-1 regression: `apply_edit_writes` prunes the jar of a mod that
    /// left the closure (the real launch path never would), invalidates the
    /// stale grounding cache, rewrites the .mrpack, and returns the updated
    /// instance — all under a tempdir (no ~/.anvil side effects).
    #[test]
    fn apply_edit_writes_prunes_stale_jar_and_invalidates_cache() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        std::fs::create_dir_all(dir.join("mods")).unwrap();
        std::fs::write(dir.join("mods/keepme.jar"), b"x").unwrap();
        std::fs::write(dir.join("mods/goneme.jar"), b"x").unwrap();
        std::fs::write(dir.join("anvil-registry.json"), b"{}").unwrap();

        let base = Instance {
            id: "i".into(),
            name: "P".into(),
            mc_version: "1.20.1".into(),
            loader: "fabric".into(),
            loader_version: "0".into(),
            created: "x".into(),
            last_played: None,
            mods: vec![],
            roots: vec![],
        };
        let kept = ModEntry {
            project_id: "K".into(),
            version_id: "k1".into(),
            path: "mods/keepme.jar".into(),
            sha1: "a".into(),
            sha512: "b".into(),
            downloads: vec!["https://cdn.modrinth.com/x.jar".into()],
            file_size: 1,
            game_versions: vec!["1.20.1".into()],
            loaders: vec!["fabric".into()],
            client_side: "required".into(),
            server_side: "required".into(),
        };
        let res = EditResult {
            entries: vec![kept],
            roots: vec!["K".into()],
            added: vec!["keepme".into()],
            pulled_deps: vec![],
            removed: vec!["goneme".into()],
            pruned_orphans: vec![],
            noop: false,
        };
        let updated = apply_edit_writes(dir, &base, &res).expect("writes ok");
        assert!(dir.join("mods/keepme.jar").exists(), "kept jar survives");
        assert!(
            !dir.join("mods/goneme.jar").exists(),
            "removed mod's jar pruned (Blocker 1)"
        );
        assert!(
            !dir.join("anvil-registry.json").exists(),
            "stale grounding cache invalidated"
        );
        assert!(dir.join("P.mrpack").exists(), ".mrpack rewritten");
        assert_eq!(updated.mods.len(), 1);
        assert_eq!(updated.mods[0].project_id, "K");
        assert_eq!(updated.roots, vec!["K".to_string()]);
    }

    /// End-to-end of the UI add/remove route the new lib.rs commands take,
    /// minus the global-dir `load_instances`/`save_instance` (untestable
    /// offline): the SAME pieces `apply_pack_edit` chains —
    /// `edit_instance_mods_with_state` (real resolver + gates, no network) →
    /// `apply_edit_writes` (real on-disk prune + .mrpack) under a tempdir.
    ///
    /// ADD: a mod whose REAL Modrinth edge requires a dep ⇒ the updated
    /// Instance.mods CONTAINS the added project AND its pulled dep, both jars
    /// land, .mrpack exists. REMOVE the added root ⇒ it is gone from
    /// Instance.mods, the dep auto-prunes as an orphan, and BOTH stale jars
    /// are deleted off disk (seeded fakes prove the prune, not just absence).
    /// This is the "the mod actually gets added/removed" guarantee the naive
    /// append/retain commands did not provide.
    #[tokio::test]
    async fn ui_route_add_then_remove_adds_and_prunes_on_disk() {
        let td = tempfile::tempdir().unwrap();
        let dir = td.path();
        std::fs::create_dir_all(dir.join("mods")).unwrap();

        // Start: instance has only A. NEW requires DEP (a real required edge).
        let inst = instance(&["A"], &[("A", "a1")]);
        let pv = [
            ("A", ver("A", "a1", "1.20.1", &[])),
            ("NEW", ver("NEW", "n1", "1.20.1", &["DEP"])),
            ("DEP", ver("DEP", "d1", "1.20.1", &[])),
        ];

        // --- ADD NEW (auto-pick version, like version_id: None) -----------
        let add_res = run(&inst, &add1("NEW"), &[], &pv, Default::default())
            .await
            .expect("add resolves");
        assert!(!add_res.noop, "add changed the closure");
        let after_add =
            apply_edit_writes(dir, &inst, &add_res).expect("add writes ok");

        let add_pids: std::collections::HashSet<&str> = after_add
            .mods
            .iter()
            .map(|m| m.project_id.as_str())
            .collect();
        assert!(
            add_pids.contains("NEW"),
            "added project is in Instance.mods, got {add_pids:?}"
        );
        assert!(
            add_pids.contains("DEP"),
            "required dep pulled into Instance.mods, got {add_pids:?}"
        );
        assert!(add_pids.contains("A"), "existing mod retained");
        assert!(after_add.roots.contains(&"NEW".to_string()));
        assert!(
            !after_add.roots.contains(&"DEP".to_string()),
            "a pulled dep is NOT a root"
        );
        assert!(
            dir.join(format!("{}.mrpack", after_add.name)).exists(),
            ".mrpack written on add"
        );

        // --- REMOVE NEW: DEP should auto-prune as an orphan ---------------
        // Seed BOTH jars so the prune (not mere absence) is what we observe.
        std::fs::write(dir.join("mods/NEW.jar"), b"x").unwrap();
        std::fs::write(dir.join("mods/DEP.jar"), b"x").unwrap();
        std::fs::write(dir.join("mods/A.jar"), b"x").unwrap();
        assert!(dir.join("mods/NEW.jar").exists());
        assert!(dir.join("mods/DEP.jar").exists());

        let rm_res = run(
            &after_add,
            &[],
            &["NEW".into()],
            &pv,
            Default::default(),
        )
        .await
        .expect("remove resolves");
        assert!(!rm_res.noop, "remove changed the closure");
        assert!(
            rm_res.removed.contains(&"NEW".to_string()),
            "NEW recorded as removed"
        );
        assert!(
            rm_res.pruned_orphans.contains(&"DEP".to_string()),
            "DEP auto-pruned as orphan, got {:?}",
            rm_res.pruned_orphans
        );
        let after_rm =
            apply_edit_writes(dir, &after_add, &rm_res).expect("rm writes ok");

        let rm_pids: std::collections::HashSet<&str> = after_rm
            .mods
            .iter()
            .map(|m| m.project_id.as_str())
            .collect();
        assert!(
            !rm_pids.contains("NEW"),
            "removed project gone from Instance.mods"
        );
        assert!(
            !rm_pids.contains("DEP"),
            "orphaned dep gone from Instance.mods"
        );
        assert!(rm_pids.contains("A"), "untouched mod survives");
        assert!(
            !dir.join("mods/NEW.jar").exists(),
            "removed mod's jar pruned off disk"
        );
        assert!(
            !dir.join("mods/DEP.jar").exists(),
            "orphaned dep's jar pruned off disk"
        );
        assert!(
            dir.join("mods/A.jar").exists(),
            "kept mod's jar untouched"
        );
    }

    /// Lock the cross-team JSON wire shape of `ApplyEditError`. The frontend
    /// is built in parallel against EXACTLY these strings — a serde-attr drift
    /// (tag rename, casing, struct-variant flattening) must fail here, not in
    /// production. Covers every variant.
    #[test]
    fn apply_edit_error_serde_shape_is_locked() {
        let still = ApplyEditError::StillRequired {
            items: vec![StillRequiredView {
                label: "sodium".into(),
                required_by: vec!["iris".into()],
            }],
        };
        assert_eq!(
            serde_json::to_string(&still).unwrap(),
            r#"{"kind":"still_required","items":[{"label":"sodium","required_by":["iris"]}]}"#
        );

        let resolve = ApplyEditError::Resolve {
            message: "modrinth down".into(),
        };
        assert_eq!(
            serde_json::to_string(&resolve).unwrap(),
            r#"{"kind":"resolve","message":"modrinth down"}"#
        );

        let nf = ApplyEditError::NotFound {
            instance_id: "inst-1".into(),
        };
        assert_eq!(
            serde_json::to_string(&nf).unwrap(),
            r#"{"kind":"not_found","instance_id":"inst-1"}"#
        );

        // Conflicts: outer shape locked; inner is pack::ValidationIssue's own
        // (already serde-tested in pack.rs). Just assert the envelope.
        let conf = ApplyEditError::Conflicts {
            issues: vec![pack::ValidationIssue::IncompatibleGameVersion {
                project_id: "p".into(),
                want: "1.20.1".into(),
            }],
        };
        let s = serde_json::to_string(&conf).unwrap();
        assert!(
            s.starts_with(r#"{"kind":"conflicts","issues":["#),
            "conflicts envelope shape, got {s}"
        );
    }
}
