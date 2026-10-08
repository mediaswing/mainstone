//! A copy of the directory in a MariaDB (or MySQL) server.
//!
//! The export reads fresh from Graph rather than from what the panes happen to
//! have loaded, so what lands in the database is the tenant as it is now. The
//! tables are made if they are not there, every row is keyed on the tenant and
//! the object ID — so several tenants can share one database — and a re-run
//! updates rows in place. "Mirror" additionally removes rows for objects that
//! have since gone from the tenant. Each table is written in one transaction:
//! a failure part-way leaves it as it was.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use mysql::prelude::Queryable as _;
use mysql::{Conn, OptsBuilder, Params, SslOpts, TxOpts, Value};
use serde::{Deserialize, Serialize};

use crate::graph::Graph;
use crate::graph::mailbox::MailboxUsage;
use crate::graph::models::DeviceRow;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MariaDbSettings {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    /// Never written to the config file; see [`crate::secrets`].
    #[serde(skip)]
    pub password: String,
    pub use_tls: bool,
    /// Accept a certificate that does not match the hostname.
    pub tls_skip_verify: bool,
    /// An extra certificate authority to trust, for a server issued by a
    /// private CA.
    pub ca_cert_path: String,
}

impl Default for MariaDbSettings {
    fn default() -> Self {
        Self {
            host: "localhost".to_owned(),
            port: 3306,
            database: "gcm".to_owned(),
            username: String::new(),
            password: String::new(),
            use_tls: false,
            tls_skip_verify: false,
            ca_cert_path: String::new(),
        }
    }
}

impl std::fmt::Debug for MariaDbSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MariaDbSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("database", &self.database)
            .field("username", &self.username)
            .field(
                "password",
                &if self.password.is_empty() {
                    "<none>"
                } else {
                    "<hidden>"
                },
            )
            .field("use_tls", &self.use_tls)
            .finish()
    }
}

impl MariaDbSettings {
    pub fn label(&self) -> String {
        format!(
            "{}@{}:{}/{}",
            self.username.trim(),
            self.host.trim(),
            self.port,
            self.database.trim()
        )
    }

    fn opts(&self) -> Result<OptsBuilder, String> {
        if self.host.trim().is_empty() {
            return Err("Enter the server's host name.".into());
        }
        if self.database.trim().is_empty() {
            return Err("Enter a database name.".into());
        }
        if self.username.trim().is_empty() {
            return Err("Enter a user name.".into());
        }
        let ssl = self.use_tls.then(|| {
            let ssl = SslOpts::default().with_danger_skip_domain_validation(self.tls_skip_verify);
            match self.ca_cert_path.trim() {
                "" => ssl,
                ca => ssl.with_root_cert_path(Some(std::path::PathBuf::from(ca))),
            }
        });
        Ok(OptsBuilder::new()
            .ip_or_hostname(Some(self.host.trim()))
            .tcp_port(self.port)
            .db_name(Some(self.database.trim()))
            .user(Some(self.username.trim()))
            .pass(Some(self.password.clone()))
            .tcp_connect_timeout(Some(CONNECT_TIMEOUT))
            .read_timeout(Some(IO_TIMEOUT))
            .write_timeout(Some(IO_TIMEOUT))
            .ssl_opts(ssl))
    }

    pub fn connect(&self) -> Result<Conn, String> {
        let opts = self.opts()?;
        log::debug!(
            "connecting to MariaDB at {} (TLS {}, skip host name check {}, extra CA {})",
            self.label(),
            self.use_tls,
            self.tls_skip_verify,
            !self.ca_cert_path.trim().is_empty()
        );
        let conn = Conn::new(opts).map_err(|e| {
            log::warn!("MariaDB connection to {} failed: {e}", self.label());
            format!("Could not connect to {}: {e}", self.label())
        })?;
        log::debug!("connected to MariaDB at {}", self.label());
        Ok(conn)
    }

