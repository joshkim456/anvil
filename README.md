# Anvil

**An AI Minecraft modpack curator and launcher.** Converse with Claude to
design a modpack (genre, version, single vs. multiplayer, performance target);
Anvil resolves dependencies, validates it, writes a standard `.mrpack` and
instance, and launches the game. Think "WorldForge, but for Minecraft
modpacks": chat in, a coherent playable artifact out.

License: **GPL-3.0-only** (open source). Anvil is an unofficial third-party
launcher and is not affiliated with Mojang or Microsoft.

---

## Status

Anvil is a working desktop app (macOS / Windows via Tauri). What is built:

| Area | State |
|---|---|
| **Browse mods** | Live Modrinth catalog, pre-populated, with sort, MC version, loader, multi-select genre filters and infinite scroll. Modrinth-style mod detail (description, versions, dependencies, environment, license, links). |
| **Curator** | Conversational modpack designer: Anthropic Messages API streaming with a phased tool-use loop (`propose_pack`, `search_mods`, `get_mod`, `validate_pack`, `assemble_pack`, `verify_pack`, `seed_from_pack`, `generate_quests`, `query_registry`) over the live Modrinth catalog and the pack engine. Free-form chat that probes for theme, version + loader, single vs. multiplayer, and performance target before proposing, with per-mod rationale. |
| **Pack engine** | Transitive dependency-closure resolution, `validate_pack` (loader / MC / side-flag / duplicate / insecure-URL gate), standard `.mrpack` emit and import. Covered by `cargo test`. |
| **Progression** | AI-authored progression graph: ordered quest chapters, custom recipes, and self-contained content/boss encounters, emitted as Heracles quest JSON plus Open Loader datapacks. Every id is grounded against the resolved pack's real registry (a fabricated id is rejected, not shipped). Implemented and unit-tested; end-to-end in-game verification is in progress. |
| **Instances** | Version-pinned instance model with persistence; `.mrpack` import; duplicate-by-name reuse; configurable light/dark theme. |
| **Sign in with Microsoft** | Legacy MSA OAuth (authorization code + PKCE) → Sisu → Xbox Live → XSTS → Minecraft Services, using the well-known public Minecraft-launcher client id. Uses the official Microsoft identity platform; users sign in to their own accounts. Authentication is never bypassed or emulated. |
| **Launch core** | Custom Rust launcher: Mojang piston-meta version manifest, library/native and asset download with host-arch native selection, Fabric and Quilt loader install with newest-stable loader resolution, classpath/JVM/argument construction, live log streaming. Vanilla + Fabric + Quilt implemented; Forge/NeoForge planned. |

Known external gate: Minecraft's `login_with_xbox` API only accepts a
Microsoft-approved client id. Anvil ships the well-known public
Minecraft-launcher client id that established open-source launchers use; a
freshly registered Azure app returns `403 Invalid app registration` until
approved via <https://aka.ms/mce-reviewappid>. The auth chain is implemented
and the crypto path is unit-tested; sign-in is usable with an approved client
id, and a user can supply their own approved id in Settings. This is a
Microsoft policy surface, not a code limitation.

## Features

- **Conversational curation.** Describe the pack you want in plain language;
  Claude asks natural follow-ups, searches real Modrinth mods (never invented),
  explains each inclusion, validates the set, and assembles a real instance and
  `.mrpack`.
- **Authored progression.** Optionally, Claude designs an ordered questline
  with custom recipes and self-contained boss/content encounters, delivered as
  Heracles quest JSON and Open Loader datapacks. Every entity, item, and recipe
  id is grounded against the resolved pack's real registry, so a quest never
  references something the pack does not contain.
- **Full catalog browser.** Popular-by-default grid, sort (Popular / Relevant /
  Followed / Updated / Newest), MC version, loader, and multi-select genre, with
  infinite scroll and a Modrinth-style detail view.
- **Standalone and license-clean.** Modrinth-only sourcing: no API key, every
  download auto-resolvable, mod jars never bundled (manifest references the
  Modrinth CDN; the client downloads and SHA-512-verifies at install time).
- **Real launcher.** Custom launch core provisions the game from Mojang's
  official metadata and streams logs; the player signs in with their own
  Microsoft account.
- **Claude-minimalist UI.** Warm paper canvas, serif display, a single
  sage-green accent; a Claude-style chat surface for the curator.

## How it works

