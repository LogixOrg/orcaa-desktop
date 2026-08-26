# Desktop source icons

Drop the desktop app's PNG here. The Tauri icon generator reads from this
folder, NOT from `apps/*/src/assets/public/icons/` (those are shared across all
three PWAs and are identical, which would produce a desktop installer
indistinguishable from a browser-installed PWA).

## Required files

| File | Used by | Notes |
|------|---------|-------|
| `business.png` | `pnpm desktop:icons:business` | Default = same as the PWA icon. Safe to keep. |

One app ships. The platform console is not a separate build — an admin signs
in through the same window and lands on `admin.orcaa.cloud`, so it wears the
same icon.

## Spec

- **Format**: PNG with alpha
- **Size**: at least **512×512**, ideally **1024×1024** (Tauri downscales for every target size — give it room to look crisp)
- **Padding**: leave ~10% transparent margin on all sides so the icon doesn't get clipped by Windows' rounded corner mask
- **Square aspect ratio** — non-square images get squashed

## Regenerate after swapping

```powershell
pnpm desktop:icons:business
```

Outputs to `desktop/src-tauri/icons/business/` (full set: 32×32, 128×128, 128×128@2x, icon.ico for Windows, icon.icns for macOS, etc.).
