//! The sign-in log and the directory audit log.
//!
//! Both need `AuditLog.Read.All`, and reading sign-ins through Graph also
//! needs an Entra ID P1 or P2 licence in the tenant. Entra keeps the entries
//! for 7 days without one and 30 days with one, so a longer range than the
//! tenant keeps simply returns less.
//!
//! A busy tenant can log hundreds of thousands of sign-ins a month, far more
//! than is worth holding in a table on screen, so a load stops at
//! [`MAX_ENTRIES`], newest first, and says it has.

use serde::{Deserialize, Serialize};

use super::{Graph, Result, encode_query, odata_quote};

/// The most entries one load reads.
pub const MAX_ENTRIES: usize = 5000;

/// How far back to read.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    Hour,
    #[default]
    Day,
    Week,
    Month,
}

impl Range {
    pub const ALL: [Self; 4] = [Self::Hour, Self::Day, Self::Week, Self::Month];

    pub fn label(self) -> &'static str {
        match self {
            Self::Hour => "Last hour",
            Self::Day => "Last 24 hours",
            Self::Week => "Last 7 days",
            Self::Month => "Last 30 days",
        }
    }

    fn duration(self) -> chrono::Duration {
        match self {
            Self::Hour => chrono::Duration::hours(1),
            Self::Day => chrono::Duration::days(1),
            Self::Week => chrono::Duration::days(7),
            Self::Month => chrono::Duration::days(30),
        }
    }

    /// The start of the range, in the form Graph's filters take.
    fn since(self) -> String {
        (chrono::Utc::now() - self.duration())
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }
}

/// What to read, beyond which log.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LogQuery {
    pub range: Range,
    /// A sign-in name: whose sign-ins, or who made the change. Empty for
    /// everyone.
    pub user: String,
    pub failures_only: bool,
}

impl LogQuery {
    /// What was asked for, in words, to put above what came back.
    pub fn describe(&self) -> String {
        let mut text = self.range.label().to_owned();
        if !self.user.trim().is_empty() {
            text.push_str(&format!(", {}", self.user.trim()));
        }
        if self.failures_only {
            text.push_str(", failures only");
        }
        text
    }
}

