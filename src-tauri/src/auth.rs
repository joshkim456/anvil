//! Microsoft / Xbox sign-in for Minecraft — Modrinth/Theseus "legacy"
//! `login.live.com` scheme with the well-known Minecraft launcher client id
//! `00000000402b5328`. This avoids the Mojang app-registration allow-list that
//! blocks self-registered Azure client ids (`aka.ms/mce-reviewappid`).
//!
//! This module is pure protocol (no Tauri). The interactive browser step is
//! driven by `lib.rs`, which opens a webview at [`LoginFlow::auth_url`], polls
//! its URL, and on the `oauth20_desktop.srf?code=` redirect calls
//! [`finish_login`]. Refresh ([`ensure_fresh`]) is non-interactive and reuses
//! the persisted device key.
//!
//! Flow: device key + device token -> Sisu Authenticate (returns the MS login
//! URL) -> user signs in -> auth code -> oauth20_token -> Sisu Authorize ->
//! XSTS -> `/launcher/login` -> entitlements + profile.
//!
//! Every Xbox-side POST is signed with a per-install P-256 device key
//! (ProofOfPossession). The signature scheme is Microsoft's: a packed buffer
//! over a Windows FILETIME timestamp, the method, the URL path and the body,
//! ECDSA-P256/SHA-256 signed, then framed and base64'd.

use anyhow::{anyhow, Context};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use chrono::{DateTime, Utc};
use p256::ecdsa::{signature::Signer, Signature, SigningKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const CLIENT_ID: &str = "00000000402b5328";
const SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
const REDIRECT_URI: &str = "https://login.live.com/oauth20_desktop.srf";
const TITLE_ID: &str = "1794566092";
const TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";

/// Prefix of the URL the webview lands on once the user has signed in; `lib.rs`
/// detects this to pull the `?code=` out and call [`finish_login`].
pub const REDIRECT_PREFIX: &str = "https://login.live.com/oauth20_desktop.srf";

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

/// Carried between [`begin_login`] (built the device key + got the MS login
/// URL) and [`finish_login`] (exchanges the returned auth code).
pub struct LoginFlow {
    device: DeviceKey,
    device_token: String,
    verifier: String,
    session_id: String,
    /// The Microsoft sign-in URL to open in a webview.
    pub auth_url: String,
}

fn http() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("Anvil/0.1")
        .build()
        .context("building HTTP client")
}

// ---- Device key (P-256, ProofOfPossession) -------------------------------

/// A per-install ECDSA P-256 key plus its Xbox device id. Persisted so refresh
/// can re-mint a device token with the same proof key.
#[derive(Clone)]
struct DeviceKey {
    key: SigningKey,
    /// `{UPPERCASE-UUID}` form Xbox expects in the device-token request.
    id: String,
}

#[derive(Serialize, Deserialize)]
struct DeviceKeyFile {
    id: String,
    /// base64 of the 32-byte private scalar.
    key: String,
}

impl DeviceKey {
    fn generate() -> Self {
        let key = SigningKey::random(&mut rand::rngs::OsRng);
        let id = format!("{{{}}}", uuid::Uuid::new_v4().to_string().to_uppercase());
        Self { key, id }
    }

    fn load() -> Option<Self> {
        let s = std::fs::read_to_string(device_key_path()).ok()?;
        let f: DeviceKeyFile = serde_json::from_str(&s).ok()?;
        let bytes = B64.decode(f.key).ok()?;
        let key = SigningKey::from_slice(&bytes).ok()?;
        Some(Self { key, id: f.id })
    }

