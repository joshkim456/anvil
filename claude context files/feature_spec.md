# Anvil — Feature Spec v0.1

> **Working title: "Anvil"** (placeholder — rename freely; no major launcher collision).
> Status: pre-implementation. This document is the *feature plan only*. Tech-stack detail and
> implementation plan live in sibling docs (`tech_stack.md`, `roadmap.md`) once we move forward.

---

## 1. Product

**One-liner:** Anvil is a standalone desktop app where you *converse with Claude to design a
Minecraft modpack* — genre, single/multiplayer, kitchen-sink vs. focused, difficulty — and it
assembles a valid, dependency-resolved, optimized profile you can **launch directly**, manage as
instances, and hand-edit from a Modrinth-style mod browser. Later: an AI-authored difficulty
curve + storyline rendered as an editable quest node-graph.

**The WorldForge analogy:** WorldForge = *talk to an AI → a coherent, internally-consistent
world persists.* Anvil = *talk to Claude → a coherent, internally-consistent, **playable**
modpack persists.* Chat in, structured playable artifact out. The "world" here is a valid
`.mrpack` + instance + (Phase 2) a quest graph.

**Like CurseForge in form factor:** a real installed desktop application you launch and that
runs the game — not a website, not a CLI.

---

## 2. Locked decisions & non-negotiables

### Locked by the user (do not re-litigate)
| Decision | Choice | Consequence |
|---|---|---|
| Launcher engine | **Embed Modrinth's Theseus** (`app-lib`, Rust) | MSA auth, Java provisioning, Fabric/Forge/NeoForge/Quilt install, instance mgmt, modpack handling are *reused, not rebuilt*. **The whole app is therefore GPL-3.0 and open-source.** Accepted. |
| Mod sources | **Modrinth only** | True zero-intervention auto-download (whitelisted CDN, hash-verified, free no-auth API). No CurseForge in v1. |
| Catalog freshness | **App polls Modrinth directly + local cache** | No backend to run or pay for. Fully standalone. Always-live data; cached index for offline/degraded use. |
| Quests / difficulty | **Phase 2** | v1 ships curation + launcher + instances + manual editing. Quests come after. |
| Stack (research-determined) | **Tauri 2 + Rust core + web frontend** | Modrinth ships this exact stack in production for the exact same job. Rust core embeds Theseus + Modrinth client + the Anthropic tool-loop. |
| Profile format | **Standard `.mrpack`** | Interoperable, hash-verified, license-safe. We never invent a format. |
| Anthropic key | **BYO user key in OS keychain** | Never ship a shared/embedded key (binaries are inspectable; Anthropic auto-revokes leaked keys). macOS Keychain / Windows Credential Manager via `keyring`. |

### Legal / correctness non-negotiables
- **N1 — Own Microsoft/Xbox OAuth client ID.** Legitimate Minecraft launch requires *our own
  approved Microsoft identity-platform client registered for the Minecraft auth scope.* Theseus
  implements the auth *code path* — it does **not** grant us a client ID. **This has a real,
  non-resumable approval lead time. It is a Day-1 administrative task that runs in parallel with
  coding, not a "later" item.** Nothing ships without it.
- **N2 — Never bundle mod `.jar` files.** Many mods are All-Rights-Reserved / non-redistributable.
  We ship a `.mrpack` manifest and the client downloads each jar from Modrinth's CDN at install
  time, verifying SHA-512. This is the universal launcher pattern and the legal requirement.
- **N3 — Never ship a shared Anthropic API key.** BYO key only (see table). Optional hosted
  metering proxy is a *much-later* monetization concern, explicitly out of scope.
- **N4 — Never bypass/emulate Minecraft auth.** User logs into *their own* owned account. No
  "cracked"/offline-as-piracy path. Offline mode only for an already-authenticated account.
- **N5 — Mojang brand compliance.** Don't imply official endorsement; don't ship Mojang assets;
  brand clearly as a third-party tool.

### Stated product limits (own these in UX, don't discover them mid-build)
- **L1 — Instance mod updates ≠ catalog freshness.** Two different things the prompt conflates:
  *(a)* the **catalog** stays fresh automatically (Modrinth polling — zero intervention, always).
  *(b)* mods **inside an existing instance** are **version-pinned snapshots**. They do **not**
  silently update — silent in-place mod updates corrupt existing worlds. Each instance gets an
  explicit **"Check for updates"** action showing a reviewable diff before applying. This default
  is a deliberate UX decision the spec owns.
