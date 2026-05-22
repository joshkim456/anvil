# Anvil

**An AI Minecraft modpack curator and launcher.** Describe the pack you want in
plain language; Claude designs it against the live Modrinth catalog, resolves
dependencies, validates and boot-tests it, writes a standard `.mrpack`, and
launches the game. Chat in, a coherent playable artifact out.

License: **GPL-3.0-only**. Anvil is an unofficial third-party launcher, not
affiliated with Mojang or Microsoft.

---

## Status

A working desktop app (macOS / Windows via Tauri):

| Area | State |
|---|---|
| **Browse** | Live Modrinth catalog with sort, MC version, loader, multi-select genre filters, infinite scroll, and a Modrinth-style detail view. |
| **Curator** | Conversational designer: Anthropic Messages API streaming over a phased tool-use loop (propose, search, assemble, verify, edit, query registry, generate quests/origins) on the live catalog and pack engine. Probes theme, version, loader, single vs. multiplayer, and performance target before proposing, with per-mod rationale. |
| **Pack engine** | Transitive dependency-closure resolution, validation (loader / MC / side-flag / duplicate / insecure-URL), `.mrpack` emit and import. Boot-verified: the curator launches the pack headless and scans the log, attributing crashes to the exact mod before shipping. |
| **Progression** | AI-authored quest graph (Heracles), custom recipes and self-contained boss encounters (Open Loader datapacks), and Origins powers, including a shared main questline with optional per-origin side paths gated to each origin. Every id is grounded against the resolved pack's real registry, so a fabricated id is rejected, not shipped. |
| **Launch core** | Custom Rust launcher: Mojang piston-meta manifest, library/native/asset download, Fabric + Quilt install, classpath/JVM/argument construction, live log streaming. Forge/NeoForge planned. |
| **Sign in** | Microsoft MSA OAuth (auth-code + PKCE) → Xbox Live → XSTS → Minecraft Services via the official identity platform. Authentication is never bypassed or emulated. |
| **Instances** | Version-pinned profiles with persistence, `.mrpack` import, duplicate-by-name reuse, light/dark theme. |

**Known external gate:** Minecraft's `login_with_xbox` only accepts a
Microsoft-approved client id. Anvil ships the well-known public
Minecraft-launcher client id that established open-source launchers use; a fresh
Azure app returns `403 Invalid app registration` until approved via
<https://aka.ms/mce-reviewappid>. Users can supply their own approved id in
Settings. This is a Microsoft policy surface, not a code limitation.

## How it works

```
┌──────────────────────── Tauri 2 app ────────────────────────┐
│  React + TypeScript frontend  (Curator · Browse · Instances) │
│        ▲ events (stream / logs / progress)  │ commands       │
│  Rust core                                                   │
│   • modrinth   live catalog client                           │
│   • curator    Anthropic Messages API SSE + phased tool loop │
│   • pack       dep resolution · validate · .mrpack I/O       │
│   • quest/recipe/content/origins  progression → Heracles +   │
│                Open Loader datapacks                          │
│   • registry   real-id grounding for authored progression    │
│   • auth       MSA auth-code+PKCE · Xbox · XSTS · Minecraft  │
│   • launch     manifest · assets · loader · JVM · log stream │
│   • instance   version-pinned profiles + persistence         │
└──────────────────────────────────────────────────────────────┘
        │ HTTPS                              │ child process
   Modrinth API / CDN · Anthropic API   java → Minecraft
```

Profiles use the standard `.mrpack` format for cross-launcher interoperability.
Instances are version-pinned snapshots; mods never update silently. Mod jars are
never bundled, the manifest references the Modrinth CDN and the client downloads
and SHA-512-verifies each at install time.

## Getting started

Prerequisites: a recent **Rust** toolchain (`rustup`), **Node.js** 18+, and the
platform's WebView (preinstalled on macOS; WebView2 on Windows).

```sh
npm install
npm run tauri dev          # build the Rust core and run the desktop app
cd src-tauri && cargo test # pack-engine + progression test suite
npm run tauri build        # distributable .app/.dmg (macOS) / installer (Windows)
```

**Configuration:** the Curator needs your own **Anthropic API key** (Settings,
stored locally in `~/.anvil/settings.json`, 0600; none is shipped). Sign-in uses
a bundled public OAuth client id, overridable in Settings.

## Project layout

```
src/                      React + TypeScript frontend (surfaces, components, lib)
src-tauri/src/
  lib.rs        Tauri commands + event bridging
  modrinth.rs   Modrinth API v2 client
  curator.rs    Anthropic streaming + phased tool-use loop
  pack.rs       dependency resolver · validate · .mrpack
  quest.rs      progression graph + Heracles emit + grounding
  recipe.rs     custom-recipe facet → Open Loader datapack
  content.rs    self-contained boss/content provisioning
  origins.rs    Origins/Apoli powers datapack + per-origin questline bridge
  registry.rs   real-id registry vocab + grounding
  auth.rs       Microsoft / Xbox / Minecraft sign-in
  launch.rs     custom Minecraft launch core
  instance.rs   instance model + persistence
```

## Tech stack

**Tauri 2** (Rust core) + **React 18** + **TypeScript** + **Vite**. Rust:
`reqwest` (rustls), `tokio`, `serde`, `anyhow`, `sha2`, `zip`, `chrono`;
`p256`/`base64`/`uuid` for the sign-in crypto path. No bundled secrets, no
redistributed mod jars.

## Roadmap

End-to-end in-game verification of authored progression, per-genre chapter
adaptation, Forge/NeoForge launch, JRE auto-provisioning, SQLite catalog cache,
OS-keychain secret storage.

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
