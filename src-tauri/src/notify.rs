//! Native toasts that actually do something when you click them.
//!
//! The app already fired OS toasts through `tauri-plugin-notification`, but that
//! plugin's desktop path builds a `notify_rust::Notification`, calls `.show()`
//! on a detached task and drops the handle — so a toast is a dead end. A
//! receptionist glancing at "New booking — Sara, 4:30 PM" has to find the window
//! and navigate there by hand, which is most of the value gone.
//!
//! On Windows we therefore build the toast ourselves with
//! `tauri-winrt-notification` (already in the tree as a transitive dependency of
//! the plugin, so this costs no extra build time). That gives three things the
//! plugin cannot:
//!
//! - **`on_activated`** — clicking the toast raises the window and navigates.
//! - **`Scenario::IncomingCall`** — a pre-expanded toast that stays on screen
//!   and loops the *system ringtone*. This is how an incoming voice call should
//!   behave, and it works with the app hidden in the tray, where the web app's
//!   own `new Audio().play()` is silently dropped by the autoplay policy.
//! - **Action buttons** — Answer / Decline, without opening the app first.
//!
//! Everywhere else this falls back to the plugin: a toast with no click
//! handling is still better than no toast.

use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tauri_plugin_notification::NotificationExt;
use url::Url;

use crate::i18n::Strings;

/// What kind of toast to draw. Anything unrecognised is treated as `Default` —
/// an unknown value from the web app must not lose the notification.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Default,
    /// Ringing, persistent, with Answer / Decline.
    IncomingCall,
}

impl Kind {
    fn parse(raw: Option<&str>) -> Self {
        match raw {
            Some("call") => Kind::IncomingCall,
            _ => Kind::Default,
        }
    }
}

#[derive(Deserialize)]
pub struct NotifyPayload {
    pub title: String,
    pub body: String,
    /// Where clicking the toast should land the user. Validated against the
    /// orcaa hosts before it is ever navigated to — this command is reachable
    /// from the remote page, so an unchecked value here would let a compromised
    /// or mistaken web build steer the webview anywhere it liked.
    pub url: Option<String>,
    /// `"call"` for an incoming voice call, otherwise omitted.
    pub kind: Option<String>,
}

/// The only URLs a toast is allowed to navigate to.
fn safe_target(raw: Option<&str>) -> Option<Url> {
    let url: Url = raw?.parse().ok()?;
    if crate::is_orcaa_host(&url) && !crate::is_sign_in_route(&url) {
        Some(url)
    } else {
        log::warn!("ignoring a notification target that is not an app page: {url}");
        None
    }
}

/// Raises the window and, when the toast carried one, opens the page it was
/// about. Runs on the WinRT event thread, so everything is dispatched onto the
/// app handle rather than touched directly.
fn activate(app: &AppHandle, target: Option<Url>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();

        if let Some(url) = target {
            if let Err(err) = window.navigate(url) {
                log::error!("failed to open the notification's page: {err}");
            }
        }
    }
}

#[cfg(windows)]
fn show_windows(app: &AppHandle, payload: &NotifyPayload, strings: &Strings) -> bool {
    use tauri_winrt_notification::{Scenario, Toast};

    // Toasts are addressed by AppUserModelID, which the NSIS installer stamps
    // onto the Start Menu shortcut. Running out of `target/{debug,release}`
    // there is no such shortcut, so the real identifier silently produces
    // nothing — the same guard the notification plugin uses.
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()))
        .map(|dir| {
            let dir = dir.display().to_string();
            !(dir.ends_with("target\\debug") || dir.ends_with("target\\release"))
        })
        .unwrap_or(false);

    let app_id = if installed {
        app.config().identifier.clone()
    } else {
        Toast::POWERSHELL_APP_ID.to_string()
    };

    let kind = Kind::parse(payload.kind.as_deref());
    let target = safe_target(payload.url.as_deref());

    let mut toast = Toast::new(&app_id)
        .title(&payload.title)
        .text1(&payload.body);

    if kind == Kind::IncomingCall {
        // Pre-expanded, stays until answered or dismissed, loops the ringtone.
        toast = toast
            .scenario(Scenario::IncomingCall)
            .add_button(&strings.call_answer(), "answer")
            .add_button(&strings.call_decline(), "decline");
    }

    let handle = app.clone();
    let toast = toast.on_activated(move |action| {
        // "decline" is the one activation that must NOT pull the app forward.
        if action.as_deref() == Some("decline") {
            return Ok(());
        }
        activate(&handle, target.clone());
        Ok(())
    });

    match toast.show() {
        Ok(()) => true,
        Err(err) => {
            log::warn!("failed to show a native toast: {err}");
            false
        }
    }
}

/// Sends a toast. Returns `false` if nothing could be shown, so the web app can
/// tell "shown" from "silently dropped".
#[tauri::command]
pub fn shell_notify(app: AppHandle, payload: NotifyPayload) -> bool {
    let strings = app
        .try_state::<Strings>()
        .map(|s| (*s).clone())
        .unwrap_or_else(|| Strings::detect("Orcaa".to_string()));

    #[cfg(windows)]
    {
        if show_windows(&app, &payload, &strings) {
            return true;
        }
    }

    // macOS and Linux, and any Windows toast that failed to build: a plain
    // toast with no click handling still beats none.
    let _ = &strings;
    match app
        .notification()
        .builder()
        .title(&payload.title)
        .body(&payload.body)
        .show()
    {
        Ok(()) => true,
        Err(err) => {
            log::warn!("failed to show notification: {err}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toast_can_only_send_the_webview_to_an_app_page() {
        // `shell_notify` is reachable from the remote page, so this is the
        // boundary that stops a notification payload from steering the window.
        for hostile in [
            "https://evil.com/steal",
            "https://orcaa.cloud.evil.com/",
            "file:///C:/Windows/System32",
            "javascript:alert(1)",
            "orcaa-shell://localhost/?update=1",
            "not a url",
        ] {
            assert!(
                safe_target(Some(hostile)).is_none(),
                "{hostile} must not be navigable from a toast"
            );
        }
    }

    #[test]
    fn a_toast_must_not_be_able_to_force_a_sign_in_page() {
        // Sign-in never renders in the webview; a toast that could navigate
        // there would put a login form inside the app.
        assert!(safe_target(Some("https://auth.orcaa.cloud/login")).is_none());
        assert!(safe_target(Some("https://clinic.orcaa.cloud/login")).is_none());
    }

    #[test]
    fn real_app_pages_are_accepted() {
        let url = safe_target(Some("https://clinic.orcaa.cloud/bookings/42")).unwrap();
        assert_eq!(url.path(), "/bookings/42");

        assert!(safe_target(Some("https://clinic.orcaa.test/chat")).is_some());
        assert!(safe_target(None).is_none());
    }

    #[test]
    fn an_unknown_kind_still_produces_a_notification() {
        // A newer web build sending a kind this shell has never heard of must
        // degrade to an ordinary toast, never to silence.
        assert!(Kind::parse(Some("something-new")) == Kind::Default);
        assert!(Kind::parse(None) == Kind::Default);
        assert!(Kind::parse(Some("call")) == Kind::IncomingCall);
    }
}