- **L2 — Modrinth-only blind spots.** Some CurseForge-exclusive mods won't be findable. Search
  must degrade gracefully: *"Not on Modrinth — here are the closest Modrinth matches,"* never a
  dead end or a silent omission in a curated pack (the curator must tell the user it substituted).
- **L3 — Client-side packs in v1.** "Single vs multiplayer" → v1 exports a **client `.mrpack`**
  using Modrinth `env` (client/server) flags so client-only mods are tagged correctly.
  Dedicated **server-pack export is Phase 2.** "Multiplayer" in v1 = a client pack suitable for
  joining servers, not a server bundle.

---

## 3. Primary user flows

1. **Converse → curated pack → playable instance** *(the hero flow)*
   New instance → chat: *"Cozy Create-based 1.21 NeoForge solo pack, medium difficulty, good
   performance on a MacBook Air."* Claude asks clarifying questions, proposes a pack with
   rationale per mod, **validates it**, you tweak in chat or in the list, confirm → Anvil
   resolves deps, writes the instance, downloads jars, applies optimization JVM flags → **Launch**.

2. **Seed from an existing Modrinth pack, then customize** *(v1 — cheap, big win)*
   *"Start from Fabulously Optimized but add Create and a minimap."* Curator imports a published
   Modrinth modpack as the base and converses about deltas instead of building from zero.

3. **Manual browse / search / edit**
   Search bar over the live Modrinth catalog → **Modrinth-style mod detail page** → add to an
   instance / remove / change version. Full hand-authoring with no AI in the loop.

4. **Instances → launch / manage**
   Grid of instances (pack name, MC version, loader, mod count, last played) → Launch with live
   log console → per-instance settings (RAM, JVM args, Java runtime, "Check for updates").

5. **Phase 2 — Quests & difficulty**
   Toggle "Add a storyline/quests" → Claude proposes a difficulty curve + quest chapters →
   editable **node-graph** → export grounded to the pack's actual item/entity IDs → quests
   appear in-game (FTB Quests).

---

## 4. Feature catalog

Tags: **[v1]** first release · **[P2]** Phase 2 · **[L]** later/nice-to-have.

### A. Conversational Curator
- **[v1]** Claude chat with the streaming **tool-use loop** as the core engine (tools in §5).
- **[v1]** Structured intake: genre/theme, MC version, loader, single vs. multiplayer, kitchen-
  sink vs. focused, difficulty intent, performance target (low-end vs. beefy), content tastes
  (tech/magic/exploration/build/combat), hard requirements/exclusions.
