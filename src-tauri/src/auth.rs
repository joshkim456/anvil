//! Microsoft / Xbox sign-in for Minecraft (device-code OAuth).
//!
//! CONTRACT — these public signatures + type shapes are preserved so `lib.rs`
//! integration compiles. Only the bodies are implemented here.
//!
//! Flow: MSA device code -> poll for MS token -> Xbox Live (user.auth.xboxlive
//! .com) -> XSTS (xsts.auth.xboxlive.com) -> Minecraft Services
//! (api.minecraftservices.com/authentication/login_with_xbox) -> profile.
//! `client_id` is the user's own Azure app id (from settings); never bypass or
//! emulate auth (spec N4). Persist account to `~/.anvil/account.json`.

use anyhow::{anyhow, Context};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub interval: u64,
    pub expires_in: u64,
}

#[derive(Debug, Clone)]
pub struct MsToken {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MinecraftAccount {
    pub username: String,
    pub uuid: String,
    /// Minecraft (not MS) bearer token used at launch.
    pub minecraft_token: String,
    pub ms_refresh_token: String,
    /// Unix seconds; refresh when close.
    pub expires_at: i64,
}

/// One poll result: `Some(token)` once the user authorized, `None` while still
/// pending (caller waits `interval` and polls again).
pub type PollResult = Option<MsToken>;

// MSA "consumers" tenant endpoints (personal Microsoft accounts).
const DEVICECODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
// Xbox Live needs XboxLive.signin; offline_access yields a refresh token.
const SCOPE: &str = "XboxLive.signin offline_access";

/// Shared HTTP client with a descriptive user agent.
fn http() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Anvil/0.1")
        .build()
        .context("building HTTP client")
}

// ---- 1. Device code ----

pub async fn begin_device_code(client_id: &str) -> anyhow::Result<DeviceCodeStart> {
    #[derive(Deserialize)]
    struct Resp {
        device_code: String,
        user_code: String,
        verification_uri: String,
        interval: u64,
        expires_in: u64,
    }

    let client = http()?;
    let resp = client
        .post(DEVICECODE_URL)
        .form(&[("client_id", client_id), ("scope", SCOPE)])
        .send()
        .await
        .context("requesting device code")?;

    let status = resp.status();
    let body = resp.text().await.context("reading device-code body")?;
    if !status.is_success() {
        return Err(anyhow!("device code request failed ({status}): {body}"));
    }

    let r: Resp = serde_json::from_str(&body)
        .with_context(|| format!("parsing device-code response: {body}"))?;
    Ok(DeviceCodeStart {
        device_code: r.device_code,
        user_code: r.user_code,
        verification_uri: r.verification_uri,
        interval: r.interval,
        expires_in: r.expires_in,
    })
}

// ---- 2. Poll for the MS token ----

pub async fn poll_token(
    client_id: &str,
    device_code: &str,
) -> anyhow::Result<PollResult> {
    #[derive(Deserialize)]
    struct Ok_ {
        access_token: String,
        refresh_token: String,
        expires_in: u64,
    }
    #[derive(Deserialize)]
    struct Err_ {
        error: String,
        #[serde(default)]
        error_description: String,
    }

    let client = http()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            (
                "grant_type",
                "urn:ietf:params:oauth:grant-type:device_code",
            ),
            ("client_id", client_id),
            ("device_code", device_code),
        ])
        .send()
        .await
        .context("polling device-code token")?;

    let status = resp.status();
    let body = resp.text().await.context("reading poll body")?;

    if status.is_success() {
        let t: Ok_ = serde_json::from_str(&body)
            .with_context(|| format!("parsing token response: {body}"))?;
        return Ok(Some(MsToken {
            access_token: t.access_token,
            refresh_token: t.refresh_token,
            expires_in: t.expires_in,
        }));
    }

    // Non-success: AAD returns 400 with a JSON `error` code for pending states.
    let e: Err_ = serde_json::from_str(&body)
        .with_context(|| format!("parsing token error ({status}): {body}"))?;
    match e.error.as_str() {
        // Still waiting on the user — caller should keep polling.
        "authorization_pending" | "slow_down" => Ok(None),
        // Terminal failures.
        "authorization_declined" => Err(anyhow!("Sign-in was declined.")),
        "expired_token" => {
            Err(anyhow!("The device code expired. Please start sign-in again."))
        }
        "bad_verification_code" => {
            Err(anyhow!("Invalid device code. Please start sign-in again."))
        }
        other => Err(anyhow!("Sign-in failed: {other} ({})", e.error_description)),
    }
}

