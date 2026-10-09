//! A small Microsoft Graph client that signs in as the app registration itself.
//!
//! The OAuth 2.0 client-credentials grant: the tenant ID, client ID and client
//! secret are exchanged directly for an app-only token, with no browser, no
//! device code and no signed-in user. What the app can do is therefore exactly
//! the *application* permissions granted to the registration (with admin
//! consent) — [`REQUIRED_ROLES`] lists them, and the Connection tab shows
//! which of them the token actually carries.
//!
//! The client is cheap to clone and safe to share between threads; every pane
//! hands a clone to a [`crate::task::Task`]. The token is cached and fetched
//! again a few minutes before it expires.

pub mod apps;
pub mod consent;
pub mod devices;
pub mod groups;
pub mod licensing;
pub mod logs;
pub mod mailbox;
pub mod models;
pub mod users;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;
use ureq::Agent;

pub const GRAPH: &str = "https://graph.microsoft.com/v1.0";
/// The beta endpoint, for the few things v1.0 does not have yet, such as
/// when each app last signed in.
pub const GRAPH_BETA: &str = "https://graph.microsoft.com/beta";
pub(crate) const LOGIN: &str = "https://login.microsoftonline.com";
const TIMEOUT: Duration = Duration::from_secs(60);
/// Fetch a new token this long before the old one runs out, so a request is
/// never sent with one that expires on the way.
const TOKEN_MARGIN: Duration = Duration::from_secs(300);
/// How many times a throttled request is sent before giving up.
const ATTEMPTS: u32 = 5;
/// The longest a single `Retry-After` is waited for, whatever Graph asks.
const LONGEST_WAIT: Duration = Duration::from_secs(60);

/// The application permissions this app uses, and what each one is for.
pub const REQUIRED_ROLES: &[(&str, &str)] = &[
    ("User.ReadWrite.All", "List, create, update and delete users"),
    ("User-PasswordProfile.ReadWrite.All", "Reset user passwords"),
    ("Group.ReadWrite.All", "List, create and delete groups"),
    ("GroupMember.ReadWrite.All", "Add and remove group members"),
    ("Device.ReadWrite.All", "List, enable, disable and delete Entra devices"),
    (
        "DeviceManagementManagedDevices.ReadWrite.All",
        "List Intune managed devices",
    ),
    (
        "DeviceManagementManagedDevices.PrivilegedOperations.All",
        "Intune actions: sync, restart, lock, scan, retire, wipe",
    ),
    (
        "LicenseAssignment.ReadWrite.All",
        "List subscriptions, and assign and remove licences",
    ),
    ("AuditLog.Read.All", "Read the sign-in and audit logs"),
    (
        "MailboxSettings.ReadWrite",
        "Automatic replies, mailbox time zone and language",
    ),
    ("Reports.Read.All", "Mailbox sizes and last activity"),
    ("Organization.Read.All", "Show the tenant's name"),
    ("Application.Read.All", "List connected apps and their permissions"),
    (
        "Directory.Read.All",
        "Show the delegated permissions users and admins consented to",
    ),
];

#[derive(Clone)]
pub struct Credentials {
    pub tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("tenant_id", &self.tenant_id)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<hidden>")
            .finish()
    }
}

struct Token {
    bearer: String,
    expires: Instant,
}

struct Inner {
    agent: Agent,
    credentials: Credentials,
    token: Mutex<Option<Token>>,
    /// A token obtained some other way, used as it is: the admin's own,
    /// from the one-off sign-in in [`consent`]. Never refreshed.
    fixed_bearer: Option<String>,
}

#[derive(Clone)]
pub struct Graph {
    inner: Arc<Inner>,
}

/// What a successful sign-in found out.
#[derive(Clone, Debug)]
pub struct Session {
    /// The tenant's display name, when the app may read it.
    pub organisation: Option<String>,
    /// The application permissions in the token's `roles` claim.
    pub roles: Vec<String>,
    /// The tenant's GUID, from the token's `tid` claim. The tenant ID that
    /// was typed in may be a domain name instead, and one tenant can have
    /// several of those.
    pub tenant_guid: Option<String>,
}

/// Why a request failed: the sentence for the status bar, and Graph's
/// status code when it got as far as answering.
struct Failure {
    status: Option<u16>,
    message: String,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            status: None,
            message,
        }
    }
}

pub type Result<T> = std::result::Result<T, String>;