- **[v1]** Per-mod **rationale** ("added Sodium → performance target you set; Create → core of
  the theme you described").
- **[v1]** **Seed-from-existing-pack** (Flow 2): import a published Modrinth pack as a base.
- **[v1]** Substitution transparency: when a requested concept isn't on Modrinth, the curator
  states what it substituted and why (ties to L2) — never a silent gap.
- **[v1]** In-chat mutation: "swap the minimap," "drop anything that hurts FPS," "make it harder."
- **[L]** Curation presets / shareable "recipes" (a saved intake you can re-run on a new MC ver).

### B. Catalog & Search
- **[v1]** Live Modrinth search: facets for `project_type`, MC `versions`, loaders,
  `categories`, `client_side`/`server_side`, `open_source`; sort by relevance/downloads/updated.
- **[v1]** **Local cache layer**: SQLite-backed index of seen projects/versions for fast
  re-browse, offline degraded mode, and a corpus the curator can reason over without re-hitting
  the API every turn. Respects the 300 req/min IP limit with backoff.
- **[v1]** Hash-based update checks (`/version_files/update`) powering the per-instance
  "Check for updates" diff (L1).
- **[v1]** **Modrinth-style mod detail page** (user asked for this verbatim — first-class, not
  buried in "search"). Panels: title + icon + author + download/follow counts; rich
  **description** (rendered markdown); **gallery** carousel; **versions table**
  (version, channel, MC versions, loaders, date, downloads); **dependencies** list
  (required/optional/incompatible/embedded, each linking to its own page);
  **license**; **environment** flags (client/server required/optional/unsupported);
  side info (categories, source/issues/wiki links); primary action: *Add to instance →*.

### C. Pack / Profile Engine
- **[v1]** **Dependency resolver**: required deps pulled transitively; conflicts/incompatibles
  surfaced; version chosen against the target MC + loader.
- **[v1]** **`validate_pack` gate** (see §5): loader/MC compatibility, side-flag consistency,
  dependency version overlap — runs *before* a pack is presented as "assembled." A pack that
  fails validation is never offered as done.
- **[v1]** Export a standard **client `.mrpack`** (`modrinth.index.json` + dual hashes +
  whitelisted download URLs + `overrides/` for configs). Importable by other launchers too.
- **[v1]** Optimization defaults: sensible JVM args + a curated performance-mod baseline
  (e.g. Sodium-class) when the user's performance target is "low-end," clearly labeled.
- **[P2]** Dedicated **server-pack export** (server jar layout, server-side `env` handling) (L3).
- **[L]** Pack diffing / versioned pack history ("what changed since I last played").

### D. Launcher & Instances (Theseus-backed)
- **[v1]** MSA login (our own client ID — N1); multi-account; token auto-refresh.
- **[v1]** Auto Java/JRE provisioning per MC version (Theseus).
- **[v1]** Mod-loader install: Fabric / Forge / NeoForge / Quilt (Theseus).
- **[v1]** Instance grid + create/duplicate/delete; per-instance RAM, JVM args, Java runtime.
- **[v1]** **Launch** with a live streamed log console (Rust `tokio::process` → UI events).
- **[v1]** Per-instance **"Check for updates"** → reviewable mod diff → apply or skip (L1).
- **[v1]** Import an existing `.mrpack` as an instance.
- **[L]** Crash-log summarizer (Claude explains a crash + suggests the offending mod).

### E. UI / Theme
- **[v1]** Claude-minimalist identity with **green** in the accent role coral normally occupies:
  - background/paper `#faf9f5` · surface `#f0eee6` · border `#e8e6dd`
  - ink `#2d2926` · muted `#6b6b6b` · **accent sage-green `#5e8a5a`** (warm/earthy so it sits
    on cream — *not* Minecraft-lime, which would clash and read "generic AI tool").
  - serif display (EB Garamond / Cormorant) + Inter body. Generous whitespace, soft borders.
- **[v1]** Three primary surfaces: **Chat** (curator), **Browse** (catalog + detail page),
  **Instances** (grid + launch). Settings: Anthropic key, accounts, default RAM/Java.
- **[v1]** First-run: paste Anthropic key (stored in OS keychain), MSA sign-in.

### F. Phase 2 — Quests & Difficulty
- **[P2]** Optional "add a storyline / quests" toggle in curation.
- **[P2]** Claude proposes a **difficulty curve**: quest-gated progression (primary lever) +
  one data-driven scaling mod config (Dynamic Difficulty / Apotheosis) + game-rule/recipe
  datapacks. No per-mod deep config, no custom-code mechanics (out of scope).
- **[P2]** **FTB Quests (SNBT)** as the target — only candidate that is machine-writable, ships
  in `config/ftbquests/`, hot-reloads external files, maps 1:1 to a visual node graph via
  per-quest `x/y`, and is actively maintained for modern MC (1.21.x, NeoForge/Forge/Fabric).
- **[P2]** **Editable node-graph UI**: nodes = quests, edges = dependencies, drag to reposition;
  AI emits a structured IR (the editable model), a **deterministic serializer** turns IR →
  SNBT (stable 16-char hex IDs, numeric suffixes). The LLM never free-writes SNBT.
- **[P2]** **Grounding gate** (the make-or-break correctness seam): resolve the pack's mods
  first → build an allowed item/entity/advancement index from the resolved jars → constrain &
  validate every quest task/reward against that index before export. (v1 cousin = `validate_pack`.)
- **[L]** Heracles (JSON) as an alternate target *only if* a pack is pinned to MC 1.20.1.

---

## 5. Architecture (feature-relevant slice)

```
┌─────────────────────────── Tauri app ───────────────────────────┐
│  Web frontend (Claude-minimalist UI: Chat · Browse · Instances)  │
│        ▲ events (stream tokens, logs, progress)   │ commands     │
│  ──────┼───────────────────────────────────────────┼──────────  │
│  Rust core                                                      │
│   • Anthropic client  → streaming Messages API + tool-use loop   │
│   • Modrinth client   → search / project / version / hash-update │
│   • Local cache       → SQLite index (offline + curation corpus) │
│   • Pack engine       → resolve deps · validate · emit .mrpack   │
│   • Theseus (app-lib) → MSA auth · Java · loaders · launch · logs │
│   • Keychain          → Anthropic key + MSA tokens               │
└──────────────────────────────────────────────────────────────────┘
        │ HTTPS                         │ child process
   Modrinth API + CDN              Java → Minecraft
```

**Tool-use contract** (client tools Claude calls; executed in the Rust core):
- `search_mods(query, facets)` → ranked Modrinth results (+ cache).
- `get_mod(project_id)` → detail (description, versions, deps, license, env).
- `resolve_dependencies(mod_set, mc_version, loader)` → closed set + conflicts.
- `validate_pack(mods, mc_version, loader)` → **must pass before Claude declares a pack
  assembled.** Checks loader/MC compatibility, client/server side-flag consistency, and that all
  (transitive) dependencies have an overlapping compatible version. Returns actionable failures
  so Claude can repair the set rather than hand back an incoherent pack that dies at install.
- `seed_from_pack(modrinth_pack_id)` → import a published pack as the working base (Flow 2).
- `assemble_pack(resolved_set, meta)` → write instance + emit client `.mrpack`.
- *(P2)* `generate_quests(graph_ir, allowed_index)` → IR validated against the pack's real IDs;
  deterministic serializer writes FTB Quests SNBT.

**Data/cache model:** SQLite holds projects, versions, dependency edges, and last-sync
timestamps. Catalog freshness = incremental poll of `/search?index=updated` (stop at last-seen)
+ hash-update checks; *never* a silent bulk dump (Modrinth has none). Instance state =
version-pinned manifest snapshot; updates are explicit and diffed (L1).

---

## 6. Phased roadmap (checkable)

**Phase 0 — Foundations (parallel tracks)**
- [ ] N1: register & submit Microsoft/Xbox OAuth client for Minecraft auth scope *(start Day 1, blocks launch, non-resumable)*
- [ ] Tauri 2 + Rust core skeleton; GPL-3.0 license + Mojang-compliance notice
- [ ] Embed Theseus `app-lib`; prove MSA login + a vanilla launch end-to-end
- [ ] Claude-minimalist theme system (green accent, serif/Inter, the three surfaces)

**Phase 1 — Manual launcher (no AI yet)**
- [ ] Modrinth client + SQLite cache + facet search UI
- [ ] Modrinth-style mod detail page (all panels in §4-B)
- [ ] Dependency resolver + `validate_pack` + client `.mrpack` export
- [ ] Instance grid, create/edit, launch + live log console
- [ ] Per-instance "Check for updates" diff (L1)
- [ ] Import existing `.mrpack`

**Phase 2 — Conversational curator**
- [ ] Anthropic streaming tool-use loop wired to the §5 tools
- [ ] Structured intake + per-mod rationale + substitution transparency (L2)
- [ ] `seed_from_pack` (Flow 2)
- [ ] Hero flow end-to-end: chat → validated pack → instance → launch

**Phase 3 — Quests & difficulty (the P2 feature set)**
- [ ] Allowed-ID index builder (grounding gate) from a resolved pack
- [ ] Quest IR + deterministic FTB Quests SNBT serializer + stable hex-ID allocator
- [ ] Node-graph editor UI
- [ ] Difficulty layer: quest gating + one scaling-mod config + datapack tweaks
- [ ] Server-pack export (L3)

---

## 7. Open decisions (deferred — not blocking this spec)

- Exact green hue: `#5e8a5a` (sage) vs. `#4a7c4e` (mossy) — pick during theming.
- How aggressively the curator auto-adds performance mods vs. asking first.
- Cache eviction / max local DB size policy.
- Multi-account UX depth (fast switch vs. just store-and-pick).
- Crash-log summarizer (E-[L]) — decide after the hero flow proves the tool-loop pattern.
- Distribution: signed/notarized builds cost ~$99/yr (Apple) + ~$120–400/yr (Windows cert);
  decide when nearing release, not now.

---

*End of feature spec v0.1. Decisions in §2 are locked. Next deliverable on go-ahead:
`tech_stack.md` + a detailed Phase 0/1 implementation plan.*
