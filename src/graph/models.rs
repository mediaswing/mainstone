//! The shapes Graph returns, trimmed to the properties the app asks for with
//! `$select`. Everything is optional because Graph leaves out what is null.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct User {
    pub id: String,
    pub display_name: Option<String>,
    pub user_principal_name: Option<String>,
    pub mail: Option<String>,
    pub given_name: Option<String>,
    pub surname: Option<String>,
    pub job_title: Option<String>,
    pub department: Option<String>,
    pub office_location: Option<String>,
    pub mobile_phone: Option<String>,
    pub usage_location: Option<String>,
    pub account_enabled: Option<bool>,
    pub user_type: Option<String>,
    pub created_date_time: Option<String>,
    pub on_premises_sync_enabled: Option<bool>,
}

impl User {
    pub const SELECT: &'static str = "id,displayName,userPrincipalName,mail,givenName,surname,\
jobTitle,department,officeLocation,mobilePhone,usageLocation,accountEnabled,userType,\
createdDateTime,onPremisesSyncEnabled";

    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or("(no name)")
    }

    pub fn upn(&self) -> &str {
        self.user_principal_name.as_deref().unwrap_or("")
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Group {
    pub id: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub mail: Option<String>,
    pub mail_nickname: Option<String>,
    pub group_types: Vec<String>,
    pub security_enabled: Option<bool>,
    pub mail_enabled: Option<bool>,
    pub membership_rule: Option<String>,
    pub created_date_time: Option<String>,
}

impl Group {
    pub const SELECT: &'static str = "id,displayName,description,mail,mailNickname,groupTypes,\
securityEnabled,mailEnabled,membershipRule,createdDateTime";

    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or("(no name)")
    }

    /// Microsoft 365, security, mail-enabled security or distribution — the
    /// same four the Entra admin centre uses.
    pub fn kind(&self) -> &'static str {
        let unified = self.group_types.iter().any(|t| t == "Unified");
        match (
            unified,
            self.security_enabled.unwrap_or(false),
            self.mail_enabled.unwrap_or(false),
        ) {
            (true, _, _) => "Microsoft 365",
            (false, true, true) => "Mail-enabled security",
            (false, true, false) => "Security",
            (false, false, _) => "Distribution",
        }
    }

    /// Dynamic groups take their members from a rule and refuse manual edits.
    pub fn is_dynamic(&self) -> bool {
        self.group_types.iter().any(|t| t == "DynamicMembership")
    }
}

/// A member of a group, or a group a user belongs to: a directory object of
/// whatever type.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DirectoryObject {
    pub id: String,
    #[serde(rename = "@odata.type")]
    pub odata_type: Option<String>,
    pub display_name: Option<String>,
    pub user_principal_name: Option<String>,
    pub mail: Option<String>,
}

impl DirectoryObject {
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or("(no name)")
    }

    /// `#microsoft.graph.user` as `user`.
    pub fn kind(&self) -> &str {
        self.odata_type
            .as_deref()
            .and_then(|t| t.rsplit('.').next())
            .unwrap_or("object")
    }

    /// The most useful second line: a sign-in name for a user, an address for
    /// anything with one.
    pub fn detail(&self) -> &str {
        self.user_principal_name
            .as_deref()
            .or(self.mail.as_deref())
            .unwrap_or("")
    }
}

/// A device as Entra ID sees it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Device {
    /// The directory object ID, used in Graph paths.
    pub id: String,
    /// The device ID, which is what Intune calls `azureADDeviceId`.
    pub device_id: Option<String>,
    pub display_name: Option<String>,
    pub operating_system: Option<String>,
    pub operating_system_version: Option<String>,
    pub trust_type: Option<String>,
    pub account_enabled: Option<bool>,
    pub is_compliant: Option<bool>,
    pub is_managed: Option<bool>,
    pub approximate_last_sign_in_date_time: Option<String>,
    pub registration_date_time: Option<String>,
}

impl Device {
    pub const SELECT: &'static str = "id,deviceId,displayName,operatingSystem,\
operatingSystemVersion,trustType,accountEnabled,isCompliant,isManaged,\
approximateLastSignInDateTime,registrationDateTime";
}

/// A device as Intune sees it.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ManagedDevice {
    pub id: String,
    pub device_name: Option<String>,
    #[serde(rename = "azureADDeviceId")]
    pub azure_ad_device_id: Option<String>,
    pub operating_system: Option<String>,
    pub os_version: Option<String>,
    pub compliance_state: Option<String>,
    pub management_agent: Option<String>,
    pub managed_device_owner_type: Option<String>,
    pub last_sync_date_time: Option<String>,
    pub enrolled_date_time: Option<String>,
    pub user_principal_name: Option<String>,
    pub serial_number: Option<String>,
    pub model: Option<String>,
    pub manufacturer: Option<String>,
}

impl ManagedDevice {
    pub const SELECT: &'static str = "id,deviceName,azureADDeviceId,operatingSystem,osVersion,\
complianceState,managementAgent,managedDeviceOwnerType,lastSyncDateTime,enrolledDateTime,\
userPrincipalName,serialNumber,model,manufacturer";
}

