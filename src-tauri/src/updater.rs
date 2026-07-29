//! Checking for, presenting and installing updates.
//!
//! The prompt used to be a native `MessageDialogButtons::OkCancelCustom` box.
//! It was unbranded, it blocked, it took its own taskbar entry (so it read as a
//! second application rather than part of this one), it showed no progress, and
//! because Win32 silently falls back to a plain `MessageBox` when it cannot
//! raise a TaskDialog, its custom "Remind me later" button routinely rendered as
//! a bare `Cancel`.
//!
//! It is now a real window the shell draws itself: frameless, parented to the
//! main window, kept out of the taskbar, centred on the app, with a live
//! progress bar and three honest choices. The native dialog survives only as a
//! fallback for the case where that window cannot be created at all — losing the
//! ability to offer an update entirely would be worse than an ugly prompt.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri::webview::PageLoadEvent;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_store::StoreExt;
use tauri_plugin_updater::{Update, UpdaterExt};

use crate::i18n::Strings;
use crate::shell_page::{shell_url, ShellPage};
use crate::{clamped_rect, notify, persist_session, STORE_FILE};

pub const UPDATER_WINDOW: &str = "updater";

const STORE_KEY_SNOOZED_UNTIL: &str = "update_snoozed_until";
const STORE_KEY_SKIPPED_VERSION: &str = "update_skipped_version";

/// How long "Remind me later" holds off the silent check.
///
/// Long enough that a reception desk isn't asked twice in a shift, short enough
/// that a security fix still lands the next day. The tray's explicit "Check for
/// Updates…" ignores it — the user just asked.
const SNOOZE: Duration = Duration::from_secs(24 * 60 * 60);

/// The update currently on offer, so the window's buttons have something to act
/// on. Held as `Arc` because `download_and_install` borrows it across an await
/// while the store lock must not be.
#[derive(Default)]
pub struct PendingUpdate(Mutex<Option<Arc<Update>>>);

