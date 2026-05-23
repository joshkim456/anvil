# Anvil

**An AI Minecraft modpack curator and launcher.** Describe the pack you want in
plain language; Claude designs it against the live Modrinth catalog, resolves
dependencies, boot-tests it headless, writes a standard `.mrpack`, authors
quests/origins as datapacks, and launches the game. Chat in, a coherent playable
artifact out.

License: **GPL-3.0-only**. Anvil is an unofficial third-party launcher, not
affiliated with Mojang or Microsoft.

---

## Status

A working desktop app (macOS / Windows via Tauri):

| Area | State |
|---|---|
| **Browse** | Live Modrinth catalog: sort, MC-version/loader/genre filters, infinite scroll, detail view. |
| **Curator** | Conversational designer over the Anthropic Messages API (SSE streaming) driving a phased tool-use loop on the live catalog + pack engine. |
| **Pack engine** | Transitive dependency-closure resolution, validation (loader / MC / side-flag / duplicate / insecure-URL / version-constraint), `.mrpack` emit + import. |
| **Verify** | Headless boot test in 3 stages (smoke → world-join → registry dump), with log-scan crash attribution to the exact mod before shipping. |
| **Progression** | AI-authored Heracles quest graph, Open Loader recipe/boss datapacks, and Origins/Apoli powers — every id grounded against the pack's real registry. |
| **Launch core** | Custom Rust launcher: piston-meta manifest, asset/library/native download, Fabric + Quilt install, JVM/classpath construction, live log streaming. |
| **Sign in** | Microsoft MSA OAuth (auth-code + PKCE) → Xbox Live → XSTS → Minecraft Services via the official identity platform. Never bypassed or emulated. |
| **Instances** | Version-pinned profiles, `.mrpack` import, duplicate-by-name reuse, light/dark theme. |

**Known external gate:** Minecraft's `login_with_xbox` only accepts a
Microsoft-approved client id. A fresh Azure app returns `403 Invalid app
registration` until approved via <https://aka.ms/mce-reviewappid>. Users can
supply their own approved id in Settings. A Microsoft policy surface, not a code
limitation.

---

## Architecture

```
┌──────────────────────── Tauri 2 app ────────────────────────┐
│  React + TypeScript frontend  (Curator · Browse · Instances) │
│        ▲ events (stream / logs / progress)  │ commands       │
│  Rust core                                                   │
│   • modrinth   live catalog client (API v2)                  │
│   • curator    Anthropic SSE + phased tool loop              │
│   • pack       dep resolution · validate · .mrpack I/O       │
│   • launch     manifest · assets · loader · JVM · verify     │
│   • registry   real-id grounding (jar-scan + runtime dump)   │
│   • quest/recipe/content/origins  progression → datapacks    │
│   • auth       MSA auth-code+PKCE · Xbox · XSTS · Minecraft  │
│   • instance   version-pinned profiles + persistence         │
└──────────────────────────────────────────────────────────────┘
        │ HTTPS                              │ child process
   Modrinth API/CDN · Anthropic API     java → Minecraft
```

Profiles use the standard `.mrpack` format. Instances are version-pinned
snapshots; mods never update silently. Mod jars are never bundled — the manifest
references the Modrinth CDN and the client downloads + SHA-512-verifies each at
install time.

---

## Algorithms

### 1. Curator tool loop (`curator.rs`)

The curator is a streaming agent over the Anthropic Messages API.