impl Graph {
    pub fn new(credentials: Credentials) -> Self {
        Self::build(credentials, None)
    }

    /// A client that sends a token it was given, rather than signing in.
    fn with_bearer(tenant_id: &str, bearer: String) -> Self {
        Self::build(
            Credentials {
                tenant_id: tenant_id.to_owned(),
                client_id: String::new(),
                client_secret: String::new(),
            },
            Some(bearer),
        )
    }

    fn build(credentials: Credentials, fixed_bearer: Option<String>) -> Self {
        Self {
            inner: Arc::new(Inner {
                agent: agent(),
                credentials,
                token: Mutex::new(None),
                fixed_bearer,
            }),
        }
    }

    pub fn tenant_id(&self) -> &str {
        &self.inner.credentials.tenant_id
    }

    /// What this client signs in with, which is not necessarily what is in
    /// the Connection pane's boxes by the time the sign-in finishes.
    pub fn credentials(&self) -> &Credentials {
        &self.inner.credentials
    }

    /// Sign in, and find out what the sign-in is good for.
    pub fn sign_in(&self) -> Result<Session> {
        let bearer = self.bearer()?;
        let claims = claims_in(&bearer);
        let roles = roles_in(&claims);
        let tenant_guid = claims["tid"].as_str().map(str::to_lowercase);
        log::info!(
            "signed in to tenant {}; the token carries {} roles: {}",
            tenant_guid.as_deref().unwrap_or("(no tid claim)"),
            roles.len(),
            roles.join(", ")
        );
        // Optional: without Organization.Read.All this is a 403, and that is
        // no reason to refuse to connect.
        let organisation = self
            .get("/organization?$select=displayName")
            .ok()
            .and_then(|v| {
                v["value"][0]["displayName"]
                    .as_str()
                    .map(str::to_owned)
            });
        Ok(Session {
            organisation,
            roles,
            tenant_guid,
        })
    }

    /// A valid access token, from the cache or freshly fetched.
    fn bearer(&self) -> Result<String> {
        if let Some(bearer) = &self.inner.fixed_bearer {
            return Ok(bearer.clone());
        }
        let mut slot = self.inner.token.lock().map_err(|_| "token cache poisoned")?;
        if let Some(token) = slot.as_ref()
            && token.expires > Instant::now() + TOKEN_MARGIN
        {
            return Ok(token.bearer.clone());
        }

        let c = &self.inner.credentials;
        if c.tenant_id.trim().is_empty() || c.client_id.trim().is_empty() {
            return Err("Enter a tenant ID and a client ID first.".to_owned());
        }
        if c.client_secret.is_empty() {
            return Err("Enter the client secret first.".to_owned());
        }

        let tenant = checked_tenant(&c.tenant_id)?;
        let url = format!("{LOGIN}/{tenant}/oauth2/v2.0/token");
        log::debug!(
            "requesting a token for tenant {tenant}, client {}",
            c.client_id.trim()
        );
        let started = Instant::now();
        let mut response = self
            .inner
            .agent
            .post(&url)
            .send_form([
                ("grant_type", "client_credentials"),
                ("client_id", c.client_id.trim()),
                ("client_secret", c.client_secret.as_str()),
                ("scope", "https://graph.microsoft.com/.default"),
            ])
            .map_err(|e| {
                log::warn!("token request failed to send: {e}");
                format!("Could not reach Microsoft sign-in: {e}")
            })?;
        let status = response.status();
        let request_id = request_id(&response);
        log::debug!(
            "token request answered {} in {} ms (request-id {request_id})",
            status.as_u16(),
            started.elapsed().as_millis()
        );
        let body: Value = response
            .body_mut()
            .read_json()
            .map_err(|e| format!("Unreadable answer from Microsoft sign-in: {e}"))?;

        if !status.is_success() {
            let description = body["error_description"]
                .as_str()
                .or_else(|| body["error"].as_str())
                .unwrap_or("unknown error");
            // The whole description goes in the log, trace and correlation
            // IDs included, since those are what Microsoft support asks for.
            log::warn!(
                "sign-in refused ({}, request-id {request_id}): {description}",
                status.as_u16()
            );
            return Err(format!("Sign-in refused: {}", aadsts_summary(description)));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: String,
            expires_in: u64,
        }
        let parsed: TokenResponse = serde_json::from_value(body)
            .map_err(|e| format!("Unexpected answer from Microsoft sign-in: {e}"))?;
        let bearer = parsed.access_token;
        log::debug!("token received, valid for {} s", parsed.expires_in);
        *slot = Some(Token {
            bearer: bearer.clone(),
            expires: Instant::now() + Duration::from_secs(parsed.expires_in),
        });
        Ok(bearer)
    }

