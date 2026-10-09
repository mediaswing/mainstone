//! Apps connected to the tenant: every enterprise app (service principal),
//! what it is allowed to do, and what about it is worth a second look.
//! Read-only.
//!
//! Most of this needs `Application.Read.All`. The delegated consents need
//! `Directory.Read.All`: Microsoft accepts nothing narrower for reading them
//! with an app-only token. When last used comes from a beta report that
//! needs `AuditLog.Read.All` and an Entra ID P1 or P2 licence. Either can be
//! missing; the list loads without it and says what is not known.
//!
//! Application permissions are read from the resource's side, which takes
//! one request per resource rather than one per app. The list does this for
//! the three resources whose permissions matter most (Microsoft Graph,
//! Exchange Online and SharePoint); an app's details read all of them.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;

use super::consent::GRAPH_APP_ID;
use super::models::DirectoryObject;
use super::{GRAPH_BETA, Graph, Result};

/// The tenants Microsoft's own apps are registered in.
const MICROSOFT_TENANTS: [&str; 4] = [
    "f8cdef31-a31e-4b4a-93e4-5f571e91255a",
    "72f988bf-86f5-41af-91ab-2d7cd011db47",
    "33e01921-4d64-4f8c-a055-5bdaffd5e33d",
    "cdc5aeea-15c5-4db6-b079-fcadd2505dc2",
];

/// The resources whose application permissions the list reads up front.
const KEY_RESOURCES: [&str; 3] = [
    GRAPH_APP_ID,
    // Office 365 Exchange Online
    "00000002-0000-0ff1-ce00-000000000000",
    // Office 365 SharePoint Online
    "00000003-0000-0ff1-ce00-000000000000",
];

/// A credential this close to its end is flagged.
const EXPIRY_WARNING_DAYS: i64 = 30;
/// An app not signed in for this long is flagged as unused.
const UNUSED_DAYS: i64 = 90;

/// Permissions that amount to control of the tenant: whoever holds one can
/// grant themselves anything else.
const CRITICAL: &[&str] = &[
    "RoleManagement.ReadWrite.Directory",
    "AppRoleAssignment.ReadWrite.All",
    "Application.ReadWrite.All",
    "Directory.ReadWrite.All",
    "Directory.AccessAsUser.All",
    "Domain.ReadWrite.All",
    "Sites.FullControl.All",
    "full_access_as_app",
    "full_access_as_user",
];

/// Application permissions that read or change everyone's content, whatever
/// their name says: `Mail.Read` granted to an app is every mailbox.
const BULK_DATA_PREFIXES: &[&str] = &[
    "Mail.",
    "MailboxSettings.",
    "Calendars.",
    "Contacts.",
    "Files.",
    "Sites.",
    "Notes.",
    "Chat.",
    "ChannelMessage.",
    "EWS.",
];

// ---------------------------------------------------------------------------
// What Graph returns.

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServicePrincipal {
    pub id: String,
    pub app_id: String,
    pub display_name: Option<String>,
    pub app_owner_organization_id: Option<String>,
    pub publisher_name: Option<String>,
    pub verified_publisher: Option<VerifiedPublisher>,
    pub account_enabled: Option<bool>,
    pub app_role_assignment_required: Option<bool>,
    pub service_principal_type: Option<String>,
    pub homepage: Option<String>,
    pub reply_urls: Vec<String>,
    pub key_credentials: Vec<Credential>,
    pub password_credentials: Vec<Credential>,
}

impl ServicePrincipal {
    const SELECT: &'static str = "id,appId,displayName,appOwnerOrganizationId,publisherName,\
verifiedPublisher,accountEnabled,appRoleAssignmentRequired,servicePrincipalType,homepage,\
replyUrls,keyCredentials,passwordCredentials";
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct VerifiedPublisher {
    pub display_name: Option<String>,
    pub verified_publisher_id: Option<String>,
}

/// A client secret or a certificate.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Credential {
    pub key_id: Option<String>,
    pub display_name: Option<String>,
    pub end_date_time: Option<String>,
    /// `Sign` or `Verify` on a certificate; absent on a secret.
    pub usage: Option<String>,
}

