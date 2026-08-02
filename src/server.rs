use std::{future::Future, path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{OriginalUri, Query, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Response, StatusCode, header},
    middleware::{self, Next},
    response::IntoResponse,
    routing::{any, get, post, put},
};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use rand::RngCore;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use url::Url;

use crate::{
    assets,
    core::{Supervisor, atomic_write, validate_tun_privileges},
    system_proxy::{Controller as SystemProxyController, parse_endpoint},
};

#[derive(Clone)]
struct AppState {
    core: Url,
    client: reqwest::Client,
    supervisor: Option<Arc<Supervisor>>,
    apply_lock: Arc<Mutex<()>>,
    system_proxy: Arc<SystemProxyController>,
}

#[derive(Clone)]
struct CspNonce(String);

pub fn router(
    core: Url,
    supervisor: Option<Arc<Supervisor>>,
    system_proxy: Arc<SystemProxyController>,
) -> Result<Router> {
    if !matches!(core.scheme(), "http" | "https") || core.host_str().is_none() {
        bail!("invalid Core address {core:?}");
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build Core HTTP client")?;
    let state = Arc::new(AppState {
        core,
        client,
        supervisor,
        apply_lock: Arc::new(Mutex::new(())),
        system_proxy,
    });
    Ok(Router::new()
        .route("/healthz", get(health))
        .route("/webui/runtime", get(runtime))
        .route("/webui/bootstrap", get(bootstrap))
        .route("/webui/config", get(get_config).put(put_yaml_config))
        .route("/webui/restart", post(restart))
        .route("/webui/routing", put(update_routing))
        .route("/webui/tun", put(update_tun))
        .route("/webui/logs", get(logs))
        .route(
            "/webui/system-proxy",
            get(system_proxy_status).put(system_proxy_configure),
        )
        .route("/api/v1/config", any(config_proxy))
        .route("/api/v1/config/reload", any(reload_proxy))
        .route("/api/{*path}", any(proxy))
        .fallback(static_or_spa)
        .with_state(state)
        .layer(middleware::from_fn(security_headers)))
}

async fn health() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        "ok\n",
    )
}

async fn runtime(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let result = match &state.supervisor {
        Some(supervisor) => {
            serde_json::to_value(supervisor.status().await).expect("runtime status is serializable")
        }
        None => json!({ "managed": false, "core_url": state.core.as_str() }),
    };
    axum::Json(json!({ "result": result }))
}

async fn bootstrap(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let result = match &state.supervisor {
        Some(supervisor) => json!({
            "managed": true,
            "token": supervisor.token().unwrap_or_default(),
            "tun_available": crate::core::tun_privileges_available(),
        }),
        None => json!({ "managed": false }),
    };
    axum::Json(json!({ "result": result }))
}

async fn get_config(State(state): State<Arc<AppState>>) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "Core is not managed by WebUI",
        );
    };
    match tokio::fs::read(&supervisor.config.paths.config).await {
        Ok(data) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/yaml; charset=utf-8")
            .body(Body::from(data))
            .expect("config response"),
        Err(error) => web_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CONFIG_UNAVAILABLE",
            &error.to_string(),
        ),
    }
}