// ---- 3. MS token -> Minecraft account (Xbox/XSTS/MC + profile) ----

pub async fn minecraft_login(ms: &MsToken) -> anyhow::Result<MinecraftAccount> {
    let client = http()?;

    // 3a. Xbox Live: trade the MS access token for an Xbox user token.
    #[derive(Deserialize)]
    struct XblResp {
        #[serde(rename = "Token")]
        token: String,
        #[serde(rename = "DisplayClaims")]
        display_claims: DisplayClaims,
    }
    #[derive(Deserialize)]
    struct DisplayClaims {
        xui: Vec<Xui>,
    }
    #[derive(Deserialize)]
    struct Xui {
        uhs: String,
    }

    let xbl: XblResp = client
        .post("https://user.auth.xboxlive.com/user/authenticate")
        .json(&json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={}", ms.access_token),
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }))
        .send()
        .await
        .context("Xbox Live authentication request")?
        .error_for_status()
        .context("Xbox Live authentication rejected")?
        .json()
        .await
        .context("parsing Xbox Live response")?;

    let uhs = xbl
        .display_claims
        .xui
        .first()
        .map(|x| x.uhs.clone())
        .ok_or_else(|| anyhow!("Xbox Live response missing user hash"))?;

    // 3b. XSTS: exchange the Xbox token for a Minecraft-relying-party token.
    let xsts_resp = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .json(&json!({
            "Properties": {
                "SandboxId": "RETAIL",
                "UserTokens": [xbl.token],
            },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT",
        }))
        .send()
        .await
        .context("XSTS authorization request")?;

    let xsts_status = xsts_resp.status();
    let xsts_body = xsts_resp.text().await.context("reading XSTS body")?;

    if xsts_status == reqwest::StatusCode::UNAUTHORIZED {
        // XSTS surfaces account problems via the numeric `XErr` code.
        #[derive(Deserialize)]
        struct XstsErr {
            #[serde(rename = "XErr", default)]
            xerr: u64,
        }
        let xerr = serde_json::from_str::<XstsErr>(&xsts_body)
            .map(|e| e.xerr)
            .unwrap_or(0);
        return Err(match xerr {
            2148916233 => anyhow!(
                "This Microsoft account has no Xbox account. Create one at xbox.com, then try again."
            ),
            2148916238 => anyhow!(
                "This account belongs to a minor and must be added to a Family group before it can sign in."
            ),
            _ => anyhow!("XSTS authorization failed (XErr {xerr}): {xsts_body}"),
        });
    }
    if !xsts_status.is_success() {
        return Err(anyhow!(
            "XSTS authorization failed ({xsts_status}): {xsts_body}"
        ));
    }

    #[derive(Deserialize)]
    struct XstsResp {
        #[serde(rename = "Token")]
        token: String,
        #[serde(rename = "DisplayClaims")]
        display_claims: DisplayClaims,
    }
    let xsts: XstsResp = serde_json::from_str(&xsts_body)
        .with_context(|| format!("parsing XSTS response: {xsts_body}"))?;

    // The user hash paired with the identity token must come from the XSTS
    // response, not the earlier Xbox Live one. They are usually identical, but
    // the XSTS uhs is the correct, documented value; fall back to the XBL one.
    let uhs = xsts
        .display_claims
        .xui
        .first()
        .map(|x| x.uhs.clone())
        .unwrap_or(uhs);

    // 3c. Minecraft Services: log in with the Xbox identity token.
    #[derive(Deserialize)]
    struct McResp {
        access_token: String,
        #[serde(default)]
        expires_in: Option<u64>,
    }
    let mc_resp = client
        .post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .header("Accept", "application/json")
        .json(&json!({
            "identityToken": format!("XBL3.0 x={};{}", uhs, xsts.token),
        }))
        .send()
        .await
        .context("Minecraft login_with_xbox request")?;
    let mc_status = mc_resp.status();
    let mc_body = mc_resp
        .text()
        .await
        .context("reading login_with_xbox body")?;
    if !mc_status.is_success() {
        return Err(anyhow!(
            "Minecraft login_with_xbox failed ({mc_status}). This usually means \
             the account does not own Minecraft: Java Edition (owning an Xbox or \
             Microsoft account is not the same as owning Java Edition). \
             Microsoft said: {mc_body}"
        ));
    }
    let mc: McResp = serde_json::from_str(&mc_body)
        .with_context(|| format!("parsing Minecraft login response: {mc_body}"))?;

    let minecraft_token = mc.access_token;
    // Minecraft tokens last ~24h; default if the field is absent.
    let expires_in = mc.expires_in.unwrap_or(86400);

    // 3d. Profile: confirm ownership and fetch username + UUID.
    let profile_resp = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&minecraft_token)
        .send()
        .await
        .context("Minecraft profile request")?;

    let profile_status = profile_resp.status();
    if profile_status == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!(
            "This Microsoft account does not own Minecraft: Java Edition (it has \
             no Java profile). Owning an Xbox/Microsoft account is not enough."
        ));
    }
    let profile_body = profile_resp
        .text()
        .await
        .context("reading Minecraft profile body")?;
    if !profile_status.is_success() {
        return Err(anyhow!(
            "Minecraft profile failed ({profile_status}): {profile_body}"
        ));
    }

    #[derive(Deserialize)]
    struct Profile {
        id: String,
        name: String,
    }
    let profile: Profile = serde_json::from_str(&profile_body)
        .with_context(|| format!("parsing Minecraft profile: {profile_body}"))?;

    Ok(MinecraftAccount {
        username: profile.name,
        uuid: profile.id, // undashed, as returned by the API
        minecraft_token,
        ms_refresh_token: ms.refresh_token.clone(),
        expires_at: chrono::Utc::now().timestamp() + expires_in as i64,
    })
}

