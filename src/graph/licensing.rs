//! Licences: the tenant's subscriptions, who holds each one, and assigning
//! and removing them.
//!
//! A user can hold a licence directly, or inherit it from a group with a
//! licence assigned to it. Only a direct one can be removed here; one that
//! comes from a group goes when the user leaves the group. Microsoft also
//! refuses to assign a licence to a user with no usage location, because what
//! a licence may include depends on the country, so that is checked first.

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Graph, Result};

/// A subscription the tenant has bought: Graph's `subscribedSku`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Sku {
    pub sku_id: String,
    pub sku_part_number: String,
    /// `User` or `Company`.
    pub applies_to: Option<String>,
    /// `Enabled`, `Warning`, `Suspended`, `Deleted` or `LockedOut`.
    pub capability_status: Option<String>,
    pub consumed_units: i64,
    pub prepaid_units: PrepaidUnits,
    pub service_plans: Vec<ServicePlan>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PrepaidUnits {
    pub enabled: i64,
    pub suspended: i64,
    pub warning: i64,
    pub locked_out: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServicePlan {
    pub service_plan_id: String,
    pub service_plan_name: String,
    pub provisioning_status: Option<String>,
    pub applies_to: Option<String>,
}

impl Sku {
    /// The product's name as the admin centres show it, where it is one this
    /// app knows; otherwise the part number, which is all Graph gives.
    pub fn name(&self) -> &str {
        product_name(&self.sku_part_number).unwrap_or(&self.sku_part_number)
    }

    /// Licences that can still be assigned. Units in their warning period
    /// still work, so they count.
    pub fn available(&self) -> i64 {
        (self.prepaid_units.enabled + self.prepaid_units.warning - self.consumed_units).max(0)
    }

    pub fn total(&self) -> i64 {
        self.prepaid_units.enabled + self.prepaid_units.warning
    }

    /// Licences for users, as opposed to the tenant-wide ones (`Company`)
    /// that cannot be assigned to anybody.
    pub fn assignable(&self) -> bool {
        self.applies_to.as_deref() != Some("Company")
    }
}

/// How a user holds one licence: Graph's `licenseAssignmentState`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LicenceState {
    pub sku_id: String,
    /// The group it comes from, or `None` for a direct assignment.
    pub assigned_by_group: Option<String>,
    /// `Active`, `ActiveWithError`, `Disabled` or `Error`.
    pub state: Option<String>,
    /// Why an assignment failed, such as `CountViolation`.
    pub error: Option<String>,
    pub disabled_plans: Vec<String>,
}

/// A user, with what is needed to show and change their licences.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Licensee {
    pub id: String,
    pub display_name: Option<String>,
    pub user_principal_name: Option<String>,
    pub usage_location: Option<String>,
    pub license_assignment_states: Vec<LicenceState>,
}

const LICENSEE_SELECT: &str = "id,displayName,userPrincipalName,usageLocation,licenseAssignmentStates";

impl Licensee {
    pub fn name(&self) -> &str {
        self.display_name.as_deref().unwrap_or("(no name)")
    }

    pub fn upn(&self) -> &str {
        self.user_principal_name.as_deref().unwrap_or("")
    }

    /// Every way this user holds this licence: directly, through groups, or
    /// both.
    pub fn states_for<'a>(&'a self, sku_id: &'a str) -> impl Iterator<Item = &'a LicenceState> {
        self.license_assignment_states
            .iter()
            .filter(move |s| s.sku_id.eq_ignore_ascii_case(sku_id))
    }

    pub fn holds(&self, sku_id: &str) -> bool {
        self.states_for(sku_id).next().is_some()
    }

    /// Whether the user has this licence directly, so it can be removed.
    pub fn holds_directly(&self, sku_id: &str) -> bool {
        self.states_for(sku_id).any(|s| s.assigned_by_group.is_none())
    }

    /// The groups the user has this licence through.
    pub fn groups_for(&self, sku_id: &str) -> Vec<String> {
        self.states_for(sku_id)
            .filter_map(|s| s.assigned_by_group.clone())
            .collect()
    }

