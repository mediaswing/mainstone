//! Users: list, create, edit, enable or disable, reset a password, delete.

use serde_json::{Map, Value, json};

use super::models::{DirectoryObject, User};
use super::{Graph, Result, encode_query, odata_quote};

/// Everything needed to create a user, from the form or from a CSV row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewUser {
    pub display_name: String,
    pub user_principal_name: String,
    /// Defaults to the part of the UPN before the `@`.
    pub mail_nickname: String,
    pub password: String,
    pub given_name: String,
    pub surname: String,
    pub job_title: String,
    pub department: String,
    pub office_location: String,
    pub mobile_phone: String,
    /// Two-letter country code. Needed before a licence can be assigned.
    pub usage_location: String,
    pub account_enabled: bool,
    pub force_change_password: bool,
}

/// The editable profile of an existing user. An empty field clears the
/// property in Entra, which is what emptying a box should mean.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UserEdit {
    pub display_name: String,
    pub given_name: String,
    pub surname: String,
    pub job_title: String,
    pub department: String,
    pub office_location: String,
    pub mobile_phone: String,
    pub usage_location: String,
}

impl UserEdit {
    pub fn from_user(user: &User) -> Self {
        let s = |v: &Option<String>| v.clone().unwrap_or_default();
        Self {
            display_name: s(&user.display_name),
            given_name: s(&user.given_name),
            surname: s(&user.surname),
            job_title: s(&user.job_title),
            department: s(&user.department),
            office_location: s(&user.office_location),
            mobile_phone: s(&user.mobile_phone),
            usage_location: s(&user.usage_location),
        }
    }
}

/// `Some(text)` for a value, JSON `null` for an empty box.
fn text_or_null(value: &str) -> Value {
    match value.trim() {
        "" => Value::Null,
        v => Value::String(v.to_owned()),
    }
}

impl NewUser {
    /// Check what Graph would otherwise reject with a less helpful message.
    pub fn validate(&self) -> Result<()> {
        if self.display_name.trim().is_empty() {
            return Err("A display name is needed.".into());
        }
        let upn = self.user_principal_name.trim();
        if upn.is_empty() {
            return Err("A user principal name is needed.".into());
        }
        if !upn.contains('@') || upn.starts_with('@') || upn.ends_with('@') {
            return Err(format!("\"{upn}\" is not a user principal name (name@domain)."));
        }
        if self.password.is_empty() {
            return Err("A password is needed.".into());
        }
        let location = self.usage_location.trim();
        if !location.is_empty() && location.len() != 2 {
            return Err("Usage location is a two-letter country code, such as GB or US.".into());
        }
        Ok(())
    }

    fn nickname(&self) -> String {
        let given = self.mail_nickname.trim();
        if !given.is_empty() {
            return given.to_owned();
        }
        // Graph refuses a few characters here that are fine in a UPN.
        self.user_principal_name
            .trim()
            .split('@')
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
            .collect()
    }

    fn body(&self) -> Value {
        let mut body = Map::new();
        body.insert("accountEnabled".into(), self.account_enabled.into());
        body.insert("displayName".into(), self.display_name.trim().into());
        body.insert("mailNickname".into(), self.nickname().into());
        body.insert(
            "userPrincipalName".into(),
            self.user_principal_name.trim().into(),
        );
        body.insert(
            "passwordProfile".into(),
            json!({
                "password": self.password,
                "forceChangePasswordNextSignIn": self.force_change_password,
            }),
        );
        for (key, value) in [
            ("givenName", &self.given_name),
            ("surname", &self.surname),
            ("jobTitle", &self.job_title),
            ("department", &self.department),
            ("officeLocation", &self.office_location),
            ("mobilePhone", &self.mobile_phone),
            ("usageLocation", &self.usage_location),
        ] {
            if !value.trim().is_empty() {
                let value = if key == "usageLocation" {
                    value.trim().to_uppercase()
                } else {
                    value.trim().to_owned()
                };
                body.insert(key.into(), value.into());
            }
        }
        Value::Object(body)
    }
}

impl Graph {
    pub fn list_users(&self) -> Result<Vec<User>> {
        let mut users: Vec<User> =
            self.get_all(&format!("/users?$select={}&$top=999", User::SELECT))?;
        users.sort_by_key(|u| u.name().to_lowercase());
        Ok(users)
    }

    /// The groups (and directory roles) a user is a direct member of.
    pub fn user_memberships(&self, user_id: &str) -> Result<Vec<DirectoryObject>> {
        let mut groups: Vec<DirectoryObject> = self.get_all(&format!(
            "/users/{user_id}/memberOf?$select=id,displayName,mail&$top=999"
        ))?;
        groups.sort_by_key(|g| g.name().to_lowercase());
        Ok(groups)
    }

