; Windows 7 lane: install the WebView2 Runtime from the bundled standalone
; installer when the machine has none.
;
; Why a hook and not `webviewInstallMode`: Windows 7 can only ever run WebView2
; 109.0.1518.140, and Tauri's built-in modes fetch or embed whatever Microsoft
; serves *today* (110+, which refuses to install on Windows 7). The one official,
; still-served copy of 109 is the "Microsoft Edge-WebView2 Runtime Version 109
; Update" package on the Microsoft Update Catalog — the Evergreen Standalone
; Installer, Authenticode-signed by Microsoft. The release workflow downloads it,
; checks its pinned SHA-256 and signature, and the Win7 overlay config ships it
; as a bundle resource at `$INSTDIR\webview2\MicrosoftEdgeWebView2RuntimeInstaller.exe`.
;
; This is Microsoft's documented offline-deployment workflow, verbatim: check the
; `pv` registry value, and if the Runtime is absent run the standalone installer
; with `/silent /install`. The 140 MB installer is deleted afterwards — it has
; done its job and nothing else needs it (the updater re-ships it anyway).
;
; Elevation: Tauri's default install mode is `currentUser`, so the installer runs
; unelevated and WebView2 lands as a per-user install — a supported configuration
; (Microsoft: a per-user install is upgraded to per-machine if a per-machine Edge
; updater exists). The `pv` check covers both.

!define WEBVIEW2_CLIENT_KEY "Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"
!define WEBVIEW2_BUNDLED_INSTALLER "$INSTDIR\webview2\MicrosoftEdgeWebView2RuntimeInstaller.exe"

!macro NSIS_HOOK_POSTINSTALL
  ; --- is a WebView2 Runtime already here? ---------------------------------
  ; Same three locations Tauri's own template (and Microsoft's docs) inspect.
  ; NSIS is a 32-bit process: on 64-bit Windows the plain SOFTWARE path is
  ; redirected to WOW6432Node, so the explicit path and the plain one both
  ; resolve on x64; the plain one is the real location on 32-bit Windows.
  ReadRegStr $0 HKLM "SOFTWARE\WOW6432Node\${WEBVIEW2_CLIENT_KEY}" "pv"
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ReadRegStr $0 HKLM "SOFTWARE\${WEBVIEW2_CLIENT_KEY}" "pv"
  ${EndIf}
  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ReadRegStr $0 HKCU "SOFTWARE\${WEBVIEW2_CLIENT_KEY}" "pv"
  ${EndIf}

  ${If} $0 == ""
  ${OrIf} $0 == "0.0.0.0"
    ${If} ${FileExists} "${WEBVIEW2_BUNDLED_INSTALLER}"
      DetailPrint "Installing Microsoft Edge WebView2 Runtime 109 (bundled, offline)..."
      ; `/silent /install` is Microsoft's documented switch set for the
      ; standalone installer. $1 receives the exit code; a non-zero result is
      ; logged rather than fatal — Orcaa's own boot page explains a missing
      ; runtime far better than an aborted installer would.
      ExecWait '"${WEBVIEW2_BUNDLED_INSTALLER}" /silent /install' $1
      ${If} $1 != 0
        DetailPrint "WebView2 Runtime installer exited with code $1"
      ${EndIf}
    ${Else}
      DetailPrint "Bundled WebView2 Runtime installer not found; skipping"
    ${EndIf}
  ${Else}
    DetailPrint "WebView2 Runtime $0 already installed; skipping bundled installer"
  ${EndIf}

  ; --- reclaim the space -----------------------------------------------------
  ; Whether it ran or was skipped, the installer is not needed after setup.
  Delete "${WEBVIEW2_BUNDLED_INSTALLER}"
  RMDir "$INSTDIR\webview2"
!macroend