    /// A failed assignment of this licence, in words, if there is one.
    pub fn problem_with(&self, sku_id: &str) -> Option<String> {
        self.states_for(sku_id).find_map(|s| {
            let error = s.error.as_deref().filter(|e| !e.is_empty() && *e != "None");
            let failing = matches!(s.state.as_deref(), Some("Error" | "ActiveWithError"));
            (failing || error.is_some()).then(|| explain_error(error.unwrap_or("Other")))
        })
    }

    fn has_usage_location(&self) -> bool {
        self.usage_location.as_deref().is_some_and(|l| !l.trim().is_empty())
    }
}

/// Microsoft's assignment errors, as something an administrator can act on.
fn explain_error(code: &str) -> String {
    match code {
        "CountViolation" => "Not enough licences left to assign this one.".to_owned(),
        "MutuallyExclusiveViolation" => {
            "It conflicts with another licence the user has.".to_owned()
        }
        "DependencyViolation" => "It needs another licence the user does not have.".to_owned(),
        "ProhibitedInUsageLocationViolation" => {
            "It is not available in the user's usage location.".to_owned()
        }
        "UniquenessViolation" => "A proxy address it needs is already in use.".to_owned(),
        other => format!("The assignment failed ({other})."),
    }
}

/// The people holding one licence, and the names of the groups any of them
/// have it through.
#[derive(Clone, Debug, Default)]
pub struct Holders {
    pub users: Vec<Licensee>,
    pub group_names: HashMap<String, String>,
}

impl Holders {
    pub fn group_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.group_names.get(id).map_or(id, String::as_str)
    }
}

impl Graph {
    pub fn list_skus(&self) -> Result<Vec<Sku>> {
        let mut skus: Vec<Sku> = self.get_all("/subscribedSkus")?;
        skus.sort_by_key(|s| (!s.assignable(), s.name().to_lowercase()));
        Ok(skus)
    }

    /// Everyone holding a licence, directly or through a group.
    pub fn sku_holders(&self, sku_id: &str) -> Result<Holders> {
        check_guid(sku_id)?;
        let mut users: Vec<Licensee> = self.get_all(&format!(
            "/users?$filter={}&$select={LICENSEE_SELECT}&$top=999",
            super::encode_query(&format!("assignedLicenses/any(x:x/skuId eq {sku_id})"))
        ))?;
        users.sort_by_key(|u| u.name().to_lowercase());

        // Group names, so "through a group" can say which. A group that
        // cannot be read keeps its ID, which is still something to search for.
        let group_ids: BTreeSet<String> = users
            .iter()
            .flat_map(|u| u.groups_for(sku_id))
            .collect();
        let mut group_names = HashMap::new();
        for id in group_ids {
            match self.get(&format!("/groups/{id}?$select=displayName")) {
                Ok(group) => {
                    if let Some(name) = group["displayName"].as_str() {
                        group_names.insert(id, name.to_owned());
                    }
                }
                Err(err) => log::debug!("could not name licensing group {id}: {err}"),
            }
        }
        Ok(Holders { users, group_names })
    }

    /// One user's licences, read fresh.
    pub fn licensee(&self, user_id: &str) -> Result<Licensee> {
        let user = self.get(&format!("/users/{user_id}?$select={LICENSEE_SELECT}"))?;
        serde_json::from_value(user).map_err(|e| format!("Unexpected answer from Microsoft Graph: {e}"))
    }

