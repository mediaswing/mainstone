# Changelog

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