/// One load's worth of entries.
#[derive(Clone, Debug, Default)]
pub struct Entries<T> {
    pub rows: Vec<T>,
    /// More matched than [`MAX_ENTRIES`]; only the newest are here.
    pub truncated: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SignIn {
    pub id: String,
    pub created_date_time: Option<String>,
    pub user_display_name: Option<String>,
    pub user_principal_name: Option<String>,
    pub user_id: Option<String>,
    pub app_display_name: Option<String>,
    pub app_id: Option<String>,
    pub resource_display_name: Option<String>,
    pub ip_address: Option<String>,
    pub client_app_used: Option<String>,
    pub correlation_id: Option<String>,
    pub conditional_access_status: Option<String>,
    pub is_interactive: Option<bool>,
    pub risk_level_during_sign_in: Option<String>,
    pub status: SignInStatus,
    pub device_detail: DeviceDetail,
    pub location: SignInLocation,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SignInStatus {
    pub error_code: Option<i64>,
    pub failure_reason: Option<String>,
    pub additional_details: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceDetail {
    pub device_id: Option<String>,
    pub display_name: Option<String>,
    pub operating_system: Option<String>,
    pub browser: Option<String>,
    pub is_compliant: Option<bool>,
    pub is_managed: Option<bool>,
    pub trust_type: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SignInLocation {
    pub city: Option<String>,
    pub state: Option<String>,
    pub country_or_region: Option<String>,
}

impl SignIn {
    pub fn succeeded(&self) -> bool {
        self.status.error_code.unwrap_or(0) == 0
    }

    /// "Success", or why not.
    pub fn outcome(&self) -> String {
        if self.succeeded() {
            return "Success".to_owned();
        }
        let code = self.status.error_code.unwrap_or_default();
        match self.status.failure_reason.as_deref().map(str::trim) {
            Some(reason) if !reason.is_empty() => format!("{reason} ({code})"),
            _ => format!("Failed ({code})"),
        }
    }

    pub fn user(&self) -> &str {
        self.user_principal_name
            .as_deref()
            .or(self.user_display_name.as_deref())
            .unwrap_or("")
    }

    pub fn app(&self) -> &str {
        self.app_display_name.as_deref().unwrap_or("")
    }

    /// `Leeds, England, GB`, leaving out whatever is missing.
    pub fn place(&self) -> String {
        let l = &self.location;
        [&l.city, &l.state, &l.country_or_region]
            .into_iter()
            .filter_map(|p| p.as_deref().map(str::trim).filter(|p| !p.is_empty()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DirectoryAudit {
    pub id: String,
    pub activity_date_time: Option<String>,
    pub activity_display_name: Option<String>,
    pub category: Option<String>,
    pub logged_by_service: Option<String>,
    pub operation_type: Option<String>,
    pub result: Option<String>,
    pub result_reason: Option<String>,
    pub correlation_id: Option<String>,
    pub initiated_by: InitiatedBy,
    pub target_resources: Vec<TargetResource>,
    pub additional_details: Vec<KeyValue>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InitiatedBy {
    pub user: Option<AuditUser>,
    pub app: Option<AuditApp>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AuditUser {
    pub id: Option<String>,
    pub display_name: Option<String>,
    pub user_principal_name: Option<String>,
    pub ip_address: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AuditApp {
    pub app_id: Option<String>,
    pub display_name: Option<String>,
    pub service_principal_name: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TargetResource {
    pub id: Option<String>,
    pub display_name: Option<String>,
    /// Graph has sent this both as `type` and as `Type`.
    #[serde(rename = "type", alias = "Type")]
    pub kind: Option<String>,
    pub user_principal_name: Option<String>,
    pub modified_properties: Vec<ModifiedProperty>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModifiedProperty {
    pub display_name: Option<String>,
    pub old_value: Option<String>,
    pub new_value: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyValue {
    pub key: Option<String>,
    pub value: Option<String>,
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

impl DirectoryAudit {
    pub fn succeeded(&self) -> bool {
        self.result.as_deref() == Some("success")
    }

    pub fn activity(&self) -> &str {
        self.activity_display_name.as_deref().unwrap_or("")
    }

    /// Who made the change: a user's sign-in name, or an app's name.
    pub fn initiator(&self) -> String {
        let by = &self.initiated_by;
        if let Some(user) = &by.user
            && let Some(name) = non_empty(user.user_principal_name.as_deref())
                .or(non_empty(user.display_name.as_deref()))
        {
            return name.to_owned();
        }
        if let Some(app) = &by.app
            && let Some(name) = non_empty(app.display_name.as_deref())
                .or(non_empty(app.service_principal_name.as_deref()))
        {
            return format!("{name} (app)");
        }
        String::new()
    }

    /// What it was done to: the first target's name, and how many more.
    pub fn target(&self) -> String {
        let mut names = self.target_resources.iter().filter_map(TargetResource::name);
        match (names.next(), names.count()) {
            (None, _) => String::new(),
            (Some(first), 0) => first.to_owned(),
            (Some(first), more) => format!("{first} +{more}"),
        }
    }
}

impl TargetResource {
    pub fn name(&self) -> Option<&str> {
        non_empty(self.user_principal_name.as_deref()).or(non_empty(self.display_name.as_deref()))
    }
}

/// A log entry's time with seconds, which a list of sign-ins needs and a
/// list of users does not: `2026-10-02 14:03:59`, in UTC.
pub fn log_time(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match chrono::DateTime::parse_from_rfc3339(value) {
        Ok(t) => t
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        Err(_) => value.to_owned(),
    }
}

/// The sign-in log's filter for a query.
fn sign_in_filter(query: &LogQuery, since: &str) -> String {
    let mut filter = format!("createdDateTime ge {since}");
    let user = query.user.trim();
    if !user.is_empty() {
        filter.push_str(&format!(" and userPrincipalName eq '{}'", odata_quote(user)));
    }
    if query.failures_only {
        filter.push_str(" and status/errorCode ne 0");
    }
    filter
}

/// The audit log's filter for a query. Failures are picked out after
/// reading rather than here: the documented filters for this log do not
/// include its result.
fn audit_filter(query: &LogQuery, since: &str) -> String {
    let mut filter = format!("activityDateTime ge {since}");
    let user = query.user.trim();
    if !user.is_empty() {
        filter.push_str(&format!(
            " and initiatedBy/user/userPrincipalName eq '{}'",
            odata_quote(user)
        ));
    }
    filter
}

impl Graph {
    pub fn sign_ins(&self, query: &LogQuery) -> Result<Entries<SignIn>> {
        let filter = sign_in_filter(query, &query.range.since());
        // Newest first is the sign-in log's own order.
        let (rows, truncated) = self.get_up_to(
            &format!("/auditLogs/signIns?$filter={}&$top=1000", encode_query(&filter)),
            MAX_ENTRIES,
        )?;
        Ok(Entries { rows, truncated })
    }

    pub fn directory_audits(&self, query: &LogQuery) -> Result<Entries<DirectoryAudit>> {
        let filter = audit_filter(query, &query.range.since());
        let (mut rows, truncated) = self.get_up_to::<DirectoryAudit>(
            &format!(
                "/auditLogs/directoryAudits?$filter={}&$orderby={}",
                encode_query(&filter),
                encode_query("activityDateTime desc")
            ),
            MAX_ENTRIES,
        )?;
        if query.failures_only {
            rows.retain(|a| !a.succeeded());
        }
        Ok(Entries { rows, truncated })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_follow_the_query() {
        let mut query = LogQuery::default();
        let since = "2026-10-01T00:00:00Z";
        assert_eq!(sign_in_filter(&query, since), "createdDateTime ge 2026-10-01T00:00:00Z");
        query.user = " jo.o'brien@contoso.com ".into();
        query.failures_only = true;
        assert_eq!(
            sign_in_filter(&query, since),
            "createdDateTime ge 2026-10-01T00:00:00Z and userPrincipalName eq 'jo.o''brien@contoso.com' and status/errorCode ne 0"
        );
        assert_eq!(
            audit_filter(&query, since),
            "activityDateTime ge 2026-10-01T00:00:00Z and initiatedBy/user/userPrincipalName eq 'jo.o''brien@contoso.com'"
        );
        assert_eq!(
            query.describe(),
            "Last 24 hours, jo.o'brien@contoso.com, failures only"
        );
    }

    #[test]
    fn sign_ins_read_from_graphs_example() {
        let json = r#"{
            "id": "66ea54eb", "createdDateTime": "2023-12-01T16:03:35Z",
            "userPrincipalName": "testaccount1@contoso.com", "appDisplayName": "Graph explorer",
            "ipAddress": "131.107.159.37", "isInteractive": true,
            "status": {"errorCode": 50126, "failureReason": "Invalid username or password.", "additionalDetails": null},
            "deviceDetail": {"deviceId": "", "operatingSystem": "Windows 10", "browser": "Edge 80.0.361", "isCompliant": null},
            "location": {"city": "Redmond", "state": "Washington", "countryOrRegion": "US", "geoCoordinates": {"latitude": 47.6}}
        }"#;
        let sign_in: SignIn = serde_json::from_str(json).unwrap();
        assert!(!sign_in.succeeded());
        assert_eq!(sign_in.outcome(), "Invalid username or password. (50126)");
        assert_eq!(sign_in.place(), "Redmond, Washington, US");
        assert_eq!(log_time(sign_in.created_date_time.as_deref()), "2023-12-01 16:03:35");
    }

    #[test]
    fn audits_read_from_graphs_example_whichever_case_type_is_in() {
        let json = r#"{
            "id": "id", "category": "UserManagement", "result": "success",
            "activityDisplayName": "Add member to group", "activityDateTime": "2018-01-09T21:20:02.7215374Z",
            "initiatedBy": {"user": {"id": "7283", "displayName": "Audry Oliver", "userPrincipalName": "bob@wingtiptoysonline.com", "ipAddress": "127.0.0.1"}, "app": null},
            "targetResources": [
                {"id": "ef7e", "displayName": "Example.com", "Type": "Group", "modifiedProperties": [{"displayName": "Action Client Name", "oldValue": null, "newValue": "DirectorySync"}]},
                {"id": "1f0e", "displayName": null, "type": "User", "modifiedProperties": [], "userPrincipalName": "bob@contoso.com"}
            ],
            "additionalDetails": [{"key": "Additional Detail Name", "value": "Additional Detail Value"}]
        }"#;
        let audit: DirectoryAudit = serde_json::from_str(json).unwrap();
        assert!(audit.succeeded());
        assert_eq!(audit.initiator(), "bob@wingtiptoysonline.com");
        assert_eq!(audit.target(), "Example.com +1");
        assert_eq!(audit.target_resources[0].kind.as_deref(), Some("Group"));
        assert_eq!(audit.target_resources[1].kind.as_deref(), Some("User"));

        let by_app: DirectoryAudit = serde_json::from_str(
            r#"{"id": "x", "initiatedBy": {"user": null, "app": {"displayName": "Intune"}}}"#,
        )
        .unwrap();
        assert_eq!(by_app.initiator(), "Intune (app)");
        assert_eq!(by_app.target(), "");
    }
}
