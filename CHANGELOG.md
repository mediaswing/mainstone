# Changelog

## [Unreleased]

### Added

- A sound when an action works and another when it fails, alongside the
  message in the status bar. The sounds are the same as speechout's, and
  **Settings → Sounds** turns them off.
- Keyboard shortcuts: ⌘1–⌘9 (Ctrl on Windows and Linux) for the tabs,
  Ctrl+Tab and Ctrl+Shift+Tab, ⌘, for Settings, ⌘F to search, ⌘R or F5 to
  refresh, ⌘N for a new user or group, Delete for the selected user or group,
  and Escape to clear the selection. They are listed in **Settings**, and on
  the buttons' tooltips.
- Right-click menus on every list's rows, on group members, licence holders
  and a user's groups, and on every value in a details panel: the actions
  for that item, links to it on another tab, and copying what it is known by.

### Changed

- Enter in a form's text box now presses its main button: Sign in, Take
  snapshot, and Create, Save or Reset in the user, group and password
  dialogs, as it already did for adding a member, assigning a licence and
  filtering the logs.

## [1.6.0]

### Added

- A **Servers** tab that takes a snapshot of an Ubuntu or Debian server over
  SSH, signing in with a password or a key file. It shows the operating
  system, kernel, installed packages, waiting updates (security updates
  marked) and whether a reboot is needed, and saves them as CSV or JSON.
  Host keys are checked against `~/.ssh/known_hosts`.

## [1.5.0]

### Changed

- Renamed to **Mainstone Cloud System**. The program is now `mainstone`
  (`mainstone.exe` on Windows), the packages are `mainstone-<version>-…`, and
  updates come from `mediaswing/mainstone`. Settings, remembered secrets and
  the debug log stay where they were.
- On the Connection tab, **Sign in again** is now **Refresh Token**, sharing
  a line with **Sign Out**, with **Grant Permissions…** underneath spanning
  both.
- The debug log is on by default.
- Problems granting permissions, and logs that can't be read, now appear in
  a dialog box rather than in the status bar. A sign-in log that needs an
  Entra ID P1 or P2 licence says so, instead of suggesting a missing
  permission.