// ---- 4. Refresh if near expiry ----

pub async fn ensure_fresh(
    client_id: &str,
    acct: MinecraftAccount,
) -> anyhow::Result<MinecraftAccount> {
    // Refresh a little early (5 min) so launches never race expiry.
    if chrono::Utc::now().timestamp() < acct.expires_at - 300 {
        return Ok(acct);
    }

    #[derive(Deserialize)]
    struct RefreshResp {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        expires_in: u64,
    }

    let client = http()?;
    let resp = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client_id),
            ("refresh_token", acct.ms_refresh_token.as_str()),
            ("scope", SCOPE),
        ])
        .send()
        .await
        .context("refreshing MS token")?;

    let status = resp.status();
    let body = resp.text().await.context("reading refresh body")?;
    if !status.is_success() {
        return Err(anyhow!("Token refresh failed ({status}): {body}"));
    }
    let r: RefreshResp = serde_json::from_str(&body)
        .with_context(|| format!("parsing refresh response: {body}"))?;

    // Keep the old refresh token if AAD didn't rotate it.
    let new_refresh = r
        .refresh_token
        .unwrap_or_else(|| acct.ms_refresh_token.clone());

    let ms = MsToken {
        access_token: r.access_token,
        refresh_token: new_refresh,
        expires_in: r.expires_in,
    };

    // Re-run the full Minecraft chain with the fresh MS token.
    minecraft_login(&ms).await
}

// ---- Persistence: ~/.anvil/account.json ----

fn account_path() -> std::path::PathBuf {
    crate::settings::data_dir().join("account.json")
}

pub fn load_account() -> Option<MinecraftAccount> {
    let s = std::fs::read_to_string(account_path()).ok()?;
    serde_json::from_str(&s).ok()
}

pub fn save_account(a: &MinecraftAccount) -> std::io::Result<()> {
    let dir = crate::settings::data_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(a).unwrap_or_default();
    std::fs::write(account_path(), json)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            account_path(),
            std::fs::Permissions::from_mode(0o600),
        );
    }
    Ok(())
}

pub fn clear_account() {
    let _ = std::fs::remove_file(account_path());
}
