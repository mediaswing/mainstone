# Mainstone Cloud System

A desktop app for managing Microsoft Entra ID: users, groups, and devices,
including Intune's remote actions. It can import and export users as CSV, and
copy the whole directory to a MariaDB server. Written in Rust, with
[egui](https://github.com/emilk/egui) for the interface. It runs on macOS,
Windows and Linux.

The program is called `mainstone` on disk and in your terminal. The window title
says **Mainstone Cloud System**.

## Signing in

The app signs in as an **app registration** using the OAuth 2.0
client-credentials grant. You give it a tenant ID, a client ID and a client
secret, and it gets an app-only token straight from Microsoft. There is no
browser, no device code and no signed-in user. It can do exactly what the
registration's *application* permissions allow, and nothing more.

### Creating the app registration

1. In the [Entra admin centre](https://entra.microsoft.com), go to
   **Identity → Applications → App registrations → New registration**. Give
   it a name and leave the redirect URI empty.
2. On its **Overview** page, copy the **Application (client) ID** and the
   **Directory (tenant) ID**.
3. Under **Certificates & secrets → Client secrets**, add a secret and copy
   its **Value**. You copy the Value, not the Secret ID, and it is shown only
   once.
4. Grant the permissions below. The easiest way is from the app: enter the
   tenant and client IDs on the **Connection** tab and press **Grant
   Permissions…** (see [Granting permissions from the
   app](#granting-permissions-from-the-app)). Or, in the portal, go to **API
   permissions → Add a permission → Microsoft Graph → Application
   permissions**, add them, and press **Grant admin consent**.

| Permission | Used for |
| --- | --- |
| `User.ReadWrite.All` | Listing, creating, editing, enabling, disabling and deleting users |
| `User-PasswordProfile.ReadWrite.All` | Resetting passwords |
| `Group.ReadWrite.All` | Listing, creating and deleting groups |
| `GroupMember.ReadWrite.All` | Adding and removing group members |
| `Device.ReadWrite.All` | Listing, enabling, disabling and deleting Entra devices |
| `DeviceManagementManagedDevices.ReadWrite.All` | Listing Intune managed devices |
| `DeviceManagementManagedDevices.PrivilegedOperations.All` | Intune actions: sync, restart, lock, Defender scan, retire, wipe |
| `LicenseAssignment.ReadWrite.All` | Listing subscriptions, and assigning and removing licences |
| `AuditLog.Read.All` | Reading the sign-in and audit logs |
| `MailboxSettings.ReadWrite` | Automatic replies, and a mailbox's time zone and language (optional) |
| `Reports.Read.All` | Mailbox sizes and last activity (optional) |
| `Organization.Read.All` | Showing the tenant's name (optional) |

For a read-only setup, grant the `.Read.All` versions of these instead. The
lists will load, and any change you try will be refused with a message saying
so. After you sign in, the **Connection** tab shows which of these
permissions the token actually carries.

`LicenseAssignment.ReadWrite.All` is the narrowest permission for licences,
but not the only one that works: `User.ReadWrite.All` can assign them too,
and `Organization.Read.All` can list the subscriptions. So the Licensing tab
works with those two even when the Connection tab shows
`LicenseAssignment.ReadWrite.All` as missing.

Two limits come from Entra itself, not from this app. An app with
`User.ReadWrite.All` cannot reset the password of, or delete, a user who
holds an admin role, unless the app has been given a suitable directory role
too. And users synchronised from on-premises Active Directory have to be
changed there.

### Granting permissions from the app

An app can't give itself permissions using its own secret. Only an
administrator can grant them. So **Grant Permissions…** on the
**Connection** tab borrows one, once:

1. Your browser opens Microsoft's sign-in page. Sign in as a **Global
   Administrator** or **Privileged Role Administrator**. Granting
   application permissions on Microsoft Graph is limited to those two
   roles.
2. The app adds any missing permissions from the table above to the app
   registration's **API permissions** list, then grants admin consent for
   them.
3. It signs in again with the client secret, so the **Connection** tab shows
   the new permissions straight away.

The administrator's sign-in is used for this alone. It is held in memory
only, and never saved.

This sign-in uses **Microsoft Graph Command Line Tools**, Microsoft's own
app for working with Graph interactively (the one behind `Connect-MgGraph`).
Because of that, your app registration needs no redirect URI or other
changes. The first time, Microsoft may ask the administrator to consent to
that tool reading and writing app registrations and role assignments. The
flow is an OAuth authorisation code flow with PKCE. Microsoft sends the
browser back to a listener the app opens on `localhost` for that one
sign-in, which accepts connections only from the same computer.

### Where things are kept

The tenant ID, the client ID and the MariaDB server details are saved in
`config.json` in the app's data directory:

- macOS: `~/Library/Application Support/GraphicalCloudManager`
- Linux: `~/.local/share/GraphicalCloudManager`
- Windows: `%APPDATA%\GraphicalCloudManager`

Secrets never go in that file. If you tick **Remember the secret**, the
client secret is saved in `.gcm-credentials.json` in your home directory, and
the app signs in by itself at the next start. The MariaDB password can be
remembered in the same file. On macOS and Linux the file is set to mode
`0400`, read-only and readable only by you. On Windows it gets the read-only
attribute, and who can read it is decided by your user profile's own
permissions, which normally admit only you and administrators. The app makes
it writable just long enough to update it, and deletes it once nothing is
left in it.

It is plain JSON, so you can also write it yourself, for example to set up a
machine without typing anything into the window:

```json
{
  "tenant_id": "00000000-0000-0000-0000-000000000000",
  "client_id": "00000000-0000-0000-0000-000000000000",
  "client_secret": "the secret's Value",
  "mariadb_password": "optional"
}
```

```sh
chmod 400 ~/.gcm-credentials.json
```

If the app has no settings saved yet, it takes the tenant and client IDs from
this file and signs in straight away. The client secret is only ever sent for
the tenant and client named next to it. Anyone who can read the file can sign
in as the app registration, so treat it like a password. When a secret is
rotated in Entra, sign in once with the new value and the file is updated.

## The window

Eight tabs run down the left-hand side, the same layout as
[watchspend](https://github.com/mediaswing/watchspend). The status bar along
the bottom shows which tenant you are signed in to, what the app is doing,
and the result of the last action. Every Graph and MariaDB call runs in the
background, so the window never freezes.

**Connection** holds the sign-in form. After you sign in, it lists the
permissions the token carries.

**Users** lists every user in the tenant. You can search by name, sign-in
name, mail, department or job title. Selecting a user opens a details panel
with their properties and group memberships. From there you can **Edit**
the profile, **Enable** or **Disable** the account, **Reset password** (a
strong password is generated for you), or **Delete** the user. A deleted user
can be restored from the admin centre for 30 days. **New user** opens a
creation form.

A user with an Exchange Online mailbox has a **Mailbox** section in their
details, and a **Mailbox** column in the list shows each mailbox's size,
marked when it is nearly full. **Automatic replies…** turns out-of-office
replies on or off, or schedules them between two times, with one message
for people inside the organisation and another, or none, for people
outside. Messages are edited as plain text; saving a changed message
replaces any formatting it was given in Outlook, and a message left alone
is not touched. Users without a mailbox, and every user in a tenant without
Exchange Online, just say so.

The sizes come from Microsoft 365's mailbox usage report, which Microsoft
refreshes about once a day, so they can be a day or two old. Tenants hide
user names in reports by default, and then the sizes can't be matched to
anyone. To show them, turn off **Display concealed user, group, and site
names in all reports** in the [Microsoft 365 admin
centre](https://admin.microsoft.com) under **Settings → Org settings →
Reports**.

Shared mailboxes, mailbox permissions (Full Access, Send As), aliases and
forwarding are not in Microsoft Graph, so the app cannot change them.

The toolbar also has the bulk tools:

- **Import CSV…** reads a file of users and shows each row before anything
  is created. Rows with problems are marked and skipped. When the import
  finishes, a summary lists any failures, and **Save results…** writes a CSV
  with each row's outcome and new object ID.
- **Export CSV…** writes the users currently shown, so a search narrows the
  export too.
- **Save CSV template…** writes an import file with the expected header and
  one example row.

**Groups** lists every group with its type (Microsoft 365, Security,
Mail-enabled security, Distribution) and whether membership is assigned or
dynamic. Selecting a group shows its members. You can add a user by sign-in
name or remove a member. Dynamic groups are read-only, because their members
come from a rule. **New group** creates a Security or Microsoft 365 group.

**Devices** joins Entra devices with their Intune records on the Entra device
ID, so each physical device appears once. You can filter to all devices, those
managed by Intune, those not in Intune, or those not compliant. Selecting a
device shows both halves. A managed device offers the Intune actions:

| Action | What it does |
| --- | --- |
| Sync | Asks the device to check in now |
| Restart | Restarts the device |
| Remote lock | Locks the screen (iOS, iPadOS, Android, macOS) |
| Defender quick / full scan | Runs a Microsoft Defender scan (Windows) |
| Retire | Removes company data and stops managing the device |
| Wipe | Factory-resets the device |
| Delete from Intune | Removes the Intune record only |

Restart, lock, retire, wipe and delete each ask for confirmation first. The
Entra half can be enabled, disabled, or deleted from Entra ID.

If the tenant has no Intune licence, or the app lacks the Intune permission,
the Entra devices are still shown, with a note saying why the Intune devices
are missing.

**Licensing** lists the tenant's subscriptions, with how many of each are
assigned, how many are left, and their status. Products are shown by the
names the admin centres use where the app knows them, and by Microsoft's
part number otherwise. Selecting one lists who holds it, and whether
directly or through a group. **Assign** gives it to a user by sign-in name,
and **✕** removes a direct assignment after asking first. A licence that
comes from a group can only be removed by taking the user out of the group.
Microsoft refuses to assign a licence to a user with no usage location, so
set one with **Edit** on the Users tab first.

A user's own licences are also listed in their details on the Users tab,
where **Licences…** ticks and unticks products for them in one go.

**Logs** reads the tenant's **Sign-ins** and its **Audit log**: who signed
in to what, from where, and whether it worked, and every change made in
Entra ID and by whom. Choose how far back to look, from the last hour to the
last 30 days. Optionally, give a sign-in name, to see that user's sign-ins
or the changes they made, and tick **Failures only**. Then press **Load**.
Selecting an entry shows everything about it, including the old and new
values of whatever was changed, and **Export … shown** writes what is
listed to a CSV file. The **Sign-ins** button in a user's details on the
Users tab opens their sign-ins here.

A load reads at most the newest 5,000 entries, and says so when there were
more; a shorter range or a single user brings in the rest. Entra keeps both
logs for 7 days, or 30 with Entra ID P1 or P2. Reading sign-ins through
Microsoft Graph needs one of those licences in the tenant; the audit log
does not.

**Export** copies the directory into a MariaDB or MySQL server; see below.

**Settings** chooses light, dark, or following the system, turns update
checks and the debug log on or off, and has a **Check now** button for
updates.

## Updates

When the app starts, it asks GitHub whether there's a newer release of
[mediaswing/mainstone](https://github.com/mediaswing/mainstone/releases). You can turn
this off under **Settings**. If there is one, a banner across the top of
the window offers it. **Install and restart** does the following:

1. Downloads the package for your platform from the release.
2. Checks it against the SHA-256 checksum GitHub publishes for that file. If
   they don't match, nothing is installed.
3. Replaces this copy of the app. On macOS that's the `.app` you're running
   it from. On Windows it's `mainstone.exe`. On Ubuntu it installs the `.deb` with
   `apt-get` through `pkexec`, which asks for an administrator's password.
4. Starts the new version and closes the old one.

**Skip this version** stops a release being offered again. A newer one is
still offered.

On macOS, the app has to be somewhere you can write to, such as
**Applications**. The update arrives without macOS's quarantine flag, so
unlike the first download it opens without the **Open Anyway** step.

A copy that wasn't installed from a release package can't replace itself.
That includes one started with `cargo run`, or a binary copied out of the
`.deb`. For those, the banner links to the release page instead.

## The debug log

The app writes `gcm-debug.log` in its data directory (see [Where things are
kept](#where-things-are-kept)). It is on by default; untick **Write a debug
log** under **Settings** to stop it. **Show the log file** opens it in the
file manager. If it has been turned off, `GCM_DEBUG=1` logs a single run from
the very start:

```sh
GCM_DEBUG=1 mainstone
```

Each request to Microsoft Graph is logged with its method, path, status, how
long it took and Microsoft's request ID, which Microsoft support will ask for.
So are each step of a MariaDB export, every background job, and any crash.
The client secret, access tokens, passwords and request bodies are never
written to it. Paths and messages can include object IDs, sign-in names and
group names, so look through the file before sharing it.

On macOS and Linux the file is readable only by you; on Windows it takes the
permissions of your user profile. Once it passes 5 MB, it is moved to
`gcm-debug.log.1` the next time logging starts, replacing any older one.

Warnings still go to the terminal as before, and `RUST_LOG` controls that as
usual.

## CSV import format

The first row is a header. Columns are matched by name, ignoring case, spaces,
underscores and hyphens, so `userPrincipalName`, `User Principal Name` and
`user_principal_name` all work. Columns the app does not know are ignored.
That means a file from **Export CSV…** can be edited and imported again.

| Column | Required | Notes |
| --- | --- | --- |
| `displayName` | yes | |
| `userPrincipalName` | yes | `name@yourdomain`, on a domain verified in the tenant |
| `password` | no | Left blank, a 16-character password is generated and written to the results file |
| `mailNickname` | no | Defaults to the part of the sign-in name before the `@` |
| `givenName`, `surname`, `jobTitle`, `department`, `officeLocation`, `mobilePhone` | no | |
| `usageLocation` | no | Two-letter country code such as `GB`. Needed before a licence can be assigned |
| `accountEnabled` | no | `true`/`false`, `yes`/`no` or `1`/`0`. Defaults to `true` |
| `forceChangePasswordNextSignIn` | no | Same values. Defaults to `true` |

The results file can contain passwords. On macOS and Linux it is written
readable only by you. On Windows it takes the permissions of the folder you
save it in, so save it inside your own user folder rather than a shared one.
Either way, keep it somewhere safe and delete it once the passwords have been
handed over.

## Exporting to MariaDB

Enter the server's details and press **Test connection**. Then choose what to
export and press **Export to MariaDB**. The export reads fresh from Graph
rather than from what the tabs have loaded, so the database matches the tenant
as it is at that moment.

The five tables are created if they do not exist. Each one is written in a
single transaction, so a failure part-way leaves that table as it was.

| Table | Contents | Key |
| --- | --- | --- |
| `gcm_users` | One row per user | `tenant_id, id` |
| `gcm_groups` | One row per group | `tenant_id, id` |
| `gcm_group_members` | One row per direct membership | `tenant_id, group_id, member_id` |
| `gcm_devices` | One row per device, Entra and Intune joined | `tenant_id, row_key` |
| `gcm_mailboxes` | One row per mailbox in the usage report, with its size and quotas in bytes | `tenant_id, user_principal_name` |

`gcm_mailboxes` is keyed on the sign-in name in lower case, because the
usage report has no object IDs; join it to `gcm_users` on
`user_principal_name`. If the report can't be read, because the permission
is missing or names are concealed, the other tables are exported anyway and
the result says why the mailboxes were left out.

Every row carries the tenant ID, so several tenants can share one database.
Running the export again updates rows in place. Ticking **Mirror** also
removes rows for objects that are no longer in the tenant. Times are stored in
UTC, and `exported_at` records when each row was last written. The full
`CREATE TABLE` statements are in the **Table definitions** section at the
bottom of the Export tab.

The account needs `CREATE`, `SELECT`, `INSERT`, `UPDATE` and `DELETE` on the
database:

```sql
CREATE DATABASE gcm CHARACTER SET utf8mb4;
CREATE USER 'gcm'@'%' IDENTIFIED BY 'a-strong-password';
GRANT CREATE, SELECT, INSERT, UPDATE, DELETE ON gcm.* TO 'gcm'@'%';
```

TLS is optional. If your server uses a certificate from a private authority,
you can point the app at that authority's `.pem` file.

## Building

You need a recent stable Rust toolchain (edition 2024).

```sh
cargo run --release
```

On Linux, install the window-system headers first:

```sh
sudo apt install build-essential pkg-config perl make \
    libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
    libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

### Packages

The packages are built by `mainstone-package`, a second binary in this crate. Its
code for each platform is in `src/package/`: `macos.rs`, `ubuntu.rs` and
`windows.rs`. It builds the package for the platform it runs on, and writes
it to `dist/`. GitHub Actions runs the same command (see
`.github/workflows/build.yml`), and publishes the packages when a `v*` tag is
pushed. The release notes are that version's section of `CHANGELOG.md`, so
before tagging `v1.1.0`, rename `## [Unreleased]` to `## [1.1.0]`. A tag
with no section of its own fails the release rather than publishing it
without notes.

#### macOS: an ad-hoc signed `.app`

```sh
cargo run --release --bin mainstone-package                # this Mac's architecture
cargo run --release --bin mainstone-package -- --universal # Apple silicon and Intel together
```

This writes `dist/Mainstone Cloud System.app` and a zip of it. The bundle is
signed ad hoc (`codesign --sign -`), which is enough for it to run on Apple
silicon. It is not notarised, so the first time a downloaded copy is opened,
macOS will refuse. To allow it, go to **System Settings → Privacy &
Security**, scroll down, and press **Open Anyway**. To give the app an icon,
put an `AppIcon.icns` in `packaging/macos/` before building.

The macOS package published with each release is built for Apple silicon
only, as `mainstone-<version>-macos-aarch64.zip`. It will not run on an Intel Mac;
build one there with the first command above, or anywhere with `--universal`.

#### Ubuntu: a `.deb`

```sh
sudo apt install dpkg-dev
cargo run --release --bin mainstone-package
sudo apt install ./dist/mainstone-*.deb
```

This installs `/usr/bin/mainstone` and a **Mainstone Cloud System** entry in the
applications menu. File dialogs use the desktop portal
(`xdg-desktop-portal`), which is present on a standard Ubuntu desktop. To give
the menu entry an icon, put a 256×256 `mainstone.png` in `packaging/ubuntu/`.

#### Windows: a `.zip`

```sh
cargo run --release --bin mainstone-package
```

This writes `dist\mainstone-<version>-windows-x86_64.zip`, holding `mainstone.exe` and
the licences.

### Tests

```sh
cargo test
```

## Licence

MIT. See [`LICENSE`](LICENSE). The bundled Ubuntu Bold font is under the
[Ubuntu Font Licence](assets/fonts/UBUNTU-FONT-LICENCE-1.0.txt).
