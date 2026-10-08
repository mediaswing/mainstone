//! Exchange Online, as far as Microsoft Graph reaches: a mailbox's automatic
//! replies, time zone and language (`MailboxSettings.ReadWrite`), and every
//! mailbox's size and last activity from the usage reports
//! (`Reports.Read.All`).
//!
//! A user with no Exchange Online mailbox — unlicensed, a guest, or in a
//! tenant without Exchange at all — has no mailbox settings, and Graph
//! answers 404 for them. That is an answer, not an error: [`MailboxSettings`]
//! comes back as `None`.
//!
//! The usage report is a CSV file, refreshed by Microsoft once a day or so,
//! so what it says can be a day or two old. Tenants hide the names in it by
//! default ("Display concealed user, group, and site names in all reports",
//! in the Microsoft 365 admin centre under Settings → Org settings →
//! Reports), and then the sign-in names are replaced by hashes that match
//! nobody; [`UsageReport::concealed`] says when that has happened.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::{Graph, Result};

/// Whether automatic replies are being sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReplyStatus {
    #[default]
    Off,
    On,
    /// Only between the scheduled start and end.
    Scheduled,
}

impl ReplyStatus {
    pub const ALL: [Self; 3] = [Self::Off, Self::On, Self::Scheduled];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::On => "On",
            Self::Scheduled => "Scheduled",
        }
    }

    fn graph(self) -> &'static str {
        match self {
            Self::Off => "disabled",
            Self::On => "alwaysEnabled",
            Self::Scheduled => "scheduled",
        }
    }

    fn from_graph(value: &str) -> Self {
        match value {
            "alwaysEnabled" => Self::On,
            "scheduled" => Self::Scheduled,
            _ => Self::Off,
        }
    }
}

/// Who outside the organisation gets the external reply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Audience {
    /// Nobody outside: they get no reply at all.
    None,
    /// Only senders in the mailbox's contacts.
    ContactsOnly,
    #[default]
    All,
}

impl Audience {
    pub const ALL: [Self; 3] = [Self::None, Self::ContactsOnly, Self::All];

    pub fn label(self) -> &'static str {
        match self {
            Self::None => "Nobody outside the organisation",
            Self::ContactsOnly => "Only senders in the contacts",
            Self::All => "Everyone outside the organisation",
        }
    }

    fn graph(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ContactsOnly => "contactsOnly",
            Self::All => "all",
        }
    }

    fn from_graph(value: &str) -> Self {
        match value {
            "none" => Self::None,
            "contactsOnly" => Self::ContactsOnly,
            _ => Self::All,
        }
    }
}

/// A mailbox's automatic replies, as stored. The messages are HTML, which is
/// what Outlook writes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoReplies {
    pub status: ReplyStatus,
    pub audience: Audience,
    /// `YYYY-MM-DD HH:MM`, in `time_zone`.
    pub start: String,
    pub end: String,
    /// The zone the schedule is written in: whatever it was set in, which is
    /// UTC unless Outlook or another tool chose otherwise.
    pub time_zone: String,
    pub internal_html: String,
    pub external_html: String,
}

/// What the details panel shows about a mailbox.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxSettings {
    pub auto_replies: AutoReplies,
    pub time_zone: Option<String>,
    pub language: Option<String>,
    /// `user`, `shared`, `room`, `equipment`…
    pub purpose: Option<String>,
}

impl MailboxSettings {
    fn from_graph(value: &Value) -> Self {
        let s = |v: &Value| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let replies = &value["automaticRepliesSetting"];
        let start = &replies["scheduledStartDateTime"];
        let end = &replies["scheduledEndDateTime"];
        Self {
            auto_replies: AutoReplies {
                status: ReplyStatus::from_graph(replies["status"].as_str().unwrap_or("")),
                audience: Audience::from_graph(replies["externalAudience"].as_str().unwrap_or("")),
                start: graph_to_minutes(start["dateTime"].as_str().unwrap_or("")),
                end: graph_to_minutes(end["dateTime"].as_str().unwrap_or("")),
                time_zone: s(&start["timeZone"]).unwrap_or_else(|| "UTC".to_owned()),
                internal_html: s(&replies["internalReplyMessage"]).unwrap_or_default(),
                external_html: s(&replies["externalReplyMessage"]).unwrap_or_default(),
            },
            time_zone: s(&value["timeZone"]),
            language: s(&value["language"]["displayName"]).or_else(|| s(&value["language"]["locale"])),
            purpose: s(&value["userPurpose"]),
        }
    }
}