    fn save(&self) -> std::io::Result<()> {
        let dir = crate::settings::data_dir();
        std::fs::create_dir_all(&dir)?;
        let f = DeviceKeyFile {
            id: self.id.clone(),
            key: B64.encode(self.key.to_bytes()),
        };
        let path = device_key_path();
        std::fs::write(&path, serde_json::to_string_pretty(&f).unwrap_or_default())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// JWK public key Xbox stores as the proof-of-possession key.
    fn proof_key(&self) -> Value {
        let vk = self.key.verifying_key();
        let pt = vk.to_encoded_point(false);
        json!({
            "crv": "P-256",
            "alg": "ES256",
            "use": "sig",
            "kty": "EC",
            "x": B64URL.encode(pt.x().expect("uncompressed point has x")),
            "y": B64URL.encode(pt.y().expect("uncompressed point has y")),
        })
    }
}

fn device_key_path() -> std::path::PathBuf {
    crate::settings::data_dir().join("device_key.json")
}

// ---- Request signing ------------------------------------------------------

/// Windows FILETIME: 100-nanosecond ticks since 1601-01-01.
fn windows_ticks(t: DateTime<Utc>) -> u64 {
    ((t.timestamp() + 11_644_473_600) * 10_000_000) as u64
}

/// Build the Microsoft `Signature` header value over `(ts, "POST", path, body)`.
fn sign_request(key: &SigningKey, ts: DateTime<Utc>, path: &str, body: &[u8]) -> String {
    let ticks = windows_ticks(ts);

    let mut buf: Vec<u8> = Vec::with_capacity(body.len() + 64);
    buf.extend_from_slice(&1u32.to_be_bytes()); // policy version
    buf.push(0);
    buf.extend_from_slice(&ticks.to_be_bytes());
    buf.push(0);
    buf.extend_from_slice(b"POST");
    buf.push(0);
    buf.extend_from_slice(path.as_bytes());
    buf.push(0);
    // Authorization header value (none for these calls).
    buf.push(0);
    buf.extend_from_slice(body);
    buf.push(0);

    // ECDSA-P256 over SHA-256(buf); `Signer::sign` digests internally.
    let sig: Signature = key.sign(&buf);
    let rs = sig.to_bytes(); // 64 bytes: r || s, fixed-width big-endian

    let mut framed: Vec<u8> = Vec::with_capacity(76);
    framed.extend_from_slice(&1i32.to_be_bytes());
    framed.extend_from_slice(&ticks.to_be_bytes());
    framed.extend_from_slice(&rs);
    B64.encode(framed)
}

/// Track the server clock from response `Date` headers so signatures stay
/// valid even when the local clock has drifted.
#[derive(Default)]
struct Clock(Option<DateTime<Utc>>);

impl Clock {
    fn now(&self) -> DateTime<Utc> {
        self.0.unwrap_or_else(Utc::now)
    }
    fn observe(&mut self, headers: &reqwest::header::HeaderMap) {
        if let Some(d) = headers.get(reqwest::header::DATE).and_then(|v| v.to_str().ok()) {
            if let Ok(parsed) = DateTime::parse_from_rfc2822(d) {
                self.0 = Some(parsed.with_timezone(&Utc));
            }
        }
    }
}

/// POST a signed JSON request. `contract_version` adds the
/// `x-xbl-contract-version: 1` header (every signed call except Sisu Authorize).
async fn signed_post(
    client: &reqwest::Client,
    key: &SigningKey,
    clock: &mut Clock,
    url: &str,
    path: &str,
    body: &Value,
    contract_version: bool,
) -> anyhow::Result<(reqwest::StatusCode, String)> {
    let bytes = serde_json::to_vec(body).context("serializing signed body")?;
    let sig = sign_request(key, clock.now(), path, &bytes);

    let mut req = client
        .post(url)
        .header("Content-Type", "application/json; charset=utf-8")
        .header("Accept", "application/json")
        .header("Signature", sig);
    if contract_version {
        req = req.header("x-xbl-contract-version", "1");
    }

    let resp = req
        .body(bytes)
        .send()
        .await
        .with_context(|| format!("POST {url}"))?;
    clock.observe(resp.headers());
    let status = resp.status();
    let text = resp
        .text()
        .await
        .with_context(|| format!("reading {url} body"))?;
    Ok((status, text))
}

// ---- PKCE -----------------------------------------------------------------

/// 64 random bytes rendered as a 128-char lowercase hex string (verifier and
/// `state` are both generated this way, per Modrinth).
fn random_hex_64() -> String {
    use rand::RngCore;
    let mut b = [0u8; 64];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

fn pkce_challenge(verifier: &str) -> String {
    B64URL.encode(Sha256::digest(verifier.as_bytes()))
}

// ---- Xbox-side typed bits -------------------------------------------------

#[derive(Deserialize)]
struct XblToken {
    #[serde(rename = "Token")]
    token: String,
}

#[derive(Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}
#[derive(Deserialize)]
struct Xui {
    uhs: String,
}

#[derive(Deserialize)]
struct OAuthToken {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

// ---- Flow steps -----------------------------------------------------------

async fn mint_device_token(
    client: &reqwest::Client,
    device: &DeviceKey,
    clock: &mut Clock,
) -> anyhow::Result<String> {
    let body = json!({
        "Properties": {
            "AuthMethod": "ProofOfPossession",
            "Id": device.id,
            "DeviceType": "Win32",
            "Version": "10.16.0",
            "ProofKey": device.proof_key(),
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT",
    });
    let (status, text) = signed_post(
        client,
        &device.key,
        clock,
        "https://device.auth.xboxlive.com/device/authenticate",
        "/device/authenticate",
        &body,
        true,
    )
    .await?;
    if !status.is_success() {
        return Err(anyhow!("Xbox device authentication failed ({status}): {text}"));
    }
    let t: XblToken = serde_json::from_str(&text)
        .with_context(|| format!("parsing device token: {text}"))?;
    Ok(t.token)
}

async fn sisu_authenticate(
    client: &reqwest::Client,
    device: &DeviceKey,
    clock: &mut Clock,
    device_token: &str,
    challenge: &str,
    state: &str,
) -> anyhow::Result<(String, String)> {
    let body = json!({
        "AppId": CLIENT_ID,
        "DeviceToken": device_token,
        "Offers": [SCOPE],
        "Query": {
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "state": state,
            "prompt": "select_account",
        },
        "RedirectUri": REDIRECT_URI,
        "Sandbox": "RETAIL",
        "TokenType": "code",
        "TitleId": TITLE_ID,
    });
    let bytes = serde_json::to_vec(&body).context("serializing Sisu Authenticate")?;
    let sig = sign_request(&device.key, clock.now(), "/authenticate", &bytes);
    let resp = client
        .post("https://sisu.xboxlive.com/authenticate")
        .header("Content-Type", "application/json; charset=utf-8")
        .header("Accept", "application/json")
        .header("Signature", sig)
        .header("x-xbl-contract-version", "1")
        .body(bytes)
        .send()
        .await
        .context("Sisu Authenticate request")?;
    clock.observe(resp.headers());

    let session_id = resp
        .headers()
        .get("X-SessionId")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("Sisu Authenticate response missing X-SessionId"))?;

    let status = resp.status();
    let text = resp.text().await.context("reading Sisu Authenticate body")?;
    if !status.is_success() {
        return Err(anyhow!("Sisu Authenticate failed ({status}): {text}"));
    }

    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "MsaOauthRedirect")]
        msa_oauth_redirect: String,
    }
    let r: Resp = serde_json::from_str(&text)
        .with_context(|| format!("parsing Sisu Authenticate: {text}"))?;
    Ok((session_id, r.msa_oauth_redirect))
}