    /// A path under [`GRAPH`], or a full URL such as an `@odata.nextLink`.
    /// A full URL has to be Graph's own, v1.0 or [`GRAPH_BETA`]: the bearer
    /// token goes with every request, and it must never be handed to
    /// another host.
    fn url(path: &str) -> Result<String> {
        let under = |base: &str| {
            path.starts_with(base) && matches!(path.as_bytes().get(base.len()), Some(b'/' | b'?'))
        };
        if path.starts_with('/') {
            Ok(format!("{GRAPH}{path}"))
        } else if under(GRAPH) || under(GRAPH_BETA) {
            Ok(path.to_owned())
        } else {
            Err(format!("Refusing to send the access token outside Microsoft Graph: {path}"))
        }
    }

    /// Send a request and read the answer, turning a Graph error into its
    /// message. `None` for the many calls that answer 202 or 204 with nothing.
    fn send(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Option<Value>> {
        self.request(method, path, body).map_err(|f| f.message)
    }

    /// [`Self::send`], keeping the status code of a failure.
    ///
    /// A throttled request (429) is sent again after the wait Graph asks for
    /// in `Retry-After`, or a growing one when it does not say. So is a GET,
    /// PATCH or DELETE that met a busy or timed-out service (503, 504):
    /// sending one of those twice does no harm. A POST that met one is not,
    /// because it may have been carried out regardless, and creating a user
    /// twice is worse than reporting the error.
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<Option<Value>, Failure> {
        let text = self.request_text(method, path, body)?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| format!("Unexpected answer from Microsoft Graph: {e}").into())
    }

    /// [`Self::request`], with the answer as it came rather than as JSON:
    /// for the usage reports, which are CSV.
    fn request_text(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<String, Failure> {
        let url = Self::url(path)?;
        let mut attempt = 1;
        loop {
            match self.attempt(method, &url, path, body)? {
                Attempt::Done(value) => return Ok(value),
                Attempt::Failed {
                    status,
                    message,
                    retry_after,
                } => {
                    let Some(wait) = retry_wait(method, status, attempt, retry_after) else {
                        return Err(Failure {
                            status: Some(status),
                            message,
                        });
                    };
                    log::info!(
                        "{method} {} answered {status}; trying again in {} s (attempt {} of {ATTEMPTS})",
                        shown(path),
                        wait.as_secs(),
                        attempt + 1
                    );
                    std::thread::sleep(wait);
                    attempt += 1;
                }
            }
        }
    }

    /// One try at a request.
    fn attempt(
        &self,
        method: &str,
        url: &str,
        path: &str,
        body: Option<&Value>,
    ) -> std::result::Result<Attempt, Failure> {
        let auth = format!("Bearer {}", self.bearer()?);
        let agent = &self.inner.agent;

        let started = Instant::now();
        let sent = match (method, body) {
            ("GET", _) => agent.get(url).header("Authorization", &auth).call(),
            ("DELETE", _) => agent.delete(url).header("Authorization", &auth).call(),
            ("POST", Some(body)) => agent
                .post(url)
                .header("Authorization", &auth)
                .send_json(body),
            ("POST", None) => agent
                .post(url)
                .header("Authorization", &auth)
                .send_empty(),
            ("PATCH", Some(body)) => agent
                .patch(url)
                .header("Authorization", &auth)
                .send_json(body),
            _ => return Err(format!("unsupported request {method} {path}").into()),
        };
        let mut response = sent.map_err(|e| {
            log::warn!("{method} {} failed to send: {e}", shown(path));
            format!("Could not reach Microsoft Graph: {e}")
        })?;
        let status = response.status();
        let request_id = request_id(&response);
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse().ok())
            .map(Duration::from_secs);
        let text = response
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| format!("Unreadable answer from Microsoft Graph: {e}"))?;