    /// Connect, and make sure the tables can be created. What "Test" does.
    pub fn test(&self) -> Result<String, String> {
        let mut conn = self.connect()?;
        create_tables(&mut conn)?;
        let version: Option<String> = conn
            .query_first("SELECT VERSION()")
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "Connected to {} (server {}). The tables are ready.",
            self.label(),
            version.unwrap_or_else(|| "unknown".into())
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Choices {
    pub users: bool,
    pub groups: bool,
    pub members: bool,
    pub devices: bool,
    pub mailboxes: bool,
    /// Delete rows for this tenant that Graph no longer returned.
    pub mirror: bool,
}

impl Default for Choices {
    fn default() -> Self {
        Self {
            users: true,
            groups: true,
            members: true,
            devices: true,
            mailboxes: true,
            mirror: false,
        }
    }
}

/// The schema, also printed in the README. Times are UTC.
pub const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS gcm_users (
        tenant_id VARCHAR(64) NOT NULL,
        id CHAR(36) NOT NULL,
        display_name VARCHAR(256),
        user_principal_name VARCHAR(320),
        mail VARCHAR(320),
        given_name VARCHAR(128),
        surname VARCHAR(128),
        job_title VARCHAR(128),
        department VARCHAR(128),
        office_location VARCHAR(128),
        mobile_phone VARCHAR(64),
        usage_location CHAR(2),
        account_enabled BOOLEAN,
        user_type VARCHAR(32),
        on_premises_sync_enabled BOOLEAN,
        created_at DATETIME,
        exported_at DATETIME NOT NULL,
        PRIMARY KEY (tenant_id, id),
        KEY idx_upn (user_principal_name)
    ) DEFAULT CHARSET=utf8mb4",
    "CREATE TABLE IF NOT EXISTS gcm_groups (
        tenant_id VARCHAR(64) NOT NULL,
        id CHAR(36) NOT NULL,
        display_name VARCHAR(256),
        description TEXT,
        mail VARCHAR(320),
        mail_nickname VARCHAR(64),
        group_kind VARCHAR(32),
        security_enabled BOOLEAN,
        mail_enabled BOOLEAN,
        membership_rule TEXT,
        created_at DATETIME,
        exported_at DATETIME NOT NULL,
        PRIMARY KEY (tenant_id, id)
    ) DEFAULT CHARSET=utf8mb4",
    "CREATE TABLE IF NOT EXISTS gcm_group_members (
        tenant_id VARCHAR(64) NOT NULL,
        group_id CHAR(36) NOT NULL,
        member_id CHAR(36) NOT NULL,
        member_type VARCHAR(64),
        member_name VARCHAR(256),
        member_upn VARCHAR(320),
        exported_at DATETIME NOT NULL,
        PRIMARY KEY (tenant_id, group_id, member_id),
        KEY idx_member (member_id)
    ) DEFAULT CHARSET=utf8mb4",
    "CREATE TABLE IF NOT EXISTS gcm_devices (
        tenant_id VARCHAR(64) NOT NULL,
        row_key VARCHAR(80) NOT NULL,
        entra_object_id CHAR(36),
        entra_device_id CHAR(36),
        intune_id CHAR(36),
        display_name VARCHAR(256),
        operating_system VARCHAR(64),
        os_version VARCHAR(64),
        trust_type VARCHAR(32),
        account_enabled BOOLEAN,
        compliance_state VARCHAR(32),
        management_agent VARCHAR(64),
        owner_type VARCHAR(32),
        user_principal_name VARCHAR(320),
        serial_number VARCHAR(128),
        manufacturer VARCHAR(128),
        model VARCHAR(128),
        registered_at DATETIME,
        enrolled_at DATETIME,
        last_sign_in_at DATETIME,
        last_sync_at DATETIME,
        exported_at DATETIME NOT NULL,
        PRIMARY KEY (tenant_id, row_key)
    ) DEFAULT CHARSET=utf8mb4",
    "CREATE TABLE IF NOT EXISTS gcm_mailboxes (
        tenant_id VARCHAR(64) NOT NULL,
        user_principal_name VARCHAR(320) NOT NULL,
        display_name VARCHAR(256),
        recipient_type VARCHAR(32),
        storage_used_bytes BIGINT UNSIGNED,
        item_count BIGINT UNSIGNED,
        issue_warning_quota_bytes BIGINT UNSIGNED,
        prohibit_send_quota_bytes BIGINT UNSIGNED,
        prohibit_send_receive_quota_bytes BIGINT UNSIGNED,
        has_archive BOOLEAN,
        last_activity_date DATE,
        report_date DATE,
        exported_at DATETIME NOT NULL,
        PRIMARY KEY (tenant_id, user_principal_name)
    ) DEFAULT CHARSET=utf8mb4",
];

fn create_tables(conn: &mut Conn) -> Result<(), String> {
    for statement in SCHEMA {
        conn.query_drop(statement)
            .map_err(|e| format!("Could not create the tables: {e}"))?;
    }
    Ok(())
}

/// A Graph timestamp as a MariaDB `DATETIME` literal, or NULL for none (and
/// for Intune's year-1 "never").
fn datetime(value: Option<&str>) -> Value {
    value
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .filter(|t| chrono::Datelike::year(t) > 1900)
        .map(|t| {
            Value::from(
                t.with_timezone(&chrono::Utc)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            )
        })
        .unwrap_or(Value::NULL)
}