async fn oauth_exchange(
    client: &reqwest::Client,
    code: &str,
    verifier: &str,
) -> anyhow::Result<OAuthToken> {
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("grant_type", "authorization_code"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
        ])
        .send()
        .await
        .context("oauth20 token exchange")?;
    let status = resp.status();
    let text = resp.text().await.context("reading token body")?;
    if !status.is_success() {
        return Err(anyhow!("Microsoft token exchange failed ({status}): {text}"));
    }
    serde_json::from_str(&text).with_context(|| format!("parsing token: {text}"))
}

async fn oauth_refresh(
    client: &reqwest::Client,
    refresh_token: &str,
) -> anyhow::Result<OAuthToken> {
    let resp = client
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .form(&[
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPE),
        ])
        .send()
        .await
        .context("oauth20 refresh")?;
    let status = resp.status();
    let text = resp.text().await.context("reading refresh body")?;
    if !status.is_success() {
        return Err(anyhow!("Session refresh failed ({status}): {text}"));
    }
    serde_json::from_str(&text).with_context(|| format!("parsing refresh: {text}"))
}

async fn sisu_authorize(
    client: &reqwest::Client,
    device: &DeviceKey,
    clock: &mut Clock,
    access_token: &str,
    device_token: &str,
    session_id: Option<&str>,
) -> anyhow::Result<(String, String)> {
    let body = json!({
        "AccessToken": format!("t={access_token}"),
        "AppId": CLIENT_ID,
        "DeviceToken": device_token,
        "ProofKey": device.proof_key(),
        "Sandbox": "RETAIL",
        "SessionId": session_id,
        "SiteName": "user.auth.xboxlive.com",
        "RelyingParty": "http://xboxlive.com",
        "UseModernGamertag": true,
    });
    // The one signed call that OMITS x-xbl-contract-version.
    let (status, text) = signed_post(
        client,
        &device.key,
        clock,
        "https://sisu.xboxlive.com/authorize",
        "/authorize",
        &body,
        false,
    )
    .await?;
    if !status.is_success() {
        return Err(anyhow!("Sisu Authorize failed ({status}): {text}"));
    }
    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "TitleToken")]
        title_token: XblToken,
        #[serde(rename = "UserToken")]
        user_token: XblToken,
    }
    let r: Resp = serde_json::from_str(&text)
        .with_context(|| format!("parsing Sisu Authorize: {text}"))?;
    Ok((r.title_token.token, r.user_token.token))
}

