# Orcaa Desktop

Tauri 2.x desktop wrapper for the [Orcaa](https://orcaa.cloud) PWAs (Business + Admin).
One Rust source tree, two app configs, builds for Windows + macOS.

| App | productName | Identifier | Initial URL (fresh install) |
|-----|-------------|------------|-----------------------------|
| Business (main) | `Orcaa`       | `cloud.orcaa.business.desktop` | `https://auth.orcaa.cloud` |
| Admin           | `Orcaa Admin` | `cloud.orcaa.admin.desktop`    | `https://admin.orcaa.cloud` |

The wrapper loads the live hosted PWA — no SPA bundling. App updates ship via the web deploy; only new native features require a desktop rebuild.

**Platform status:**

| Platform | Status | Installer formats |
|----------|--------|-------------------|
| Windows  | ✅ Wired + CI release flow | `.msi`, `.exe` (NSIS) |
| macOS    | ✅ Configs wired; needs a Mac to build | `.dmg`, `.app` |
| Linux    | ⚪ Same source builds; not configured per-app | `.deb`, `.AppImage` |

---

## Relationship to the PWA repo

The PWA source lives in a separate repo (`orcaa-apps`). This repo only contains the desktop wrapper. They communicate at runtime:

- This wrapper loads `https://auth.orcaa.cloud` (or `https://admin.orcaa.cloud`) into a WebView2 window
- The PWA detects it's inside Tauri via `window.__TAURI_INTERNALS__` and conditionally hides the "Notifications Blocked" browser banner + routes notifications through the OS via `@tauri-apps/plugin-notification`
- No compile-time coupling. You can ship desktop releases independently of PWA releases

The PWA-side glue (the `isTauri()` helper, the native-notification bridge, the suppressed banner) lives in the `orcaa-apps` repo at:
- `shared/utils/isTauri.ts`
- `shared/services/notification/tauriNotification.ts`
- `shared/components/banners/PushNotificationPrompt.tsx` (conditional return)
- `shared/context/WebSocketContext.tsx` (toast bridge)

---

## How the multi-tenant flow works

Orcaa Business is multi-tenant — every customer lives on their own subdomain (e.g. `mygym.orcaa.cloud`). The wrapper handles this:

1. **First launch** → loads `https://auth.orcaa.cloud`. User logs in / registers / picks their domain. The PWA redirects them to their tenant subdomain.
2. **Subsequent launches** → loads the last URL the user was on (persisted in `orcaa-desktop.json` in the app data dir). If session is still valid, they land back on their tenant. If expired, the PWA redirects them to `auth.orcaa.cloud` automatically.
3. **Logout** → the web app navigates to `auth.orcaa.cloud`. That URL gets saved as the "last URL" so the next launch goes straight to the login screen.
4. **External links** (OAuth providers, Stripe, marketing pages — anything not `*.orcaa.cloud`) are opened in the user's default browser via `tauri-plugin-opener`. This avoids Google/Microsoft's webview-OAuth block and keeps password managers in the loop.

The internal-URL allowlist lives in [`src-tauri/src/lib.rs`](src-tauri/src/lib.rs) — `is_internal_url()`. It allows `orcaa.cloud` + any `*.orcaa.cloud` + any `*.orcaa.test` (dev).

---

## Background mode (tray + close behavior)

Closing the window (`X` button) **does NOT quit the app** — it minimizes to the system tray. This keeps the WebSocket connection alive so notifications continue to arrive even when no window is visible (the Slack/Discord/Teams pattern).

| Action | Result |
|--------|--------|
| Click `X` (close window) | Window hides; tray icon stays; WebSocket stays connected |
| Left-click tray icon | Re-opens / focuses the window |
| Right-click tray icon → "Open Orcaa" | Same as above |
| Right-click tray icon → "Quit" | Truly exits |
| Launching the app a second time while one is already running | Focuses the existing window (single-instance via `tauri-plugin-single-instance`) |

Memory cost: ~150–200 MB (WebView2 + Rust shell). Negligible CPU when idle.

---

## Notifications

Three layers:

1. **Browser-permission banner suppression** — the PWA detects Tauri at runtime and skips its "Notifications Blocked" banner, which would be meaningless inside WebView2.
2. **In-app WebSocket events → native Windows toasts** — when a Reverb broadcast arrives and the window isn't focused, the PWA bridges through `@tauri-apps/plugin-notification` to fire a real Windows Action Center toast.
3. **Background delivery** — covered by **(2) + the tray behavior above**. As long as the app is running (even minimized to tray), the WebSocket stays connected and toasts fire.

Push-when-truly-quit (after user picks Quit in tray) is a v2 concern — requires Windows Notification Service integration on the backend.

---

## Local development

### Prereqs (one-time per machine)

**Windows:**
1. **Rust (MSVC)** — install via [rustup.rs](https://rustup.rs)
2. **Visual Studio 2022 Build Tools** — "Desktop development with C++" workload (~3 GB)
3. **WebView2 Runtime** — preinstalled on Win10 1809+ / Win11
4. **Node 20+ and pnpm 10+**
5. **Smart App Control: OFF** — Win11's SAC blocks unsigned build scripts; incompatible with Rust dev. (One-way switch — see Windows Security → App & browser control → Smart App Control settings.)

**macOS:**
1. **Xcode Command Line Tools** — `xcode-select --install`
2. **Rust** — `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. **Node 20+ and pnpm 10+** — `brew install node@20 && npm install -g pnpm`

Then in this repo:

```bash
pnpm install
```

### Generate icons

```bash
pnpm icons:business
pnpm icons:admin
```

Reads source PNGs from `source-icons/{business,admin}.png` and produces the full Tauri icon set (`32x32.png`, `128x128.png`, `icon.ico`, `icon.icns`, etc.) into `src-tauri/icons/{business,admin}/`.

> **Heads up:** `business.png` and `admin.png` start out identical. Replace `admin.png` with a differentiated variant before public release so users can tell the two apps apart in taskbar / Start menu.

### Dev

```bash
pnpm dev:business   # opens window at https://auth.orcaa.cloud
pnpm dev:admin      # opens window at https://admin.orcaa.cloud
```

DevTools: right-click → Inspect (debug builds only).

### Local release build

```bash
pnpm build:business
pnpm build:admin
```

Output (Windows):

```
src-tauri/target/release/bundle/
  msi/   Orcaa_1.0.0_x64_en-US.msi
  nsis/  Orcaa_1.0.0_x64-setup.exe
         Orcaa_1.0.0_x64-setup.nsis.zip   (updater payload)
```

Output (macOS): `dmg/Orcaa_1.0.0_aarch64.dmg`, `macos/Orcaa.app(.tar.gz)`.

---

## Releasing via GitHub Actions (recommended)

A workflow at [`.github/workflows/release.yml`](.github/workflows/release.yml) builds and publishes a GitHub release on every tag push. No manual build required.

### One-time setup

**1. Configure repo secrets** (Settings → Secrets and variables → Actions → New repository secret):

| Secret name | Value |
|-------------|-------|
| `TAURI_SIGNING_PRIVATE_KEY` | Paste the **entire contents** of `~/.tauri/orcaa.key` (the private key file, including the `untrusted comment:` header line). |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | The password you set when generating the key. |

Note: paste the **contents**, not the path. tauri-action expects the key as a string.

**2. Confirm `plugins.updater.pubkey`** in [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json) is set to the matching public key (just the base64 string from `orcaa.key.pub`).

### Cutting a release

```bash
# 1. Bump version in the per-app config
# Edit src-tauri/tauri.business.conf.json → "version": "1.0.1"

# 2. Commit + tag + push
git add src-tauri/tauri.business.conf.json
git commit -m "chore: bump business to 1.0.1"
git tag v1.0.1
git push origin main --tags
```

The workflow then:
1. Spins up `windows-latest`, installs Rust + Node + pnpm, restores caches
2. Generates icons from `source-icons/business.png`
3. Runs `tauri build --config src-tauri/tauri.business.conf.json`
4. Creates a GitHub release tagged `v1.0.1`
5. Uploads the `.exe`, `.msi`, `.nsis.zip`, `.sig`, and `latest.json`
6. Renames the NSIS installer to `orcaa-desktop.exe` so the landing-site link stays stable

Build time: ~5–10 min with caches, ~15–20 min cold. GitHub Actions is free for public repos and gives 2,000 Linux-minute equivalents/month for private (Windows costs 2×).

### Manual run (without a tag)

Go to **Actions** tab → **Build & Release Desktop** → **Run workflow** → pick `business` or `admin`. Uses the version from the chosen app's config file.

### Releasing the admin app

The default workflow builds the **business** app. For admin: trigger via "Run workflow" → admin, OR clone the workflow into `.github/workflows/release-admin.yml` and change the matrix.

---

## macOS-specific notes

### Signing (deferred)

Unsigned `.app` files trigger Gatekeeper warnings. To remove them:

1. Join the [Apple Developer Program](https://developer.apple.com) ($99/yr)
2. Create a "Developer ID Application" cert in Xcode → Settings → Accounts → Manage Certificates
3. Set `bundle.macOS.signingIdentity` in [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json) to the cert's Common Name
4. For notarization, add env vars before building:
   ```bash
   export APPLE_ID="you@example.com"
   export APPLE_PASSWORD="app-specific-password"
   export APPLE_TEAM_ID="ABCD123456"
   pnpm build:business
   ```

### Mac publish flow

Same key as Windows (Tauri's minisign key is cross-platform):

```bash
export TAURI_SIGNING_PRIVATE_KEY="$HOME/.tauri/orcaa.key"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD="your-password"

pnpm build:business
./scripts/publish-update.sh business 1.0.1 "Bug fixes."
```

Merges the `darwin-*` entry into the same `latest.json` you uploaded from the Windows runner. Requires `jq` (`brew install jq`).

---

## Auto-updater

### One-time: generate signing keys

```bash
pnpm tauri signer generate -w ~/.tauri/orcaa.key      # macOS / Linux
pnpm tauri signer generate -w "%USERPROFILE%\.tauri\orcaa.key"   # Windows (CMD)
```

Produces:
- `orcaa.key` — **private**, never commit. Store in 1Password + add to GitHub repo secrets as `TAURI_SIGNING_PRIVATE_KEY`.
- `orcaa.key.pub` — public. Paste the base64 string into [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json) → `plugins.updater.pubkey`.

Same key pair signs both Business and Admin.

### Update endpoint

Set in [`src-tauri/tauri.conf.json`](src-tauri/tauri.conf.json):

```json
"endpoints": [
  "https://github.com/LogixOrg/orcaa-desktop/releases/latest/download/latest.json"
]
```

GitHub auto-redirects `/releases/latest/download/<filename>` to the most recent release's assets. The updater polls this on app launch, downloads the `.nsis.zip`, verifies signature against the pubkey, and prompts restart if a newer version exists.

---

## Where state lives on the user's machine

| Data | Location (Windows) |
|------|-------------------|
| Cookies, localStorage, IndexedDB, service worker cache (the PWA itself) | `%LOCALAPPDATA%\<identifier>\EBWebView\` (WebView2 user data) |
| Last URL, future user prefs | `%APPDATA%\<identifier>\orcaa-desktop.json` (Tauri store plugin) |
| App install (NSIS) | `%LOCALAPPDATA%\Programs\Orcaa\` |
| App install (MSI per-machine) | `C:\Program Files\Orcaa\` |

Uninstalling via Apps & Features removes the install dir; WebView2 user data + Tauri store persist unless the user deletes them. Logged-in sessions survive reinstall, matching PWA browser behavior.

---

## Distribution

Installers ship via **GitHub Releases** at [github.com/LogixOrg/orcaa-desktop/releases](https://github.com/LogixOrg/orcaa-desktop/releases).

The [Orcaa landing site](https://orcaa.cloud) "Download for Windows" CTA links to:

```
https://github.com/LogixOrg/orcaa-desktop/releases/latest/download/orcaa-desktop.exe
```

The GitHub Actions workflow renames the versioned NSIS installer to `orcaa-desktop.exe` on each release, so this URL never changes — bumping versions doesn't break the landing page.

---

## Follow-ups (not in v1)

- **Code signing (Windows)** — Sectigo OV cert (~$200/yr) or DigiCert EV cert (~$400/yr). Removes SmartScreen warnings.
- **Code signing + notarization (macOS)** — Apple Developer Program ($99/yr). Removes Gatekeeper warnings.
- **macOS GitHub Actions matrix** — add `macos-14` runner (Apple Silicon) and `macos-13` runner (Intel) to the workflow, parallel to `windows-latest`. Same workflow, two more jobs.
- **Native deep-link auth** (`orcaa://`) — register a custom URL scheme so the auth subdomain can hand control back to a running desktop app after a system-browser SSO. Required if Orcaa moves to social-only login (Google/Microsoft).
- **Window state persistence** — add `tauri-plugin-window-state` to remember window size + position across sessions.
- **In-app updater UI** — replace the default Tauri restart prompt with a branded toast/dialog (`plugins.updater.dialog = false` already; just needs JS-side handler).
- **Push when truly quit** — current flow needs the app to be running (background tray is fine). For toasts after Quit, integrate Windows Notification Service (WNS) — needs backend push channel beyond Web Push.
- **Chat / voice-call OS toasts** — currently only the main `.Notification` broadcast fires OS toasts; chat messages and voice calls fire `chat-message` / `voice-call-incoming` custom events. Hook the bridge into those handlers in the PWA's `WebSocketContext.tsx`.
- **Notification click → focus app + navigate** — Tauri notifications can carry an `actionTypeId`. Wire a click handler that brings the window to front and navigates to the relevant URL.
