use axum::{
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

#[derive(Embed)]
#[folder = "frontend/dist/"]
struct Assets;

pub async fn static_handler(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    match Assets::get(path) {
        Some(file) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            ([(header::CONTENT_TYPE, mime.as_ref())], file.data).into_response()
        }
        None => match Assets::get("index.html") {
            Some(file) => ([(header::CONTENT_TYPE, "text/html")], file.data).into_response(),
            None => (StatusCode::NOT_FOUND, "404").into_response(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::Assets;

    #[test]
    fn embeds_installable_web_app_metadata_and_icons() {
        let manifest = Assets::get("manifest.webmanifest").expect("manifest should be embedded");
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest.data).expect("manifest should be valid JSON");

        assert_eq!(manifest["display"], "standalone");
        assert_eq!(manifest["launch_handler"]["client_mode"], "focus-existing");

        for icon in manifest["icons"]
            .as_array()
            .expect("manifest icons should be an array")
        {
            let path = icon["src"]
                .as_str()
                .expect("manifest icon should have a source")
                .trim_start_matches('/');
            assert!(Assets::get(path).is_some(), "missing embedded icon: {path}");
        }
    }
}
