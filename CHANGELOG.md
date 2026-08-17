# Changelog

Entries here are **user-facing**: the section matching the released version is
what the branded update window shows every user under "What's new". Keep bullets
short, benefit-first, and free of internal jargon (English — the update window's
localized fallback covers builds released without an entry).

The release workflow's `manifest` job reads the `## <version>` section verbatim;
no entry → users see the generic "Improvements and fixes." line and CI emits a
warning. Adding the entry is part of the version-bump checklist in the README.

## 1.1.8

- POS Station: instant receipt printing with no print dialog on thermal
  printers (network or serial), automatic cash-drawer opening on cash sales,
  and a printer test page — set it up in Point of Sale settings.
- Kiosk mode for counter PCs: launch with --kiosk and the app starts
  fullscreen, ready for the register.
- A one-time offer to start Orcaa with your computer, so notifications reach
  you all day.
- A branded Orcaa titlebar: the app's own top bar now doubles as the window
  titlebar on Windows, with the window buttons built in — no more generic
  system frame. On Mac the familiar traffic lights stay.
- Unread badge on the taskbar (Windows) and dock (Mac): see at a glance that
  something needs you, even after a toast has gone.
- Summon Orcaa from anywhere with Ctrl+Shift+O (admin app: Ctrl+Shift+A).
- Tray quick actions: jump straight to Point of Sale or your Dashboard from
  the tray icon's menu.
- Update prompts now show real release notes instead of a generic line.
- Security hardening of the app's built-in pages.
- Signed Windows installer (no more SmartScreen warning) once Azure Trusted
  Signing secrets are configured.

## 1.1.6

- Improvements and fixes.
