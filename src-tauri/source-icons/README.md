# Desktop source icons

Drop one PNG per desktop app here. The Tauri icon generator reads from this
folder, NOT from `apps/*/src/assets/public/icons/` (those are shared across all
three PWAs and are identical, which would produce indistinguishable desktop
installers).

## Required files

| File | Used by | Notes |
|------|---------|-------|
| `business.png` | `pnpm desktop:icons:business` | Default = same as the PWA icon. Safe to keep. |
| `admin.png` | `pnpm desktop:icons:admin` | **Replace with a differentiated icon** before public release so users can tell Business and Admin apart in the taskbar / Start menu. |

## Spec

- **Format**: PNG with alpha
- **Size**: at least **512×512**, ideally **1024×1024** (Tauri downscales for every target size — give it room to look crisp)
- **Padding**: leave ~10% transparent margin on all sides so the icon doesn't get clipped by Windows' rounded corner mask
- **Square aspect ratio** — non-square images get squashed

## Regenerate after swapping

```powershell
pnpm desktop:icons:business
pnpm desktop:icons:admin
```

Outputs to `desktop/src-tauri/icons/{business,admin}/` (full set: 32×32, 128×128, 128×128@2x, icon.ico for Windows, icon.icns for macOS, etc.).