```
┌──────────────────────── Tauri 2 app ────────────────────────┐
│  React + TypeScript frontend  (Curator · Browse · Instances) │
│        ▲ events (stream / logs / progress)  │ commands       │
│  Rust core                                                   │
│   • modrinth   live catalog client (contact User-Agent)      │
│   • curator    Anthropic Messages API SSE + phased tool loop │
│   • pack       dep resolution · validate · .mrpack I/O       │
│   • quest/recipe/content  progression graph → Heracles +     │
│                Open Loader datapacks                          │
│   • registry   real-id grounding for authored progression    │
│   • auth       MSA auth-code+PKCE · Sisu · Xbox · XSTS · MC  │
│   • launch     manifest · assets · loader · JVM · log stream │
│   • instance   version-pinned profiles + persistence         │
└──────────────────────────────────────────────────────────────┘
        │ HTTPS                              │ child process
   Modrinth API / CDN · Anthropic API   java → Minecraft
```

Profiles use the standard `.mrpack` format for interoperability with other
launchers. Instances are version-pinned snapshots; mods do not update silently.

## Getting started

Prerequisites: a recent **Rust** toolchain (`rustup`), **Node.js** 18+, and
the platform's WebView (preinstalled on macOS; WebView2 on Windows).

```sh
npm install
npm run tauri dev          # build the Rust core and run the desktop app

cd src-tauri && cargo test # pack-engine test suite
```

Build a distributable app:

```sh
npm run tauri build        # produces a .app + .dmg (macOS) / installer (Windows)
```

### Configuration

- **Curator** needs your own **Anthropic API key**, entered in Settings and
  stored locally in `~/.anvil/settings.json` (0600). No key is shipped.
- **Sign in with Microsoft** uses a bundled public OAuth client ID. It is
  overridable in Settings if you have your own Microsoft-approved client ID.
  (Public OAuth client IDs are not secrets; this mirrors how other open-source
  launchers ship.)

## Project layout

```
src/                      React + TypeScript frontend
  surfaces/               Curator · Browse · Instances · Quests · Settings
  components/              Dropdown, ModDetail, ...
  lib/                     api · theme · Minecraft keybinds
src-tauri/src/
  lib.rs                  Tauri commands + event bridging
  modrinth.rs             Modrinth API v2 client
  curator.rs              Anthropic streaming + phased tool-use loop
  chat.rs                 conversation/session state
  pack.rs                 dependency resolver · validate · .mrpack
  quest.rs                progression graph + Heracles emit + grounding
  recipe.rs               custom-recipe facet → Open Loader datapack
  content.rs              self-contained boss/content provisioning
  registry.rs             real-id registry vocab + grounding
  origins.rs              Origins/Apoli powers datapack (design)
  auth.rs                 Microsoft / Xbox / Minecraft sign-in
  launch.rs               custom Minecraft launch core
  instance.rs             instance model + persistence
  cache.rs                on-disk catalog/response cache
  keybinds.rs             default Minecraft keybind options
  settings.rs             settings + on-disk paths
src-tauri/tests/          real-data fixtures (Modrinth + codec shapes)
```

## Tech stack

- **Tauri 2** (Rust core) + **React 18** + **TypeScript** + **Vite**.
- Rust: `reqwest` (rustls), `tokio`, `serde`/`serde_json`, `futures-util`,
  `anyhow`, `thiserror`, `sha2`, `hex`, `zip`, `chrono`, `tracing`;
  `p256`/`base64`/`rand`/`uuid` for the MSA sign-in crypto path.
- No bundled secrets. No mod jars redistributed.

## Roadmap

Next: end-to-end in-game verification of authored progression, per-genre
chapter-archetype adaptation, Forge/NeoForge launch, JRE auto-provisioning,
SQLite catalog cache, OS-keychain secret storage.

## Legal and licensing

- **GPL-3.0-only.** See [`LICENSE`](./LICENSE) and [`NOTICE.md`](./NOTICE.md).
- Anvil is an **unofficial third-party tool**, not affiliated with, endorsed
  by, or associated with Mojang or Microsoft. "Minecraft" is a trademark of
  Mojang AB / Microsoft.
- Anvil uses the **official Microsoft identity platform**; players authenticate
  their own legitimately owned accounts. Authentication is never bypassed,
  emulated, or circumvented, and no Mojang assets are redistributed.
- Mod files are never bundled. Profiles are manifests; the client downloads
  each mod from the Modrinth CDN and verifies SHA-512 at install time.

## Acknowledgements

- [Modrinth](https://modrinth.com) for the open mod catalog and API.
- [Anthropic](https://www.anthropic.com) Claude for the curation model.
- [Heracles](https://modrinth.com/mod/heracles) and
  [Open Loader](https://modrinth.com/mod/open-loader) for the quest and
  datapack delivery the progression layer targets.