async fn put_yaml_config(State(state): State<Arc<AppState>>, request: Request) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "Core is not managed by WebUI",
        );
    };
    let authorization = request.headers().get(header::AUTHORIZATION).cloned();
    let body = match limited_body(request, 4 << 20).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let _guard = state.apply_lock.lock().await;
    let yaml = match supervisor.config.normalize_yaml(&body) {
        Ok(yaml) => yaml,
        Err(error) => {
            return web_error(
                StatusCode::BAD_REQUEST,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    let value: serde_yaml::Value = match serde_yaml::from_slice(&yaml) {
        Ok(value) => value,
        Err(error) => {
            return web_error(
                StatusCode::BAD_REQUEST,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    let json = match serde_json::to_vec(&value) {
        Ok(json) => json,
        Err(error) => {
            return web_error(
                StatusCode::BAD_REQUEST,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    apply_normalized(&state, supervisor, authorization, json, yaml).await
}

async fn restart(State(state): State<Arc<AppState>>) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "Core is not managed by WebUI",
        );
    };
    let _guard = state.apply_lock.lock().await;
    match supervisor.restart().await {
        Ok(()) => web_json(
            StatusCode::OK,
            json!({ "result": supervisor.status().await }),
        ),
        Err(error) => web_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CORE_RESTART_FAILED",
            &error.to_string(),
        ),
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingUpdate {
    mode: String,
}

async fn update_routing(State(state): State<Arc<AppState>>, request: Request) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "Core is not managed by WebUI",
        );
    };
    let authorization = request.headers().get(header::AUTHORIZATION).cloned();
    let update: RoutingUpdate = match parse_json(request, 4 << 20).await {
        Ok(update) => update,
        Err(response) => return response,
    };
    let _guard = state.apply_lock.lock().await;
    let current = match tokio::fs::read(&supervisor.config.paths.config).await {
        Ok(data) => data,
        Err(error) => {
            return web_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    let yaml = match supervisor
        .config
        .update_routing_yaml(&current, &update.mode)
    {
        Ok(yaml) => yaml,
        Err(error) => {
            return web_error(
                StatusCode::BAD_REQUEST,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    persist_and_reload(&state, supervisor, authorization, yaml).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TunUpdate {
    enabled: bool,
}

async fn update_tun(State(state): State<Arc<AppState>>, request: Request) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "Core is not managed by WebUI",
        );
    };
    let authorization = request.headers().get(header::AUTHORIZATION).cloned();
    let update: TunUpdate = match parse_json(request, 1 << 20).await {
        Ok(update) => update,
        Err(response) => return response,
    };
    if update.enabled {
        if let Err(error) = validate_tun_privileges() {
            return web_error(
                StatusCode::FORBIDDEN,
                "TUN_PRIVILEGES_REQUIRED",
                &error.to_string(),
            );
        }
    }
    let _guard = state.apply_lock.lock().await;
    let current = match tokio::fs::read(&supervisor.config.paths.config).await {
        Ok(data) => data,
        Err(error) => {
            return web_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    let yaml = match supervisor.config.update_tun_yaml(&current, update.enabled) {
        Ok(yaml) => yaml,
        Err(error) => {
            return web_error(
                StatusCode::BAD_REQUEST,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    persist_and_reload(&state, supervisor, authorization, yaml).await
}

#[derive(serde::Deserialize)]
struct LogQuery {
    #[serde(default)]
    after: u64,
}

async fn logs(State(state): State<Arc<AppState>>, Query(query): Query<LogQuery>) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "Core is not managed by WebUI",
        );
    };
    let (entries, next) = supervisor.logs_after(query.after, 500);
    web_json(
        StatusCode::OK,
        json!({ "result": { "entries": entries, "next": next } }),
    )
}

async fn system_proxy_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "system proxy is only available in managed mode",
        );
    };
    if !authorized(supervisor, &headers) {
        return web_error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "invalid managed Core token",
        );
    }
    web_json(
        StatusCode::OK,
        json!({ "result": state.system_proxy.status().await }),
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SystemProxyUpdate {
    enabled: bool,
    #[serde(default)]
    http_address: String,
    #[serde(default)]
    socks_address: String,
}

async fn system_proxy_configure(
    State(state): State<Arc<AppState>>,
    request: Request,
) -> Response<Body> {
    let Some(supervisor) = &state.supervisor else {
        return web_error(
            StatusCode::CONFLICT,
            "CORE_NOT_MANAGED",
            "system proxy is only available in managed mode",
        );
    };
    if !authorized(supervisor, request.headers()) {
        return web_error(
            StatusCode::UNAUTHORIZED,
            "UNAUTHORIZED",
            "invalid managed Core token",
        );
    }
    let update: SystemProxyUpdate =
        match parse_json_with_code(request, 1 << 20, "SYSTEM_PROXY_INVALID").await {
            Ok(update) => update,
            Err(response) => return response,
        };
    if update.enabled {
        if update.http_address.trim().is_empty() && update.socks_address.trim().is_empty() {
            return web_error(
                StatusCode::BAD_REQUEST,
                "SYSTEM_PROXY_INVALID",
                "at least one proxy address is required",
            );
        }
        for (label, address) in [
            ("HTTP", update.http_address.as_str()),
            ("SOCKS", update.socks_address.as_str()),
        ] {
            if !address.trim().is_empty() {
                if let Err(error) = parse_endpoint(address) {
                    return web_error(
                        StatusCode::BAD_REQUEST,
                        "SYSTEM_PROXY_INVALID",
                        &format!("invalid {label} proxy address: {error}"),
                    );
                }
            }
        }
    }
    match state
        .system_proxy
        .configure(update.enabled, &update.http_address, &update.socks_address)
        .await
    {
        Ok(status) => web_json(StatusCode::OK, json!({ "result": status })),
        Err(error) => {
            let status = state.system_proxy.status().await;
            web_error(
                if status.supported {
                    StatusCode::INTERNAL_SERVER_ERROR
                } else {
                    StatusCode::NOT_IMPLEMENTED
                },
                "SYSTEM_PROXY_FAILED",
                &error.to_string(),
            )
        }
    }
}

fn authorized(supervisor: &Supervisor, headers: &HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| supervisor.authorized(value))
}

async fn config_proxy(State(state): State<Arc<AppState>>, request: Request) -> Response<Body> {
    if state.supervisor.is_none() || request.method() != Method::PUT {
        return proxy(State(state), OriginalUri(request.uri().clone()), request).await;
    }
    let supervisor = state.supervisor.as_ref().expect("managed supervisor");
    let authorization = request.headers().get(header::AUTHORIZATION).cloned();
    let body = match limited_body(request, 4 << 20).await {
        Ok(body) => body,
        Err(response) => return response,
    };
    let _guard = state.apply_lock.lock().await;
    let (json, yaml) = match supervisor.config.normalize_json(&body) {
        Ok(config) => config,
        Err(error) => {
            return web_error(
                StatusCode::BAD_REQUEST,
                "CONFIG_INVALID",
                &error.to_string(),
            );
        }
    };
    apply_normalized(&state, supervisor, authorization, json, yaml).await
}

async fn reload_proxy(State(state): State<Arc<AppState>>, request: Request) -> Response<Body> {
    if request.method() == Method::POST {
        if let Some(supervisor) = &state.supervisor {
            if let Err(error) = supervisor.config.prepare() {
                return web_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "CONFIG_PREPARE_FAILED",
                    &error.to_string(),
                );
            }
        }
    }
    proxy(State(state), OriginalUri(request.uri().clone()), request).await
}

async fn apply_normalized(
    state: &AppState,
    supervisor: &Supervisor,
    authorization: Option<HeaderValue>,
    json: Vec<u8>,
    yaml: Vec<u8>,
) -> Response<Body> {
    let applied = match buffered_core_request(
        state,
        Method::PUT,
        "/api/v1/config",
        authorization.clone(),
        Some(json),
        Some("application/json"),
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            return web_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "CONFIG_APPLY_FAILED",
                &error.to_string(),
            );
        }
    };
    if !applied.status.is_success()
        && core_error_code(&applied.body).as_deref() != Some("RESTART_REQUIRED")
    {
        return applied.into_response();
    }
    persist_and_reload(state, supervisor, authorization, yaml).await
}

async fn persist_and_reload(
    state: &AppState,
    supervisor: &Supervisor,
    authorization: Option<HeaderValue>,
    yaml: Vec<u8>,
) -> Response<Body> {
    let path = &supervisor.config.paths.config;
    let result = persist_with_reload(path, &yaml, || {
        buffered_core_request(
            state,
            Method::POST,
            "/api/v1/config/reload",
            authorization.clone(),
            None,
            None,
        )
    })
    .await;
    match result {
        Ok(response) => response.into_response(),
        Err(error) => web_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "CONFIG_PERSIST_FAILED",
            &error.to_string(),
        ),
    }
}

async fn persist_with_reload<F, Fut>(
    path: &Path,
    yaml: &[u8],
    mut reload: F,
) -> Result<BufferedResponse>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<BufferedResponse>>,
{
    let previous = match tokio::fs::read(path).await {
        Ok(previous) => Some(previous),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let had_previous = previous.is_some();
    if let Err(error) = atomic_write(path, yaml, 0o600) {
        if had_previous {
            let _ = reload().await;
        }
        return Err(error);
    }
    let first_reload = reload().await;
    if let Ok(response) = &first_reload {
        if response.status.is_success() {
            return Ok(response.clone());
        }
    }
    let rollback = match previous {
        Some(previous) => atomic_write(path, &previous, 0o600),
        None => std::fs::remove_file(path)
            .or_else(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    Ok(())
                } else {
                    Err(error)
                }
            })
            .map_err(Into::into),
    };
    if rollback.is_ok() && had_previous {
        let _ = reload().await;
    }
    match first_reload {
        Ok(response) if rollback.is_ok() => Ok(response),
        Ok(response) => bail!("reload failed with {}; rollback failed", response.status),
        Err(error) if rollback.is_ok() => Err(error),
        Err(error) => Err(error.context("reload failed and rollback failed")),
    }
}