/// An app registration: only the parts the enterprise app does not carry.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Registration {
    pub id: String,
    pub app_id: String,
    pub created_date_time: Option<String>,
    pub sign_in_audience: Option<String>,
    pub key_credentials: Vec<Credential>,
    pub password_credentials: Vec<Credential>,
}

impl Registration {
    const SELECT: &'static str =
        "id,appId,createdDateTime,signInAudience,keyCredentials,passwordCredentials";
}

/// An app role granted to something: an application permission when the
/// principal is an app, an assignment when it is a user or group.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RoleAssignment {
    pub id: String,
    pub app_role_id: String,
    pub principal_id: String,
    pub principal_display_name: Option<String>,
    /// `User`, `Group` or `ServicePrincipal`.
    pub principal_type: Option<String>,
    pub resource_id: String,
    pub resource_display_name: Option<String>,
    pub created_date_time: Option<String>,
}

/// Delegated permissions granted to an app, for everyone or for one user.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Grant {
    pub client_id: String,
    /// `AllPrincipals` when an administrator consented for everyone,
    /// `Principal` when one user consented for themselves.
    pub consent_type: Option<String>,
    pub principal_id: Option<String>,
    pub resource_id: String,
    pub scope: Option<String>,
}

impl Grant {
    pub fn for_everyone(&self) -> bool {
        self.consent_type.as_deref() == Some("AllPrincipals")
    }

    pub fn scopes(&self) -> impl Iterator<Item = &str> {
        self.scope.as_deref().unwrap_or("").split_whitespace()
    }
}

/// An API that apps are given permissions on, with the names of those
/// permissions.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Resource {
    pub id: String,
    pub app_id: String,
    pub display_name: Option<String>,
    pub app_roles: Vec<AppRole>,
    pub oauth2_permission_scopes: Vec<Scope>,
}

impl Resource {
    const SELECT: &'static str = "id,appId,displayName,appRoles,oauth2PermissionScopes";

    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or("(unnamed API)")
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppRole {
    pub id: String,
    pub value: Option<String>,
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Scope {
    pub value: Option<String>,
    pub admin_consent_display_name: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct SignInActivity {
    app_id: String,
    last_sign_in_activity: Option<LastSignIn>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LastSignIn {
    last_sign_in_date_time: Option<String>,
}

// ---------------------------------------------------------------------------
// What the app makes of it.

/// Who an app belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Microsoft,
    /// Registered in this tenant.
    ThisTenant,
    ThirdParty,
    ManagedIdentity,
    /// No owner tenant recorded, as on some legacy apps.
    Other,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Microsoft => "Microsoft",
            Self::ThisTenant => "This tenant's",
            Self::ThirdParty => "Third-party",
            Self::ManagedIdentity => "Managed identity",
            Self::Other => "Other",
        }
    }
}

fn kind_of(sp: &ServicePrincipal, tenant_guid: Option<&str>) -> Kind {
    if sp.service_principal_type.as_deref() == Some("ManagedIdentity") {
        return Kind::ManagedIdentity;
    }
    let Some(owner) = sp.app_owner_organization_id.as_deref().map(str::to_lowercase) else {
        return Kind::Other;
    };
    if MICROSOFT_TENANTS.contains(&owner.as_str()) {
        Kind::Microsoft
    } else if tenant_guid.is_some_and(|t| t.eq_ignore_ascii_case(&owner)) {
        Kind::ThisTenant
    } else {
        Kind::ThirdParty
    }
}

/// How much a permission lets an app do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Privilege {
    Normal,
    /// Everyone's mail, files, chats, or the power to change the directory.
    High,
    /// Enough to take over the tenant.
    Critical,
}

/// How much `value` lets an app do, as an application permission (with no
/// user) or a delegated one (as the signed-in user).
pub fn privilege(value: &str, application: bool) -> Privilege {
    if CRITICAL.contains(&value) {
        return Privilege::Critical;
    }
    // `.Selected` permissions reach only what each app is given one by one.
    if value.ends_with(".Selected") {
        return Privilege::Normal;
    }
    let high = if application {
        value.ends_with(".ReadWrite.All")
            || value == "Directory.Read.All"
            || BULK_DATA_PREFIXES.iter().any(|p| value.starts_with(p))
    } else {
        value.ends_with(".ReadWrite.All")
            || matches!(value, "Mail.ReadWrite" | "Mail.Send" | "EWS.AccessAsUser.All")
    };
    if high { Privilege::High } else { Privilege::Normal }
}

