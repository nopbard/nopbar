use super::credentials::{self, Result, WindowsVault};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
pub const AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
pub const TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
pub const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPE: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
pub fn now() -> u64 {
    super::super::convenience::now()
}
pub fn client() -> Result<reqwest::Client> {
    super::proxy::configure(reqwest::Client::builder())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| crate::i18n::t("chat.error.network_init"))
}
pub fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| crate::i18n::t("chat.error.connection_init"))
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_at: u64,
    pub earliest_refresh_at: u64,
    pub scope: String,
    #[serde(default)]
    pub nonce: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Account {
    pub client_id: String,
    pub subject: String,
    pub name: String,
    pub tokens: Option<Tokens>,
}
#[derive(Default, Serialize, Deserialize)]
pub struct Accounts {
    pub host: String,
    #[serde(default)]
    pub hint_shown: bool,
    pub active: usize,
    pub registrations: Vec<Account>,
    #[serde(default)]
    pub refreshing: HashMap<String, (String, u64)>,
}
pub fn load() -> Result<Accounts> {
    Ok(credentials::load(&WindowsVault, "accounts")?.unwrap_or_default())
}
fn save(a: &Accounts) -> Result<()> {
    credentials::save(&WindowsVault, "accounts", a)?;
    super::session::changed();
    Ok(())
}
pub fn connected() -> bool {
    super::session::snapshot().key.is_some()
}
pub fn active_key() -> Option<String> {
    super::session::snapshot().key
}
pub fn label() -> String {
    super::session::snapshot().label
}
pub fn select(index: usize) -> Result<()> {
    let _lock = credentials::Lock::take()?;
    let mut a = load()?;
    if index >= a.registrations.len() {
        return Err(crate::i18n::t("chat.error.account_missing"));
    }
    a.active = index;
    save(&a)
}
fn first_connection_hint(a: &mut Accounts) -> bool {
    !std::mem::replace(&mut a.hint_shown, true)
}
/// Same persisted first-hint marker for every provider, in NopBar's own vault.
pub fn cli_first_connection() {
    std::thread::spawn(|| {let _=(|| -> Result<()> {
        let _lock=credentials::Lock::take()?;let mut a=load()?;
        if first_connection_hint(&mut a){save(&a)?;super::hint::notify();} Ok(())
    })();});
}
pub struct Pending {
    pub state: String,
    pub nonce: String,
    pub verifier: String,
    pub redirect: String,
    pub previous: Option<Account>,
}
fn random() -> String {
    let mut b = Vec::new();
    b.extend(uuid::Uuid::new_v4().as_bytes());
    b.extend(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(b)
}
pub fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
impl Pending {
    pub fn new(port: u16, previous: Option<Account>) -> Self {
        Self {
            state: random(),
            nonce: random(),
            verifier: random(),
            redirect: format!("http://127.0.0.1:{port}/auth/callback"),
            previous,
        }
    }
    pub fn url(&self, host: &str) -> String {
        let mut u = reqwest::Url::parse(AUTHORIZE).expect("constant URL");
        let mut q = u.query_pairs_mut();
        q.append_pair(
            "client_id",
            self.previous
                .as_ref()
                .map_or("dynamic_agent_client", |a| a.client_id.as_str()),
        );
        if let Some(a) = &self.previous {
            if let Some(t) = &a.tokens {
                q.append_pair("id_token_hint", &t.id_token);
            }
        } else {
            q.append_pair("agent_name_hint", "NopBar");
        }
        for (k, v) in [
            ("ext_agent_host_id", host),
            ("response_type", "code"),
            ("redirect_uri", &self.redirect),
            ("scope", SCOPE),
            ("resource", RESOURCE),
            ("state", &self.state),
            ("nonce", &self.nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge(&self.verifier)),
        ] {
            q.append_pair(k, v);
        }
        drop(q);
        u.into()
    }
    pub fn callback(&self, target: &str) -> Result<(String, String)> {
        if !target.starts_with("/auth/callback?") {
            return Err(crate::i18n::t("chat.error.callback_address"));
        }
        let u = reqwest::Url::parse(&format!("http://127.0.0.1{target}"))
            .map_err(|_| crate::i18n::t("chat.error.callback_address"))?;
        let mut p = HashMap::new();
        for (k, v) in u.query_pairs() {
            if p.insert(k.to_string(), v.to_string()).is_some() {
                return Err(crate::i18n::t("chat.error.callback_duplicate"));
            }
        }
        if p.get("state") != Some(&self.state) {
            return Err(crate::i18n::t("chat.error.callback_invalid"));
        }
        if p.contains_key("error") {
            return Err(crate::i18n::t("chat.error.cancelled"));
        }
        let id = match (&self.previous, p.get("client_id")) {
            (Some(a), Some(id)) if id != &a.client_id => {
                return Err(crate::i18n::t("chat.error.callback_account"))
            }
            (Some(a), _) => a.client_id.clone(),
            (None, Some(id)) if !id.is_empty() && id != "dynamic_agent_client" => id.clone(),
            _ => return Err(crate::i18n::t("chat.error.registration")),
        };
        let code = p
            .get("code")
            .filter(|v| !v.is_empty())
            .ok_or(crate::i18n::t("chat.error.code_missing"))?
            .clone();
        Ok((code, id))
    }
}
#[derive(Clone, Deserialize)]
struct Claims {
    sub: String,
    nonce: Option<String>,
    name: Option<String>,
    email: Option<String>,
}
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: String,
    token_type: String,
    expires_in: u64,
    #[serde(default)]
    earliest_refresh_at: u64,
    scope: String,
}
fn token_set(r: TokenResponse, old: Option<&Tokens>) -> Result<Tokens> {
    if !r.token_type.eq_ignore_ascii_case("bearer")
        || !["chatgpt.tokens.use.direct", "resource.invoke"]
            .iter()
            .all(|scope| r.scope.split_whitespace().any(|s| s == *scope))
        || r.access_token.is_empty()
        || r.refresh_token.is_empty()
    {
        return Err(crate::i18n::t("chat.error.consent"));
    }
    Ok(Tokens {
        access_token: r.access_token,
        refresh_token: r.refresh_token,
        id_token: if r.id_token.is_empty() {
            old.map(|t| t.id_token.clone()).unwrap_or_default()
        } else {
            r.id_token
        },
        expires_at: now().saturating_add(r.expires_in),
        earliest_refresh_at: r.earliest_refresh_at,
        scope: r.scope,
        nonce: old.and_then(|t| t.nonce.clone()),
    })
}
async fn discovery(c: &reqwest::Client) -> Result<serde_json::Value> {
    let v: serde_json::Value = c
        .get("https://auth.openai.com/.well-known/openid-configuration")
        .send()
        .await
        .map_err(|_| crate::i18n::t("chat.error.auth_connection"))?
        .error_for_status()
        .map_err(|_| crate::i18n::t("chat.error.auth_connection"))?
        .json()
        .await
        .map_err(|_| crate::i18n::t("chat.error.auth_info"))?;
    if v["issuer"] != "https://auth.openai.com" {
        return Err(crate::i18n::t("chat.error.auth_server"));
    }
    Ok(v)
}
fn auth_url(v: &serde_json::Value, key: &str) -> Result<reqwest::Url> {
    let u = reqwest::Url::parse(v[key].as_str().unwrap_or(""))
        .map_err(|_| crate::i18n::t("chat.error.auth_address"))?;
    if u.scheme() != "https"
        || u.host_str() != Some("auth.openai.com")
        || u.port_or_known_default() != Some(443)
        || !u.username().is_empty()
        || u.password().is_some()
    {
        return Err(crate::i18n::t("chat.error.auth_address"));
    }
    Ok(u)
}
async fn verify(c: &reqwest::Client, t: &Tokens, p: &Pending, id: &str) -> Result<Claims> {
    let d = discovery(c).await?;
    let keys: jsonwebtoken::jwk::JwkSet = c
        .get(auth_url(&d, "jwks_uri")?)
        .send()
        .await
        .map_err(|_| crate::i18n::t("chat.error.signature_check"))?
        .json()
        .await
        .map_err(|_| crate::i18n::t("chat.error.signature_check"))?;
    verify_id(t, p, id, &keys)
}
fn verify_id(
    t: &Tokens,
    p: &Pending,
    id: &str,
    keys: &jsonwebtoken::jwk::JwkSet,
) -> Result<Claims> {
    verify_id_mode(t, p, id, keys, false)
}
fn verify_id_mode(
    t: &Tokens,
    p: &Pending,
    id: &str,
    keys: &jsonwebtoken::jwk::JwkSet,
    refresh: bool,
) -> Result<Claims> {
    let h = jsonwebtoken::decode_header(&t.id_token).map_err(|_| crate::i18n::t("chat.error.signature_invalid"))?;
    if h.alg != jsonwebtoken::Algorithm::RS256 {
        return Err(crate::i18n::t("chat.error.signature_invalid"));
    }
    let key = keys
        .find(h.kid.as_deref().ok_or(crate::i18n::t("chat.error.signature_missing"))?)
        .ok_or(crate::i18n::t("chat.error.signature_not_found"))?;
    let key = jsonwebtoken::DecodingKey::from_jwk(key).map_err(|_| crate::i18n::t("chat.error.signature_invalid"))?;
    let mut v = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    v.set_audience(&[id]);
    v.set_issuer(&["https://auth.openai.com"]);
    v.validate_nbf = true;
    v.leeway = 30;
    let claims = jsonwebtoken::decode::<Claims>(&t.id_token, &key, &v)
        .map_err(|_| crate::i18n::t("chat.error.sign_in_check"))?
        .claims;
    let bad_nonce = if refresh {
        claims
            .nonce
            .as_ref()
            .is_some_and(|n| t.nonce.as_ref() != Some(n))
    } else {
        claims.nonce.as_deref() != Some(&p.nonce)
    };
    if bad_nonce
        || claims.sub.is_empty()
        || p.previous.as_ref().is_some_and(|a| a.subject != claims.sub)
    {
        return Err(crate::i18n::t("chat.error.account_mismatch"));
    }
    Ok(claims)
}
/// Invoked only by the user's settings button. Never by tests or startup.
pub async fn sign_in(add: bool) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (host, previous) = {
        let _lock = credentials::Lock::take()?;
        let mut a = load()?;
        if a.host.is_empty() {
            a.host = format!("urn:uuid:{}", uuid::Uuid::new_v4());
            save(&a)?;
        }
        (
            a.host,
            if add {
                None
            } else {
                a.registrations.get(a.active).cloned()
            },
        )
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| crate::i18n::t("chat.error.callback_port"))?;
    let p = Pending::new(
        listener
            .local_addr()
            .map_err(|_| crate::i18n::t("chat.error.port_check"))?
            .port(),
        previous,
    );
    // Do not use launch::open: it logs its target, including id_token_hint.
    let url = p.url(&host);
    if !matches!(tokio::task::spawn_blocking(move ||
        crate::launch::environment::launch("open", &url, "", "")
    ).await, Ok(Ok(()))) {
        return Err(crate::i18n::t("chat.error.browser"));
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(180);
    let (code, id) = loop {
        let (mut socket, peer) = tokio::time::timeout_at(deadline, listener.accept())
            .await
            .map_err(|_| crate::i18n::t("chat.error.login_timeout"))?
            .map_err(|_| crate::i18n::t("chat.error.callback_missing"))?;
        if !peer.ip().is_loopback() {
            continue;
        }
        let mut bytes = Vec::new();
        let mut one = [0; 1];
        while bytes.len() < 8192 && !bytes.ends_with(b"\r\n\r\n") {
            match tokio::time::timeout(std::time::Duration::from_secs(2), socket.read(&mut one))
                .await
            {
                Ok(Ok(1)) => bytes.push(one[0]),
                _ => break,
            }
        }
        let line = std::str::from_utf8(&bytes).unwrap_or("");
        let mut words = line.lines().next().unwrap_or("").split_whitespace();
        let method = words.next();
        let target = words.next().unwrap_or("");
        let result = if method == Some("GET") {
            p.callback(target)
        } else {
            Err(crate::i18n::t("chat.error.request_invalid"))
        };
        let body = if result.is_ok() {
            "NopBar: You may close this window."
        } else {
            "NopBar: Invalid callback."
        };
        let status = if result.is_ok() {
            "200 OK"
        } else {
            "400 Bad Request"
        };
        let reply=format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",body.len());
        let _ = socket.write_all(reply.as_bytes()).await;
        // Only a validated callback may finish or cancel this attempt.
        match result {
            Ok(pair) => break pair,
            Err(e) if e == crate::i18n::t("chat.error.cancelled") => return Err(crate::i18n::t("chat.error.cancelled")),
            _ => {}
        }
    };
    let c = client()?;
    let response = c
        .post(TOKEN)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &id),
            ("code", &code),
            ("code_verifier", &p.verifier),
            ("redirect_uri", &p.redirect),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(|_| crate::i18n::t("chat.error.sign_in_server"))?;
    if !response.status().is_success() {
        return Err(crate::i18n::t("chat.error.sign_in_failed"));
    }
    let mut tokens = token_set(
        response
            .json()
            .await
            .map_err(|_| crate::i18n::t("chat.error.callback_read"))?,
        None,
    )?;
    let claims = verify(&c, &tokens, &p, &id).await?;
    tokens.nonce = Some(p.nonce.clone());
    let _lock = credentials::Lock::take()?;
    let mut a = load()?;
    let index = a
        .registrations
        .iter()
        .position(|r| r.client_id == id && r.subject == claims.sub)
        .unwrap_or(a.registrations.len());
    let record = Account {
        client_id: id,
        subject: claims.sub.clone(),
        name: claims.name.or(claims.email).unwrap_or(claims.sub),
        tokens: Some(tokens),
    };
    if index == a.registrations.len() {
        a.registrations.push(record);
    } else {
        a.registrations[index] = record;
    }
    a.active = index;
    let hint = first_connection_hint(&mut a);
    save(&a)?;
    if hint {
        super::hint::notify();
    }
    Ok(())
}
pub fn needs_refresh(t: &Tokens, at: u64) -> bool {
    at >= t.expires_at.saturating_sub(60) && at >= t.earliest_refresh_at
}
pub async fn access() -> Result<String> {
    super::protected::run(|| runtime()?.block_on(access_inner())).await
}
fn record_key(r: &Account) -> String {
    format!("{}:{}", r.client_id, r.subject)
}
fn still_current(a: &Accounts, key: &str, refresh: &str, lease: &str) -> Option<usize> {
    if a.refreshing.get(key).is_none_or(|(id, _)| id != lease) {
        return None;
    }
    a.registrations.iter().position(|r| {
        record_key(r) == key
            && r.tokens
                .as_ref()
                .is_some_and(|t| t.refresh_token == refresh)
    })
}
async fn access_inner() -> Result<String> {
    let (record, lease) = loop {
        let snapshot = {
            let _lock = credentials::Lock::take()?;
            let mut a = load()?;
            let r = a
                .registrations
                .get(a.active)
                .filter(|r| r.tokens.is_some())
                .cloned()
                .ok_or(crate::i18n::t("chat.error.disconnected"))?;
            let old = r.tokens.as_ref().unwrap();
            if !needs_refresh(old, now()) {
                return if now() < old.expires_at {
                    Ok(old.access_token.clone())
                } else {
                    Err(crate::i18n::t("chat.error.chatgpt_expired"))
                };
            }
            let key = record_key(&r);
            if a.refreshing
                .get(&key)
                .is_some_and(|(_, until)| *until > now())
            {
                None
            } else {
                let lease = uuid::Uuid::new_v4().to_string();
                a.refreshing.insert(key, (lease.clone(), now() + 300));
                save(&a)?;
                Some((r, lease))
            }
        };
        if let Some(snapshot) = snapshot {
            break snapshot;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    };
    let old = record.tokens.as_ref().unwrap();
    let renewed = async {
        let c = client()?;
        // Fetch validation material before rotating the refresh token. A failed
        // discovery/JWKS request must not strand an already rotated credential.
        let d = discovery(&c).await?;
        let keys = c
            .get(auth_url(&d, "jwks_uri")?)
            .timeout(std::time::Duration::from_secs(60))
            .send()
            .await
            .map_err(|_| crate::i18n::t("chat.error.refresh_account"))?
            .json()
            .await
            .map_err(|_| crate::i18n::t("chat.error.account_key"))?;
        let r = c
            .post(TOKEN)
            .timeout(std::time::Duration::from_secs(60))
            .form(&[
                ("grant_type", "refresh_token"),
                ("client_id", record.client_id.as_str()),
                ("refresh_token", old.refresh_token.as_str()),
                ("resource", RESOURCE),
            ])
            .send()
            .await
            .map_err(|_| crate::i18n::t("chat.error.refresh_failed"))?;
        if !r.status().is_success() {
            return Err(crate::i18n::t("chat.error.chatgpt_expired"));
        }
        let response: TokenResponse = r
            .json()
            .await
            .map_err(|_| crate::i18n::t("chat.error.refresh_read"))?;
        let new_id = !response.id_token.is_empty();
        let tokens = token_set(response, Some(old))?;
        if new_id {
            let p = Pending::new(0, Some(record.clone()));
            verify_id_mode(&tokens, &p, &record.client_id, &keys, true)?;
        }
        Ok(tokens)
    }
    .await;
    let _lock = credentials::Lock::take()?;
    let mut a = load()?;
    let key = record_key(&record);
    let index = still_current(&a, &key, &old.refresh_token, &lease);
    if a.refreshing.get(&key).is_some_and(|(id, _)| id == &lease) {
        a.refreshing.remove(&key);
    }
    let Some(index) = index else {
        save(&a)?;
        return Err(crate::i18n::t("chat.error.connection_changed"));
    };
    match renewed {
        Ok(tokens) => {
            let access = tokens.access_token.clone();
            a.registrations[index].tokens = Some(tokens);
            save(&a)?;
            if a.active != index {
                return Err(crate::i18n::t("chat.error.account_changed"));
            }
            Ok(access)
        }
        Err(e) => {
            save(&a)?;
            Err(e)
        }
    }
}
pub async fn disconnect() -> Result<()> {
    let (client_id, t) = {
        let _lock = credentials::Lock::take()?;
        let mut a = load()?;
        let Some(r) = a.registrations.get_mut(a.active) else {
            return Ok(());
        };
        let Some(t) = r.tokens.take() else {
            return Ok(());
        };
        let client_id = r.client_id.clone();
        let key = record_key(r);
        a.refreshing.remove(&key);
        save(&a)?;
        (client_id, t)
    };
    let c = super::proxy::configure(reqwest::Client::builder())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|_| crate::i18n::t("chat.error.remote_disconnect"))?;
    let mut revoked = false;
    if let Ok(d) = discovery(&c).await {
        if let Ok(url) = auth_url(&d, "revocation_endpoint") {
            for wait in [0, 250, 750] {
                tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                if let Ok(reply) = c
                    .post(url.clone())
                    .form(&[
                        ("token", t.refresh_token.as_str()),
                        ("token_type_hint", "refresh_token"),
                        ("client_id", client_id.as_str()),
                    ])
                    .send()
                    .await
                {
                    if reply.status().is_success() {
                        revoked = true;
                        break;
                    }
                    if reply.status().is_client_error() {
                        break;
                    }
                }
            }
        }
    }
    if revoked {
        Ok(())
    } else {
        Err(crate::i18n::t("chat.error.remote_disconnect"))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_success_hint_is_persisted_once_across_reconnects() {
        let mut accounts = Accounts::default();
        assert!(!accounts.hint_shown);
        assert!(first_connection_hint(&mut accounts));
        let mut restored: Accounts =
            serde_json::from_str(&serde_json::to_string(&accounts).unwrap()).unwrap();
        assert!(!first_connection_hint(&mut restored));
        let old: Accounts =
            serde_json::from_str(r#"{"host":"fixture","active":0,"registrations":[]}"#).unwrap();
        assert!(!old.hint_shown);
    }
    #[test]
    fn signed_id_requires_signature_audience_issuer_expiry_nonce_and_selected_identity() {
        use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
        let keys = serde_json::from_str(include_str!("fixtures/test-only-jwks.json")).unwrap();
        let encoding =
            EncodingKey::from_rsa_pem(include_bytes!("fixtures/test-only-rsa.pem")).unwrap();
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("fixture".into());
        let mut p = Pending::new(1234, None);
        let claims = serde_json::json!({"sub":"fixture-user","iss":"https://auth.openai.com","aud":"test-client","exp":now()+300,"nonce":p.nonce,"name":"Test user"});
        let make = |v: &serde_json::Value| Tokens {
            access_token: "fake-access".into(),
            refresh_token: "fake-refresh".into(),
            id_token: encode(&header, v, &encoding).unwrap(),
            expires_at: now() + 300,
            earliest_refresh_at: 0,
            scope: SCOPE.into(),
            nonce: None,
        };
        assert_eq!(
            verify_id(&make(&claims), &p, "test-client", &keys)
                .unwrap()
                .sub,
            "fixture-user"
        );
        for (key, value) in [
            ("aud", serde_json::json!("other")),
            ("iss", serde_json::json!("https://other.invalid")),
            ("exp", serde_json::json!(now() - 120)),
            ("nonce", serde_json::json!("wrong")),
        ] {
            let mut bad = claims.clone();
            bad[key] = value;
            assert!(
                verify_id(&make(&bad), &p, "test-client", &keys).is_err(),
                "{key}"
            );
        }
        let mut corrupt = make(&claims);
        corrupt.id_token.push('x');
        assert!(verify_id(&corrupt, &p, "test-client", &keys).is_err());
        p.previous = Some(Account {
            client_id: "test-client".into(),
            subject: "other-user".into(),
            name: "Other".into(),
            tokens: None,
        });
        assert!(verify_id(&make(&claims), &p, "test-client", &keys).is_err());
        // Refresh ID tokens receive the same signature/issuer/audience/expiry
        // checks, bind the selected subject, and check nonce when returned.
        p.previous.as_mut().unwrap().subject = "fixture-user".into();
        let mut refresh_claims = claims.clone();
        refresh_claims.as_object_mut().unwrap().remove("nonce");
        assert!(verify_id_mode(&make(&refresh_claims), &p, "test-client", &keys, true).is_ok());
        for (field, value) in [
            ("sub", serde_json::json!("another")),
            ("aud", serde_json::json!("another")),
            ("iss", serde_json::json!("https://invalid")),
            ("exp", serde_json::json!(now() - 120)),
            ("nonce", serde_json::json!("wrong")),
        ] {
            let mut bad = refresh_claims.clone();
            bad[field] = value;
            assert!(verify_id_mode(&make(&bad), &p, "test-client", &keys, true).is_err());
        }
        assert!(verify_id_mode(&corrupt, &p, "test-client", &keys, true).is_err());
    }
    #[test]
    fn credential_rotation_rejects_missing_permission_and_replaces_refresh_token() {
        let response = |scope: &str| TokenResponse {
            access_token: "new-access".into(),
            refresh_token: "new-refresh".into(),
            id_token: String::new(),
            token_type: "Bearer".into(),
            expires_in: 3600,
            earliest_refresh_at: 0,
            scope: scope.into(),
        };
        assert!(token_set(response("openid profile"), None).is_err());
        let old = Tokens {
            access_token: "old-access".into(),
            refresh_token: "old-refresh".into(),
            id_token: "retained-id".into(),
            expires_at: 0,
            earliest_refresh_at: 0,
            scope: SCOPE.into(),
            nonce: None,
        };
        let t = token_set(response(SCOPE), Some(&old)).unwrap();
        assert_eq!(t.refresh_token, "new-refresh");
        assert_eq!(t.id_token, "retained-id");
        assert!(t.expires_at > now());
        let record = Account {
            client_id: "test".into(),
            subject: "user".into(),
            name: "Fixture".into(),
            tokens: Some(old),
        };
        let key = record_key(&record);
        let mut a = Accounts {
            registrations: vec![record],
            ..Default::default()
        };
        a.refreshing
            .insert(key.clone(), ("lease".into(), now() + 300));
        assert_eq!(still_current(&a, &key, "old-refresh", "lease"), Some(0));
        assert_eq!(
            still_current(&a, &key, "old-refresh", "different-lease"),
            None
        );
        a.registrations[0].tokens = None;
        assert_eq!(
            still_current(&a, &key, "old-refresh", "lease"),
            None,
            "late refresh cannot resurrect a signed-out account"
        );
    }
    #[test]
    fn pkce_rfc_vector_randomness_and_callback_validation() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let p = Pending::new(1234, None);
        let q = Pending::new(1234, None);
        assert_ne!(p.state, q.state);
        assert_ne!(p.nonce, p.state);
        assert!(p.verifier.len() >= 43);
        let good = format!(
            "/auth/callback?state={}&code=x&client_id=oaiapp_test",
            p.state
        );
        assert!(p.callback(&good).is_ok());
        assert!(p.callback(&good.replace(&p.state, "bad")).is_err());
        assert!(p.callback(&format!("{good}&state={}", p.state)).is_err());
        assert!(p
            .callback(&good.replace("oaiapp_test", "dynamic_agent_client"))
            .is_err());
        assert!(p
            .callback(&good.replace("/auth/callback", "/callback"))
            .is_err());
    }
    #[test]
    fn returning_client_cannot_change() {
        let a = Account {
            client_id: "a".into(),
            subject: "s".into(),
            name: "n".into(),
            tokens: None,
        };
        let p = Pending::new(1234, Some(a));
        assert!(p
            .callback(&format!(
                "/auth/callback?state={}&code=x&client_id=b",
                p.state
            ))
            .is_err());
        assert_eq!(
            p.callback(&format!("/auth/callback?state={}&code=x", p.state))
                .unwrap()
                .1,
            "a"
        );
        assert!(!p.url("urn:uuid:test").contains("agent_name_hint"));
    }
    #[test]
    fn renewal_boundary() {
        let t = Tokens {
            access_token: "x".into(),
            refresh_token: "y".into(),
            id_token: "z".into(),
            expires_at: 3600,
            earliest_refresh_at: 3550,
            scope: SCOPE.into(),
            nonce: None,
        };
        assert!(!needs_refresh(&t, 3549));
        assert!(needs_refresh(&t, 3550));
    }
}