#[derive(Clone)]
struct BufferedResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl BufferedResponse {
    fn into_response(self) -> Response<Body> {
        let mut headers = self.headers;
        remove_hop_by_hop_headers(&mut headers);
        headers.remove(header::CONTENT_LENGTH);
        let mut response = Response::builder().status(self.status);
        *response.headers_mut().expect("response headers") = headers;
        response
            .body(Body::from(self.body))
            .expect("upstream response")
    }
}

async fn buffered_core_request(
    state: &AppState,
    method: Method,
    path: &str,
    authorization: Option<HeaderValue>,
    body: Option<Vec<u8>>,
    content_type: Option<&str>,
) -> Result<BufferedResponse> {
    let mut target = state.core.clone();
    set_target_path(&mut target, path, None);
    let mut request = state.client.request(method, target);
    if let Some(authorization) = authorization {
        request = request.header(header::AUTHORIZATION, authorization);
    }
    if let Some(content_type) = content_type {
        request = request.header(header::CONTENT_TYPE, content_type);
    }
    if let Some(body) = body {
        request = request.body(body);
    }
    let response = request.send().await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await?.to_vec();
    Ok(BufferedResponse {
        status,
        headers,
        body,
    })
}

fn core_error_code(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value.get("error")?.get("code")?.as_str().map(str::to_owned)
}

