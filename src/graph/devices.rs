//! Devices: the Entra device objects, the Intune managed devices, and the
//! remote actions Intune can send to them.

use serde_json::json;

use super::models::{Device, DeviceRow, ManagedDevice, join_devices};
use super::{Graph, Result};

/// What came back from loading devices. Intune is optional: a tenant without
/// an Intune licence, or an app without the Intune permission, still has
/// Entra devices worth showing.
pub struct DeviceList {
    pub rows: Vec<DeviceRow>,
    pub intune_error: Option<String>,
}

/// A remote action Intune can send to a managed device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntuneAction {
    Sync,
    Restart,
    Lock,
    QuickScan,
    FullScan,
    Retire,
    Wipe,
    Delete,
}

impl IntuneAction {
    pub const ALL: [Self; 8] = [
        Self::Sync,
        Self::Restart,
        Self::Lock,
        Self::QuickScan,
        Self::FullScan,
        Self::Retire,
        Self::Wipe,
        Self::Delete,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Sync => "Sync",
            Self::Restart => "Restart",
            Self::Lock => "Remote lock",
            Self::QuickScan => "Defender quick scan",
            Self::FullScan => "Defender full scan",
            Self::Retire => "Retire",
            Self::Wipe => "Wipe",
            Self::Delete => "Delete from Intune",
        }
    }

    /// What will happen, said plainly before anyone presses the button.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Sync => "Ask the device to check in with Intune now.",
            Self::Restart => "Restart the device. Unsaved work on it will be lost.",
            Self::Lock => "Lock the device's screen (mobile devices).",
            Self::QuickScan => "Run a Microsoft Defender quick scan (Windows).",
            Self::FullScan => "Run a Microsoft Defender full scan (Windows).",
            Self::Retire => {
                "Remove company data, apps and profiles, and stop managing the device. Personal data is kept."
            }
            Self::Wipe => {
                "Factory-reset the device. Everything on it, company and personal, is erased. This cannot be undone."
            }
            Self::Delete => {
                "Remove the device's record from Intune without touching the device. It re-appears if it checks in again."
            }
        }
    }

    /// Whether the action needs a second "are you sure".
    pub fn is_drastic(self) -> bool {
        !matches!(self, Self::Sync | Self::QuickScan | Self::FullScan)
    }

    pub fn applies_to(self, device: &ManagedDevice) -> bool {
        let os = device
            .operating_system
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        match self {
            Self::QuickScan | Self::FullScan => os.contains("windows"),
            Self::Lock => {
                os.contains("ios") || os.contains("ipados") || os.contains("android") || os.contains("macos")
            }
            _ => true,
        }
    }
}

impl Graph {
    pub fn list_devices(&self) -> Result<DeviceList> {
        let entra: Vec<Device> =
            self.get_all(&format!("/devices?$select={}&$top=999", Device::SELECT))?;
        let (intune, intune_error) = match self.get_all::<ManagedDevice>(&format!(
            "/deviceManagement/managedDevices?$select={}",
            ManagedDevice::SELECT
        )) {
            Ok(list) => (list, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        Ok(DeviceList {
            rows: join_devices(entra, intune),
            intune_error,
        })
    }

    pub fn intune_action(&self, managed_id: &str, action: IntuneAction) -> Result<()> {
        let base = format!("/deviceManagement/managedDevices/{managed_id}");
        match action {
            IntuneAction::Sync => self.post_empty(&format!("{base}/syncDevice")),
            IntuneAction::Restart => self.post_empty(&format!("{base}/rebootNow")),
            IntuneAction::Lock => self.post_empty(&format!("{base}/remoteLock")),
            IntuneAction::QuickScan | IntuneAction::FullScan => self
                .post(
                    &format!("{base}/windowsDefenderScan"),
                    &json!({ "quickScan": action == IntuneAction::QuickScan }),
                )
                .map(drop),
            IntuneAction::Retire => self.post_empty(&format!("{base}/retire")),
            IntuneAction::Wipe => self
                .post(
                    &format!("{base}/wipe"),
                    &json!({ "keepEnrollmentData": false, "keepUserData": false }),
                )
                .map(drop),
            IntuneAction::Delete => self.delete(&base),
        }
    }

    pub fn set_device_enabled(&self, object_id: &str, enabled: bool) -> Result<()> {
        self.patch(
            &format!("/devices/{object_id}"),
            &json!({ "accountEnabled": enabled }),
        )
    }

    pub fn delete_device(&self, object_id: &str) -> Result<()> {
        self.delete(&format!("/devices/{object_id}"))
    }
}