async fn xsts_authorize(
    client: &reqwest::Client,
    device: &DeviceKey,
    clock: &mut Clock,
    user_token: &str,
    device_token: &str,
    title_token: &str,
) -> anyhow::Result<(String, String)> {
    let body = json!({
        "RelyingParty": "rp://api.minecraftservices.com/",
        "TokenType": "JWT",
        "Properties": {
            "SandboxId": "RETAIL",
            "UserTokens": [user_token],
            "DeviceToken": device_token,
            "TitleToken": title_token,
        },
    });
    let (status, text) = signed_post(
        client,
        &device.key,
        clock,
        "https://xsts.auth.xboxlive.com/xsts/authorize",
        "/xsts/authorize",
        &body,
        true,
    )
    .await?;

    if status == reqwest::StatusCode::UNAUTHORIZED {
        #[derive(Deserialize)]
        struct XstsErr {
            #[serde(rename = "XErr", default)]
            xerr: u64,
        }
        let xerr = serde_json::from_str::<XstsErr>(&text)
            .map(|e| e.xerr)
            .unwrap_or(0);
        return Err(match xerr {
            2148916233 => anyhow!(
                "This Microsoft account has no Xbox profile. Sign in once at minecraft.net or xbox.com to create one, then try again."
            ),
            2148916235 => anyhow!("Xbox Live is not available in this account's country/region."),
            2148916236 | 2148916237 => {
                anyhow!("This account needs adult verification before it can sign in.")
            }
            2148916238 => anyhow!(
                "This account belongs to a minor and must be added to a Microsoft Family group before it can sign in."
            ),
            _ => anyhow!("XSTS authorization failed (XErr {xerr}): {text}"),
        });
    }
    if !status.is_success() {
        return Err(anyhow!("XSTS authorization failed ({status}): {text}"));
    }

    #[derive(Deserialize)]
    struct Resp {
        #[serde(rename = "Token")]
        token: String,
        #[serde(rename = "DisplayClaims")]
        display_claims: DisplayClaims,
    }
    let r: Resp = serde_json::from_str(&text)
        .with_context(|| format!("parsing XSTS: {text}"))?;
    let uhs = r
        .display_claims
        .xui
        .first()
        .map(|x| x.uhs.clone())
        .ok_or_else(|| anyhow!("XSTS response missing user hash"))?;
    Ok((uhs, r.token))
}

