//! Groups: list, create, delete, and manage direct members.

use serde_json::json;

use super::models::{DirectoryObject, Group};
use super::{GRAPH, Graph, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NewGroupKind {
    #[default]
    Security,
    Microsoft365,
}

impl NewGroupKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Security => "Security",
            Self::Microsoft365 => "Microsoft 365",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct NewGroup {
    pub display_name: String,
    pub mail_nickname: String,
    pub description: String,
    pub kind: NewGroupKind,
}

impl NewGroup {
    fn nickname(&self) -> String {
        let source = if self.mail_nickname.trim().is_empty() {
            &self.display_name
        } else {
            &self.mail_nickname
        };
        source
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .collect()
    }
}

impl Graph {
    pub fn list_groups(&self) -> Result<Vec<Group>> {
        let mut groups: Vec<Group> =
            self.get_all(&format!("/groups?$select={}&$top=999", Group::SELECT))?;
        groups.sort_by_key(|g| g.name().to_lowercase());
        Ok(groups)
    }

    pub fn group_members(&self, group_id: &str) -> Result<Vec<DirectoryObject>> {
        self.group_members_if_found(group_id)?
            .ok_or_else(|| "The group no longer exists. Refresh the list.".to_owned())
    }

    /// The members, or `None` if the group has been deleted since it was
    /// listed.
    pub fn group_members_if_found(&self, group_id: &str) -> Result<Option<Vec<DirectoryObject>>> {
        let Some(mut members) = self.get_all_if_found::<DirectoryObject>(&format!(
            "/groups/{group_id}/members?$select=id,displayName,userPrincipalName,mail&$top=999"
        ))?
        else {
            return Ok(None);
        };
        members.sort_by_key(|m| m.name().to_lowercase());
        Ok(Some(members))
    }

    pub fn create_group(&self, group: &NewGroup) -> Result<Group> {
        if group.display_name.trim().is_empty() {
            return Err("A group name is needed.".into());
        }
        let nickname = group.nickname();
        if nickname.is_empty() {
            return Err("A mail nickname is needed (letters, digits, . - _).".into());
        }
        let mut body = json!({
            "displayName": group.display_name.trim(),
            "mailNickname": nickname,
        });
        match group.kind {
            NewGroupKind::Security => {
                body["mailEnabled"] = false.into();
                body["securityEnabled"] = true.into();
                body["groupTypes"] = json!([]);
            }
            NewGroupKind::Microsoft365 => {
                body["mailEnabled"] = true.into();
                body["securityEnabled"] = false.into();
                body["groupTypes"] = json!(["Unified"]);
            }
        }
        if !group.description.trim().is_empty() {
            body["description"] = group.description.trim().into();
        }
        let created = self
            .post("/groups", &body)?
            .ok_or("Microsoft Graph did not return the new group.")?;
        serde_json::from_value(created).map_err(|e| e.to_string())
    }

    pub fn delete_group(&self, group_id: &str) -> Result<()> {
        self.delete(&format!("/groups/{group_id}"))
    }

    /// Add a directory object (a user, device or group) by its object ID.
    pub fn add_member(&self, group_id: &str, member_id: &str) -> Result<()> {
        self.post(
            &format!("/groups/{group_id}/members/$ref"),
            &json!({ "@odata.id": format!("{GRAPH}/directoryObjects/{member_id}") }),
        )
        .map(drop)
    }

    pub fn remove_member(&self, group_id: &str, member_id: &str) -> Result<()> {
        self.delete(&format!("/groups/{group_id}/members/{member_id}/$ref"))
    }
}