async fn limited_body(request: Request, limit: usize) -> Result<Vec<u8>, Response<Body>> {
    limited_body_with_code(request, limit, "CONFIG_INVALID").await
}

async fn limited_body_with_code(
    request: Request,
    limit: usize,
    code: &str,
) -> Result<Vec<u8>, Response<Body>> {
    to_bytes(request.into_body(), limit)
        .await
        .map(|bytes| bytes.to_vec())
        .map_err(|error| web_error(StatusCode::BAD_REQUEST, code, &error.to_string()))
}

async fn parse_json<T: serde::de::DeserializeOwned>(
    request: Request,
    limit: usize,
) -> Result<T, Response<Body>> {
    parse_json_with_code(request, limit, "CONFIG_INVALID").await
}

async fn parse_json_with_code<T: serde::de::DeserializeOwned>(
    request: Request,
    limit: usize,
    code: &str,
) -> Result<T, Response<Body>> {
    let body = limited_body_with_code(request, limit, code).await?;
    serde_json::from_slice(&body)
        .map_err(|error| web_error(StatusCode::BAD_REQUEST, code, &error.to_string()))
}

async fn proxy(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
    request: Request,
) -> Response<Body> {
    match proxy_request(&state, uri.clone(), request).await {
        Ok(response) => response,
        Err(error) => {
            let suppress = state.supervisor.is_some()
                && uri.path() == "/api/v1/hello"
                && is_connection_refused(&error);
            if !suppress {
                tracing::error!(%error, "Core proxy request failed");
            }
            (StatusCode::BAD_GATEWAY, "Core is unavailable\n").into_response()
        }
    }
}

