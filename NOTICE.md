# NOTICE

Anvil — AI Minecraft modpack curator + launcher.
Copyright (C) 2026 Joshua Kim.

This program is free software: you can redistribute it and/or modify it under
the terms of the **GNU General Public License v3.0** as published by the Free
Software Foundation. See [`LICENSE`](./LICENSE) for the full text.

Anvil is released under GPL-3.0 as a deliberate open-source choice. It does
**not** embed Modrinth's Theseus core: the Microsoft sign-in chain and the
Minecraft launch core (version manifest, libraries, assets, loader install,
JVM/argument construction, log streaming) are an independent, from-scratch
Rust implementation built on permissively licensed crates.

## Third-party services & trademarks

- **Modrinth** — mod metadata and downloads are sourced from the public
  Modrinth API (https://modrinth.com) under their API terms. Mod `.jar` files
  are **never bundled**; the client downloads them from Modrinth's CDN at
  install time and verifies SHA-512 hashes.
- **Minecraft** is a trademark of Mojang AB / Microsoft. Anvil is an
  **unofficial, third-party tool** and is not affiliated with, endorsed by, or
  associated with Mojang or Microsoft. Anvil never bypasses Minecraft
  authentication; players sign in to their own legitimately-owned accounts via
  Microsoft's identity platform. No Mojang assets are redistributed.
- **Anthropic / Claude** — conversational curation uses the Anthropic API with
  a user-supplied API key stored locally in `~/.anvil/settings.json` (0600).
  No shared key is shipped.

See `claude context files/feature_spec.md` for the full design and the locked
non-negotiables (N1–N5).