impl AutoReplies {
    /// One line for the details panel.
    pub fn summary(&self) -> String {
        match self.status {
            ReplyStatus::Off => "Off".to_owned(),
            ReplyStatus::On => "On".to_owned(),
            ReplyStatus::Scheduled => format!(
                "Scheduled, {} to {} ({})",
                self.start, self.end, self.time_zone
            ),
        }
    }
}

/// The automatic-replies form: the messages as plain text to type in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoReplyEdit {
    pub status: ReplyStatus,
    pub audience: Audience,
    pub start: String,
    pub end: String,
    pub time_zone: String,
    pub internal: String,
    pub external: String,
    /// The messages as first shown, so ones left alone are not rewritten:
    /// saving a typed message replaces any formatting set in Outlook.
    original_internal: String,
    original_external: String,
}

impl AutoReplyEdit {
    pub fn from_settings(replies: &AutoReplies) -> Self {
        let internal = html_to_text(&replies.internal_html);
        let external = html_to_text(&replies.external_html);
        let (start, end) = if replies.start.is_empty() || replies.end.is_empty() {
            // Nothing scheduled yet: offer the next week, from the hour.
            let now = chrono::Utc::now().naive_utc();
            let hour = now.format("%Y-%m-%d %H:00").to_string();
            let week = (now + chrono::Duration::days(7)).format("%Y-%m-%d %H:00").to_string();
            (hour, week)
        } else {
            (replies.start.clone(), replies.end.clone())
        };
        Self {
            status: replies.status,
            audience: replies.audience,
            start,
            end,
            time_zone: if replies.time_zone.is_empty() {
                "UTC".to_owned()
            } else {
                replies.time_zone.clone()
            },
            original_internal: internal.clone(),
            original_external: external.clone(),
            internal,
            external,
        }
    }

    /// Whether either message has been changed in the form.
    pub fn messages_changed(&self) -> bool {
        self.internal != self.original_internal || self.external != self.original_external
    }

    /// The PATCH body, or why the form cannot be saved yet.
    fn body(&self) -> Result<Value> {
        let mut replies = Map::new();
        replies.insert("status".into(), self.status.graph().into());
        replies.insert("externalAudience".into(), self.audience.graph().into());
        if self.status == ReplyStatus::Scheduled {
            let start = minutes_to_graph(&self.start)
                .ok_or("The start is a date and time such as 2026-10-05 09:00.")?;
            let end = minutes_to_graph(&self.end)
                .ok_or("The end is a date and time such as 2026-10-12 17:00.")?;
            if end <= start {
                return Err("The end has to be after the start.".into());
            }
            for (key, value) in [("scheduledStartDateTime", start), ("scheduledEndDateTime", end)] {
                replies.insert(
                    key.into(),
                    json!({ "dateTime": value, "timeZone": self.time_zone }),
                );
            }
        }
        if self.internal != self.original_internal {
            replies.insert("internalReplyMessage".into(), text_to_html(&self.internal).into());
        }
        if self.external != self.original_external {
            replies.insert("externalReplyMessage".into(), text_to_html(&self.external).into());
        }
        Ok(json!({ "automaticRepliesSetting": replies }))
    }

    /// Check the form without sending anything.
    pub fn validate(&self) -> Result<()> {
        self.body().map(drop)
    }
}