/// Something about an app worth a second look.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Flag {
    Critical,
    HighPrivilege,
    CredentialExpired,
    UserConsented,
    CredentialExpiring,
    Unverified,
    Unused,
    Disabled,
}

impl Flag {
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "Critical permissions",
            Self::HighPrivilege => "High privilege",
            Self::CredentialExpired => "Credentials expired",
            Self::UserConsented => "User consent",
            Self::CredentialExpiring => "Credentials expiring",
            Self::Unverified => "Unverified publisher",
            Self::Unused => "Unused",
            Self::Disabled => "Disabled",
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            Self::Critical => {
                "Holds a permission that is enough to take over the tenant, such as managing roles or every app's permissions."
            }
            Self::HighPrivilege => {
                "Can read or change everyone's data, such as every mailbox or every file, or change the directory."
            }
            Self::CredentialExpired => {
                "Every secret and certificate has expired, so the app can no longer sign in with them."
            }
            Self::UserConsented => {
                "Users consented to this app themselves, without an administrator."
            }
            Self::CredentialExpiring => "Its last valid secret or certificate expires within 30 days.",
            Self::Unverified => "The publisher has not been verified by Microsoft.",
            Self::Unused => "No sign-ins in the last 90 days.",
            Self::Disabled => "Sign-in is turned off for this app.",
        }
    }

    /// Whether this is worse than worth a look.
    pub fn severe(self) -> bool {
        matches!(self, Self::Critical | Self::HighPrivilege | Self::CredentialExpired)
    }
}

/// One enterprise app and everything known about it from the list's load.
#[derive(Clone, Debug)]
pub struct ConnectedApp {
    pub sp: ServicePrincipal,
    pub registration: Option<Registration>,
    pub kind: Kind,
    /// This is the registration Mainstone signs in as.
    pub is_self: bool,
    /// Application permissions on the [`KEY_RESOURCES`].
    pub app_roles: Vec<RoleAssignment>,
    /// Delegated permission grants, when they could be read.
    pub grants: Vec<Grant>,
    /// The last sign-in as Graph wrote it, when the report could be read.
    pub last_sign_in: Option<String>,
    pub flags: Vec<Flag>,
}

impl ConnectedApp {
    pub fn name(&self) -> &str {
        self.sp.display_name.as_deref().unwrap_or("(no name)")
    }

    /// The publisher Microsoft verified, or else the one the app claims.
    pub fn publisher(&self) -> &str {
        self.verified_publisher()
            .or(self.sp.publisher_name.as_deref())
            .unwrap_or("")
    }

    pub fn verified_publisher(&self) -> Option<&str> {
        let v = self.sp.verified_publisher.as_ref()?;
        v.verified_publisher_id.as_deref().filter(|id| !id.is_empty())?;
        v.display_name.as_deref()
    }

    pub fn enabled(&self) -> bool {
        self.sp.account_enabled.unwrap_or(true)
    }

    /// Secrets and certificates, the registration's first. A certificate
    /// on the enterprise app itself is usually a SAML signing certificate.
    pub fn credentials(&self) -> Vec<CredentialInfo<'_>> {
        let mut all = Vec::new();
        if let Some(r) = &self.registration {
            all.extend(r.password_credentials.iter().map(|c| CredentialInfo::new(c, "Secret", "Registration")));
            all.extend(r.key_credentials.iter().map(|c| CredentialInfo::new(c, "Certificate", "Registration")));
        }
        all.extend(self.sp.password_credentials.iter().map(|c| CredentialInfo::new(c, "Secret", "Enterprise app")));
        all.extend(self.sp.key_credentials.iter().map(|c| CredentialInfo::new(c, "Certificate", "Enterprise app")));
        all
    }

    /// When the last of its credentials runs out, if it has any.
    pub fn credentials_end(&self) -> Option<DateTime<Utc>> {
        self.credentials().iter().filter_map(|c| c.end).max()
    }

    pub fn last_sign_in_time(&self) -> Option<DateTime<Utc>> {
        parse_time(self.last_sign_in.as_deref()?)
    }

    pub fn user_grants(&self) -> impl Iterator<Item = &Grant> {
        self.grants.iter().filter(|g| !g.for_everyone())
    }

    /// The users who consented for themselves.
    pub fn consenting_users(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .user_grants()
            .filter_map(|g| g.principal_id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// The delegated permissions it holds, each once, however many grants
    /// they come from.
    pub fn delegated_scopes(&self) -> Vec<&str> {
        let mut scopes: Vec<&str> = self.grants.iter().flat_map(Grant::scopes).collect();
        scopes.sort_unstable();
        scopes.dedup();
        scopes
    }

    /// The worst flag, for the colour of the list's Flags column.
    pub fn worst(&self) -> Option<Flag> {
        self.flags.iter().copied().min()
    }
}

/// A secret or certificate, ready to show.
#[derive(Clone, Debug)]
pub struct CredentialInfo<'a> {
    pub kind: &'static str,
    pub on: &'static str,
    pub name: &'a str,
    pub end: Option<DateTime<Utc>>,
    pub raw_end: Option<&'a str>,
}