    /// Find a user's object ID from their sign-in name.
    pub fn user_id_for(&self, upn: &str) -> Result<String> {
        let upn = upn.trim();
        let filter = format!("userPrincipalName eq '{}'", odata_quote(upn));
        let found = self.get(&format!(
            "/users?$filter={}&$select=id",
            encode_query(&filter)
        ))?;
        found["value"][0]["id"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("No user has the sign-in name {upn}."))
    }

    pub fn create_user(&self, user: &NewUser) -> Result<User> {
        user.validate()?;
        let created = self
            .post("/users", &user.body())?
            .ok_or("Microsoft Graph did not return the new user.")?;
        serde_json::from_value(created).map_err(|e| e.to_string())
    }

    pub fn update_user(&self, user_id: &str, edit: &UserEdit) -> Result<()> {
        if edit.display_name.trim().is_empty() {
            return Err("A display name is needed.".into());
        }
        let location = edit.usage_location.trim();
        if !location.is_empty() && location.len() != 2 {
            return Err("Usage location is a two-letter country code, such as GB or US.".into());
        }
        self.patch(
            &format!("/users/{user_id}"),
            &json!({
                "displayName": edit.display_name.trim(),
                "givenName": text_or_null(&edit.given_name),
                "surname": text_or_null(&edit.surname),
                "jobTitle": text_or_null(&edit.job_title),
                "department": text_or_null(&edit.department),
                "officeLocation": text_or_null(&edit.office_location),
                "mobilePhone": text_or_null(&edit.mobile_phone),
                "usageLocation": text_or_null(&location.to_uppercase()),
            }),
        )
    }

    pub fn set_user_enabled(&self, user_id: &str, enabled: bool) -> Result<()> {
        self.patch(
            &format!("/users/{user_id}"),
            &json!({ "accountEnabled": enabled }),
        )
    }

    pub fn reset_password(&self, user_id: &str, password: &str, force_change: bool) -> Result<()> {
        if password.is_empty() {
            return Err("A password is needed.".into());
        }
        self.patch(
            &format!("/users/{user_id}"),
            &json!({
                "passwordProfile": {
                    "password": password,
                    "forceChangePasswordNextSignIn": force_change,
                }
            }),
        )
    }

    /// Soft-delete: the user goes to the recycle bin for 30 days.
    pub fn delete_user(&self, user_id: &str) -> Result<()> {
        self.delete(&format!("/users/{user_id}"))
    }
}

/// A password that satisfies Entra's default complexity rules: sixteen
/// characters with upper, lower, digits and symbols, and none of the
/// characters that are easy to misread when handed over. It always starts
/// with a letter, so a spreadsheet opening the import results file shows it
/// as it is instead of evaluating `=…` or `+…` as a formula.
pub fn generate_password() -> String {
    use rand::Rng as _;
    use rand::seq::SliceRandom as _;

    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghijkmnpqrstuvwxyz";
    const DIGIT: &[u8] = b"23456789";
    const SYMBOL: &[u8] = b"!#$%*+-=?@";
    let mut rng = rand::rng();
    let all: Vec<u8> = [UPPER, LOWER, DIGIT, SYMBOL].concat();

    let mut chars: Vec<u8> = [UPPER, LOWER, DIGIT, SYMBOL]
        .iter()
        .map(|set| set[rng.random_range(0..set.len())])
        .collect();
    while chars.len() < 16 {
        chars.push(all[rng.random_range(0..all.len())]);
    }
    chars.shuffle(&mut rng);
    if let Some(at) = chars.iter().position(u8::is_ascii_alphabetic) {
        chars.swap(0, at);
    }
    String::from_utf8(chars).expect("ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nickname_defaults_to_the_upn_local_part() {
        let user = NewUser {
            user_principal_name: "jo.o'brien@contoso.com".into(),
            ..Default::default()
        };
        assert_eq!(user.nickname(), "jo.obrien");
    }

    #[test]
    fn generated_passwords_have_every_class() {
        for _ in 0..50 {
            let p = generate_password();
            assert_eq!(p.len(), 16);
            assert!(p.chars().any(|c| c.is_ascii_uppercase()));
            assert!(p.chars().any(|c| c.is_ascii_lowercase()));
            assert!(p.chars().any(|c| c.is_ascii_digit()));
            assert!(p.chars().any(|c| !c.is_ascii_alphanumeric()));
            assert!(p.starts_with(|c: char| c.is_ascii_alphabetic()));
        }
    }

    #[test]
    fn validation_catches_the_obvious() {
        let mut user = NewUser {
            display_name: "Jo".into(),
            user_principal_name: "jo@contoso.com".into(),
            password: "x".into(),
            ..Default::default()
        };
        assert!(user.validate().is_ok());
        user.usage_location = "GBR".into();
        assert!(user.validate().is_err());
        user.usage_location.clear();
        user.user_principal_name = "jo".into();
        assert!(user.validate().is_err());
    }
}