        let millis = started.elapsed().as_millis();
        if !status.is_success() {
            let message = graph_error(status.as_u16(), &text);
            log::warn!(
                "{method} {} answered {} in {millis} ms (request-id {request_id}): {message}",
                shown(path),
                status.as_u16()
            );
            return Ok(Attempt::Failed {
                status: status.as_u16(),
                message,
                retry_after,
            });
        }
        log::debug!(
            "{method} {} answered {} in {millis} ms, {} bytes (request-id {request_id})",
            shown(path),
            status.as_u16(),
            text.len()
        );
        Ok(Attempt::Done(text))
    }

    pub fn get(&self, path: &str) -> Result<Value> {
        self.send("GET", path, None)?
            .ok_or_else(|| "Microsoft Graph sent an empty answer.".to_owned())
    }

    /// A single object, or `None` when Graph says it is not there (404).
    pub fn get_if_found(&self, path: &str) -> Result<Option<Value>> {
        match self.request("GET", path, None) {
            Ok(Some(value)) => Ok(Some(value)),
            Ok(None) => Err("Microsoft Graph sent an empty answer.".to_owned()),
            Err(Failure {
                status: Some(404), ..
            }) => Ok(None),
            Err(failure) => Err(failure.message),
        }
    }

    /// A GET whose answer is not JSON. Graph answers a report request by
    /// redirecting to a download on another host; the agent follows it, and
    /// does not send the token there (ureq forwards `Authorization` on a
    /// redirect only when told to).
    pub fn get_text(&self, path: &str) -> Result<String> {
        self.request_text("GET", path, None).map_err(|f| f.message)
    }

    /// Every page of a collection, following `@odata.nextLink` to the end.
    pub fn get_all<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Vec<T>> {
        self.collect(path, None)
            .map(|(items, _)| items)
            .map_err(|f| f.message)
    }

    /// The first `max` items of a collection, and whether there were more.
    /// Only as many pages are read as it takes to reach `max`.
    pub fn get_up_to<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        max: usize,
    ) -> Result<(Vec<T>, bool)> {
        self.collect(path, Some(max)).map_err(|f| f.message)
    }

    /// [`Self::get_all`], or `None` when what it belongs to has gone (404):
    /// deleted, say, between being listed and being read.
    pub fn get_all_if_found<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Option<Vec<T>>> {
        match self.collect(path, None) {
            Ok((items, _)) => Ok(Some(items)),
            Err(Failure {
                status: Some(404), ..
            }) => Ok(None),
            Err(failure) => Err(failure.message),
        }
    }

    /// Pages of a collection, up to `max` items if there is a limit, and
    /// whether there were more than that.
    fn collect<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        max: Option<usize>,
    ) -> std::result::Result<(Vec<T>, bool), Failure> {
        let mut items = Vec::new();
        let mut more = false;
        let mut next = Some(path.to_owned());
        while let Some(page_url) = next.take() {
            let mut page = self
                .request("GET", &page_url, None)?
                .ok_or_else(|| "Microsoft Graph sent an empty answer.".to_owned())?;
            if let Value::Array(values) = page["value"].take() {
                for value in values {
                    items.push(
                        serde_json::from_value(value)
                            .map_err(|e| format!("Unexpected item from Microsoft Graph: {e}"))?,
                    );
                }
            }
            next = page["@odata.nextLink"].as_str().map(str::to_owned);
            if let Some(max) = max
                && items.len() >= max
            {
                more = items.len() > max || next.is_some();
                items.truncate(max);
                break;
            }
        }
        log::debug!(
            "{} items from {}{}",
            items.len(),
            shown(path),
            if more { ", stopped at the limit" } else { "" }
        );
        Ok((items, more))
    }

    pub fn post(&self, path: &str, body: &Value) -> Result<Option<Value>> {
        self.send("POST", path, Some(body))
    }

    pub fn post_empty(&self, path: &str) -> Result<()> {
        self.send("POST", path, None).map(drop)
    }

    pub fn patch(&self, path: &str, body: &Value) -> Result<()> {
        self.send("PATCH", path, Some(body)).map(drop)
    }

    pub fn delete(&self, path: &str) -> Result<()> {
        self.send("DELETE", path, None).map(drop)
    }
}