    /// Add and remove licences in one go. Only licences the user holds
    /// directly can be removed, and nothing can be added without a usage
    /// location; both are checked here, where the reason can be put better
    /// than Graph puts it.
    pub fn change_licences(&self, user: &Licensee, add: &[String], remove: &[String]) -> Result<()> {
        if add.is_empty() && remove.is_empty() {
            return Ok(());
        }
        for sku in add.iter().chain(remove) {
            check_guid(sku)?;
        }
        if !add.is_empty() && !user.has_usage_location() {
            return Err(format!(
                "{} has no usage location, and Microsoft needs one before a licence can be assigned. Set it with Edit on the Users tab.",
                user.upn()
            ));
        }
        if let Some(sku) = remove.iter().find(|sku| !user.holds_directly(sku)) {
            return Err(if user.holds(sku) {
                format!(
                    "{} has that licence through a group. Remove them from the group instead.",
                    user.upn()
                )
            } else {
                format!("{} does not have that licence.", user.upn())
            });
        }
        log::info!(
            "changing licences for {}: adding {add:?}, removing {remove:?}",
            user.upn()
        );
        self.post(
            &format!("/users/{}/assignLicense", user.id),
            &json!({
                "addLicenses": add
                    .iter()
                    .map(|sku| json!({ "skuId": sku, "disabledPlans": [] }))
                    .collect::<Vec<_>>(),
                "removeLicenses": remove,
            }),
        )
        .map(drop)
    }
}

/// SKU IDs go into filters and request bodies, so anything that is not a
/// GUID is refused before it gets there.
fn check_guid(id: &str) -> Result<()> {
    let ok = id.len() == 36
        && id.chars().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    if ok {
        Ok(())
    } else {
        Err(format!("\"{id}\" is not a licence ID."))
    }
}