impl<'a> CredentialInfo<'a> {
    fn new(c: &'a Credential, kind: &'static str, on: &'static str) -> Self {
        Self {
            kind,
            on,
            name: c.display_name.as_deref().unwrap_or(""),
            end: c.end_date_time.as_deref().and_then(parse_time),
            raw_end: c.end_date_time.as_deref(),
        }
    }
}

/// Everything the Apps tab lists.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub apps: Vec<ConnectedApp>,
    /// The APIs whose permission names are known, by object ID.
    pub resources: HashMap<String, Resource>,
    /// Whether the delegated grants could be read.
    pub grants_known: bool,
    /// Whether the last-sign-in report could be read.
    pub activity_known: bool,
    /// What could not be read, as sentences to show above the list.
    pub notes: Vec<String>,
}

/// A permission, named and rated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permission {
    pub resource: String,
    pub value: String,
    pub description: String,
    pub privilege: Privilege,
}

impl Inventory {
    /// An application permission by its role ID, or the ID itself when the
    /// resource's roles are not known.
    pub fn app_permission(&self, assignment: &RoleAssignment) -> Permission {
        let resource = self.resources.get(&assignment.resource_id);
        let role = resource.and_then(|r| r.app_roles.iter().find(|a| a.id == assignment.app_role_id));
        let value = role
            .and_then(|r| r.value.clone())
            .unwrap_or_else(|| assignment.app_role_id.clone());
        Permission {
            resource: resource
                .map(|r| r.name().to_owned())
                .or_else(|| assignment.resource_display_name.clone())
                .unwrap_or_default(),
            description: role.and_then(|r| r.display_name.clone()).unwrap_or_default(),
            privilege: privilege(&value, true),
            value,
        }
    }

    /// A delegated permission, from a grant's scope.
    pub fn delegated_permission(&self, resource_id: &str, value: &str) -> Permission {
        let resource = self.resources.get(resource_id);
        let scope = resource.and_then(|r| {
            r.oauth2_permission_scopes
                .iter()
                .find(|s| s.value.as_deref() == Some(value))
        });
        Permission {
            resource: resource.map(|r| r.name().to_owned()).unwrap_or_default(),
            value: value.to_owned(),
            description: scope
                .and_then(|s| s.admin_consent_display_name.clone())
                .unwrap_or_default(),
            privilege: privilege(value, false),
        }
    }