/// The HTTP client every request goes through. Graph's error bodies carry the
/// only useful explanation of what went wrong, so a 4xx is read like any
/// other response rather than turned into a bare status code.
pub(crate) fn agent() -> Agent {
    Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(TIMEOUT))
        .user_agent(concat!("mainstone/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The tenant as typed, trimmed, once it is known to be safe to put in a
/// sign-in URL: a `/`, `?` or `#` typed into the box would otherwise point
/// the request at a different endpoint.
pub(crate) fn checked_tenant(tenant: &str) -> Result<&str> {
    let tenant = tenant.trim();
    if tenant.is_empty()
        || !tenant
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.'))
    {
        return Err("The tenant ID is a GUID or a domain such as contoso.onmicrosoft.com.".to_owned());
    }
    Ok(tenant)
}

/// How one try at a request went, short of failing to reach Graph at all.
enum Attempt {
    /// The body, which may be empty.
    Done(String),
    Failed {
        status: u16,
        message: String,
        retry_after: Option<Duration>,
    },
}

/// How long to wait before sending a failed request again, or `None` to give
/// up; see [`Graph::request`] for which failures are worth another try.
fn retry_wait(method: &str, status: u16, attempt: u32, retry_after: Option<Duration>) -> Option<Duration> {
    let again = status == 429 || (matches!(status, 503 | 504) && method != "POST");
    (again && attempt < ATTEMPTS).then(|| {
        retry_after
            .unwrap_or_else(|| Duration::from_secs(2u64.pow(attempt)))
            .min(LONGEST_WAIT)
    })
}

/// An AADSTS description without what follows the sentence that matters.
/// Microsoft appends the trace and correlation IDs and a timestamp, on new
/// lines or, lately, on the same one; they are in the log, and in the status
/// bar they only push the explanation out of sight.
pub(crate) fn aadsts_summary(description: &str) -> &str {
    let first = description.lines().next().unwrap_or(description);
    first
        .split(" Trace ID:")
        .next()
        .unwrap_or(first)
        .trim()
}

/// The ID Microsoft gives each request, which its support asks for. Graph
/// calls the header `request-id`; the sign-in endpoint, `x-ms-request-id`.
pub(crate) fn request_id(response: &ureq::http::Response<ureq::Body>) -> String {
    let headers = response.headers();
    ["request-id", "x-ms-request-id"]
        .iter()
        .find_map(|name| headers.get(*name)?.to_str().ok())
        .unwrap_or("none")
        .to_owned()
}

/// A request's path for the log, without Graph's address in front of a
/// `@odata.nextLink`, and with the skip token that follows it cut short:
/// it is long, opaque, and says nothing a reader can use. A beta request
/// keeps `/beta` in front, so the two can be told apart.
fn shown(path: &str) -> String {
    let path = path
        .strip_prefix(GRAPH)
        .or_else(|| path.strip_prefix("https://graph.microsoft.com"))
        .unwrap_or(path);
    match path.find("$skiptoken=") {
        Some(at) => format!("{}$skiptoken=…", &path[..at]),
        None => path.to_owned(),
    }
}

/// Graph's `{"error": {"code": …, "message": …}}`, as a sentence.
fn graph_error(status: u16, body: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(body).ok();
    let error = parsed.as_ref().map(|v| &v["error"]);
    let code = error.and_then(|e| e["code"].as_str()).unwrap_or("");
    let message = error.and_then(|e| e["message"].as_str()).unwrap_or("");
    let hint = match status {
        401 => " Press Refresh Token on the Connection tab.",
        // Sign-in logs, among other things, need Entra ID P1 or P2, and
        // Graph says so with a 403 that no permission will fix.
        403 if message.contains("premium license") => {
            " This needs a Microsoft Entra ID P1 or P2 licence in the tenant; granting permissions will not help."
        }
        403 => {
            " The app registration may be missing a permission, or admin consent. Grant Permissions on the Connection tab adds any that are missing."
        }
        _ => "",
    };
    match (code, message) {
        ("", "") => format!("Microsoft Graph answered {status}.{hint}"),
        (code, "") => format!("Microsoft Graph answered {status} ({code}).{hint}"),
        (_, message) => format!("{message}{hint}"),
    }
}

/// The claims in an access token's payload, or `null` if it has none that
/// can be read. The token is only read, never verified: it came straight
/// from Microsoft over TLS, and nothing is decided on it except what to show
/// and which tenant the export files rows under.
fn claims_in(token: &str) -> Value {
    token
        .split('.')
        .nth(1)
        .and_then(|payload| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

/// The `roles` claim, which is where an app-only token lists the application
/// permissions it was granted.
fn roles_in(claims: &Value) -> Vec<String> {
    let mut roles: Vec<String> = claims["roles"]
        .as_array()
        .map(|r| r.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    roles.sort();
    roles
}

/// Escape a value for use inside single quotes in an OData filter or key.
pub fn odata_quote(value: &str) -> String {
    value.replace('\'', "''")
}

/// Percent-encode a query-string value. Graph filters are full of spaces and
/// quotes, and a UPN can hold `+`, `#` and `&`.
pub fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'\'') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_and_tenant_are_read_from_the_token_payload() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            r#"{"tid":"ABC","roles":["User.ReadWrite.All","Device.ReadWrite.All"]}"#,
        );
        let claims = claims_in(&format!("header.{payload}.signature"));
        assert_eq!(
            roles_in(&claims),
            vec!["Device.ReadWrite.All", "User.ReadWrite.All"]
        );
        assert_eq!(claims["tid"], "ABC");
        assert!(roles_in(&claims_in("not a token")).is_empty());
    }

    #[test]
    fn throttling_is_waited_out_but_a_post_is_never_repeated_blindly() {
        let secs = Duration::from_secs;
        assert_eq!(retry_wait("GET", 429, 1, Some(secs(7))), Some(secs(7)));
        assert_eq!(retry_wait("POST", 429, 1, None), Some(secs(2)));
        assert_eq!(retry_wait("GET", 503, 3, None), Some(secs(8)));
        assert_eq!(retry_wait("GET", 429, 1, Some(secs(3600))), Some(LONGEST_WAIT));
        assert_eq!(retry_wait("POST", 503, 1, None), None);
        assert_eq!(retry_wait("GET", 404, 1, None), None);
        assert_eq!(retry_wait("GET", 429, ATTEMPTS, None), None);
    }

    #[test]
    fn sign_in_errors_lose_their_trace_ids() {
        let one_line = "AADSTS90002: Tenant 'x' not found. Check the tenant ID. Trace ID: eb43 Correlation ID: 1c36 Timestamp: 2026-10-02 20:30:48Z";
        assert_eq!(
            aadsts_summary(one_line),
            "AADSTS90002: Tenant 'x' not found. Check the tenant ID."
        );
        let lines = "AADSTS7000215: Invalid client secret provided.\r\nTrace ID: eb43\r\n";
        assert_eq!(aadsts_summary(lines), "AADSTS7000215: Invalid client secret provided.");
    }

    #[test]
    fn the_token_only_goes_to_graph() {
        assert_eq!(Graph::url("/users").unwrap(), format!("{GRAPH}/users"));
        let next = format!("{GRAPH}/users?$skiptoken=abc");
        assert_eq!(Graph::url(&next).unwrap(), next);
        assert!(Graph::url("https://graph.microsoft.com.evil.example/v1.0/users").is_err());
        assert!(Graph::url("https://evil.example/users").is_err());
        assert!(Graph::url("users").is_err());
        let beta = format!("{GRAPH_BETA}/reports/servicePrincipalSignInActivities");
        assert_eq!(Graph::url(&beta).unwrap(), beta);
        assert!(Graph::url("https://graph.microsoft.com/betamax/users").is_err());
        assert!(Graph::url("https://graph.microsoft.com/v2/users").is_err());
    }

    #[test]
    fn logged_paths_lose_the_host_and_the_skip_token() {
        assert_eq!(shown("/users?$top=999"), "/users?$top=999");
        assert_eq!(
            shown(&format!("{GRAPH}/users?$top=999&$skiptoken=RFNwdAIAAQAAAD")),
            "/users?$top=999&$skiptoken=…"
        );
        assert_eq!(
            shown(&format!("{GRAPH_BETA}/reports/servicePrincipalSignInActivities")),
            "/beta/reports/servicePrincipalSignInActivities"
        );
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(
            encode_query("userPrincipalName eq 'a+b@x.com'"),
            "userPrincipalName%20eq%20'a%2Bb%40x.com'"
        );
    }

    #[test]
    fn graph_errors_become_their_message() {
        let body = r#"{"error":{"code":"Request_BadRequest","message":"Another object with the same value for property userPrincipalName already exists."}}"#;
        assert_eq!(
            graph_error(400, body),
            "Another object with the same value for property userPrincipalName already exists."
        );
        assert!(graph_error(403, "").contains("permission"));
    }
}