fn text(value: &Option<String>) -> Value {
    value.clone().map_or(Value::NULL, Value::from)
}

fn flag(value: Option<bool>) -> Value {
    value.map_or(Value::NULL, Value::from)
}

/// Insert or update every row in one transaction, after optionally clearing
/// the tenant's old rows from the same table.
fn write_table(
    conn: &mut Conn,
    table: &str,
    columns: &[&str],
    key_columns: usize,
    tenant_id: &str,
    rows: Vec<Vec<Value>>,
    mirror: bool,
) -> Result<usize, String> {
    let placeholders = vec!["?"; columns.len()].join(",");
    let updates = columns[key_columns..]
        .iter()
        .map(|c| format!("{c}=VALUES({c})"))
        .collect::<Vec<_>>()
        .join(",");
    let statement = format!(
        "INSERT INTO {table} ({}) VALUES ({placeholders}) ON DUPLICATE KEY UPDATE {updates}",
        columns.join(",")
    );
    let count = rows.len();
    let started = std::time::Instant::now();

    let mut tx = conn
        .start_transaction(TxOpts::default())
        .map_err(|e| e.to_string())?;
    if mirror {
        tx.exec_drop(format!("DELETE FROM {table} WHERE tenant_id = ?"), (tenant_id,))
            .map_err(|e| format!("Could not clear {table}: {e}"))?;
    }
    tx.exec_batch(&statement, rows.into_iter().map(Params::Positional))
        .map_err(|e| format!("Could not write {table}: {e}"))?;
    tx.commit().map_err(|e| e.to_string())?;
    log::debug!(
        "{table}: wrote {count} rows in {} ms (mirror {mirror})",
        started.elapsed().as_millis()
    );
    Ok(count)
}

