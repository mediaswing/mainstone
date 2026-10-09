//! Granting the app registration the permissions it needs, from inside the
//! app.
//!
//! An app cannot raise its own permissions with its own secret: that is the
//! point of admin consent. So this borrows an administrator for one sign-in.
//! The browser opens Microsoft's sign-in page; the administrator signs in;
//! Microsoft sends the browser back to a one-shot listener on `localhost`
//! with a code; the code becomes a token, and the token is used once to:
//!
//! 1. add the missing Graph application permissions to the app
//!    registration's *API permissions* list, so the portal shows them, and
//! 2. grant each one — an app role assignment on the registration's service
//!    principal, which is exactly what *Grant admin consent* creates.
//!
//! The token is held only in memory and dropped when this finishes. The
//! sign-in uses Microsoft's own public client, *Microsoft Graph Command Line
//! Tools* (the one behind `Connect-MgGraph`), which accepts a `localhost`
//! redirect on any port. That keeps the app registration free of redirect
//! URIs and public-client settings, so it stays a plain secret-only
//! registration. It is an OAuth authorisation code flow with PKCE, so the
//! code is useless to anything but the process that started it.
//!
//! The administrator needs to be a Global Administrator or a Privileged Role
//! Administrator: granting application permissions to Microsoft Graph is
//! reserved for those two roles.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use rand::Rng as _;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::{Graph, LOGIN, REQUIRED_ROLES, Result, aadsts_summary, checked_tenant, request_id};

/// Microsoft Graph Command Line Tools: Microsoft's public client for
/// administrators working with Graph interactively.
const ADMIN_CLIENT_ID: &str = "14d82eec-204b-4c2f-b7e8-296a70dab67e";
/// Microsoft Graph's own application ID, the same in every tenant.
pub(crate) const GRAPH_APP_ID: &str = "00000003-0000-0000-c000-000000000000";
/// Just enough to edit app registrations and assign app roles.
const ADMIN_SCOPES: &str = "https://graph.microsoft.com/Application.ReadWrite.All \
https://graph.microsoft.com/AppRoleAssignment.ReadWrite.All";
/// How long to wait for the administrator to finish in the browser.
const SIGN_IN_WAIT: Duration = Duration::from_secs(300);

/// What granting did.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Permissions granted just now.
    pub granted: Vec<String>,
    /// Permissions the registration already had.
    pub already: Vec<String>,
    /// Something that went wrong without stopping the grants, such as the
    /// API permissions list not being updatable.
    pub warnings: Vec<String>,
}

/// A sign-in waiting for the browser. Made on the drawing thread, so the URL
/// to open is known at once; [`AdminSignIn::finish`] then runs on another.
pub struct AdminSignIn {
    listener: TcpListener,
    redirect_uri: String,
    verifier: String,
    state: String,
    tenant: String,
    client_id: String,
    url: String,
    cancel: Arc<AtomicBool>,
}

impl AdminSignIn {
    /// Get ready to sign in as an administrator of `tenant`, to grant the
    /// app registration `client_id` its permissions.
    pub fn prepare(tenant: &str, client_id: &str) -> Result<Self> {
        let tenant = checked_tenant(tenant)?.to_owned();
        let client_id = client_id.trim().to_lowercase();
        if client_id.len() != 36 || !client_id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            return Err("The client ID is a GUID, such as 00000000-0000-0000-0000-000000000000.".into());
        }

        // Port 0: whatever the system has free. Bound to the loopback address
        // only, so nothing else on the network can reach it.
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("Could not listen for the sign-in to finish: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;

        let redirect_uri = format!("http://localhost:{port}");
        let verifier = random_string(64);
        let state = random_string(32);
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        let url = format!(
            "{LOGIN}/{tenant}/oauth2/v2.0/authorize?client_id={ADMIN_CLIENT_ID}\
&response_type=code&response_mode=query&prompt=select_account\
&redirect_uri={}&scope={}&state={state}\
&code_challenge={challenge}&code_challenge_method=S256",
            super::encode_query(&redirect_uri),
            super::encode_query(ADMIN_SCOPES),
        );
        Ok(Self {
            listener,
            redirect_uri,
            verifier,
            state,
            tenant,
            client_id,
            url,
            cancel: Arc::default(),
        })
    }

    /// The page to open in the browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Set this to stop waiting for the browser.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Wait for the administrator, then grant every permission in
    /// [`REQUIRED_ROLES`] that the registration does not have yet.
    pub fn finish(self) -> Result<Report> {
        let code = self.wait_for_code()?;
        let token = self.exchange(&code)?;
        let admin = Graph::with_bearer(&self.tenant, token);
        grant(&admin, &self.client_id)
    }

