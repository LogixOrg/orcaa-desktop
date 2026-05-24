use serde_json::json;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WindowEvent,
};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_store::StoreExt;
use url::Url;

const STORE_FILE: &str = "orcaa-desktop.json";
const STORE_KEY_LAST_URL: &str = "last_url";

const FALLBACK_URL_BUSINESS: &str = "https://auth.orcaa.cloud";
const FALLBACK_URL_ADMIN: &str = "https://admin.orcaa.cloud";

fn fallback_url(identifier: &str) -> &'static str {
    if identifier.contains("admin") {
        FALLBACK_URL_ADMIN
    } else {
        FALLBACK_URL_BUSINESS
    }
}

fn is_internal_url(url: &Url) -> bool {
    match url.scheme() {
        "http" | "https" => url
            .host_str()
            .map(|h| h == "orcaa.cloud" || h.ends_with(".orcaa.cloud") || h.ends_with(".orcaa.test"))
            .unwrap_or(false),
        "about" | "data" | "blob" | "tauri" => true,
        _ => false,
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_store::Builder::new().build())
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
            let fallback = fallback_url(&identifier);

            let store = app.store(STORE_FILE)?;
            let saved = store
                .get(STORE_KEY_LAST_URL)
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_else(|| fallback.to_string());

            let initial_url: Url = saved
                .parse()
                .or_else(|_| fallback.parse())
                .expect("fallback URL must parse");

            let handle = app.handle().clone();

            tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(initial_url),
            )
            .title(&product_name)
            .inner_size(1440.0, 900.0)
            .min_inner_size(1024.0, 640.0)
            .resizable(true)
            .on_navigation(move |url| {
                if is_internal_url(url) {
                    true
                } else {
                    let _ = handle.opener().open_url(url.to_string(), None::<&str>);
                    false
                }
            })
            .build()?;

            let show_label = format!("Open {}", &product_name);
            let show_item = MenuItem::with_id(app, "show", &show_label, true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let mut tray_builder = TrayIconBuilder::with_id("main")
                .tooltip(&product_name)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
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

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                if let Some(wv) = app.get_webview_window("main") {
                    if let Ok(url) = wv.url() {
                        if let Ok(store) = app.store(STORE_FILE) {
                            store.set(STORE_KEY_LAST_URL, json!(url.to_string()));
                            let _ = store.save();
                        }
                    }
                }
                // Hide to tray instead of quitting so WebSocket stays connected
                // and OS toasts continue to fire for incoming notifications.
                // True exit is via the tray menu's Quit item.
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running orcaa desktop");
}