/// Run the export. `tenant` is what every row is filed under: the tenant's
/// GUID, so that it is the same however the tenant was typed in at sign-in.
/// `progress` is a line for the pane to show while it works.
pub fn run(
    graph: &Graph,
    tenant: &str,
    settings: &MariaDbSettings,
    choices: Choices,
    progress: &Arc<Mutex<String>>,
) -> Result<String, String> {
    log::info!("export to {} started: {choices:?}", settings.label());
    let say = |text: &str| {
        log::debug!("export: {text}");
        if let Ok(mut p) = progress.lock() {
            *p = text.to_owned();
        }
    };

    say("Connecting to MariaDB…");
    let mut conn = settings.connect()?;
    create_tables(&mut conn)?;

    let tenant = tenant.trim().to_lowercase();
    let now = Value::from(
        chrono::Utc::now()
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
    );
    let mut summary = Vec::new();

    if choices.users {
        say("Reading users from Entra ID…");
        let users = graph.list_users()?;
        say(&format!("Writing {} users…", users.len()));
        let rows = users
            .iter()
            .map(|u| {
                vec![
                    Value::from(tenant.as_str()),
                    Value::from(u.id.as_str()),
                    text(&u.display_name),
                    text(&u.user_principal_name),
                    text(&u.mail),
                    text(&u.given_name),
                    text(&u.surname),
                    text(&u.job_title),
                    text(&u.department),
                    text(&u.office_location),
                    text(&u.mobile_phone),
                    text(&u.usage_location),
                    flag(u.account_enabled),
                    text(&u.user_type),
                    flag(u.on_premises_sync_enabled),
                    datetime(u.created_date_time.as_deref()),
                    now.clone(),
                ]
            })
            .collect();
        let n = write_table(
            &mut conn,
            "gcm_users",
            &[
                "tenant_id",
                "id",
                "display_name",
                "user_principal_name",
                "mail",
                "given_name",
                "surname",
                "job_title",
                "department",
                "office_location",
                "mobile_phone",
                "usage_location",
                "account_enabled",
                "user_type",
                "on_premises_sync_enabled",
                "created_at",
                "exported_at",
            ],
            2,
            &tenant,
            rows,
            choices.mirror,
        )?;
        summary.push(format!("{n} users"));
    }

    let groups = if choices.groups || choices.members {
        say("Reading groups from Entra ID…");
        graph.list_groups()?
    } else {
        Vec::new()
    };

    if choices.groups {
        say(&format!("Writing {} groups…", groups.len()));
        let rows = groups
            .iter()
            .map(|g| {
                vec![
                    Value::from(tenant.as_str()),
                    Value::from(g.id.as_str()),
                    text(&g.display_name),
                    text(&g.description),
                    text(&g.mail),
                    text(&g.mail_nickname),
                    Value::from(g.kind()),
                    flag(g.security_enabled),
                    flag(g.mail_enabled),
                    text(&g.membership_rule),
                    datetime(g.created_date_time.as_deref()),
                    now.clone(),
                ]
            })
            .collect();
        let n = write_table(
            &mut conn,
            "gcm_groups",
            &[
                "tenant_id",
                "id",
                "display_name",
                "description",
                "mail",
                "mail_nickname",
                "group_kind",
                "security_enabled",
                "mail_enabled",
                "membership_rule",
                "created_at",
                "exported_at",
            ],
            2,
            &tenant,
            rows,
            choices.mirror,
        )?;
        summary.push(format!("{n} groups"));
    }

    let mut warnings = Vec::new();
    if choices.members {
        let mut rows = Vec::new();
        let mut gone = 0;
        for (i, group) in groups.iter().enumerate() {
            say(&format!(
                "Reading members of {} ({} of {})…",
                group.name(),
                i + 1,
                groups.len()
            ));
            // A group deleted since the list was read has no members to
            // copy, and is no reason to abandon all the others.
            let Some(members) = graph.group_members_if_found(&group.id)? else {
                log::info!("group {} ({}) has gone; skipping its members", group.name(), group.id);
                gone += 1;
                continue;
            };
            for m in members {
                rows.push(vec![
                    Value::from(tenant.as_str()),
                    Value::from(group.id.as_str()),
                    Value::from(m.id.as_str()),
                    Value::from(m.kind()),
                    text(&m.display_name),
                    text(&m.user_principal_name),
                    now.clone(),
                ]);
            }
        }
        if gone > 0 {
            warnings.push(format!(
                "{gone} groups were deleted during the export, so their members were left out."
            ));
        }
        say(&format!("Writing {} memberships…", rows.len()));
        let n = write_table(
            &mut conn,
            "gcm_group_members",
            &[
                "tenant_id",
                "group_id",
                "member_id",
                "member_type",
                "member_name",
                "member_upn",
                "exported_at",
            ],
            3,
            &tenant,
            rows,
            choices.mirror,
        )?;
        summary.push(format!("{n} memberships"));
    }

    if choices.devices {
        say("Reading devices from Entra ID and Intune…");
        let list = graph.list_devices()?;
        if let Some(e) = list.intune_error {
            warnings.push(format!("Intune devices were left out: {e}"));
        }
        say(&format!("Writing {} devices…", list.rows.len()));
        let rows = list.rows.iter().map(|r| device_values(&tenant, r, &now)).collect();
        let n = write_table(
            &mut conn,
            "gcm_devices",
            &[
                "tenant_id",
                "row_key",
                "entra_object_id",
                "entra_device_id",
                "intune_id",
                "display_name",
                "operating_system",
                "os_version",
                "trust_type",
                "account_enabled",
                "compliance_state",
                "management_agent",
                "owner_type",
                "user_principal_name",
                "serial_number",
                "manufacturer",
                "model",
                "registered_at",
                "enrolled_at",
                "last_sign_in_at",
                "last_sync_at",
                "exported_at",
            ],
            2,
            &tenant,
            rows,
            choices.mirror,
        )?;
        summary.push(format!("{n} devices"));
    }

    if choices.mailboxes {
        say("Reading the mailbox usage report…");
        // Optional, like the permission it needs: a tenant without Exchange,
        // or without Reports.Read.All, still exports everything else.
        match graph.mailbox_usage() {
            Err(err) => warnings.push(format!("Mailboxes were left out: {err}")),
            Ok(report) if report.concealed => warnings.push(format!(
                "Mailboxes were left out. {}",
                crate::graph::mailbox::CONCEALED_HINT
            )),
            Ok(report) => {
                say(&format!("Writing {} mailboxes…", report.len()));
                let rows = report
                    .mailboxes()
                    .map(|m| mailbox_values(&tenant, m, &report.refresh_date, &now))
                    .collect();
                let n = write_table(
                    &mut conn,
                    "gcm_mailboxes",
                    &[
                        "tenant_id",
                        "user_principal_name",
                        "display_name",
                        "recipient_type",
                        "storage_used_bytes",
                        "item_count",
                        "issue_warning_quota_bytes",
                        "prohibit_send_quota_bytes",
                        "prohibit_send_receive_quota_bytes",
                        "has_archive",
                        "last_activity_date",
                        "report_date",
                        "exported_at",
                    ],
                    2,
                    &tenant,
                    rows,
                    choices.mirror,
                )?;
                summary.push(format!("{n} mailboxes"));
            }
        }
    }

    if summary.is_empty() {
        if !warnings.is_empty() {
            return Err(warnings.join(" "));
        }
        return Err("Choose at least one thing to export.".into());
    }
    let mut message = format!("Exported {} to {}.", summary.join(", "), settings.label());
    for warning in warnings {
        message.push(' ');
        message.push_str(&warning);
    }
    Ok(message)
}

