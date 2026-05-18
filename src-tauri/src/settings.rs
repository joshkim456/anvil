//! App settings + on-disk paths. Keychain storage is a deferred milestone; for
//! now secrets live in `~/.anvil/settings.json` (0600). The Anthropic key and
//! the Microsoft OAuth client ID are user-supplied — never shipped.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Anvil's own registered Azure app client ID (public OAuth client — NOT a
/// secret; this is the CurseForge/Modrinth model: the app ships its own ID so
/// users never register anything). Overridable via Settings if desired.
pub const DEFAULT_MS_CLIENT_ID: &str = "c43883a5-dbae-4e96-8bfe-8d1fca0c0e81";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// User's own Anthropic API key (BYO; powers the curator).
    pub anthropic_api_key: Option<String>,
    /// Optional override for the Azure app client ID; falls back to
    /// `DEFAULT_MS_CLIENT_ID` when unset.
    pub ms_client_id: Option<String>,
    /// UI theme preference: "light" | "dark" | "system". Unset = "system".
    pub theme: Option<String>,
}

/// Effective theme preference; always one of "light" | "dark" | "system".
pub fn theme() -> String {
    load()
        .theme
        .filter(|t| matches!(t.as_str(), "light" | "dark" | "system"))
        .unwrap_or_else(|| "system".to_string())
}

/// Effective Anthropic key: the Settings value if set, else the
/// `ANTHROPIC_API_KEY` environment variable (handy for `tauri dev`). A
/// shipped app has no `.env`; Settings is the real mechanism.
pub fn anthropic_key() -> Option<String> {
    load()
        .anthropic_api_key
        .filter(|k| !k.trim().is_empty())
        .or_else(|| {
            std::env::var("ANTHROPIC_API_KEY")
                .ok()
                .filter(|k| !k.trim().is_empty())
        })
}

/// Effective Microsoft client ID. Resolution order:
/// 1. Settings override (`ms_client_id` in settings.json / Settings UI)
/// 2. `ANVIL_MS_CLIENT_ID` env var (handy in `tauri dev` via gitignored
///    `.env`; lets you test launch with a known-approved client ID while
///    Anvil's own ID is pending the Microsoft mce-reviewappid approval)
/// 3. The built-in `DEFAULT_MS_CLIENT_ID`
/// Always returns a usable value.
pub fn ms_client_id() -> String {
    load()
        .ms_client_id
        .filter(|c| !c.trim().is_empty())
        .or_else(|| {
            std::env::var("ANVIL_MS_CLIENT_ID")
                .ok()
                .filter(|c| !c.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_MS_CLIENT_ID.to_string())
}

pub fn data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".anvil")
}

pub fn instances_dir() -> PathBuf {
    data_dir().join("instances")
}

/// Shared Minecraft assets/libraries/versions (deduped across instances).
pub fn shared_mc_dir() -> PathBuf {
    data_dir().join("minecraft")
}

fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

pub fn load() -> Settings {
    std::fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(s: &Settings) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir())?;
    let p = settings_path();
    std::fs::write(&p, serde_json::to_string_pretty(s).unwrap_or_default())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}