/// The common products' names, from Microsoft's "Product names and service
/// plan identifiers for licensing" list. Anything not here is shown by its
/// part number, which is also what that list is searched by.
fn product_name(part_number: &str) -> Option<&'static str> {
    Some(match part_number {
        "O365_BUSINESS_ESSENTIALS" => "Microsoft 365 Business Basic",
        "O365_BUSINESS_PREMIUM" => "Microsoft 365 Business Standard",
        "SPB" => "Microsoft 365 Business Premium",
        "O365_BUSINESS" => "Microsoft 365 Apps for business",
        "OFFICESUBSCRIPTION" => "Microsoft 365 Apps for enterprise",
        "STANDARDPACK" => "Office 365 E1",
        "ENTERPRISEPACK" => "Office 365 E3",
        "ENTERPRISEPREMIUM" => "Office 365 E5",
        "ENTERPRISEPREMIUM_NOPSTNCONF" => "Office 365 E5 (without Audio Conferencing)",
        "DESKLESSPACK" => "Office 365 F3",
        "SPE_E3" => "Microsoft 365 E3",
        "SPE_E5" => "Microsoft 365 E5",
        "SPE_F1" => "Microsoft 365 F3",
        "DEVELOPERPACK_E5" => "Microsoft 365 E5 Developer",
        "EXCHANGESTANDARD" => "Exchange Online (Plan 1)",
        "EXCHANGEENTERPRISE" => "Exchange Online (Plan 2)",
        "EXCHANGEDESKLESS" => "Exchange Online Kiosk",
        "EMS" => "Enterprise Mobility + Security E3",
        "EMSPREMIUM" => "Enterprise Mobility + Security E5",
        "AAD_PREMIUM" => "Microsoft Entra ID P1",
        "AAD_PREMIUM_P2" => "Microsoft Entra ID P2",
        "INTUNE_A" => "Microsoft Intune Plan 1",
        "WIN_DEF_ATP" => "Microsoft Defender for Endpoint P2",
        "ATP_ENTERPRISE" => "Microsoft Defender for Office 365 (Plan 1)",
        "THREAT_INTELLIGENCE" => "Microsoft Defender for Office 365 (Plan 2)",
        "RIGHTSMANAGEMENT" => "Azure Information Protection Premium P1",
        "Win10_VDA_E3" => "Windows Enterprise E3",
        "POWER_BI_STANDARD" => "Power BI (free)",
        "POWER_BI_PRO" => "Power BI Pro",
        "PBI_PREMIUM_PER_USER" => "Power BI Premium Per User",
        "FLOW_FREE" => "Microsoft Power Automate Free",
        "POWERAPPS_VIRAL" => "Microsoft Power Apps Plan 2 Trial",
        "PROJECTPROFESSIONAL" => "Project Plan 3",
        "PROJECTPREMIUM" => "Project Plan 5",
        "VISIOCLIENT" => "Visio Plan 2",
        "TEAMS_EXPLORATORY" => "Microsoft Teams Exploratory",
        "MCOEV" => "Microsoft Teams Phone Standard",
        "MCOMEETADV" => "Microsoft 365 Audio Conferencing",
        "STREAM" => "Microsoft Stream",
        "Microsoft_365_Copilot" => "Microsoft 365 Copilot",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skus_read_from_graphs_example() {
        let sku: Sku = serde_json::from_str(
            r#"{"appliesTo": "User", "capabilityStatus": "Enabled", "consumedUnits": 14,
                "prepaidUnits": {"enabled": 25, "lockedOut": 0, "suspended": 0, "warning": 2},
                "servicePlans": [{"servicePlanId": "8c09", "servicePlanName": "ADALLOM_S_O365", "provisioningStatus": "Success", "appliesTo": "Company"}],
                "skuId": "c7df2760-2c81-4ef7-b578-5b5392b571df", "skuPartNumber": "ENTERPRISEPREMIUM"}"#,
        )
        .unwrap();
        assert_eq!(sku.name(), "Office 365 E5");
        assert_eq!(sku.total(), 27);
        assert_eq!(sku.available(), 13);
        assert!(sku.assignable());

        let unknown = Sku {
            sku_part_number: "SOMETHING_NEW".into(),
            consumed_units: 9,
            ..Default::default()
        };
        assert_eq!(unknown.name(), "SOMETHING_NEW");
        assert_eq!(unknown.available(), 0);
    }

    fn licensee() -> Licensee {
        serde_json::from_str(
            r#"{"id": "u1", "userPrincipalName": "jo@contoso.com", "usageLocation": null,
                "licenseAssignmentStates": [
                  {"skuId": "C7DF2760-2C81-4EF7-B578-5B5392B571DF", "assignedByGroup": null, "state": "Active", "error": "None"},
                  {"skuId": "c7df2760-2c81-4ef7-b578-5b5392b571df", "assignedByGroup": "g1", "state": "Active", "error": "None"},
                  {"skuId": "d17b27af-3f49-4822-99f9-56a661538792", "assignedByGroup": "g2", "state": "Error", "error": "CountViolation"}
                ]}"#,
        )
        .unwrap()
    }

    #[test]
    fn direct_and_group_licences_are_told_apart() {
        let jo = licensee();
        let e5 = "c7df2760-2c81-4ef7-b578-5b5392b571df";
        let crm = "d17b27af-3f49-4822-99f9-56a661538792";
        assert!(jo.holds_directly(e5));
        assert_eq!(jo.groups_for(e5), vec!["g1"]);
        assert!(jo.problem_with(e5).is_none());
        assert!(jo.holds(crm) && !jo.holds_directly(crm));
        assert_eq!(jo.problem_with(crm).unwrap(), "Not enough licences left to assign this one.");
    }

    #[test]
    fn changes_are_checked_before_they_are_sent() {
        // These all fail before any request, so a client with no
        // credentials is enough.
        let graph = Graph::new(crate::graph::Credentials {
            tenant_id: String::new(),
            client_id: String::new(),
            client_secret: String::new(),
        });
        let jo = licensee();
        let crm = "d17b27af-3f49-4822-99f9-56a661538792".to_owned();
        let other = "84a661c4-e949-4bd2-a560-ed7766fcaf2b".to_owned();

        let err = graph.change_licences(&jo, std::slice::from_ref(&other), &[]).unwrap_err();
        assert!(err.contains("usage location"), "{err}");
        let err = graph.change_licences(&jo, &[], std::slice::from_ref(&crm)).unwrap_err();
        assert!(err.contains("through a group"), "{err}");
        let err = graph.change_licences(&jo, &[], std::slice::from_ref(&other)).unwrap_err();
        assert!(err.contains("does not have"), "{err}");
        let err = graph.change_licences(&jo, &["x' or 1 eq 1".to_owned()], &[]).unwrap_err();
        assert!(err.contains("not a licence ID"), "{err}");
        assert!(graph.change_licences(&jo, &[], &[]).is_ok());
    }
}