/// Graph's `2026-10-05T09:00:00.0000000` as `2026-10-05 09:00`.
fn graph_to_minutes(value: &str) -> String {
    let trimmed = value.split('.').next().unwrap_or(value);
    chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S")
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// `2026-10-05 09:00` as Graph's `2026-10-05T09:00:00`, or `None` if it is
/// not a date and time.
fn minutes_to_graph(value: &str) -> Option<String> {
    chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M")
        .ok()
        .map(|t| t.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// An automatic reply's HTML as text to edit: line breaks where the blocks
/// and breaks were, no tags, and the common entities decoded.
pub fn html_to_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while i < html.len() {
        let rest = &lower[i..];
        if rest.starts_with('<') {
            // Whatever is inside <head>, <style> and <script> is not text.
            let skipped = ["head", "style", "script"].iter().find_map(|tag| {
                let opens = rest.starts_with(&format!("<{tag}"))
                    && rest[tag.len() + 1..].starts_with(['>', ' ', '\t', '\r', '\n']);
                opens.then(|| rest.find(&format!("</{tag}>")).map(|at| at + tag.len() + 3))
            });
            if let Some(skip) = skipped {
                i += skip.unwrap_or(rest.len());
                continue;
            }
            let Some(close) = rest.find('>') else { break };
            let inside = &rest[1..close];
            let closing = inside.starts_with('/');
            let name: String = inside
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            if name == "br" || (closing && matches!(name.as_str(), "p" | "div" | "li" | "tr" | "h1" | "h2" | "h3")) {
                out.push('\n');
            }
            i += close + 1;
        } else if rest.starts_with('&') {
            let end = rest.bytes().take(12).position(|b| b == b';');
            match end.and_then(|e| entity(&html[i + 1..i + e]).map(|c| (c, e))) {
                Some((c, e)) => {
                    out.push(c);
                    i += e + 1;
                }
                None => {
                    out.push('&');
                    i += 1;
                }
            }
        } else {
            let ch = html[i..].chars().next().expect("in bounds");
            // Source line breaks are not breaks in HTML.
            out.push(if matches!(ch, '\r' | '\n') { ' ' } else { ch });
            i += ch.len_utf8();
        }
    }
    // Tidy what the markup leaves: spaces at line ends, and runs of blank
    // lines from nested blocks.
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let mut tidy = String::new();
    let mut blanks = 0;
    for line in lines {
        if line.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
        } else {
            blanks = 0;
        }
        tidy.push_str(line);
        tidy.push('\n');
    }
    tidy.trim().to_owned()
}

fn entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    })
}

/// Typed text as the HTML an automatic reply is stored in: escaped, one
/// `<div>` a line, as Outlook writes it.
pub fn text_to_html(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let mut html = String::from("<html><body>");
    for line in text.trim_end().lines() {
        let escaped = line
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;");
        if escaped.trim().is_empty() {
            html.push_str("<div><br></div>");
        } else {
            html.push_str(&format!("<div>{escaped}</div>"));
        }
    }
    html.push_str("</body></html>");
    html
}

/// One mailbox's line in the usage report.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailboxUsage {
    pub user_principal_name: String,
    pub display_name: String,
    /// `User`, `Shared`, `Room`, `Equipment`…
    pub recipient_type: String,
    pub storage_used: Option<u64>,
    pub item_count: Option<u64>,
    pub issue_warning_quota: Option<u64>,
    pub prohibit_send_quota: Option<u64>,
    pub prohibit_send_receive_quota: Option<u64>,
    /// `YYYY-MM-DD`.
    pub last_activity: String,
    pub has_archive: Option<bool>,
}

impl MailboxUsage {
    /// `1.2 GB of 49.5 GB`, against the quota that stops it receiving.
    pub fn size_text(&self) -> String {
        match (self.storage_used, self.prohibit_send_receive_quota) {
            (Some(used), Some(quota)) => format!("{} of {}", human_bytes(used), human_bytes(quota)),
            (Some(used), None) => human_bytes(used),
            _ => String::new(),
        }
    }