/// One physical device, joined across Entra and Intune on the Entra device
/// ID. Either half may be missing: a registered device that was never
/// enrolled has no Intune record, and an enrolment can outlive its Entra
/// object.
#[derive(Clone, Debug, Default)]
pub struct DeviceRow {
    pub entra: Option<Device>,
    pub intune: Option<ManagedDevice>,
}

impl DeviceRow {
    pub fn name(&self) -> &str {
        self.entra
            .as_ref()
            .and_then(|d| d.display_name.as_deref())
            .or_else(|| self.intune.as_ref().and_then(|d| d.device_name.as_deref()))
            .unwrap_or("(no name)")
    }

    pub fn os(&self) -> String {
        let (os, version) = match (&self.entra, &self.intune) {
            (_, Some(i)) => (i.operating_system.as_deref(), i.os_version.as_deref()),
            (Some(e), None) => (
                e.operating_system.as_deref(),
                e.operating_system_version.as_deref(),
            ),
            (None, None) => (None, None),
        };
        match (os, version) {
            (Some(os), Some(v)) => format!("{os} {v}"),
            (Some(os), None) => os.to_owned(),
            _ => String::new(),
        }
    }

    pub fn user(&self) -> &str {
        self.intune
            .as_ref()
            .and_then(|d| d.user_principal_name.as_deref())
            .unwrap_or("")
    }

    pub fn compliance(&self) -> &str {
        if let Some(state) = self.intune.as_ref().and_then(|d| d.compliance_state.as_deref()) {
            return state;
        }
        match self.entra.as_ref().and_then(|d| d.is_compliant) {
            Some(true) => "compliant",
            Some(false) => "noncompliant",
            None => "",
        }
    }

    pub fn last_seen(&self) -> Option<&str> {
        self.intune
            .as_ref()
            .and_then(|d| d.last_sync_date_time.as_deref())
            .or_else(|| {
                self.entra
                    .as_ref()
                    .and_then(|d| d.approximate_last_sign_in_date_time.as_deref())
            })
    }
}

/// Join the two lists into one row per device.
pub fn join_devices(entra: Vec<Device>, intune: Vec<ManagedDevice>) -> Vec<DeviceRow> {
    use std::collections::HashMap;

    let mut by_device_id: HashMap<String, ManagedDevice> = HashMap::new();
    let mut unmatched = Vec::new();
    for managed in intune {
        match managed
            .azure_ad_device_id
            .clone()
            .filter(|id| !id.is_empty() && id != "00000000-0000-0000-0000-000000000000")
        {
            Some(id) => {
                by_device_id.insert(id.to_lowercase(), managed);
            }
            None => unmatched.push(managed),
        }
    }

    let mut rows: Vec<DeviceRow> = entra
        .into_iter()
        .map(|device| {
            let intune = device
                .device_id
                .as_ref()
                .and_then(|id| by_device_id.remove(&id.to_lowercase()));
            DeviceRow {
                entra: Some(device),
                intune,
            }
        })
        .collect();
    rows.extend(
        by_device_id
            .into_values()
            .chain(unmatched)
            .map(|managed| DeviceRow {
                entra: None,
                intune: Some(managed),
            }),
    );
    rows.sort_by_key(|r| r.name().to_lowercase());
    rows
}

/// A Graph timestamp as something a person reads: `2026-10-02 14:03`, in UTC,
/// or empty for Intune's "never" (year 1).
pub fn short_time(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    match chrono::DateTime::parse_from_rfc3339(value) {
        Ok(t) if chrono::Datelike::year(&t) > 1900 => t.format("%Y-%m-%d %H:%M").to_string(),
        Ok(_) => String::new(),
        Err(_) => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices_join_on_the_entra_device_id() {
        let entra = vec![Device {
            id: "obj-1".into(),
            device_id: Some("ABC".into()),
            display_name: Some("Laptop".into()),
            ..Default::default()
        }];
        let intune = vec![
            ManagedDevice {
                id: "mdm-1".into(),
                azure_ad_device_id: Some("abc".into()),
                ..Default::default()
            },
            ManagedDevice {
                id: "mdm-2".into(),
                device_name: Some("Orphan".into()),
                ..Default::default()
            },
        ];
        let rows = join_devices(entra, intune);
        assert_eq!(rows.len(), 2);
        let laptop = rows.iter().find(|r| r.name() == "Laptop").unwrap();
        assert_eq!(laptop.intune.as_ref().unwrap().id, "mdm-1");
        let orphan = rows.iter().find(|r| r.name() == "Orphan").unwrap();
        assert!(orphan.entra.is_none());
    }

    #[test]
    fn intune_never_is_blank() {
        assert_eq!(short_time(Some("0001-01-01T00:00:00Z")), "");
        assert_eq!(
            short_time(Some("2026-10-02T14:03:59.123Z")),
            "2026-10-02 14:03"
        );
    }
}