1. Build a **phased** tool set. The conversation has a phase (`curating` →
   `assembled` → `progression` → `iterating`); each phase exposes only its valid
   tools (e.g. quest/origin tools don't exist until the pack is `verified`). This
   constrains the model structurally rather than by instruction.
2. System prompt is assembled from **cache-stable blocks** (main prompt, active-
   pack preamble, origin/quest catalogs), each with a 1h `cache_control`
   breakpoint. Anthropic caps breakpoints at 4/request, so sibling catalogs are
   concatenated into one block.
3. POST with `stream: true`; parse SSE deltas → forward text + tool-call chips to
   the UI as Tauri events.
4. On a `tool_use` block, dispatch to the Rust implementation (`execute_tool`),
   append the result, and re-enter the loop until the model stops.

Tools: `propose_pack`, `search_mods`, `get_mod`, `assemble_pack`, `verify_pack`,
`edit_pack`, `query_registry`, `generate_quests`, `generate_origin_intents` (+ a
last-resort raw-Apoli fallback).

### 2. Dependency resolver (`pack.rs`)

Produces a resolved, version-pinned closure from a set of root mods.

1. **Seed** the worklist with the roots; mark visited (cycle guard).
2. **Iterative DFS**: pop a mod, walk its `DepEdge`s. Skip `optional` /
   `incompatible` / `embedded`; follow `required`. Unseen deps are fetched via
   the driver (live Modrinth) and pushed.
3. **Version pick** per dependency (`pick_best`): filter candidates to those
   compatible with the pack's MC version + loader, then deterministic sort —
   stability rank (`release` < `beta` < `alpha`) → newest `date_published` →
   `version_id`. Exact pins are honored verbatim.
4. **Closure assembly**: emit entries deterministically (roots first, then
   transitive deps sorted by id).
5. **Validation** (`ValidationIssue`): unresolved required dep, incompatible-dep
   present, duplicate project, insecure (non-HTTPS) URL, and **semantic version
   constraints** — `check_version_constraints` maps each `modid` to the highest
   version provided across the pack (including JIJ-bundled jars) and evaluates
   every `requires`/`breaks` range using a real Fabric `VersionReq` comparator
   (`version.rs`), so a silent runtime "Incompatible mods found" crash becomes a
   precise pre-assemble block.

### 3. Verify pipeline (`launch.rs`)

Boot-tests the assembled pack headless before it ships. Three stages, JVM-mutex
guarded so concurrent verifies can't co-boot.

- **Stage 1 — smoke test** (`smoke_test`): spawn the client with
  `-Djava.awt.headless=true` (suppresses blocking Swing dialogs on resolution
  failure). Scan stdout line-by-line (`classify_smoke_line`) for failure
  signatures ("Incompatible mods found", missing-dependency, entrypoint crash,
  `NoClassDefFoundError`, "Crash report saved to") vs the success milestone
  ("Sound engine started"). Full stdout is tee'd to `.verify-logs/`.
- **Stage 2 — world-join probe** (`world_join_probe`): boot with
  `--quickPlaySingleplayer` to auto-create + join a throwaway world. Classifier
  catches world-load-only crashes (e.g. the IPN Kotlin-reflection `onJoinWorld`
  bug, world-join Mixin failures) vs the "Loaded the worlds" success line.
- **Stage 3 — registry dump** (`registry_dump_pass`): populate a throwaway
  server dir with the full mod set (hard-linked from the SHA-1 cache), boot a
  headless Fabric server, wait for the "Done" ready line, send `/dump registry`
  over stdin, then `/stop`. Output is the modded registry JSON.

Fabric resolution errors are parsed (`parse_fabric_remediation`) into actionable
re-pin instructions (holder mod, required dep + version floor) the curator can
apply via `edit_pack` autonomously.

### 4. Registry grounding (`registry.rs`)

Stops the model shipping hallucinated ids.

1. **Populate `RegistryVocab`** — primary source is the Stage-3 runtime dump;
   static fallback is a jar scan (`scan_instance`) that reads each
   `mods/*.jar`'s `data/<ns>/{tags,recipes,advancements,…}` and lang files.
2. **Ground each authored id** through a tiered ladder: concrete match in vocab →
   Anvil-authored (`anvil:` namespace) → low-confidence (namespace present but
   jar unscanned, non-blocking) → **hard reject** (id absent from a scanned
   namespace, e.g. `cobblemon:mewtwo` when the cobblemon jar was scanned).
3. **No jars scanned** degrades to namespace-only checking (legacy mode). Tags
   are grounded against `vocab.tags` with the leading `#` stripped.

### 5. Origins intent engine (`origins.rs`)

Compiles a closed, typed `PerkIntent` enum (45 variants) to valid Apoli powers +
companion `.mcfunction`s, written under `config/openloader/data/anvil-origins/`.

1. **Density budget**: each origin targets `light` / `standard` / `rich`, which
   fixes the allowed counts of passive / active / lifetime perks. The batch
   density is the player-chosen *average*; per-origin overrides may deviate, and
   the batch is rejected if the per-origin mean strays >0.6 from it.
2. **Capability gate**: mod-integrated intents (Lifetimes need Open Loader's
   datapack channel; Familiar needs Bewitchment; Scale needs Pehkui; …) are
   checked against the pack's mods. Lookups normalize identifiers (lowercase +
   strip non-alphanumeric) so a Modrinth slug, opaque project id, or display name
   all resolve to the same capability bucket.
3. **Grounding** of every id (§4), then **`emit_perk`** lowers each intent to
   Apoli factory JSON, then **`validate`** gates the whole `OriginsSet`. On any
   issue, nothing is written.
4. **Impact** per origin is derived from its actual forecast (passive/active/
   lifetime counts), not the batch density — so origins display distinct
   LOW/MODERATE/HIGH tiers.

### 6. Quest generation (`quest.rs`)

LLM-authored `QuestGraph` → valid Heracles datapack.

1. **Validate**: cycle detection over quest deps, grounding of every id, and
   **difficulty tiering** — `task_tier` rates each task T1–T5; each chapter has a
   floor and a ramped ceiling (Ch1 T1–2 … final chapter up to T5), and
   over/under-difficult tasks are hard-rejected.
2. **Layout** (`layout_graph`): per chapter, compute a longest-path topological
   rank (Kahn) → x = depth, y = centered within the rank's column. Dependency
   edges point strictly left→right; the bbox centroid lands on the root quest for
   Heracles' camera focus. Cross-chapter deps draw as gutter nodes.
3. **Emit**: stamp recipe ids (`anvil:<hex>`), serialize per-quest Heracles JSON
   plus the recipe/content datapacks.

### 7. Recipe + content datapacks (`recipe.rs`, `content.rs`)