    /// An app's delegated permissions, by resource, admin-consented first.
    pub fn delegated_permissions(&self, app: &ConnectedApp, for_everyone: bool) -> Vec<Permission> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for g in app.grants.iter().filter(|g| g.for_everyone() == for_everyone) {
            for scope in g.scopes() {
                if seen.insert((g.resource_id.clone(), scope.to_owned())) {
                    out.push(self.delegated_permission(&g.resource_id, scope));
                }
            }
        }
        out.sort_by(|a, b| (&a.resource, &a.value).cmp(&(&b.resource, &b.value)));
        out
    }

    /// One line per application permission, for the list and the CSV:
    /// `Microsoft Graph: Mail.Read`.
    pub fn app_permission_names(&self, assignments: &[RoleAssignment]) -> Vec<String> {
        let mut names: Vec<String> = assignments
            .iter()
            .map(|a| {
                let p = self.app_permission(a);
                format!("{}: {}", p.resource, p.value)
            })
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// What an app's details panel reads when it is opened.
#[derive(Clone, Debug, Default)]
pub struct AppDetails {
    /// Application permissions on every resource, not only the key ones.
    pub app_roles: Vec<RoleAssignment>,
    /// Users and groups assigned to the app.
    pub assigned: Vec<RoleAssignment>,
    pub owners: Vec<DirectoryObject>,
    /// The users who consented for themselves, by name. `None` when they
    /// could not be looked up.
    pub consenters: Option<Vec<DirectoryObject>>,
    /// APIs whose permission names were not known before.
    pub new_resources: Vec<Resource>,
}

// ---------------------------------------------------------------------------
// Reading it.

impl Graph {
    /// Every enterprise app, with what is known about each.
    pub fn list_apps(&self, tenant_guid: Option<&str>) -> Result<Inventory> {
        let sps: Vec<ServicePrincipal> = self.get_all(&format!(
            "/servicePrincipals?$select={}&$top=999",
            ServicePrincipal::SELECT
        ))?;
        let mut inventory = Inventory::default();

        let registrations: HashMap<String, Registration> = match self.get_all::<Registration>(
            &format!("/applications?$select={}&$top=999", Registration::SELECT),
        ) {
            Ok(list) => list.into_iter().map(|r| (r.app_id.clone(), r)).collect(),
            Err(err) => {
                inventory
                    .notes
                    .push(format!("App registrations could not be read, so secret and certificate expiry is incomplete: {err}"));
                HashMap::new()
            }
        };

        let mut roles_by_app: HashMap<String, Vec<RoleAssignment>> = HashMap::new();
        for app_id in KEY_RESOURCES {
            let resource = match self.get_if_found(&format!(
                "/servicePrincipals(appId='{app_id}')?$select={}",
                Resource::SELECT
            )) {
                Ok(Some(value)) => serde_json::from_value::<Resource>(value)
                    .map_err(|e| format!("Unexpected answer from Microsoft Graph: {e}"))?,
                // Not every tenant has Exchange or SharePoint.
                Ok(None) => continue,
                Err(err) => {
                    inventory
                        .notes
                        .push(format!("Application permissions could not be read: {err}"));
                    break;
                }
            };
            let assigned: Vec<RoleAssignment> = match self.get_all(&format!(
                "/servicePrincipals/{}/appRoleAssignedTo",
                resource.id
            )) {
                Ok(list) => list,
                Err(err) => {
                    inventory
                        .notes
                        .push(format!("Permissions on {} could not be read: {err}", resource.name()));
                    Vec::new()
                }
            };
            for a in assigned {
                if a.principal_type.as_deref() == Some("ServicePrincipal") {
                    roles_by_app.entry(a.principal_id.clone()).or_default().push(a);
                }
            }
            inventory.resources.insert(resource.id.clone(), resource);
        }

        let mut grants_by_app: HashMap<String, Vec<Grant>> = HashMap::new();
        match self.get_all::<Grant>("/oauth2PermissionGrants") {
            Ok(grants) => {
                inventory.grants_known = true;
                for g in grants {
                    grants_by_app.entry(g.client_id.clone()).or_default().push(g);
                }
            }
            Err(err) => inventory.notes.push(format!(
                "Delegated permissions could not be read. They need Directory.Read.All: {err}"
            )),
        }

        let mut last_sign_in: HashMap<String, String> = HashMap::new();
        match self.get_all::<SignInActivity>(&format!(
            "{GRAPH_BETA}/reports/servicePrincipalSignInActivities"
        )) {
            Ok(list) => {
                inventory.activity_known = true;
                for a in list {
                    if let Some(when) = a.last_sign_in_activity.and_then(|l| l.last_sign_in_date_time) {
                        last_sign_in.insert(a.app_id.to_lowercase(), when);
                    }
                }
            }
            Err(err) => inventory.notes.push(format!(
                "When each app was last used is not known. It needs AuditLog.Read.All and an Entra ID P1 or P2 licence: {err}"
            )),
        }

        let own_client = self.credentials().client_id.trim().to_lowercase();
        let now = Utc::now();
        let mut apps: Vec<ConnectedApp> = sps
            .into_iter()
            .map(|sp| {
                let mut app = ConnectedApp {
                    kind: kind_of(&sp, tenant_guid),
                    is_self: sp.app_id.eq_ignore_ascii_case(&own_client),
                    registration: registrations.get(&sp.app_id).cloned(),
                    app_roles: roles_by_app.remove(&sp.id).unwrap_or_default(),
                    grants: grants_by_app.remove(&sp.id).unwrap_or_default(),
                    last_sign_in: last_sign_in.get(&sp.app_id.to_lowercase()).cloned(),
                    flags: Vec::new(),
                    sp,
                };
                app.flags = flags(&app, &inventory, now);
                app
            })
            .collect();
        apps.sort_by_key(|a| a.name().to_lowercase());
        inventory.apps = apps;
        Ok(inventory)
    }

    /// What an app's details panel needs beyond the list. `known` holds the
    /// object IDs of the APIs whose permission names are already known.
    pub fn app_details(&self, app: &ConnectedApp, known: &HashSet<String>) -> Result<AppDetails> {
        let sp_id = &app.sp.id;
        let app_roles: Vec<RoleAssignment> =
            self.get_all(&format!("/servicePrincipals/{sp_id}/appRoleAssignments"))?;
        let assigned: Vec<RoleAssignment> =
            self.get_all(&format!("/servicePrincipals/{sp_id}/appRoleAssignedTo"))?;
        let mut owners: Vec<DirectoryObject> = self.get_all(&format!(
            "/servicePrincipals/{sp_id}/owners?$select=id,displayName,userPrincipalName,mail"
        ))?;
        owners.sort_by_key(|o| o.name().to_lowercase());

        let ids = app.consenting_users();
        let consenters = if ids.is_empty() {
            Some(Vec::new())
        } else {
            match self.objects_by_id(&ids) {
                Ok(mut found) => {
                    found.sort_by_key(|o| o.name().to_lowercase());
                    Some(found)
                }
                Err(err) => {
                    log::warn!("could not look up the users who consented to {}: {err}", app.name());
                    None
                }
            }
        };

        let mut wanted: Vec<&str> = app_roles
            .iter()
            .map(|a| a.resource_id.as_str())
            .chain(app.grants.iter().map(|g| g.resource_id.as_str()))
            .filter(|id| !known.contains(*id))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        let mut new_resources = Vec::new();
        for id in wanted {
            // A resource that has gone, or cannot be read, leaves its
            // permissions shown by ID.
            match self.get_if_found(&format!("/servicePrincipals/{id}?$select={}", Resource::SELECT)) {
                Ok(Some(value)) => {
                    if let Ok(resource) = serde_json::from_value(value) {
                        new_resources.push(resource);
                    }
                }
                Ok(None) => {}
                Err(err) => log::warn!("could not read the API {id}: {err}"),
            }
        }

        Ok(AppDetails {
            app_roles,
            assigned,
            owners,
            consenters,
            new_resources,
        })
    }

    /// Directory objects by ID, a thousand at a time, which is as many as
    /// Graph takes in one request.
    fn objects_by_id(&self, ids: &[String]) -> Result<Vec<DirectoryObject>> {
        let mut found = Vec::new();
        for chunk in ids.chunks(1000) {
            let answer = self
                .post("/directoryObjects/getByIds", &json!({ "ids": chunk }))?
                .ok_or("Microsoft Graph sent an empty answer.")?;
            let values: Vec<DirectoryObject> = serde_json::from_value(answer["value"].clone())
                .map_err(|e| format!("Unexpected answer from Microsoft Graph: {e}"))?;
            found.extend(values);
        }
        Ok(found)
    }
}

/// What is worth a second look about `app`, worst first.
fn flags(app: &ConnectedApp, inventory: &Inventory, now: DateTime<Utc>) -> Vec<Flag> {
    let mut flags = Vec::new();
    let worst = app
        .app_roles
        .iter()
        .map(|a| inventory.app_permission(a).privilege)
        .chain(app.grants.iter().flat_map(|g| g.scopes().map(|s| privilege(s, false))))
        .max()
        .unwrap_or(Privilege::Normal);
    match worst {
        Privilege::Critical => flags.push(Flag::Critical),
        Privilege::High => flags.push(Flag::HighPrivilege),
        Privilege::Normal => {}
    }
    // Old secrets left behind beside a newer one are not a problem; what
    // matters is when the last of them runs out.
    if let Some(end) = app.credentials_end() {
        if end < now {
            flags.push(Flag::CredentialExpired);
        } else if end < now + chrono::Duration::days(EXPIRY_WARNING_DAYS) {
            flags.push(Flag::CredentialExpiring);
        }
    }
    if app.user_grants().next().is_some() {
        flags.push(Flag::UserConsented);
    }
    if app.kind == Kind::ThirdParty && app.verified_publisher().is_none() {
        flags.push(Flag::Unverified);
    }
    // Microsoft's own apps come and go with the services the tenant has,
    // and an unknown last sign-in is not the same as none.
    if inventory.activity_known
        && app.kind != Kind::Microsoft
        && app
            .last_sign_in_time()
            .is_none_or(|t| t < now - chrono::Duration::days(UNUSED_DAYS))
    {
        flags.push(Flag::Unused);
    }
    if !app.enabled() {
        flags.push(Flag::Disabled);
    }
    flags.sort();
    flags
}

/// A Graph date and time. The sign-in report has been seen writing an
/// offset without its leading zero (`-8:00`), which is mended first.
pub fn parse_time(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(value) {
        return Some(t.with_timezone(&Utc));
    }
    let at = value.rfind(['+', '-']).filter(|&i| i > 10)?;
    let (head, offset) = value.split_at(at);
    let mended = format!("{head}{}0{}", &offset[..1], &offset[1..]);
    DateTime::parse_from_rfc3339(&mended)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    const TENANT: &str = "84841066-274d-4ec0-a5c1-276be684bdd3";

    fn sp(owner: Option<&str>) -> ServicePrincipal {
        ServicePrincipal {
            id: "sp".into(),
            app_id: "app".into(),
            display_name: Some("Contoso Sync".into()),
            app_owner_organization_id: owner.map(str::to_owned),
            service_principal_type: Some("Application".into()),
            account_enabled: Some(true),
            ..Default::default()
        }
    }

    fn app(sp: ServicePrincipal) -> ConnectedApp {
        ConnectedApp {
            kind: kind_of(&sp, Some(TENANT)),
            is_self: false,
            registration: None,
            app_roles: Vec::new(),
            grants: Vec::new(),
            last_sign_in: None,
            flags: Vec::new(),
            sp,
        }
    }

    fn now() -> DateTime<Utc> {
        parse_time("2026-10-09T12:00:00Z").unwrap()
    }

    #[test]
    fn apps_are_told_apart_by_who_owns_them() {
        assert_eq!(kind_of(&sp(Some("F8CDEF31-A31E-4B4A-93E4-5F571E91255A")), Some(TENANT)), Kind::Microsoft);
        assert_eq!(kind_of(&sp(Some(TENANT)), Some(TENANT)), Kind::ThisTenant);
        assert_eq!(kind_of(&sp(Some("11111111-0000-0000-0000-000000000000")), Some(TENANT)), Kind::ThirdParty);
        assert_eq!(kind_of(&sp(None), Some(TENANT)), Kind::Other);
        let mut mi = sp(None);
        mi.service_principal_type = Some("ManagedIdentity".into());
        assert_eq!(kind_of(&mi, Some(TENANT)), Kind::ManagedIdentity);
    }

    #[test]
    fn permissions_are_rated() {
        assert_eq!(privilege("RoleManagement.ReadWrite.Directory", true), Privilege::Critical);
        assert_eq!(privilege("Directory.AccessAsUser.All", false), Privilege::Critical);
        // Every mailbox when granted to an app; one's own when delegated.
        assert_eq!(privilege("Mail.Read", true), Privilege::High);
        assert_eq!(privilege("Mail.Read", false), Privilege::Normal);
        assert_eq!(privilege("Group.ReadWrite.All", false), Privilege::High);
        assert_eq!(privilege("Sites.Selected", true), Privilege::Normal);
        assert_eq!(privilege("User.Read.All", true), Privilege::Normal);
        assert_eq!(privilege("User.Read", false), Privilege::Normal);
    }

    #[test]
    fn the_last_credential_decides_expiry() {
        let secret = |end: &str| Credential {
            end_date_time: Some(end.into()),
            ..Default::default()
        };
        let inventory = Inventory::default();
        let mut a = app(sp(Some(TENANT)));
        a.registration = Some(Registration {
            password_credentials: vec![secret("2025-01-01T00:00:00Z"), secret("2027-01-01T00:00:00Z")],
            ..Default::default()
        });
        assert!(flags(&a, &inventory, now()).is_empty());

        a.registration.as_mut().unwrap().password_credentials = vec![secret("2026-10-20T00:00:00Z")];
        assert_eq!(flags(&a, &inventory, now()), vec![Flag::CredentialExpiring]);

        a.registration.as_mut().unwrap().password_credentials = vec![secret("2026-10-01T00:00:00Z")];
        assert_eq!(flags(&a, &inventory, now()), vec![Flag::CredentialExpired]);
    }

    #[test]
    fn third_party_apps_are_flagged_by_what_they_hold_and_how_they_got_it() {
        let graph_sp: Resource = serde_json::from_value(serde_json::json!({
            "id": "graph-sp",
            "appId": GRAPH_APP_ID,
            "displayName": "Microsoft Graph",
            "appRoles": [{"id": "810c84a8-4a9e-49e6-bf7d-12d183f40d01", "value": "Mail.Read", "displayName": "Read mail in all mailboxes"}],
            "oauth2PermissionScopes": []
        }))
        .unwrap();
        let mut inventory = Inventory {
            activity_known: true,
            ..Default::default()
        };
        inventory.resources.insert(graph_sp.id.clone(), graph_sp);

        let mut a = app(sp(Some("11111111-0000-0000-0000-000000000000")));
        a.app_roles.push(RoleAssignment {
            app_role_id: "810c84a8-4a9e-49e6-bf7d-12d183f40d01".into(),
            resource_id: "graph-sp".into(),
            ..Default::default()
        });
        a.grants.push(Grant {
            client_id: "sp".into(),
            consent_type: Some("Principal".into()),
            principal_id: Some("user-1".into()),
            resource_id: "graph-sp".into(),
            scope: Some("User.Read".into()),
        });
        a.last_sign_in = Some("2026-01-01T00:00:00Z".into());
        assert_eq!(
            flags(&a, &inventory, now()),
            vec![Flag::HighPrivilege, Flag::UserConsented, Flag::Unverified, Flag::Unused]
        );
        assert_eq!(
            inventory.app_permission_names(&a.app_roles),
            vec!["Microsoft Graph: Mail.Read"]
        );
        assert_eq!(a.consenting_users(), vec!["user-1"]);
    }

    #[test]
    fn microsoft_apps_are_not_called_unused_or_unverified() {
        let inventory = Inventory {
            activity_known: true,
            ..Default::default()
        };
        let a = app(sp(Some("f8cdef31-a31e-4b4a-93e4-5f571e91255a")));
        assert!(flags(&a, &inventory, now()).is_empty());
        // Nor is anything, when the report could not be read.
        let b = app(sp(Some(TENANT)));
        assert!(flags(&b, &Inventory::default(), now()).is_empty());
    }

    #[test]
    fn service_principals_read_from_graphs_example() {
        let sp: ServicePrincipal = serde_json::from_value(serde_json::json!({
            "id": "59e617e5-e447-4adc-8b88-00af644d7c92",
            "appId": "65415bb1-9267-4313-bbf5-ae259732ee12",
            "displayName": "My App",
            "appOwnerOrganizationId": TENANT,
            "verifiedPublisher": {"displayName": null, "verifiedPublisherId": null, "addedDateTime": null},
            "accountEnabled": true,
            "replyUrls": [],
            "keyCredentials": [],
            "passwordCredentials": [{"displayName": "Password friendly name", "endDateTime": "2021-12-31T00:00:00Z", "keyId": "4fa4e8a0-48ea-4a53-8a31-c0dd2d2b5b48"}]
        }))
        .unwrap();
        let a = app(sp);
        assert_eq!(a.kind, Kind::ThisTenant);
        assert_eq!(a.verified_publisher(), None);
        assert_eq!(a.credentials()[0].kind, "Secret");
        assert_eq!(a.credentials_end(), parse_time("2021-12-31T00:00:00Z"));
    }

    #[test]
    fn report_times_without_a_padded_offset_are_mended() {
        assert_eq!(
            parse_time("2021-03-01T00:00:00-8:00"),
            parse_time("2021-03-01T08:00:00Z")
        );
        assert!(parse_time("not a time").is_none());
    }
}