/// A device row is keyed on its Entra object ID when it has one, and on its
/// Intune ID otherwise, prefixed so the two can never collide.
fn device_values(tenant: &str, row: &DeviceRow, now: &Value) -> Vec<Value> {
    let e = row.entra.as_ref();
    let i = row.intune.as_ref();
    let key = match (e, i) {
        (Some(e), _) => format!("entra:{}", e.id),
        (None, Some(i)) => format!("intune:{}", i.id),
        (None, None) => String::new(),
    };
    let os_version = i
        .and_then(|i| i.os_version.clone())
        .or_else(|| e.and_then(|e| e.operating_system_version.clone()));
    let os = i
        .and_then(|i| i.operating_system.clone())
        .or_else(|| e.and_then(|e| e.operating_system.clone()));
    let compliance = Some(row.compliance().to_owned()).filter(|c| !c.is_empty());
    vec![
        Value::from(tenant),
        Value::from(key),
        text(&e.map(|e| e.id.clone())),
        text(&e.and_then(|e| e.device_id.clone())),
        text(&i.map(|i| i.id.clone())),
        // Not `row.name()`, whose "(no name)" is for the screen.
        text(
            &e.and_then(|e| e.display_name.clone())
                .or_else(|| i.and_then(|i| i.device_name.clone())),
        ),
        text(&os),
        text(&os_version),
        text(&e.and_then(|e| e.trust_type.clone())),
        flag(e.and_then(|e| e.account_enabled)),
        text(&compliance),
        text(&i.and_then(|i| i.management_agent.clone())),
        text(&i.and_then(|i| i.managed_device_owner_type.clone())),
        text(&i.and_then(|i| i.user_principal_name.clone())),
        text(&i.and_then(|i| i.serial_number.clone())),
        text(&i.and_then(|i| i.manufacturer.clone())),
        text(&i.and_then(|i| i.model.clone())),
        datetime(e.and_then(|e| e.registration_date_time.as_deref())),
        datetime(i.and_then(|i| i.enrolled_date_time.as_deref())),
        datetime(e.and_then(|e| e.approximate_last_sign_in_date_time.as_deref())),
        datetime(i.and_then(|i| i.last_sync_date_time.as_deref())),
        now.clone(),
    ]
}

/// A `YYYY-MM-DD` from the usage report as a MariaDB `DATE`, or NULL.
fn date(value: &str) -> Value {
    chrono::NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d")
        .map(|d| Value::from(d.format("%Y-%m-%d").to_string()))
        .unwrap_or(Value::NULL)
}

fn mailbox_values(tenant: &str, m: &MailboxUsage, report_date: &str, now: &Value) -> Vec<Value> {
    let number = |n: Option<u64>| n.map_or(Value::NULL, Value::from);
    let text = |s: &str| {
        if s.is_empty() {
            Value::NULL
        } else {
            Value::from(s)
        }
    };
    vec![
        Value::from(tenant),
        // Lower case, as the key: the report's own case is not stable.
        Value::from(m.user_principal_name.to_lowercase()),
        text(&m.display_name),
        text(&m.recipient_type),
        number(m.storage_used),
        number(m.item_count),
        number(m.issue_warning_quota),
        number(m.prohibit_send_quota),
        number(m.prohibit_send_receive_quota),
        flag(m.has_archive),
        date(&m.last_activity),
        date(report_date),
        now.clone(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_rows_have_a_column_for_every_value() {
        let row = mailbox_values("t", &MailboxUsage::default(), "2026-10-01", &Value::NULL);
        assert_eq!(row.len(), 13);
        assert_eq!(row[11], Value::from("2026-10-01"));
        assert_eq!(row[10], Value::NULL);
    }

    #[test]
    fn timestamps_become_utc_datetimes() {
        assert_eq!(
            datetime(Some("2026-10-02T14:03:59.5+01:00")),
            Value::from("2026-10-02 13:03:59")
        );
        assert_eq!(datetime(Some("0001-01-01T00:00:00Z")), Value::NULL);
        assert_eq!(datetime(None), Value::NULL);
    }

    #[test]
    fn device_rows_have_a_column_for_every_value() {
        let row = DeviceRow::default();
        assert_eq!(device_values("t", &row, &Value::NULL).len(), 22);
    }
}
