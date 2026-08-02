use axum::{
    body::Body,
    http::{Response, StatusCode, header},
};
use include_dir::{Dir, include_dir};

static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/web");

pub fn index(nonce: &str) -> Response<Body> {
    let Some(file) = WEB.get_file("index.html") else {
        return unavailable();
    };
    let html = String::from_utf8_lossy(file.contents()).replace("{{CSP_NONCE}}", nonce);
    response(StatusCode::OK, "text/html; charset=utf-8", html.into())
}

pub fn file(path: &str) -> Response<Body> {
    let path = path.trim_start_matches('/');
    let Some(file) = WEB.get_file(path) else {
        return response(
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            "Not Found\n".into(),
        );
    };
    let content_type = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    response(
        StatusCode::OK,
        &content_type,
        Body::from(file.contents().to_vec()),
    )
}

fn unavailable() -> Response<Body> {
    response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "text/plain; charset=utf-8",
        "WebUI assets are unavailable\n".into(),
    )
}

fn response(status: StatusCode, content_type: &str, body: Body) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .expect("valid embedded asset response")
}