async fn proxy_request(
    state: &AppState,
    uri: http::Uri,
    request: Request,
) -> Result<Response<Body>> {
    let mut target = state.core.clone();
    set_target_path(&mut target, uri.path(), uri.query());
    let (parts, body) = request.into_parts();
    let mut headers = parts.headers;
    headers.remove(header::HOST);
    headers.remove(header::ORIGIN);
    headers.remove(header::REFERER);
    let upstream = state
        .client
        .request(parts.method, target)
        .headers(headers)
        .body(reqwest::Body::wrap_stream(body.into_data_stream()))
        .send()
        .await?;
    let status = upstream.status();
    let mut headers = upstream.headers().clone();
    remove_hop_by_hop_headers(&mut headers);
    let stream = upstream.bytes_stream();
    let mut response = Response::builder().status(status);
    *response.headers_mut().expect("response headers") = headers;
    Ok(response.body(Body::from_stream(stream))?)
}

fn remove_hop_by_hop_headers(headers: &mut HeaderMap) {
    let connection_headers: Vec<HeaderName> = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in connection_headers {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION,
        HeaderName::from_static("keep-alive"),
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        headers.remove(name);
    }
}

fn set_target_path(target: &mut Url, path: &str, query: Option<&str>) {
    let joined = if target.path().is_empty() || target.path() == "/" {
        format!("/{}", path.trim_start_matches('/'))
    } else {
        format!(
            "{}/{}",
            target.path().trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    };
    target.set_path(&joined);
    let combined_query = match (target.query(), query) {
        (Some(base), Some(request)) => Some(format!("{base}&{request}")),
        (Some(base), None) => Some(base.to_owned()),
        (None, Some(request)) => Some(request.to_owned()),
        (None, None) => None,
    };
    target.set_query(combined_query.as_deref());
}

fn is_connection_refused(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
    })
}

async fn static_or_spa(OriginalUri(uri): OriginalUri, request: Request) -> Response<Body> {
    if uri.path() != "/" && uri.path().contains('.') {
        return assets::file(uri.path());
    }
    let nonce = request
        .extensions()
        .get::<CspNonce>()
        .map_or("", |nonce| nonce.0.as_str());
    assets::index(nonce)
}

async fn security_headers(mut request: Request, next: Next) -> Response<Body> {
    let no_store =
        request.uri().path().starts_with("/api/") || request.uri().path().starts_with("/webui/");
    let mut random = [0_u8; 18];
    rand::rng().fill_bytes(&mut random);
    let nonce = STANDARD_NO_PAD.encode(random);
    request.extensions_mut().insert(CspNonce(nonce.clone()));
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    if no_store {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    let policy = format!(
        "default-src 'self'; img-src 'self' data:; style-src 'self' 'nonce-{nonce}'; script-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&policy).expect("valid CSP header"),
    );
    response
}

fn web_json(status: StatusCode, value: Value) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            serde_json::to_vec(&value).expect("JSON response"),
        ))
        .expect("web response")
}