- **Recipes**: deterministic Open Loader serializer for quest-embedded recipes
  (`crafting_shaped/shapeless`, `smelting`) → `anvil-recipes/` datapack.
- **Content**: self-contained boss/site nodes → `anvil-content/` datapack with
  boss-summon functions, a kill-detection advancement → token-grant function, and
  a GatherItem task on an NBT-marked token. All ids key off
  `content_hex(chapter, node)` so they're collision-free by construction.

---

## Security & data handling

Anvil is local-first. There is no Anvil backend; the app talks directly to
Modrinth, Anthropic, and Microsoft/Mojang from your machine.

**Anthropic API key (you supply your own; none is shipped):**

1. Entered in Settings → the `set_settings` Tauri command → written to
   `~/.anvil/settings.json`.
2. On Unix the file is chmod'd `0600` (owner read/write only). It is stored as
   **plaintext JSON** — macOS Keychain storage is a tracked roadmap item, not yet
   implemented.
3. At request time `settings::anthropic_key()` reads it back (env var
   `ANTHROPIC_API_KEY` is a dev-only fallback).
4. It is sent **only** to `https://api.anthropic.com/v1/messages` via the
   `x-api-key` header, directly from your machine — no proxy, no middleman.
5. It is **never logged** (no `tracing`/`println` touches it) and **never
   returned to the frontend** — `get_settings` exposes only a `has_anthropic_key`
   boolean, so the key cannot be read back out of the UI after entry.

**Microsoft sign-in:** the OAuth **refresh token** is persisted to
`~/.anvil/account.json` (also `0600`, also plaintext). Auth uses the official
auth-code + PKCE flow; the access token is minted on demand and not persisted.
The bundled MS **client id** is a public OAuth client (the CurseForge/Modrinth
model) — not a secret — and is overridable in Settings.

**Modrinth:** read-only, unauthenticated. Mod jars are downloaded from the
Modrinth CDN and SHA-512-verified before use; nothing is uploaded.

**Caveat:** secrets at rest are plaintext-on-disk protected only by file
permissions. Anyone with read access to your home directory (another process
running as you, an unencrypted backup) can read them. Treat `~/.anvil/` as
sensitive until keychain storage lands.

---

## Getting started

Prerequisites: a recent **Rust** toolchain (`rustup`), **Node.js** 18+, and the
platform WebView (preinstalled on macOS; WebView2 on Windows).

```sh
npm install
npm run tauri dev          # build the Rust core and run the desktop app
cd src-tauri && cargo test # pack-engine + progression test suite
npm run tauri build        # distributable .app/.dmg (macOS) / installer (Windows)
```

**Configuration:** the Curator needs your own **Anthropic API key** (Settings;
see Security above). Sign-in uses a bundled public OAuth client id, overridable
in Settings.

---

## Project layout

```
src/                      React + TypeScript frontend (surfaces, components, lib)
src-tauri/src/
  lib.rs        Tauri commands + event bridging
  modrinth.rs   Modrinth API v2 client
  curator.rs    Anthropic streaming + phased tool-use loop
  pack.rs       dependency resolver · validate · .mrpack
  version.rs    Fabric semantic version + range comparator
  launch.rs     custom Minecraft launch core + 3-stage verify
  registry.rs   real-id registry vocab + grounding
  quest.rs      progression graph + Heracles emit + tiering/layout
  recipe.rs     custom-recipe facet → Open Loader datapack
  content.rs    self-contained boss/content provisioning
  origins.rs    Origins/Apoli intent engine + density/capability/grounding
  auth.rs       Microsoft / Xbox / Minecraft sign-in
  instance.rs   instance model + persistence
  settings.rs   ~/.anvil paths + secret storage
```

---

## Tech stack

**Tauri 2** (Rust core) + **React 18** + **TypeScript** + **Vite**. Rust:
`reqwest` (rustls), `tokio`, `serde`, `anyhow`, `sha2`, `zip`, `chrono`;
`p256`/`base64`/`uuid` for the sign-in crypto path. No bundled secrets, no
redistributed mod jars.

## Roadmap

End-to-end in-game verification of authored progression, per-genre chapter
adaptation, Forge/NeoForge launch, JRE auto-provisioning, SQLite catalog cache,
**OS-keychain secret storage**.

## Legal and licensing

- **GPL-3.0-only.** See [`LICENSE`](./LICENSE) and [`NOTICE.md`](./NOTICE.md).
- Anvil is an **unofficial third-party tool**, not affiliated with or endorsed by
  Mojang or Microsoft. "Minecraft" is a trademark of Mojang AB / Microsoft.
- Anvil uses the **official Microsoft identity platform**; players authenticate
  their own accounts. Authentication is never bypassed, emulated, or
  circumvented, and no Mojang assets are redistributed.

## Acknowledgements

[Modrinth](https://modrinth.com) for the open mod catalog and API,
[Anthropic](https://www.anthropic.com) Claude for the curation model, and
[Heracles](https://modrinth.com/mod/heracles) +
[Open Loader](https://modrinth.com/mod/open-loader) for the quest and datapack
delivery the progression layer targets.
