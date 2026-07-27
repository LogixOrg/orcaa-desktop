mod i18n;
mod signin;

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use serde_json::json;
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    webview::PageLoadEvent,
    AppHandle, Manager, WindowEvent,
};
use tauri_plugin_deep_link::DeepLinkExt;
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_store::StoreExt;
use tauri_plugin_updater::UpdaterExt;
use tauri_plugin_window_state::{AppHandleExt as WindowStateExt, StateFlags};
use url::Url;

use crate::i18n::Strings;
use crate::signin::PendingSignIn;

const STORE_FILE: &str = "orcaa-desktop.json";
const STORE_KEY_LAST_URL: &str = "last_url";
const STORE_KEY_TRAY_HINT_SEEN: &str = "tray_hint_seen";

const FALLBACK_URL_BUSINESS: &str = "https://auth.orcaa.cloud";
const FALLBACK_URL_ADMIN: &str = "https://admin.orcaa.cloud";

/// How long to wait for the remote app to paint before showing the window
/// anyway. Without a ceiling a dead network would leave the user staring at a
/// tray icon and no window at all.
const FIRST_PAINT_TIMEOUT: Duration = Duration::from_secs(12);

/// Delay before the startup update check. Long enough that the window has
/// painted and the WebSocket has connected, so we're never swapping the binary
/// out from under someone who is still watching the app boot.
const UPDATE_CHECK_DELAY: Duration = Duration::from_secs(20);

/// Size, position and maximized/fullscreen are restored between launches.
/// Visibility deliberately is **not** — the app hides to tray on close, so
/// restoring a "hidden" state would make the next launch appear to do nothing.
fn window_state_flags() -> StateFlags {
    StateFlags::SIZE | StateFlags::POSITION | StateFlags::MAXIMIZED | StateFlags::FULLSCREEN
}

fn fallback_url(identifier: &str) -> &'static str {
    if identifier.contains("admin") {
        FALLBACK_URL_ADMIN
    } else {
        FALLBACK_URL_BUSINESS
    }
}

/// The auth app is the one orcaa host the webview must never render — sign-in
/// belongs in the user's real browser (see [`signin`]).
fn is_auth_host(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url
            .host_str()
            .map(|h| h.starts_with("auth."))
            .unwrap_or(false)
}

/// The registrable domain the tenant subdomains hang off, derived from whatever
/// host the app is configured against so dev (`orcaa.test`) works unchanged.
fn base_domain_of(url: &Url) -> String {
    url.host_str()
        .map(|host| {
            let labels: Vec<&str> = host.split('.').collect();
            if labels.len() >= 2 {
                labels[labels.len() - 2..].join(".")
            } else {
                host.to_string()
            }
        })
        .unwrap_or_else(|| "orcaa.cloud".to_string())
}

/// A real Orcaa web address — as opposed to the shell's own scaffolding page.
fn is_orcaa_host(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url
            .host_str()
            .map(|h| {
                h == "orcaa.cloud" || h.ends_with(".orcaa.cloud") || h.ends_with(".orcaa.test")
            })
            .unwrap_or(false)
}

/// The shell's own page, which arrives as `http://orcaa-shell.localhost` on
/// Windows and `orcaa-shell://` elsewhere.
fn is_shell_url(url: &Url) -> bool {
    url.scheme() == SHELL_SCHEME
        || url.host_str() == Some(&format!("{SHELL_SCHEME}.localhost"))
}

fn is_internal_url(url: &Url) -> bool {
    // The shell page must be recognised here, or `on_navigation` would hand the
    // app's own UI to the system browser.
    is_orcaa_host(url)
        || is_shell_url(url)
        || matches!(url.scheme(), "about" | "data" | "blob" | "tauri")
}