async fn minecraft_token(
    client: &reqwest::Client,
    uhs: &str,
    xsts_token: &str,
) -> anyhow::Result<String> {
    let resp = client
        .post("https://api.minecraftservices.com/launcher/login")
        .header("Accept", "application/json")
        .json(&json!({
            "platform": "PC_LAUNCHER",
            "xtoken": format!("XBL3.0 x={uhs};{xsts_token}"),
        }))
        .send()
        .await
        .context("Minecraft launcher login")?;
    let status = resp.status();
    let text = resp.text().await.context("reading launcher login body")?;
    if !status.is_success() {
        return Err(anyhow!(
            "Minecraft sign-in was rejected ({status}). Microsoft said: {text}"
        ));
    }
    #[derive(Deserialize)]
    struct Resp {
        access_token: String,
    }
    let r: Resp = serde_json::from_str(&text)
        .with_context(|| format!("parsing launcher login: {text}"))?;
    Ok(r.access_token)
}

/// Best-effort: called as part of the official flow; failures are non-fatal
/// (ownership is determined by the profile call).
async fn touch_entitlements(client: &reqwest::Client, mc_token: &str) {
    let url = format!(
        "https://api.minecraftservices.com/entitlements/license?requestId={}",
        uuid::Uuid::new_v4()
    );
    let _ = client
        .get(url)
        .header("Accept", "application/json")
        .bearer_auth(mc_token)
        .send()
        .await;
}

async fn fetch_profile(
    client: &reqwest::Client,
    mc_token: &str,
) -> anyhow::Result<(String, String)> {
    let resp = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .header("Accept", "application/json")
        .bearer_auth(mc_token)
        .send()
        .await
        .context("Minecraft profile request")?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Err(anyhow!(
            "This Microsoft account does not own Minecraft: Java Edition (no Java profile)."
        ));
    }
    let status = resp.status();
    let text = resp.text().await.context("reading profile body")?;
    if !status.is_success() {
        return Err(anyhow!("Minecraft profile failed ({status}): {text}"));
    }
    #[derive(Deserialize)]
    struct Profile {
        id: String,
        name: String,
    }
    let p: Profile = serde_json::from_str(&text)
        .with_context(|| format!("parsing profile: {text}"))?;
    Ok((p.id, p.name))
}

/// Run the Xbox half (device token already minted): Sisu Authorize -> XSTS ->
/// `/launcher/login` -> entitlements -> profile.
async fn xbox_chain(
    client: &reqwest::Client,
    device: &DeviceKey,
    clock: &mut Clock,
    device_token: &str,
    ms_access: &str,
    session_id: Option<&str>,
    ms_refresh: String,
) -> anyhow::Result<MinecraftAccount> {
    let (title_token, user_token) =
        sisu_authorize(client, device, clock, ms_access, device_token, session_id).await?;
    let (uhs, xsts) =
        xsts_authorize(client, device, clock, &user_token, device_token, &title_token).await?;
    let mc = minecraft_token(client, &uhs, &xsts).await?;
    touch_entitlements(client, &mc).await;
    let (uuid, username) = fetch_profile(client, &mc).await?;

    Ok(MinecraftAccount {
        username,
        uuid,
        minecraft_token: mc,
        ms_refresh_token: ms_refresh,
        // Minecraft tokens last ~24h; refresh proactively before that.
        expires_at: Utc::now().timestamp() + 86_400,
    })
}

// ---- Public API -----------------------------------------------------------

/// Build the device key + device token, run Sisu Authenticate, and return the
/// Microsoft sign-in URL to open in a webview.
pub async fn begin_login() -> anyhow::Result<LoginFlow> {
    let client = http()?;
    let mut clock = Clock::default();
    let device = DeviceKey::generate();

    let device_token = mint_device_token(&client, &device, &mut clock).await?;
    let verifier = random_hex_64();
    let challenge = pkce_challenge(&verifier);
    let state = random_hex_64();

    let (session_id, auth_url) = sisu_authenticate(
        &client,
        &device,
        &mut clock,
        &device_token,
        &challenge,
        &state,
    )
    .await?;

    Ok(LoginFlow {
        device,
        device_token,
        verifier,
        session_id,
        auth_url,
    })
}