impl PendingUpdate {
    fn set(&self, update: Arc<Update>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(update);
        }
    }

    pub fn get(&self) -> Option<Arc<Update>> {
        self.0.lock().ok()?.clone()
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether a *silent* check should stay quiet about this version.
///
/// Only ever consulted for the background check. Deliberately fails open: if the
/// store can't be read we prompt, because a missed update is worse than a
/// repeated prompt.
fn is_muted(app: &AppHandle, version: &str) -> bool {
    let Ok(store) = app.store(STORE_FILE) else {
        return false;
    };

    let skipped = store
        .get(STORE_KEY_SKIPPED_VERSION)
        .and_then(|v| v.as_str().map(String::from));
    if skipped.as_deref() == Some(version) {
        log::info!("update {version} was skipped by the user");
        return true;
    }

    let snoozed_until = store
        .get(STORE_KEY_SNOOZED_UNTIL)
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    if now_secs() < snoozed_until {
        log::info!("update prompt snoozed for another {}s", snoozed_until - now_secs());
        return true;
    }

    false
}

/// Checks for an update and, if there is one, presents it.
///
/// `interactive` distinguishes the tray's "Check for Updates…" (which owes the
/// user an answer either way, and overrides snooze/skip) from the silent startup
/// check (which stays quiet unless it has something to offer).
pub async fn check_for_updates(app: AppHandle, strings: Strings, interactive: bool) {
    let before_exit = app.clone();
    let updater = app
        .updater_builder()
        .on_before_exit(move || persist_session(&before_exit))
        .build();

    let updater = match updater {
        Ok(updater) => updater,
        Err(err) => {
            log::error!("failed to build updater: {err}");
            if interactive {
                notify(&app, &strings.update_failed_title(), &strings.update_failed_body());
            }
            return;
        }
    };

    match updater.check().await {
        Ok(Some(update)) => {
            log::info!(
                "update available: {} -> {}",
                update.current_version,
                update.version
            );

            if !interactive && is_muted(&app, &update.version) {
                return;
            }

            let update = Arc::new(update);
            app.state::<PendingUpdate>().set(update.clone());

            if let Err(err) = open_update_window(&app, &strings) {
                // Falling back rather than giving up: an ugly prompt still gets
                // the fix installed, a swallowed error never does.
                log::error!("failed to open the update window, falling back to a dialog: {err}");
                fallback_prompt(app, strings, update).await;
            }
        }
        Ok(None) => {
            log::info!("no update available");
            if interactive {
                notify(&app, &strings.update_none_title(), &strings.update_none_body());
            }
        }
        Err(err) => {
            log::error!("update check failed: {err}");
            if interactive {
                notify(&app, &strings.update_failed_title(), &strings.update_failed_body());
            }
        }
    }
}

/// Places the update window over the middle of the app.
///
/// Centred on the *main window* rather than the screen — that is most of what
/// makes it read as part of the app instead of a stray dialog. Clamped through
/// the same work-area helper the main window uses, so a main window sitting near
/// a screen edge can't push this one off it.
fn center_on_main(app: &AppHandle, window: &tauri::WebviewWindow) {
    let Some(main) = app.get_webview_window("main") else {
        let _ = window.center();
        return;
    };

    let (Ok(main_pos), Ok(main_size), Ok(size)) =
        (main.outer_position(), main.outer_size(), window.outer_size())
    else {
        let _ = window.center();
        return;
    };

    let x = main_pos.x + (main_size.width as i32 - size.width as i32) / 2;
    let y = main_pos.y + (main_size.height as i32 - size.height as i32) / 2;

    let area = match window.monitor_from_point(x as f64, y as f64) {
        Ok(Some(monitor)) => Some(monitor),
        _ => window.current_monitor().ok().flatten(),
    };

    let (x, y) = match area {
        Some(monitor) => {
            let work = monitor.work_area();
            let (x, y, _, _) = clamped_rect(
                (x, y, size.width, size.height),
                (work.position.x, work.position.y, work.size.width, work.size.height),
            );
            (x, y)
        }
        None => (x, y),
    };

    let _ = window.set_position(tauri::PhysicalPosition::new(x, y));
}

/// Built twice when parenting fails, so the builder is produced by a function
/// rather than assembled inline — `parent()` consumes the builder and hands back
/// nothing on error.
fn updater_window_builder<'a>(
    app: &'a AppHandle,
    strings: &Strings,
) -> WebviewWindowBuilder<'a, tauri::Wry, AppHandle> {
    WebviewWindowBuilder::new(
        app,
        UPDATER_WINDOW,
        WebviewUrl::CustomProtocol(shell_url(&ShellPage::Update, false)),
    )
    .title(strings.update_prompt_title())
    .inner_size(460.0, 430.0)
    .resizable(false)
    .minimizable(false)
    .maximizable(false)
    .decorations(false)
    .shadow(true)
    // The complaint that started this: an unparented dialog gets its own
    // taskbar button, which reads as the app having opened "another tab".
    .skip_taskbar(true)
    .focused(true)
    // Revealed once it has painted, so the user never sees a white rectangle.
    .visible(false)
    .on_page_load(|window, payload| {
        if matches!(payload.event(), PageLoadEvent::Finished) {
            let app = window.app_handle().clone();
            center_on_main(&app, &window);
            let _ = window.show();
            let _ = window.set_focus();
        }
    })
}

fn open_update_window(app: &AppHandle, strings: &Strings) -> tauri::Result<()> {
    if let Some(existing) = app.get_webview_window(UPDATER_WINDOW) {
        let _ = existing.show();
        let _ = existing.unminimize();
        let _ = existing.set_focus();
        return Ok(());
    }

    // Parenting keeps the window above the app and tied to its lifetime. It is
    // not supported everywhere, and an unparented window is still far better
    // than the native dialog — so a failure here retries without it rather than
    // falling all the way back.
    if let Some(main) = app.get_webview_window("main") {
        match updater_window_builder(app, strings).parent(&main) {
            Ok(builder) => return builder.build().map(|_| ()),
            Err(err) => log::warn!("could not parent the update window, opening it standalone: {err}"),
        }
    }

    updater_window_builder(app, strings).build().map(|_| ())
}