/// Shrinks the window to fit the monitor it opened on, and re-centres it if it
/// had to shrink.
///
/// Two cases this catches: the 1440x900 default is larger than the 1366x768
/// panels a lot of clinic reception desks run, and a geometry restored from a
/// big external display would otherwise reopen off-screen on a laptop. Uses the
/// monitor's *work area* so the window never hides behind the taskbar.
fn fit_window_to_monitor(window: &tauri::WebviewWindow) {
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };

    let work_area = monitor.work_area();
    let (max_w, max_h) = (work_area.size.width, work_area.size.height);

    let Ok(size) = window.outer_size() else {
        return;
    };

    if size.width <= max_w && size.height <= max_h {
        return;
    }

    let fitted = tauri::PhysicalSize::new(size.width.min(max_w), size.height.min(max_h));
    if window.set_size(fitted).is_ok() {
        let _ = window.center();
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

/// Reveals the window the first time the remote app paints.
///
/// The window is built hidden so the user never sees WebView2's blank white
/// canvas while the PWA boots. Guarded by `shown` because `on_page_load` fires
/// on *every* navigation — without it, an in-app route change would yank the
/// window back out of the tray after the user deliberately hid it.
fn reveal_main_window(app: &AppHandle, shown: &Arc<AtomicBool>) {
    if shown.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(window) = app.get_webview_window("main") {
        // Runs after the window-state plugin has restored geometry, so this
        // sees the size the user is actually about to get.
        fit_window_to_monitor(&window);
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The wrapper bundles no frontend assets (`frontendDist` is null — it only
/// renders the remote PWA), so its one local page is served from a registered
/// URI scheme instead.
///
/// It is emphatically **not** a `data:` URL: Chromium blocks top-level
/// navigation to those, so WebView2 renders its own "can't reach this page"
/// error instead of the markup. Tauri's `webview-data-url` feature only
/// silences Tauri's own check — the engine still refuses.
const SHELL_SCHEME: &str = "orcaa-shell";

/// Windows serves custom schemes through `http://<scheme>.localhost`; the other
/// platforms use the scheme directly. Webview *creation* converts this for us,
/// but `navigate()` needs the already-converted form.
fn shell_url(waiting: bool, converted: bool) -> Url {
    let base = if converted && cfg!(windows) {
        format!("http://{SHELL_SCHEME}.localhost/")
    } else {
        format!("{SHELL_SCHEME}://localhost/")
    };

    let mut url: Url = base.parse().expect("shell URL must parse");
    if waiting {
        url.set_query(Some("waiting=1"));
    }
    url
}

/// The shell's sign-in page.
///
/// Two states, one document: an entry state with the primary call to action,
/// and a waiting state once the browser has been handed control. Styling stays
/// on system colors deliberately — the desktop shell must not introduce brand
/// tokens that would then need maintaining alongside the real theme.
///
/// The button is a plain link to the auth host, which needs no IPC:
/// `on_navigation` already intercepts that host, so the same rule drives the
/// first click, a retry, and a session expiring mid-use.
fn holding_page_html(strings: &Strings, auth_base: &str, waiting: bool) -> String {
    let (title, body, cta) = if waiting {
        (
            strings.signin_title(),
            strings.signin_body(),
            strings.signin_retry(),
        )
    } else {
        (
            strings.signin_welcome_title(),
            strings.signin_welcome_body(),
            strings.signin_cta(),
        )
    };

    format!(
        r##"<!doctype html>
<html lang="{lang}" dir="{dir}">
<meta charset="utf-8">
<title>{title}</title>
<style>
  :root {{ color-scheme: light dark; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100vh; display: flex; align-items: center;
    justify-content: center; text-align: center; padding: 2rem;
    font-family: "Segoe UI", system-ui, -apple-system, "Helvetica Neue", sans-serif;
  }}
  main {{ max-width: 26rem; }}
  h1 {{ font-size: 1.5rem; font-weight: 600; margin: 0 0 .85rem; line-height: 1.3; }}
  p {{ margin: 0 0 2rem; line-height: 1.65; opacity: .7; font-size: .9375rem; }}
  a.cta {{
    display: inline-block; padding: .8rem 2rem; border-radius: .5rem;
    text-decoration: none; font-size: .9375rem; font-weight: 600;
    background: CanvasText; color: Canvas; border: 1px solid CanvasText;
  }}
  a.cta:hover {{ opacity: .85; }}
  a.cta:focus-visible {{ outline: 2px solid CanvasText; outline-offset: 3px; }}
</style>
<main>
  <h1>{title}</h1>
  <p>{body}</p>
  <a class="cta" href="{auth_base}">{cta}</a>
</main>
</html>"##,
        lang = strings.html_lang(),
        dir = if strings.is_rtl() { "rtl" } else { "ltr" },
        title = title,
        body = body,
        cta = cta,
        auth_base = auth_base,
    )
}

#[cfg(test)]
mod shell_page_tests {
    use super::*;

    #[test]
    fn both_states_render_a_call_to_action_pointing_at_the_auth_host() {
        let strings = Strings::detect("Orcaa".to_string());

        for waiting in [false, true] {
            let html = holding_page_html(&strings, "https://auth.orcaa.cloud", waiting);
            assert!(html.contains("<a class=\"cta\" href=\"https://auth.orcaa.cloud\">"));
            assert!(html.contains("<h1>"));
        }
    }

    #[test]
    fn the_shell_page_is_never_stored_as_the_last_visited_url() {
        // Storing it reopened the app on its own waiting screen instead of the
        // welcome screen.
        for raw in [
            "http://orcaa-shell.localhost/",
            "http://orcaa-shell.localhost/?waiting=1",
            "orcaa-shell://localhost/",
        ] {
            let url: Url = raw.parse().unwrap();
            assert!(is_shell_url(&url), "{raw} should be recognised as the shell");
            assert!(!is_orcaa_host(&url), "{raw} must not be persisted");
            // Still internal — it must never be handed to the system browser.
            assert!(is_internal_url(&url));
        }
    }

    #[test]
    fn real_tenant_pages_are_still_persisted() {
        let url: Url = "https://clinic.orcaa.cloud/dashboard".parse().unwrap();
        assert!(is_orcaa_host(&url));
        assert!(!is_auth_host(&url));
    }
}

/// Hands sign-in to the system browser and switches the window to the waiting
/// state. Driven by the user clicking the call to action — the browser is never
/// opened behind their back on launch.
fn start_browser_sign_in(app: &AppHandle) {
    let Some(config) = app.try_state::<AppUrls>() else {
        return;
    };
    let pending = app.state::<PendingSignIn>();

    let Some(browser_url) = pending.begin(&config.auth_base) else {
        log::error!("failed to start browser sign-in");
        return;
    };

    log::info!("handing sign-in to the system browser");

    if let Err(err) = app
        .opener()
        .open_url(browser_url.to_string(), None::<&str>)
    {
        log::error!("failed to open the browser for sign-in: {err}");
    }

    if let Some(window) = app.get_webview_window("main") {
        if let Err(err) = window.navigate(shell_url(true, true)) {
            log::error!("failed to show the waiting page: {err}");
        }
    }
}

/// Completes a sign-in that came back over `orcaa://`, by pointing the webview
/// at the tenant's handoff route with the ticket and the verifier.
fn complete_browser_sign_in(app: &AppHandle, incoming: &Url) {
    let Some(config) = app.try_state::<AppUrls>() else {
        return;
    };

    let Some(resolved) = app.state::<PendingSignIn>().resolve(incoming, &config.base_domain) else {
        // Unsolicited, replayed, or tampered — indistinguishable on purpose.
        log::warn!("ignoring a deep link that does not match a pending sign-in");
        return;
    };

    log::info!("resuming sign-in on {}", resolved.url.host_str().unwrap_or("?"));

    if let Some(window) = app.get_webview_window("main") {
        if let Err(err) = window.navigate(resolved.url) {
            log::error!("failed to resume sign-in: {err}");
        }
    }

    show_main_window(app);
}

/// The hosts this build talks to, resolved once from config so dev
/// (`*.orcaa.test`) needs no special-casing downstream.
struct AppUrls {
    auth_base: String,
    base_domain: String,
}

fn notify(app: &AppHandle, title: &str, body: &str) {
    if let Err(err) = app.notification().builder().title(title).body(body).show() {
        log::warn!("failed to show notification: {err}");
    }
}

/// Persists everything the next launch needs to feel continuous: the page the
/// user was on, plus the window geometry.
///
/// Called from every exit path — the close button, the tray's Quit, and the
/// updater's pre-exit hook. That last one matters: on Windows the updater
/// hands off to the NSIS installer and calls `process::exit(0)` itself, so no
/// window event ever fires and anything not saved here is lost.
fn persist_session(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if let Ok(url) = window.url() {
            // Only ever remember a real page the user was working on. Saving
            // the shell's own sign-in scaffolding would reopen the app on its
            // waiting screen instead of the welcome screen, and saving the auth
            // host is pointless — the fallback already resolves there.
            if is_orcaa_host(&url) && !is_auth_host(&url) {
                if let Ok(store) = app.store(STORE_FILE) {
                    store.set(STORE_KEY_LAST_URL, json!(url.to_string()));
                    let _ = store.save();
                }
            }
        }
    }

    if let Err(err) = app.save_window_state(window_state_flags()) {
        log::warn!("failed to save window state: {err}");
    }
}

/// One-time explanation of where the app went when the user first clicks X.
/// Hiding to tray is the right default (it keeps the WebSocket alive so
/// notifications keep arriving) but it is surprising the first time.
fn maybe_show_tray_hint(app: &AppHandle, strings: &Strings) {
    let Ok(store) = app.store(STORE_FILE) else {
        return;
    };

    let already_seen = store
        .get(STORE_KEY_TRAY_HINT_SEEN)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if already_seen {
        return;
    }

    store.set(STORE_KEY_TRAY_HINT_SEEN, json!(true));
    let _ = store.save();

    notify(app, &strings.tray_hint_title(), &strings.tray_hint_body());
}

/// Checks for, downloads and installs an update.
///
/// `interactive` distinguishes the tray's "Check for Updates…" (which owes the
/// user an answer either way) from the silent startup check (which should stay
/// quiet unless it actually has something to install).
async fn check_for_updates(app: AppHandle, strings: Strings, interactive: bool) {
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
                notify(
                    &app,
                    &strings.update_failed_title(),
                    &strings.update_failed_body(),
                );
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
            notify(
                &app,
                &strings.update_downloading_title(),
                &strings.update_downloading_body(),
            );

            match update.download_and_install(|_, _| {}, || {}).await {
                Ok(()) => {
                    // Windows never gets here: `install()` launches the NSIS
                    // installer with /P /R and exits the process itself, and the
                    // installer relaunches us. macOS and Linux swap the bundle
                    // in place and need the restart spelled out.
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
                    if interactive {
                        notify(
                            &app,
                            &strings.update_failed_title(),
                            &strings.update_failed_body(),
                        );
                    }
                }
            }
        }
        Ok(None) => {
            log::info!("no update available");
            if interactive {
                notify(
                    &app,
                    &strings.update_none_title(),
                    &strings.update_none_body(),
                );
            }
        }
        Err(err) => {
            log::error!("update check failed: {err}");
            if interactive {
                notify(
                    &app,
                    &strings.update_failed_title(),
                    &strings.update_failed_body(),
                );
            }
        }
    }
}

pub fn run() {
    tauri::Builder::default()
        // Serves the shell's own sign-in page. Everything it needs is derived
        // from config here rather than read from managed state, so the page
        // cannot depend on setup having run first.
        .register_uri_scheme_protocol(SHELL_SCHEME, |ctx, request| {
            let app = ctx.app_handle();
            let product = app
                .config()
                .product_name
                .clone()
                .unwrap_or_else(|| "Orcaa".into());
            let identifier = app.config().identifier.clone();

            let fallback: Url = fallback_url(&identifier)
                .parse()
                .expect("fallback URL must parse");
            let auth_base = format!("https://auth.{}", base_domain_of(&fallback));

            let waiting = request.uri().query().unwrap_or_default().contains("waiting");
            let html = holding_page_html(&Strings::detect(product), &auth_base, waiting);

            tauri::http::Response::builder()
                .header(tauri::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
                .body(html.into_bytes())
                .expect("shell page response must build")
        })
        // Must be registered FIRST. On Windows an `orcaa://` link launches a
        // whole new process with the URL in argv; the single-instance plugin's
        // `deep-link` feature forwards it into the running app so `on_open_url`
        // fires there instead of a second window appearing.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_deep_link::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: None,
                    }),
                ])
                .build(),
        )
        .plugin(tauri_plugin_store::Builder::new().build())
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(window_state_flags())
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let identifier = app.config().identifier.clone();
            let product_name = app
                .config()
                .product_name
                .clone()
                .unwrap_or_else(|| "Orcaa".into());
            let version = app.package_info().version.to_string();
            let strings = Strings::detect(product_name.clone());
            let fallback = fallback_url(&identifier);

            log::info!("starting {product_name} {version} ({identifier})");

            let store = app.store(STORE_FILE)?;
            let saved = store
                .get(STORE_KEY_LAST_URL)
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| fallback.to_string());

            let initial_url: Url = saved
                .parse()
                .or_else(|_| fallback.parse())
                .expect("fallback URL must parse");

            let fallback_url: Url = fallback.parse().expect("fallback URL must parse");
            app.manage(AppUrls {
                auth_base: format!(
                    "https://auth.{}",
                    base_domain_of(&fallback_url)
                ),
                base_domain: base_domain_of(&fallback_url),
            });
            app.manage(PendingSignIn::default());
            app.manage(strings.clone());

            // The saved URL is the page the user was last on, which may well be
            // the auth app. Sign-in never renders in here, so start the browser
            // flow instead and park on the holding page.
            let needs_sign_in = is_auth_host(&initial_url);
            let start_webview_url = if needs_sign_in {
                tauri::WebviewUrl::CustomProtocol(shell_url(false, false))
            } else {
                tauri::WebviewUrl::External(initial_url)
            };

            // Set before the window exists so a `single_instance` re-launch
            // during boot doesn't race the first-paint reveal.
            let shown = Arc::new(AtomicBool::new(false));

            let nav_handle = app.handle().clone();
            let load_handle = app.handle().clone();
            let load_shown = shown.clone();

            tauri::WebviewWindowBuilder::new(app, "main", start_webview_url)
            .title(&product_name)
            .inner_size(1440.0, 900.0)
            .min_inner_size(1024.0, 640.0)
            .resizable(true)
            .visible(false)
            .on_navigation(move |url| {
                // One rule covers every route into sign-in: the initial load, a
                // session expiring mid-use and bouncing to auth, and the retry
                // link on the holding page.
                if is_auth_host(url) {
                    let handle = nav_handle.clone();
                    tauri::async_runtime::spawn(async move {
                        start_browser_sign_in(&handle);
                    });
                    return false;
                }

                if is_internal_url(url) {
                    true
                } else {
                    let _ = nav_handle.opener().open_url(url.to_string(), None::<&str>);
                    false
                }
            })
            .on_page_load(move |_window, payload| {
                if matches!(payload.event(), PageLoadEvent::Finished) {
                    log::info!("page loaded: {}", payload.url());
                    reveal_main_window(&load_handle, &load_shown);
                }
            })
            .build()?;

            // The browser hands the finished session back here.
            let deep_link_handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    complete_browser_sign_in(&deep_link_handle, &url);
                }
            });

            // A cold start *from* a deep link (app not running when the browser
            // handed back) delivers the URL through `get_current` rather than
            // the event — but only a link this process initiated can resolve,
            // so a launch-by-link with no pending attempt is correctly inert.
            if let Ok(Some(urls)) = app.deep_link().get_current() {
                let cold_handle = app.handle().clone();
                for url in urls {
                    complete_browser_sign_in(&cold_handle, &url);
                }
            }

            // Safety net: a failed or endlessly-hanging load must still end up
            // with a visible window the user can retry from (Ctrl+R), not an
            // app that silently never appears.
            let timeout_handle = app.handle().clone();
            let timeout_shown = shown.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(FIRST_PAINT_TIMEOUT).await;
                reveal_main_window(&timeout_handle, &timeout_shown);
            });

            let show_item =
                MenuItem::with_id(app, "show", strings.tray_open(), true, None::<&str>)?;
            let update_item = MenuItem::with_id(
                app,
                "check_updates",
                strings.tray_check_updates(),
                true,
                None::<&str>,
            )?;
            let quit_item =
                MenuItem::with_id(app, "quit", strings.tray_quit(), true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &show_item,
                    &PredefinedMenuItem::separator(app)?,
                    &update_item,
                    &PredefinedMenuItem::separator(app)?,
                    &quit_item,
                ],
            )?;

            let menu_strings = strings.clone();
            let mut tray_builder = TrayIconBuilder::with_id("main")
                .tooltip(format!("{product_name} {version}"))
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "check_updates" => {
                        let handle = app.clone();
                        let strings = menu_strings.clone();
                        tauri::async_runtime::spawn(async move {
                            check_for_updates(handle, strings, true).await;
                        });
                    }
                    "quit" => {
                        persist_session(app);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                });

            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }

            tray_builder.build(app)?;

            let update_handle = app.handle().clone();
            let update_strings = strings.clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(UPDATE_CHECK_DELAY).await;
                check_for_updates(update_handle, update_strings, false).await;
            });

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                persist_session(app);

                if let Some(strings) = app.try_state::<Strings>() {
                    maybe_show_tray_hint(app, &strings);
                }

                // Hide to tray instead of quitting so the WebSocket stays
                // connected and OS toasts keep firing for incoming
                // notifications. True exit is via the tray menu's Quit item.
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running orcaa desktop");
}