    /// Accept connections on the listener until one brings the code back.
    fn wait_for_code(&self) -> Result<String> {
        let deadline = Instant::now() + SIGN_IN_WAIT;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("Granting permissions was cancelled.".into());
            }
            if Instant::now() > deadline {
                return Err("The browser sign-in was not finished within five minutes.".into());
            }
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(answer) = self.answer(stream) {
                        return answer;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(format!("The sign-in listener failed: {e}")),
            }
        }
    }

    /// Read one request. `None` for anything that is not the redirect, such
    /// as a browser asking for a favicon, so the wait goes on.
    fn answer(&self, mut stream: TcpStream) -> Option<Result<String>> {
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).ok()?;
        // "GET /?code=…&state=… HTTP/1.1"
        let target = line.split_whitespace().nth(1)?;
        let query = target.strip_prefix("/?")?;
        let params = parse_query(query);
        let get = |name: &str| params.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

        let outcome = if get("state") != Some(self.state.as_str()) {
            // Not from the sign-in this started: ignore it and keep waiting.
            if get("code").is_some() || get("error").is_some() {
                log::warn!("ignoring a sign-in redirect with the wrong state");
            }
            respond(&mut stream, "404 Not Found", "Not found.");
            return None;
        } else if let Some(code) = get("code") {
            Ok(code.to_owned())
        } else {
            let description = get("error_description").or(get("error")).unwrap_or("unknown error");
            log::warn!("admin sign-in refused: {description}");
            Err(format!("Sign-in refused: {}", aadsts_summary(description)))
        };
        let page = match &outcome {
            Ok(_) => "Signed in. Mainstone Cloud System is granting the permissions; you can close this tab and go back to the app.",
            Err(_) => "The sign-in did not work. Go back to Mainstone Cloud System to see why.",
        };
        respond(&mut stream, "200 OK", page);
        Some(outcome)
    }

    /// Swap the code for the administrator's access token.
    fn exchange(&self, code: &str) -> Result<String> {
        let url = format!("{LOGIN}/{}/oauth2/v2.0/token", self.tenant);
        let mut response = super::agent()
            .post(&url)
            .send_form([
                ("grant_type", "authorization_code"),
                ("client_id", ADMIN_CLIENT_ID),
                ("code", code),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("code_verifier", self.verifier.as_str()),
                ("scope", ADMIN_SCOPES),
            ])
            .map_err(|e| format!("Could not reach Microsoft sign-in: {e}"))?;
        let status = response.status();
        let request_id = request_id(&response);
        let body: Value = response
            .body_mut()
            .read_json()
            .map_err(|e| format!("Unreadable answer from Microsoft sign-in: {e}"))?;
        if !status.is_success() {
            let description = body["error_description"]
                .as_str()
                .or_else(|| body["error"].as_str())
                .unwrap_or("unknown error");
            log::warn!(
                "admin token request refused ({}, request-id {request_id}): {description}",
                status.as_u16()
            );
            return Err(format!("Sign-in refused: {}", aadsts_summary(description)));
        }
        body["access_token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| "Microsoft sign-in did not return a token.".to_owned())
    }
}

/// Add the missing permissions to the registration and grant them.
fn grant(admin: &Graph, client_id: &str) -> Result<Report> {
    let mut report = Report::default();

    // Microsoft Graph's service principal, and the IDs of its app roles.
    let graph_sp = admin.get(&format!(
        "/servicePrincipals(appId='{GRAPH_APP_ID}')?$select=id,appRoles"
    ))?;
    let graph_sp_id = graph_sp["id"]
        .as_str()
        .ok_or("Microsoft Graph's service principal was not found in this tenant.")?
        .to_owned();
    let role_id = |name: &str| -> Option<String> {
        graph_sp["appRoles"].as_array()?.iter().find_map(|role| {
            let for_apps = role["allowedMemberTypes"]
                .as_array()
                .is_some_and(|types| types.iter().any(|t| t == "Application"));
            (for_apps && role["value"] == name).then(|| role["id"].as_str().map(str::to_owned))?
        })
    };
    let wanted: Vec<(&str, String)> = REQUIRED_ROLES
        .iter()
        .filter_map(|(name, _)| match role_id(name) {
            Some(id) => Some((*name, id)),
            None => {
                report
                    .warnings
                    .push(format!("Microsoft Graph has no application permission called {name}."));
                None
            }
        })
        .collect();

    // List them on the registration, so the portal's API permissions page
    // agrees with what is granted. Only possible when the registration lives
    // in this tenant; a multi-tenant app registered elsewhere is skipped.
    match admin.get(&format!(
        "/applications(appId='{client_id}')?$select=id,requiredResourceAccess"
    )) {
        Ok(application) => {
            if let Err(err) = list_on_registration(admin, &application, &wanted) {
                report
                    .warnings
                    .push(format!("The permissions were granted, but not added to the registration's API permissions list: {err}"));
            }
        }
        Err(err) => report.warnings.push(format!(
            "The app registration could not be read in this tenant, so its API permissions list was left alone: {err}"
        )),
    }

    // The registration's service principal, which is what roles are granted
    // to. It exists once the app has been used in the tenant; if not, make it.
    let our_sp_id = match admin.get(&format!("/servicePrincipals(appId='{client_id}')?$select=id")) {
        Ok(sp) => sp["id"].as_str().map(str::to_owned),
        Err(_) => admin
            .post("/servicePrincipals", &json!({ "appId": client_id }))?
            .and_then(|sp| sp["id"].as_str().map(str::to_owned)),
    }
    .ok_or("The app registration's service principal could not be found or created.")?;

    let assigned: Vec<Value> = admin.get_all(&format!(
        "/servicePrincipals/{our_sp_id}/appRoleAssignments?$select=appRoleId,resourceId"
    ))?;
    let has = |role: &str| {
        assigned
            .iter()
            .any(|a| a["resourceId"] == graph_sp_id.as_str() && a["appRoleId"] == role)
    };

    for (name, role) in &wanted {
        if has(role) {
            report.already.push((*name).to_owned());
            continue;
        }
        admin.post(
            &format!("/servicePrincipals/{graph_sp_id}/appRoleAssignedTo"),
            &json!({
                "principalId": our_sp_id,
                "resourceId": graph_sp_id,
                "appRoleId": role,
            }),
        )?;
        log::info!("granted {name}");
        report.granted.push((*name).to_owned());
    }
    Ok(report)
}

