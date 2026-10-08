//! Users in and out of CSV files, and logs and server packages out.
//!
//! The import reads a header row and matches columns by name, ignoring case,
//! spaces and underscores, so `userPrincipalName`, `User Principal Name` and
//! `user_principal_name` are all the same column. Columns it does not know are
//! ignored — which is what lets a file written by the export go straight back
//! in, `id` column and all.

use std::path::Path;

use crate::graph::logs::{DirectoryAudit, SignIn, log_time};
use crate::graph::models::User;
use crate::graph::users::NewUser;
use crate::servers::Snapshot;

/// The columns the import understands, in the order the template writes them.
pub const IMPORT_COLUMNS: &[&str] = &[
    "displayName",
    "userPrincipalName",
    "password",
    "mailNickname",
    "givenName",
    "surname",
    "jobTitle",
    "department",
    "officeLocation",
    "mobilePhone",
    "usageLocation",
    "accountEnabled",
    "forceChangePasswordNextSignIn",
];

const EXPORT_COLUMNS: &[&str] = &[
    "id",
    "displayName",
    "userPrincipalName",
    "mail",
    "givenName",
    "surname",
    "jobTitle",
    "department",
    "officeLocation",
    "mobilePhone",
    "usageLocation",
    "accountEnabled",
    "userType",
    "createdDateTime",
    "onPremisesSyncEnabled",
];

/// One data row of an import file: the line it came from, and either a user
/// ready to create or why it is not.
#[derive(Debug)]
pub struct ImportRow {
    pub line: u64,
    pub user: Result<NewUser, String>,
    /// The password was blank and one was generated, so it has to be handed
    /// back in the results file or nobody will ever know it.
    pub generated_password: bool,
}

fn normalise(header: &str) -> String {
    header
        .trim()
        .trim_start_matches('\u{feff}')
        .chars()
        .filter(|c| !matches!(c, ' ' | '_' | '-'))
        .collect::<String>()
        .to_lowercase()
}

/// Whether a spreadsheet would read this cell as a formula rather than text.
/// A phone number such as `+44 20 7946 0000` starts with `+` too, but is only
/// digits and punctuation, and nothing a spreadsheet can run.
fn looks_like_formula(cell: &str) -> bool {
    let mut chars = cell.chars();
    match chars.next() {
        Some('=' | '@' | '\t' | '\r') => true,
        Some('+' | '-') => !chars.all(|c| c.is_ascii_digit() || " ().-".contains(c)),
        _ => false,
    }
}

/// A value from the directory, made safe to open in a spreadsheet.
///
/// Anyone in the tenant can choose their own job title or department, and a
/// cell beginning `=` runs as a formula when the export is opened in Excel or
/// LibreOffice — CSV injection. A leading apostrophe makes it plain text;
/// [`from_cell`] takes it off again on import, so an exported file still goes
/// straight back in.
fn to_cell(value: &str) -> String {
    if looks_like_formula(value) {
        format!("'{value}")
    } else {
        value.to_owned()
    }
}

/// The inverse of [`to_cell`].
fn from_cell(cell: &str) -> &str {
    match cell.strip_prefix('\'') {
        Some(rest) if looks_like_formula(rest) => rest,
        _ => cell,
    }
}

fn parse_bool(text: &str, default: bool) -> Result<bool, String> {
    match text.trim().to_lowercase().as_str() {
        "" => Ok(default),
        "true" | "yes" | "y" | "1" | "enabled" => Ok(true),
        "false" | "no" | "n" | "0" | "disabled" => Ok(false),
        other => Err(format!("\"{other}\" is not true or false")),
    }
}