    /// Past the point where Exchange starts warning, or worse.
    pub fn quota_state(&self) -> Option<&'static str> {
        let used = self.storage_used?;
        let past = |quota: Option<u64>| quota.is_some_and(|q| used >= q);
        if past(self.prohibit_send_receive_quota) {
            Some("Full: it can no longer send or receive mail")
        } else if past(self.prohibit_send_quota) {
            Some("Over quota: it can receive mail but not send it")
        } else if past(self.issue_warning_quota) {
            Some("Nearly full: past the warning quota")
        } else {
            None
        }
    }
}

/// Every mailbox in the usage report, by sign-in name.
#[derive(Clone, Debug, Default)]
pub struct UsageReport {
    by_upn: HashMap<String, MailboxUsage>,
    /// The day Microsoft last refreshed the report, `YYYY-MM-DD`.
    pub refresh_date: String,
    /// The tenant hides names in reports, so nothing in it can be matched.
    pub concealed: bool,
}

impl UsageReport {
    pub fn get(&self, upn: &str) -> Option<&MailboxUsage> {
        self.by_upn.get(&upn.to_lowercase())
    }

    pub fn len(&self) -> usize {
        self.by_upn.len()
    }

    pub fn mailboxes(&self) -> impl Iterator<Item = &MailboxUsage> {
        self.by_upn.values()
    }

    /// The report's CSV. Columns are found by name, since Microsoft has added
    /// to them over the years; deleted mailboxes are left out.
    pub fn parse(csv_text: &str) -> Result<Self> {
        let csv_text = csv_text.trim_start_matches('\u{feff}');
        let mut reader = csv::ReaderBuilder::new()
            .flexible(true)
            .from_reader(csv_text.as_bytes());
        let headers = reader
            .headers()
            .map_err(|e| format!("The mailbox usage report could not be read: {e}"))?
            .clone();
        let key = |h: &str| -> String {
            h.chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase()
        };
        let column = |name: &str| headers.iter().position(|h| key(h) == key(name));
        let upn_at = column("User Principal Name")
            .ok_or("The mailbox usage report has no User Principal Name column.")?;
        let at = |name: &str| column(name);
        let (name_at, deleted_at, refresh_at, activity_at, items_at, used_at) = (
            at("Display Name"),
            at("Is Deleted"),
            at("Report Refresh Date"),
            at("Last Activity Date"),
            at("Item Count"),
            at("Storage Used (Byte)"),
        );
        let (warn_at, send_at, send_receive_at, archive_at, type_at) = (
            at("Issue Warning Quota (Byte)"),
            at("Prohibit Send Quota (Byte)"),
            at("Prohibit Send/Receive Quota (Byte)"),
            at("Has Archive"),
            at("Recipient Type"),
        );

        let mut report = Self::default();
        for record in reader.records() {
            let record =
                record.map_err(|e| format!("The mailbox usage report could not be read: {e}"))?;
            let field = |at: Option<usize>| at.and_then(|i| record.get(i)).unwrap_or("").trim();
            let number = |at: Option<usize>| field(at).parse::<u64>().ok();
            let flag = |at: Option<usize>| match field(at).to_ascii_lowercase().as_str() {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            };
            if report.refresh_date.is_empty() {
                report.refresh_date = field(refresh_at).to_owned();
            }
            let upn = field(Some(upn_at));
            if upn.is_empty() || flag(deleted_at) == Some(true) {
                continue;
            }
            report.by_upn.insert(
                upn.to_lowercase(),
                MailboxUsage {
                    user_principal_name: upn.to_owned(),
                    display_name: field(name_at).to_owned(),
                    recipient_type: field(type_at).to_owned(),
                    storage_used: number(used_at),
                    item_count: number(items_at),
                    issue_warning_quota: number(warn_at),
                    prohibit_send_quota: number(send_at),
                    prohibit_send_receive_quota: number(send_receive_at),
                    last_activity: field(activity_at).to_owned(),
                    has_archive: flag(archive_at),
                },
            );
        }
        // Concealed names are hex hashes, so not one of them is an address.
        report.concealed =
            !report.by_upn.is_empty() && !report.by_upn.keys().any(|upn| upn.contains('@'));
        Ok(report)
    }
}

