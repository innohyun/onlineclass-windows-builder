use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use url::Url;

use crate::{AppState, BrowserLinkStore, DESKTOP_BROWSER_LINK_AUDIENCE};

const ORIGIN: &str = "https://t.classaimate.com";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalRequest {
    url: String,
    method: String,
    headers: BTreeMap<String, String>,
    body_base64: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LocalResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body_base64: String,
}

fn allowed_caller(label: &str, url: &Url) -> bool {
    (label == "teacher-home" || label.starts_with("teacher-popup-"))
        && crate::is_teacher_home_url(url)
}

fn validate(request: &LocalRequest, endpoint: &str, links: &BrowserLinkStore) -> Result<Url, String> {
    let url = Url::parse(&request.url).map_err(|_| "desktop_local_url_invalid")?;
    if url.origin().ascii_serialization() != endpoint
        || url.scheme() != "http" || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some()
        || !url.path().starts_with("/v1/")
        || !matches!(request.method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD")
    {
        return Err("desktop_local_target_denied".into());
    }
    for (name, value) in &request.headers {
        if !matches!(name.as_str(), "content-type" | "accept" | "range" | "x-onlineclass-local-browser-token")
            || value.len() > 512 || value.chars().any(char::is_control)
        {
            return Err("desktop_local_header_denied".into());
        }
    }
    if request.method == "GET" && url.path() == "/v1/device-authorization/browser-link" {
        if request.headers.contains_key("x-onlineclass-local-browser-token") {
            return Err("desktop_local_pickup_denied".into());
        }
        let id = url.query_pairs().find(|(key, _)| key == "requestId").map(|(_, value)| value.into_owned()).unwrap_or_default();
        let link = links.read_for_request(&id)?.ok_or("device_authorization_pending")?;
        if link.audience != DESKTOP_BROWSER_LINK_AUDIENCE {
            return Err("desktop_local_pickup_denied".into());
        }
    } else if let Some(token) = request.headers.get("x-onlineclass-local-browser-token") {
        let tokens = links.tokens.lock().map_err(|_| "browser_link_lock_failed")?;
        if !tokens.iter().any(|entry| entry.token == *token && entry.audience == DESKTOP_BROWSER_LINK_AUDIENCE) {
            return Err("desktop_local_browser_token_required".into());
        }
    } else if request.method != "GET" || url.path() != "/v1/health" {
        return Err("desktop_local_browser_token_required".into());
    }
    Ok(url)
}

fn forward(request: LocalRequest, endpoint: String, links: Arc<BrowserLinkStore>) -> Result<LocalResponse, String> {
    let url = validate(&request, &endpoint, &links)?;
    let encoded = request.body_base64.as_deref().unwrap_or("");
    let body = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|_| "desktop_local_body_invalid")?;
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(false).redirects(0)
        .timeout(Duration::from_secs(30)).build();
    let mut call = agent.request(&request.method, url.as_str()).set("Origin", ORIGIN);
    for (name, value) in &request.headers { call = call.set(name, value); }
    let result = if body.is_empty() { call.call() } else { call.send_bytes(&body) };
    let response = match result {
        Ok(response) | Err(ureq::Error::Status(_, response)) => response,
        Err(ureq::Error::Transport(_)) => return Err("desktop_local_transport_unavailable".into()),
    };
    let status = response.status();
    if (300..400).contains(&status) { return Err("desktop_local_redirect_denied".into()); }
    let headers = ["content-type", "content-disposition", "accept-ranges", "content-range"]
        .into_iter().filter_map(|name| response.header(name).map(|value| (name.to_string(), value.to_string()))).collect();
    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)
        .map_err(|_| "desktop_local_response_failed")?;
    Ok(LocalResponse { status, headers, body_base64: base64::engine::general_purpose::STANDARD.encode(bytes) })
}

#[tauri::command]
pub(crate) async fn teacher_local_request(
    webview: tauri::Webview,
    state: tauri::State<'_, AppState>,
    request: LocalRequest,
) -> Result<LocalResponse, String> {
    if !cfg!(target_os = "macos") || !allowed_caller(webview.label(), &webview.url().map_err(|_| "desktop_local_caller_denied")?) {
        return Err("desktop_local_caller_denied".into());
    }
    let endpoint = {
        let status = state.status.lock().map_err(|_| "local_store_service_unavailable")?;
        if !status.ok { return Err("local_store_service_unavailable".into()); }
        status.endpoint.clone()
    };
    let links = state.browser_links.lock().map_err(|_| "browser_link_lock_failed")?.clone()
        .ok_or("local_store_service_unavailable")?;
    tauri::async_runtime::spawn_blocking(move || forward(request, endpoint, links)).await
        .map_err(|_| "desktop_local_transport_unavailable".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_boundary_rejects_other_webviews_and_remote_origins() {
        let good = Url::parse("https://t.classaimate.com/admin/settings").unwrap();
        assert!(allowed_caller("teacher-home", &good));
        assert!(allowed_caller("teacher-popup-1", &good));
        assert!(!allowed_caller("main", &good));
        for url in ["http://t.classaimate.com/admin/", "https://t.classaimate.com.evil.test/admin/", "https://t.classaimate.com/student/", "https://t.classaimate.com:444/admin/"] {
            assert!(!allowed_caller("teacher-home", &Url::parse(url).unwrap()));
        }
    }

    #[test]
    fn only_existing_desktop_authority_can_reach_the_owned_loopback_service() {
        let directory = std::env::temp_dir().join(format!("desktop-transport-qa-{}", crate::random_url_token()));
        let links = BrowserLinkStore::open(&directory).unwrap();
        let previous = links.issue(&serde_json::json!({ "tenantId": "tenant-a", "uid": "teacher-a" })).unwrap();
        let request_id = crate::random_url_token();
        let desktop = links.issue_desktop_for_request(&request_id).unwrap();
        let endpoint = "http://127.0.0.1:51273";
        let mut request = LocalRequest { url: format!("{endpoint}/v1/overview"), method: "GET".into(), headers: BTreeMap::new(), body_base64: None };
        assert!(validate(&request, endpoint, &links).is_err());
        request.headers.insert("x-onlineclass-local-browser-token".into(), previous.token.clone());
        assert!(validate(&request, endpoint, &links).is_err());
        request.headers.insert("x-onlineclass-local-browser-token".into(), desktop.token);
        assert!(validate(&request, endpoint, &links).is_ok());
        for target in ["http://127.0.0.1:51274/v1/overview", "https://example.com/v1/overview", "http://127.0.0.1:51273/other", "http://user@127.0.0.1:51273/v1/overview"] {
            request.url = target.into();
            assert!(validate(&request, endpoint, &links).is_err());
        }
        request.url = format!("{endpoint}/v1/overview");
        request.headers.insert("x-onlineclass-local-store-key".into(), "not-allowed".into());
        assert!(validate(&request, endpoint, &links).is_err());
        request.headers.clear();
        request.url = format!("{endpoint}/v1/device-authorization/browser-link?requestId={request_id}");
        assert!(validate(&request, endpoint, &links).is_ok());
        request.headers.insert("x-onlineclass-local-browser-token".into(), previous.token);
        assert!(validate(&request, endpoint, &links).is_err());
        request.headers.clear();
        let external_id = crate::random_url_token();
        links.issue_for_request(&external_id, &serde_json::json!({ "tenantId": "tenant-a", "uid": "teacher-a" })).unwrap();
        request.url = format!("{endpoint}/v1/device-authorization/browser-link?requestId={external_id}");
        assert!(validate(&request, endpoint, &links).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