pub fn read_import(path: &Path) -> Result<Vec<ImportRow>, String> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .trim(csv::Trim::All)
        .from_path(path)
        .map_err(|e| format!("Could not open {}: {e}", path.display()))?;

    let headers = reader
        .headers()
        .map_err(|e| format!("Could not read the header row: {e}"))?
        .clone();
    let index: Vec<Option<usize>> = IMPORT_COLUMNS
        .iter()
        .map(|wanted| {
            let wanted = wanted.to_lowercase();
            headers.iter().position(|h| normalise(h) == wanted)
        })
        .collect();
    for required in ["displayName", "userPrincipalName"] {
        let at = IMPORT_COLUMNS.iter().position(|c| *c == required).unwrap();
        if index[at].is_none() {
            return Err(format!(
                "The file has no {required} column. Save the template to see the expected columns."
            ));
        }
    }

    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(|e| format!("Could not read the file: {e}"))?;
        let line = record.position().map_or(0, |p| p.line());
        if record.iter().all(str::is_empty) {
            continue;
        }
        let raw = |name: &str| -> String {
            let at = IMPORT_COLUMNS.iter().position(|c| *c == name).unwrap();
            index[at]
                .and_then(|i| record.get(i))
                .unwrap_or_default()
                .to_owned()
        };
        let field = |name: &str| from_cell(&raw(name)).to_owned();

        let mut generated_password = false;
        let user = (|| {
            // Taken exactly as written: the export never writes passwords, so
            // a leading apostrophe here is part of the password.
            let mut password = raw("password");
            if password.is_empty() {
                password = crate::graph::users::generate_password();
                generated_password = true;
            }
            let user = NewUser {
                display_name: field("displayName"),
                user_principal_name: field("userPrincipalName"),
                mail_nickname: field("mailNickname"),
                password,
                given_name: field("givenName"),
                surname: field("surname"),
                job_title: field("jobTitle"),
                department: field("department"),
                office_location: field("officeLocation"),
                mobile_phone: field("mobilePhone"),
                usage_location: field("usageLocation"),
                account_enabled: parse_bool(&field("accountEnabled"), true)
                    .map_err(|e| format!("accountEnabled: {e}"))?,
                force_change_password: parse_bool(&field("forceChangePasswordNextSignIn"), true)
                    .map_err(|e| format!("forceChangePasswordNextSignIn: {e}"))?,
            };
            user.validate()?;
            Ok(user)
        })();
        if let Err(err) = &user {
            log::debug!("import line {line} skipped: {err}");
        }
        rows.push(ImportRow {
            line,
            user,
            generated_password,
        });
    }
    log::info!(
        "read {} rows from {}, {} ready to create",
        rows.len(),
        path.display(),
        rows.iter().filter(|r| r.user.is_ok()).count()
    );
    Ok(rows)
}

pub fn write_template(path: &Path) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path).map_err(|e| e.to_string())?;
    writer.write_record(IMPORT_COLUMNS).map_err(|e| e.to_string())?;
    writer
        .write_record([
            "Jo Bloggs",
            "jo.bloggs@contoso.com",
            "",
            "",
            "Jo",
            "Bloggs",
            "Analyst",
            "Finance",
            "London",
            "",
            "GB",
            "true",
            "true",
        ])
        .map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