/// The old native prompt, kept only for the case where the real window fails.
async fn fallback_prompt(app: AppHandle, strings: Strings, update: Arc<Update>) {
    let accepted = app
        .dialog()
        .message(strings.update_prompt_body(&update.current_version, &update.version))
        .title(strings.update_prompt_title())
        .buttons(MessageDialogButtons::OkCancelCustom(
            strings.update_install_now(),
            strings.update_remind_later(),
        ))
        .blocking_show();

    if accepted {
        install(app, strings, update).await;
    } else {
        snooze(&app);
    }
}

fn store_set(app: &AppHandle, key: &str, value: serde_json::Value) {
    if let Ok(store) = app.store(STORE_FILE) {
        store.set(key, value);
        let _ = store.save();
    }
}

fn snooze(app: &AppHandle) {
    store_set(app, STORE_KEY_SNOOZED_UNTIL, json!(now_secs() + SNOOZE.as_secs()));
    log::info!("update postponed for {}h", SNOOZE.as_secs() / 3600);
}

fn close_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(UPDATER_WINDOW) {
        let _ = window.close();
    }
}

/// Pushes progress straight into the window rather than emitting an event.
///
/// `eval` needs no permission; the event plugin would need `core:event` added to
/// a capability for a window that otherwise needs nothing at all.
fn push(app: &AppHandle, js: &str) {
    if let Some(window) = app.get_webview_window(UPDATER_WINDOW) {
        if let Err(err) = window.eval(js) {
            log::warn!("failed to update the update window: {err}");
        }
    }
}

async fn install(app: AppHandle, strings: Strings, update: Arc<Update>) {
    let mut downloaded: u64 = 0;
    let mut last_reported = 0u64;
    let progress_app = app.clone();

    let result = update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;

                // Throttled to whole percent (or every 512 KB when the manifest
                // omits a length): a fast connection delivers thousands of
                // chunks and one `eval` each would starve the webview it is
                // trying to animate.
                let step = match total {
                    Some(total) if total > 0 => total / 100,
                    _ => 512 * 1024,
                }
                .max(1);

                if downloaded - last_reported < step {
                    return;
                }
                last_reported = downloaded;

                push(
                    &progress_app,
                    &format!(
                        "window.orcaaUpdate&&window.orcaaUpdate.progress({},{})",
                        downloaded,
                        total.unwrap_or(0)
                    ),
                );
            },
            || {},
        )
        .await;

    match result {
        Ok(()) => {
            // Windows never gets here: `install()` launches the NSIS installer
            // with /P /R and exits the process itself, and the installer
            // relaunches us. macOS and Linux swap the bundle in place and need
            // the restart spelled out.
            push(&app, "window.orcaaUpdate&&window.orcaaUpdate.installing()");
            notify(
                &app,
                &strings.update_installing_title(),
                &strings.update_installing_body(),
            );
            persist_session(&app);
            app.restart();
        }
        Err(err) => {
            log::error!("failed to install update: {err}");
            push(
                &app,
                &format!(
                    "window.orcaaUpdate&&window.orcaaUpdate.failed({})",
                    serde_json::to_string(&strings.update_download_failed())
                        .unwrap_or_else(|_| "\"\"".into())
                ),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Commands — reachable only from the update window's own local page.
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn update_install(app: AppHandle) {
    let Some(update) = app.state::<PendingUpdate>().get() else {
        log::warn!("install requested with no pending update");
        close_window(&app);
        return;
    };

    let strings = app
        .try_state::<Strings>()
        .map(|s| (*s).clone())
        .unwrap_or_else(|| Strings::detect("Orcaa".to_string()));

    notify(
        &app,
        &strings.update_downloading_title(),
        &strings.update_downloading_body(),
    );

    tauri::async_runtime::spawn(async move {
        install(app, strings, update).await;
    });
}

#[tauri::command]
pub fn update_snooze(app: AppHandle) {
    snooze(&app);
    close_window(&app);
}

#[tauri::command]
pub fn update_skip(app: AppHandle) {
    if let Some(update) = app.state::<PendingUpdate>().get() {
        store_set(&app, STORE_KEY_SKIPPED_VERSION, json!(update.version.clone()));
        log::info!("user skipped version {}", update.version);
    }
    close_window(&app);
}