fn web_error(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    web_json(
        status,
        json!({ "error": { "code": code, "message": message } }),
    )
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex as StdMutex};

    use super::*;
    use tower::ServiceExt;

    #[test]
    fn rejects_non_http_core_url() {
        assert!(
            router(
                "file:///tmp/core".parse().unwrap(),
                None,
                SystemProxyController::new()
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn serves_spa_with_matching_nonce_and_security_headers() {
        let app = router(
            "http://127.0.0.1:19090".parse().unwrap(),
            None,
            SystemProxyController::new(),
        )
        .unwrap();
        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::X_FRAME_OPTIONS], "DENY");
        let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
            .to_str()
            .unwrap()
            .to_owned();
        let nonce = csp
            .split("style-src 'self' 'nonce-")
            .nth(1)
            .and_then(|value| value.split('\'').next())
            .unwrap();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Sumire"));
        assert!(body.contains(&format!("name=\"csp-nonce\" content=\"{nonce}\"")));
        assert!(!body.contains("{{CSP_NONCE}}"));
    }

    #[tokio::test]
    async fn api_responses_disable_caching() {
        let app = router(
            "http://127.0.0.1:19090".parse().unwrap(),
            None,
            SystemProxyController::new(),
        )
        .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/webui/runtime")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn managed_only_config_endpoint_rejects_external_mode() {
        let app = router(
            "http://127.0.0.1:19090".parse().unwrap(),
            None,
            SystemProxyController::new(),
        )
        .unwrap();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/webui/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }

    fn buffered(status: StatusCode, body: &str) -> BufferedResponse {
        BufferedResponse {
            status,
            headers: HeaderMap::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn buffered_response_removes_transport_headers() {
        let mut response = buffered(StatusCode::OK, "{}");
        response.headers.insert(
            header::TRANSFER_ENCODING,
            HeaderValue::from_static("chunked"),
        );
        response
            .headers
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("2"));
        response
            .headers
            .insert(header::CONNECTION, HeaderValue::from_static("x-upstream"));
        response.headers.insert(
            HeaderName::from_static("x-upstream"),
            HeaderValue::from_static("remove-me"),
        );

        let response = response.into_response();

        assert!(!response.headers().contains_key(header::TRANSFER_ENCODING));
        assert!(!response.headers().contains_key(header::CONTENT_LENGTH));
        assert!(!response.headers().contains_key(header::CONNECTION));
        assert!(!response.headers().contains_key("x-upstream"));
    }

    #[tokio::test]
    async fn persists_configuration_after_successful_reload() {
        let root = std::env::temp_dir().join(format!(
            "sumire-persist-success-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.yaml");
        std::fs::write(&path, b"routing:\n  mode: rule\n").unwrap();
        let replies = StdMutex::new(VecDeque::from([buffered(StatusCode::OK, "{}")]));
        let response = persist_with_reload(&path, b"routing:\n  mode: global\n", || {
            let response = replies.lock().unwrap().pop_front().unwrap();
            async move { Ok(response) }
        })
        .await
        .unwrap();
        assert_eq!(response.status, StatusCode::OK);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("mode: global")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn rolls_back_configuration_after_failed_reload() {
        let root = std::env::temp_dir().join(format!(
            "sumire-persist-rollback-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.yaml");
        std::fs::write(&path, b"routing:\n  mode: rule\n").unwrap();
        let replies = StdMutex::new(VecDeque::from([
            buffered(
                StatusCode::BAD_REQUEST,
                r#"{"error":{"code":"CONFIG_INVALID"}}"#,
            ),
            buffered(StatusCode::OK, "{}"),
        ]));
        let response = persist_with_reload(&path, b"routing:\n  mode: global\n", || {
            let response = replies.lock().unwrap().pop_front().unwrap();
            async move { Ok(response) }
        })
        .await
        .unwrap();
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        let persisted = std::fs::read_to_string(&path).unwrap();
        assert!(persisted.contains("mode: rule"));
        assert!(!persisted.contains("mode: global"));
        assert!(replies.lock().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn removes_new_configuration_without_second_reload() {
        let root = std::env::temp_dir().join(format!(
            "sumire-persist-new-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("config.yaml");
        let replies = StdMutex::new(VecDeque::from([buffered(
            StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"CONFIG_INVALID"}}"#,
        )]));
        let response = persist_with_reload(&path, b"version: 1\n", || {
            let response = replies.lock().unwrap().pop_front().unwrap();
            async move { Ok(response) }
        })
        .await
        .unwrap();
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert!(!path.exists());
        assert!(replies.lock().unwrap().is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recognizes_restart_required_core_error() {
        assert_eq!(
            core_error_code(br#"{"error":{"code":"RESTART_REQUIRED"}}"#).as_deref(),
            Some("RESTART_REQUIRED")
        );
    }

    #[test]
    fn joins_core_base_path_and_queries() {
        let mut target: Url = "https://core.example/base?token=one".parse().unwrap();
        set_target_path(&mut target, "/api/v1/hello", Some("lang=en"));
        assert_eq!(
            target.as_str(),
            "https://core.example/base/api/v1/hello?token=one&lang=en"
        );
    }

    #[test]
    fn identifies_connection_refused_errors() {
        let error = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        assert!(is_connection_refused(&error));
        assert!(!is_connection_refused(&anyhow::anyhow!("other")));
    }
}