/// Merge the wanted roles into the registration's `requiredResourceAccess`.
fn list_on_registration(admin: &Graph, application: &Value, wanted: &[(&str, String)]) -> Result<()> {
    let object_id = application["id"].as_str().ok_or("no object ID")?;
    let mut access: Vec<Value> = application["requiredResourceAccess"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let at = match access.iter().position(|r| r["resourceAppId"] == GRAPH_APP_ID) {
        Some(at) => at,
        None => {
            access.push(json!({ "resourceAppId": GRAPH_APP_ID, "resourceAccess": [] }));
            access.len() - 1
        }
    };
    let entries = access[at]["resourceAccess"]
        .as_array_mut()
        .ok_or("unexpected requiredResourceAccess")?;
    let before = entries.len();
    for (_, role) in wanted {
        if !entries.iter().any(|e| e["id"] == role.as_str() && e["type"] == "Role") {
            entries.push(json!({ "id": role, "type": "Role" }));
        }
    }
    if entries.len() == before {
        return Ok(());
    }
    admin.patch(
        &format!("/applications/{object_id}"),
        &json!({ "requiredResourceAccess": access }),
    )
}

/// Characters a PKCE verifier and an OAuth state may use.
fn random_string(len: usize) -> String {
    const SET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let mut rng = rand::rng();
    (0..len).map(|_| SET[rng.random_range(0..SET.len())] as char).collect()
}

/// `a=1&b=x%20y` as pairs, decoded.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            Some((percent_decode(k)?, percent_decode(v)?))
        })
        .collect()
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 2;
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8(out).ok()
}

fn respond(stream: &mut TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>Mainstone Cloud System</title>\
<body style=\"font-family:system-ui,sans-serif;max-width:36em;margin:4em auto;line-height:1.5\">\
<h1>Mainstone Cloud System</h1><p>{message}</p></body>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_queries_are_decoded() {
        let pairs = parse_query("code=0.AB%2Bc-d&state=xyz&error_description=Need+admin%3A+yes");
        assert_eq!(pairs[0], ("code".into(), "0.AB+c-d".into()));
        assert_eq!(pairs[1], ("state".into(), "xyz".into()));
        assert_eq!(pairs[2].1, "Need admin: yes");
        assert!(percent_decode("%zz").is_none());
    }

    #[test]
    fn the_sign_in_url_carries_pkce_and_a_localhost_redirect() {
        let sign_in = AdminSignIn::prepare(
            "contoso.onmicrosoft.com",
            "11111111-2222-3333-4444-555555555555",
        )
        .unwrap();
        let url = sign_in.url();
        assert!(url.starts_with(&format!("{LOGIN}/contoso.onmicrosoft.com/oauth2/v2.0/authorize?")));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A"));
        assert!(url.contains(&format!("state={}", sign_in.state)));
        assert_eq!(sign_in.verifier.len(), 64);
    }

    #[test]
    fn a_client_id_must_be_a_guid() {
        assert!(AdminSignIn::prepare("contoso.onmicrosoft.com", "not-a-guid").is_err());
        assert!(AdminSignIn::prepare("contoso/evil", "11111111-2222-3333-4444-555555555555").is_err());
    }

    #[test]
    fn the_redirect_is_answered_and_its_code_returned() {
        let sign_in = AdminSignIn::prepare(
            "contoso.onmicrosoft.com",
            "11111111-2222-3333-4444-555555555555",
        )
        .unwrap();
        let port = sign_in.listener.local_addr().unwrap().port();
        let state = sign_in.state.clone();
        let browser = std::thread::spawn(move || {
            use std::io::Read as _;
            // A stray request first, as browsers make, then the redirect.
            let mut stray = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(stray, "GET /favicon.ico HTTP/1.1\r\n\r\n").unwrap();
            let mut page = String::new();
            let mut redirect = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(redirect, "GET /?code=abc%2B1&state={state} HTTP/1.1\r\n\r\n").unwrap();
            redirect.read_to_string(&mut page).unwrap();
            page
        });
        assert_eq!(sign_in.wait_for_code().unwrap(), "abc+1");
        assert!(browser.join().unwrap().contains("200 OK"));
    }
}