pub fn write_users(path: &Path, users: &[&User]) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path).map_err(|e| e.to_string())?;
    writer.write_record(EXPORT_COLUMNS).map_err(|e| e.to_string())?;
    let s = |v: &Option<String>| to_cell(v.as_deref().unwrap_or_default());
    let b = |v: Option<bool>| v.map(|b| b.to_string()).unwrap_or_default();
    for u in users {
        writer
            .write_record([
                to_cell(&u.id),
                s(&u.display_name),
                s(&u.user_principal_name),
                s(&u.mail),
                s(&u.given_name),
                s(&u.surname),
                s(&u.job_title),
                s(&u.department),
                s(&u.office_location),
                s(&u.mobile_phone),
                s(&u.usage_location),
                b(u.account_enabled),
                s(&u.user_type),
                s(&u.created_date_time),
                b(u.on_premises_sync_enabled),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())
}

/// Every package on a server, one per row, with the update waiting for it.
/// The server's own details are repeated on each row, so the files from
/// several servers can be pasted together and still be told apart.
pub fn write_packages(path: &Path, snapshot: &Snapshot) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path).map_err(|e| e.to_string())?;
    writer
        .write_record([
            "address",
            "hostname",
            "os",
            "kernel",
            "taken",
            "package",
            "version",
            "architecture",
            "updateAvailable",
            "securityUpdate",
        ])
        .map_err(|e| e.to_string())?;
    for p in &snapshot.packages {
        let upgrade = snapshot.upgrade_for(&p.name);
        writer
            .write_record([
                to_cell(&snapshot.address),
                to_cell(&snapshot.hostname),
                to_cell(&snapshot.os),
                to_cell(&snapshot.kernel),
                snapshot.taken.clone(),
                to_cell(&p.name),
                to_cell(&p.version),
                to_cell(&p.architecture),
                to_cell(upgrade.map_or("", |u| u.available.as_str())),
                upgrade.is_some_and(|u| u.security).to_string(),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())
}

pub fn write_sign_ins(path: &Path, sign_ins: &[&SignIn]) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path).map_err(|e| e.to_string())?;
    writer
        .write_record([
            "time", "userPrincipalName", "userDisplayName", "app", "resource", "result",
            "errorCode", "failureReason", "ipAddress", "location", "clientApp", "interactive",
            "conditionalAccess", "operatingSystem", "browser", "deviceName", "correlationId", "id",
        ])
        .map_err(|e| e.to_string())?;
    let s = |v: &Option<String>| to_cell(v.as_deref().unwrap_or_default());
    for i in sign_ins {
        let d = &i.device_detail;
        writer
            .write_record([
                log_time(i.created_date_time.as_deref()),
                s(&i.user_principal_name),
                s(&i.user_display_name),
                s(&i.app_display_name),
                s(&i.resource_display_name),
                (if i.succeeded() { "success" } else { "failure" }).to_owned(),
                i.status.error_code.map(|c| c.to_string()).unwrap_or_default(),
                s(&i.status.failure_reason),
                s(&i.ip_address),
                to_cell(&i.place()),
                s(&i.client_app_used),
                i.is_interactive.map(|b| b.to_string()).unwrap_or_default(),
                s(&i.conditional_access_status),
                s(&d.operating_system),
                s(&d.browser),
                s(&d.display_name),
                s(&i.correlation_id),
                to_cell(&i.id),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())
}

/// One row per audit entry. The targets and the properties changed are
/// joined into one cell each, `name: old -> new` separated by `; `.
pub fn write_audits(path: &Path, audits: &[&DirectoryAudit]) -> Result<(), String> {
    let mut writer = csv::Writer::from_path(path).map_err(|e| e.to_string())?;
    writer
        .write_record([
            "time", "activity", "category", "service", "result", "resultReason", "initiatedBy",
            "initiatorIpAddress", "targets", "changes", "correlationId", "id",
        ])
        .map_err(|e| e.to_string())?;
    let s = |v: &Option<String>| to_cell(v.as_deref().unwrap_or_default());
    for a in audits {
        let targets = a
            .target_resources
            .iter()
            .filter_map(|t| t.name())
            .collect::<Vec<_>>()
            .join("; ");
        let changes = a
            .target_resources
            .iter()
            .flat_map(|t| &t.modified_properties)
            .map(|p| {
                format!(
                    "{}: {} -> {}",
                    p.display_name.as_deref().unwrap_or("?"),
                    p.old_value.as_deref().unwrap_or(""),
                    p.new_value.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        let ip = a
            .initiated_by
            .user
            .as_ref()
            .and_then(|u| u.ip_address.clone());
        writer
            .write_record([
                log_time(a.activity_date_time.as_deref()),
                s(&a.activity_display_name),
                s(&a.category),
                s(&a.logged_by_service),
                s(&a.result),
                s(&a.result_reason),
                to_cell(&a.initiator()),
                s(&ip),
                to_cell(&targets),
                to_cell(&changes),
                s(&a.correlation_id),
                to_cell(&a.id),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())
}

/// How one import row went, for the results file.
#[derive(Clone, Debug)]
pub struct ImportResult {
    pub line: u64,
    pub user_principal_name: String,
    pub outcome: Result<String, String>,
    /// Only when the password was generated here.
    pub password: Option<String>,
}

pub fn write_results(path: &Path, results: &[ImportResult]) -> Result<(), String> {
    // The file can hold passwords, so it is private from the moment it exists
    // rather than narrowed once the passwords are already in it.
    let file = crate::config::create_private(path).map_err(|e| e.to_string())?;
    let mut writer = csv::Writer::from_writer(file);
    writer
        .write_record(["line", "userPrincipalName", "result", "id", "error", "temporaryPassword"])
        .map_err(|e| e.to_string())?;
    for r in results {
        let (result, id, error) = match &r.outcome {
            Ok(id) => ("created", id.as_str(), ""),
            Err(e) => ("failed", "", e.as_str()),
        };
        writer
            .write_record([
                r.line.to_string().as_str(),
                &to_cell(&r.user_principal_name),
                result,
                &to_cell(id),
                &to_cell(error),
                // Generated passwords can begin with `-` or `+`. They are
                // left exactly as they are: an apostrophe here would be
                // copied along with them.
                r.password.as_deref().unwrap_or(""),
            ])
            .map_err(|e| e.to_string())?;
    }
    writer.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("gcm-test-{}-{name}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn headers_match_loosely_and_unknown_columns_are_ignored() {
        let path = temp(
            "loose.csv",
            "\u{feff}ID,Display Name,user_principal_name,Password,Usage Location,Account Enabled\n\
             x,Jo Bloggs,jo@contoso.com,Secret#123,gb,no\n\
             ,,,,,\n\
             y,Bad Row,not-an-upn,Secret#123,,\n",
        );
        let rows = read_import(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(rows.len(), 2);
        let jo = rows[0].user.as_ref().unwrap();
        assert_eq!(jo.display_name, "Jo Bloggs");
        assert_eq!(jo.user_principal_name, "jo@contoso.com");
        assert!(!jo.account_enabled);
        assert!(jo.force_change_password);
        assert!(!rows[0].generated_password);
        assert!(rows[1].user.is_err());
    }

    #[test]
    fn a_blank_password_is_generated() {
        let path = temp("blank.csv", "displayName,userPrincipalName\nJo,jo@contoso.com\n");
        let rows = read_import(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(rows[0].generated_password);
        assert_eq!(rows[0].user.as_ref().unwrap().password.len(), 16);
    }

    #[test]
    fn a_file_without_the_required_columns_is_refused() {
        let path = temp("nocols.csv", "name,email\nJo,jo@contoso.com\n");
        let err = read_import(&path).unwrap_err();
        std::fs::remove_file(&path).ok();
        assert!(err.contains("displayName"), "{err}");
    }

    #[test]
    fn formulas_are_neutralised_and_survive_a_round_trip() {
        assert_eq!(to_cell("=HYPERLINK(\"http://x\")"), "'=HYPERLINK(\"http://x\")");
        assert_eq!(to_cell("@SUM(A1)"), "'@SUM(A1)");
        assert_eq!(to_cell("+cmd|' /C calc'!A0"), "'+cmd|' /C calc'!A0");
        assert_eq!(to_cell("+44 (20) 7946-0000"), "+44 (20) 7946-0000");
        assert_eq!(to_cell("Finance"), "Finance");
        for value in ["=1+1", "-2+3", "Finance", "+44 20 7946 0000", "'quoted"] {
            assert_eq!(from_cell(&to_cell(value)), value);
        }

        let path = std::env::temp_dir().join(format!("gcm-test-{}-formula.csv", std::process::id()));
        let user = User {
            id: "1".into(),
            display_name: Some("Jo".into()),
            user_principal_name: Some("jo@contoso.com".into()),
            job_title: Some("=2+3".into()),
            ..Default::default()
        };
        write_users(&path, &[&user]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let rows = read_import(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(text.contains("'=2+3"), "{text}");
        assert_eq!(rows[0].user.as_ref().unwrap().job_title, "=2+3");
    }

    #[test]
    fn log_exports_neutralise_formulas_too() {
        let path = std::env::temp_dir().join(format!("gcm-test-{}-logs.csv", std::process::id()));
        let sign_in = SignIn {
            id: "1".into(),
            user_display_name: Some("=HYPERLINK(\"http://x\")".into()),
            ..Default::default()
        };
        write_sign_ins(&path, &[&sign_in]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("'=HYPERLINK"), "{text}");
        assert!(text.contains(",success,"), "{text}");

        let audit: DirectoryAudit = serde_json::from_str(
            r#"{"id": "a", "activityDisplayName": "Update user",
                "targetResources": [{"userPrincipalName": "jo@contoso.com",
                  "modifiedProperties": [{"displayName": "JobTitle", "oldValue": "[\"A\"]", "newValue": "[\"=1+1\"]"}]}]}"#,
        )
        .unwrap();
        write_audits(&path, &[&audit]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(text.contains("jo@contoso.com"), "{text}");
        assert!(text.contains(r#"JobTitle: [""A""] -> [""=1+1""]"#), "{text}");
    }

    #[test]
    fn the_template_reads_back_cleanly() {
        let path = std::env::temp_dir().join(format!("gcm-test-{}-template.csv", std::process::id()));
        write_template(&path).unwrap();
        let rows = read_import(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].user.is_ok(), "{:?}", rows[0].user);
    }
}
