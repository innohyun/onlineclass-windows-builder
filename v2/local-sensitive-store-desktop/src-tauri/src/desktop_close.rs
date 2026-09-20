use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{Emitter, Manager};

#[derive(Default)]
pub(crate) struct DesktopCloseGuard(AtomicBool);

pub(crate) fn request(app: &tauri::AppHandle, intent: &str) {
    let guarded = app
        .try_state::<DesktopCloseGuard>()
        .is_some_and(|state| state.0.load(Ordering::SeqCst));
    if guarded {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
            let _ = window.set_focus();
        }
        // A failed event delivery must keep the editor alive. Durable draft recovery
        // also covers process termination outside this application's close flow.
        let _ = app.emit("desktop-close-requested", json!({"intent":intent}));
    } else {
        finish(app, intent);
    }
}
fn finish(app: &tauri::AppHandle, intent: &str) {
    let keep_running = app
        .try_state::<crate::AppState>()
        .map(|state| state.preferences.snapshot().keep_running_on_close)
        .unwrap_or(true);
    if intent == "close" && keep_running {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.hide();
        }
    } else {
        app.exit(0);
    }
}
#[tauri::command]
pub(crate) fn set_desktop_close_guard(
    state: tauri::State<'_, DesktopCloseGuard>,
    ready: bool,
) -> Value {
    state.0.store(ready, Ordering::SeqCst);
    json!({"ok":true})
}
#[tauri::command]
pub(crate) fn finish_desktop_close(app: tauri::AppHandle, intent: String) -> Value {
    if !["close", "quit"].contains(&intent.as_str()) {
        return json!({"ok":false,"error":"desktop_close_intent_invalid"});
    }
    finish(&app, &intent);
    json!({"ok":true})
}