/// Exchange the auth code captured from the webview redirect and finish the
/// Xbox chain. Persists the device key for later non-interactive refresh.
pub async fn finish_login(
    code: &str,
    flow: LoginFlow,
) -> anyhow::Result<MinecraftAccount> {
    let client = http()?;
    let mut clock = Clock::default();

    let token = oauth_exchange(&client, code, &flow.verifier).await?;
    let refresh = token
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("Microsoft did not return a refresh token"))?;

    let acct = xbox_chain(
        &client,
        &flow.device,
        &mut clock,
        &flow.device_token,
        &token.access_token,
        Some(&flow.session_id),
        refresh,
    )
    .await?;

    // Needed for non-interactive refresh; ignore disk errors (refresh will
    // just fall back to asking the user to sign in again).
    let _ = flow.device.save();
    Ok(acct)
}

/// Non-interactive refresh when the stored token is near expiry. Reuses the
/// persisted device key; if it is missing the user must sign in again.
pub async fn ensure_fresh(
    _client_id: &str,
    acct: MinecraftAccount,
) -> anyhow::Result<MinecraftAccount> {
    if Utc::now().timestamp() < acct.expires_at - 300 {
        return Ok(acct);
    }

    let device = DeviceKey::load().ok_or_else(|| {
        anyhow!("Sign-in has expired and the device key is missing. Please sign in again.")
    })?;
    let client = http()?;
    let mut clock = Clock::default();

    let token = oauth_refresh(&client, &acct.ms_refresh_token).await?;
    let refresh = token
        .refresh_token
        .clone()
        .unwrap_or_else(|| acct.ms_refresh_token.clone());
    let device_token = mint_device_token(&client, &device, &mut clock).await?;

    xbox_chain(
        &client,
        &device,
        &mut clock,
        &device_token,
        &token.access_token,
        None,
        refresh,
    )
    .await
}

// ---- Persistence: ~/.anvil/account.json -----------------------------------

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
    let _ = std::fs::remove_file(device_key_path());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc7636_vector() {
        // RFC 7636 Appendix B.
        let v = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(pkce_challenge(v), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn verifier_is_128_hex_chars() {
        let v = random_hex_64();
        assert_eq!(v.len(), 128);
        assert!(v.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(random_hex_64(), v); // overwhelmingly distinct
    }

    #[test]
    fn windows_ticks_known_value() {
        // 1970-01-01T00:00:00Z is 11644473600 seconds after the FILETIME epoch.
        let unix_epoch = DateTime::from_timestamp(0, 0).unwrap();
        assert_eq!(windows_ticks(unix_epoch), 11_644_473_600 * 10_000_000);
    }

    #[test]
    fn signature_is_deterministic_base64_and_key_roundtrips() {
        let d = DeviceKey::generate();
        let t = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let a = sign_request(&d.key, t, "/authenticate", b"{\"hello\":1}");
        let b = sign_request(&d.key, t, "/authenticate", b"{\"hello\":1}");
        assert_eq!(a, b, "RFC6979 ECDSA is deterministic");
        assert!(B64.decode(&a).is_ok());
        assert_eq!(B64.decode(&a).unwrap().len(), 4 + 8 + 64);

        // Private-scalar persistence roundtrip.
        let bytes = B64.encode(d.key.to_bytes());
        let restored = SigningKey::from_slice(&B64.decode(bytes).unwrap()).unwrap();
        assert_eq!(restored.to_bytes(), d.key.to_bytes());
    }

    #[test]
    fn proof_key_is_well_formed_jwk() {
        let d = DeviceKey::generate();
        let jwk = d.proof_key();
        assert_eq!(jwk["kty"], "EC");
        assert_eq!(jwk["crv"], "P-256");
        // 32-byte coords, base64url-no-pad => 43 chars.
        assert_eq!(jwk["x"].as_str().unwrap().len(), 43);
        assert_eq!(jwk["y"].as_str().unwrap().len(), 43);
        assert!(d.id.starts_with('{') && d.id.ends_with('}'));
    }
}