/// A size as Exchange shows one, in binary units: `49.5 GB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// What the details panel and the export say when the report names nobody.
pub const CONCEALED_HINT: &str = "The tenant hides user names in usage reports, so mailbox sizes \
cannot be matched to users. To show them, turn off \"Display concealed user, group, and site \
names in all reports\" in the Microsoft 365 admin centre, under Settings > Org settings > Reports.";

impl Graph {
    /// A user's mailbox settings, or `None` if they have no Exchange Online
    /// mailbox.
    pub fn mailbox_settings(&self, user_id: &str) -> Result<Option<MailboxSettings>> {
        Ok(self
            .get_if_found(&format!("/users/{user_id}/mailboxSettings"))?
            .map(|v| MailboxSettings::from_graph(&v)))
    }

    pub fn set_auto_replies(&self, user_id: &str, edit: &AutoReplyEdit) -> Result<()> {
        self.patch(&format!("/users/{user_id}/mailboxSettings"), &edit.body()?)
    }

    /// Every mailbox's size and activity, as of Microsoft's last refresh.
    pub fn mailbox_usage(&self) -> Result<UsageReport> {
        let text = self.get_text("/reports/getMailboxUsageDetail(period='D7')")?;
        let report = UsageReport::parse(&text)?;
        log::debug!(
            "mailbox usage report: {} mailboxes, refreshed {}, names concealed {}",
            report.len(),
            report.refresh_date,
            report.concealed
        );
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = r#"{
        "automaticRepliesSetting": {
            "status": "scheduled",
            "externalAudience": "contactsOnly",
            "scheduledStartDateTime": {"dateTime": "2026-10-05T09:00:00.0000000", "timeZone": "UTC"},
            "scheduledEndDateTime": {"dateTime": "2026-10-12T17:30:00.0000000", "timeZone": "UTC"},
            "internalReplyMessage": "<html>\r\n<head><style>p {margin:0}</style></head><body><div>Away until Monday.</div><div><br></div><div>Ask Sam &amp; Kim.</div></body></html>",
            "externalReplyMessage": ""
        },
        "timeZone": "GMT Standard Time",
        "language": {"locale": "en-GB", "displayName": "English (United Kingdom)"},
        "userPurpose": "user"
    }"#;

    #[test]
    fn mailbox_settings_are_read() {
        let s = MailboxSettings::from_graph(&serde_json::from_str(SETTINGS).unwrap());
        let r = &s.auto_replies;
        assert_eq!(r.status, ReplyStatus::Scheduled);
        assert_eq!(r.audience, Audience::ContactsOnly);
        assert_eq!(r.start, "2026-10-05 09:00");
        assert_eq!(r.end, "2026-10-12 17:30");
        assert_eq!(r.time_zone, "UTC");
        assert_eq!(s.time_zone.as_deref(), Some("GMT Standard Time"));
        assert_eq!(s.language.as_deref(), Some("English (United Kingdom)"));
        assert_eq!(html_to_text(&r.internal_html), "Away until Monday.\n\nAsk Sam & Kim.");
    }

    #[test]
    fn untouched_messages_are_not_rewritten() {
        let s = MailboxSettings::from_graph(&serde_json::from_str(SETTINGS).unwrap());
        let mut edit = AutoReplyEdit::from_settings(&s.auto_replies);
        edit.status = ReplyStatus::On;
        let body = edit.body().unwrap();
        let replies = &body["automaticRepliesSetting"];
        assert_eq!(replies["status"], "alwaysEnabled");
        assert_eq!(replies["externalAudience"], "contactsOnly");
        assert!(replies.get("internalReplyMessage").is_none());
        assert!(replies.get("scheduledStartDateTime").is_none());

        edit.external = "Back <soon>".into();
        let body = edit.body().unwrap();
        assert_eq!(
            body["automaticRepliesSetting"]["externalReplyMessage"],
            "<html><body><div>Back &lt;soon&gt;</div></body></html>"
        );
    }

