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
}

/// Effective Microsoft client ID: the user's override if set, else the
/// built-in default. Always returns a usable value.
pub fn ms_client_id() -> String {
    load()
        .ms_client_id
        .filter(|c| !c.trim().is_empty())
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