    #[test]
    fn a_schedule_has_to_make_sense() {
        let mut edit = AutoReplyEdit::from_settings(&AutoReplies::default());
        edit.status = ReplyStatus::Scheduled;
        assert!(edit.validate().is_ok(), "the offered week is valid");
        edit.start = "2026-10-12 09:00".into();
        edit.end = "2026-10-05 09:00".into();
        assert!(edit.validate().is_err());
        edit.end = "next week".into();
        assert!(edit.validate().is_err());
        edit.end = "2026-10-19 09:00".into();
        let body = edit.body().unwrap();
        assert_eq!(
            body["automaticRepliesSetting"]["scheduledEndDateTime"],
            json!({"dateTime": "2026-10-19T09:00:00", "timeZone": "UTC"})
        );
    }

    #[test]
    fn html_becomes_text_and_back() {
        assert_eq!(html_to_text("<p>One</p><p>Two&nbsp;&#8211;&#x21;</p>"), "One\nTwo –!");
        assert_eq!(html_to_text("Tom &amp Jerry <b>bold</b>"), "Tom &amp Jerry bold");
        assert_eq!(html_to_text(""), "");
        let text = "Line one\n\nLine \"three\"";
        assert_eq!(html_to_text(&text_to_html(text)), text);
        assert_eq!(text_to_html("  "), "");
    }

    const REPORT: &str = "\u{feff}Report Refresh Date,User Principal Name,Display Name,Is Deleted,Deleted Date,Created Date,Last Activity Date,Item Count,Storage Used (Byte),Issue Warning Quota (Byte),Prohibit Send Quota (Byte),Prohibit Send/Receive Quota (Byte),Deleted Item Count,Deleted Item Size (Byte),Deleted Item Quota (Byte),Has Archive,Recipient Type,Report Period
2026-10-01,Jo.Bloggs@contoso.com,Jo Bloggs,False,,2024-01-02,2026-09-30,1234,1288490188,52613349376,53687091200,53687091200,10,2048,32212254720,True,User,7
2026-10-01,old@contoso.com,Old,True,2026-09-01,2020-01-01,,0,0,,,,0,0,,False,User,7
2026-10-01,help@contoso.com,Help desk,False,,2024-01-02,,10,53687091200,52613349376,53687091200,53687091200,0,0,,False,Shared,7
";

    #[test]
    fn the_usage_report_is_read_by_column_name() {
        let report = UsageReport::parse(REPORT).unwrap();
        assert_eq!(report.len(), 2, "the deleted mailbox is left out");
        assert_eq!(report.refresh_date, "2026-10-01");
        assert!(!report.concealed);
        let jo = report.get("jo.bloggs@CONTOSO.com").unwrap();
        assert_eq!(jo.size_text(), "1.2 GB of 50.0 GB");
        assert_eq!(jo.item_count, Some(1234));
        assert_eq!(jo.has_archive, Some(true));
        assert_eq!(jo.last_activity, "2026-09-30");
        assert_eq!(jo.quota_state(), None);
        let help = report.get("help@contoso.com").unwrap();
        assert_eq!(help.recipient_type, "Shared");
        assert!(help.quota_state().unwrap().starts_with("Full"));
    }

    #[test]
    fn concealed_names_and_empty_reports_are_recognised() {
        let concealed = "Report Refresh Date,User Principal Name,Storage Used (Byte)\n2026-10-01,5F2B9A01C3D4E5F6,100\n";
        assert!(UsageReport::parse(concealed).unwrap().concealed);
        // A tenant without Exchange: the header and nothing else.
        let empty = UsageReport::parse("Report Refresh Date,User Principal Name\n").unwrap();
        assert_eq!(empty.len(), 0);
        assert!(!empty.concealed);
        assert!(UsageReport::parse("").is_err());
    }

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(human_bytes(512), "512 bytes");
        assert_eq!(human_bytes(1536), "1.5 KB");
        assert_eq!(human_bytes(53_687_091_200), "50.0 GB");
    }
}
