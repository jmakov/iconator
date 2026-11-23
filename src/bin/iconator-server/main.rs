//! Serve the iconator lookup tables and their matching artwork over HTTP.
//!
//! The file and folder routes pass the client's path to [`iconator::get_icon_for_file`]
//! or [`iconator::get_icon_for_folder`]. Only the resulting numeric ID selects an
//! embedded SVG, so clients can query paths that exist only on their own machines.

use std::{net, path, time};

use axum::response::IntoResponse;
use axum::{extract, http, middleware, response, routing};
use clap::Parser;
use eyre::WrapErr;

#[derive(clap::Parser)]
#[command(about = "Serve embedded file and folder icons over HTTP")]
struct Cli {
    /// Address and port to listen on.
    #[arg(long, env = "BIND_ADDR", default_value = "127.0.0.1:50000")]
    bind: net::SocketAddr,
}

/// Bundled SVGs keyed by the filenames corresponding to library icon IDs.
#[derive(rust_embed::Embed)]
#[folder = "icons/"]
#[include = "*.svg"]
struct Artwork;

/// Decoded query shared by both icon routes.
#[derive(serde::Deserialize)]
struct IconQuery {
    /// Client-side path used as a lookup key.
    path: path::PathBuf,
}

/// Lookup failures that determine the HTTP status returned to the client.
#[derive(Debug, thiserror::Error)]
enum IconError {
    /// The supplied path has no final name to look up.
    #[error("path must have a basename")]
    InvalidPath,
    /// The lookup table has no match for this path.
    #[error("no matching icon")]
    NoMatch,
    /// A mapped ID promises artwork. A missing SVG means the build is incomplete.
    #[error("missing artwork for icon {0}")]
    MissingArtwork(u64),
}

impl IntoResponse for IconError {
    /// Translate a lookup failure into a client-visible HTTP response.
    ///
    /// # Returns
    ///
    /// A plain-text 400 for an invalid path, 404 for a lookup miss, or 500 for
    /// missing artwork. The last case logs the missing ID internally.
    fn into_response(self) -> response::Response {
        match self {
            Self::InvalidPath => (http::StatusCode::BAD_REQUEST, self.to_string()).into_response(),
            Self::NoMatch => (http::StatusCode::NOT_FOUND, self.to_string()).into_response(),
            Self::MissingArtwork(icon_id) => {
                tracing::error!(name: "icon.artwork.missing", icon_id, "Missing embedded SVG");
                (http::StatusCode::INTERNAL_SERVER_ERROR, "icon unavailable").into_response()
            }
        }
    }
}

/// Serve the artwork selected by a successful lookup.
///
/// # Arguments
///
/// * `icon_id` - Numeric ID returned by the file or folder table.
///
/// # Returns
///
/// A 200 response containing the embedded bytes with media type `image/svg+xml`.
///
/// # Errors
///
/// Returns [`IconError::MissingArtwork`] if the bundle lacks `{icon_id}.svg`.
fn svg(icon_id: u64) -> Result<response::Response, IconError> {
    let asset =
        Artwork::get(&format!("{icon_id}.svg")).ok_or(IconError::MissingArtwork(icon_id))?;
    Ok(([(http::header::CONTENT_TYPE, "image/svg+xml")], asset.data).into_response())
}

impl IconQuery {
    /// Resolve this query using the route's lookup function and load its artwork.
    ///
    /// # Arguments
    ///
    /// * `lookup` - File or folder lookup that maps a client path to an optional ID.
    ///
    /// # Returns
    ///
    /// The SVG response from [`svg`] for a matched ID.
    ///
    /// # Errors
    ///
    /// Returns [`IconError::InvalidPath`] for a missing basename, [`IconError::NoMatch`]
    /// for an unlisted name, or [`IconError::MissingArtwork`] for an unbundled ID.
    fn icon(
        self,
        lookup: impl FnOnce(&path::Path) -> Option<u64>,
    ) -> Result<response::Response, IconError> {
        // Both library lookups return None for a missing basename and for an unknown
        // name. Check the basename here so HTTP clients can distinguish HTTP error 400 from 404.
        self.path.file_name().ok_or(IconError::InvalidPath)?;

        svg(lookup(&self.path).ok_or(IconError::NoMatch)?)
    }
}

/// Handle `GET /icons/file` using [`iconator::get_icon_for_file`].
///
/// # Arguments
///
/// * `query` - URL-decoded path of the file on the client.
///
/// # Returns
///
/// SVG for an exact filename match or, failing that, its final extension.
///
/// # Errors
///
/// [`IconQuery::icon`] reports an invalid path, a lookup miss, or missing artwork.
/// Axum rejects a missing `path` parameter before calling this handler.
async fn file(
    extract::Query(query): extract::Query<IconQuery>,
) -> Result<response::Response, IconError> {
    query.icon(|path| iconator::get_icon_for_file(path))
}

/// Handle `GET /icons/folder` using [`iconator::get_icon_for_folder`].
///
/// # Arguments
///
/// * `query` - URL-decoded path of the folder on the client.
///
/// # Returns
///
/// SVG for the folder's own name.
///
/// # Errors
///
/// [`IconQuery::icon`] reports an invalid path, a lookup miss, or missing artwork.
/// Axum rejects a missing `path` parameter before calling this handler.
async fn folder(
    extract::Query(query): extract::Query<IconQuery>,
) -> Result<response::Response, IconError> {
    query.icon(|path| iconator::get_icon_for_folder(path))
}

/// Record the route, status, and time spent obtaining the handler's response.
///
/// # Arguments
///
/// * `request` - Incoming request carrying Axum's matched route when available.
/// * `next` - Remaining middleware and handler to invoke.
///
/// # Returns
///
/// The downstream response. Timing ends before the body is sent to the client.
async fn log_request(request: extract::Request, next: middleware::Next) -> response::Response {
    // The matched route identifies the endpoint without logging a client's file path.
    let route = request.extensions().get::<extract::MatchedPath>().cloned();
    let start = time::Instant::now();
    let response = next.run(request).await;

    tracing::info!(
        name: "http.request.complete",
        route = route.as_ref().map_or("<unmatched>", extract::MatchedPath::as_str),
        status = response.status().as_u16(),
        elapsed_us = start.elapsed().as_micros(),
        "Request complete"
    );

    response
}

/// Build the service used by the TCP listener and in-process tests.
///
/// # Returns
///
/// A router serving [`file()`] and [`folder()`] with [`log_request`] middleware.
fn router() -> axum::Router {
    axum::Router::new()
        .route("/icons/file", routing::get(file))
        .route("/icons/folder", routing::get(folder))
        .layer(middleware::from_fn(log_request))
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();
    let listener = tokio::net::TcpListener::bind(cli.bind)
        .await
        .wrap_err_with(|| format!("failed to bind {}", cli.bind))?;
    let address = listener
        .local_addr()
        .wrap_err("failed to read listener address")?;

    tracing::info!(name: "http.server.listening", %address, "Listening");

    axum::serve(listener, router())
        .await
        .wrap_err("HTTP server failed")
}

#[cfg(test)]
mod tests {
    use axum::{body, http, response};
    use tower::ServiceExt;

    use super::{Artwork, router, svg};

    include!("../../../icons/lookup_table.rs");


    /// Exercise the complete router without opening a socket.
    ///
    /// # Arguments
    ///
    /// * `uri` - Request target including the route and any encoded query.
    ///
    /// # Returns
    ///
    /// The response produced by one GET request.
    ///
    /// # Panics
    ///
    /// Panics if the supplied fixture is an invalid request URI.
    async fn get(uri: &str) -> response::Response {
        let request = http::Request::get(uri).body(body::Body::empty()).unwrap();
        router().oneshot(request).await.unwrap()
    }

    /// Check media type and bytes for extension, exact filename, and folder matches.
    #[tokio::test]
    async fn matches_return_exact_svg_bytes() {
        let cases: &[(&str, &[u8])] = &[
            (
                "/icons/file?path=src%2Flib.rs",
                include_bytes!("../../../icons/525.svg"),
            ),
            (
                "/icons/file?path=project%2F.babelrc.js",
                include_bytes!("../../../icons/40.svg"),
            ),
            (
                "/icons/folder?path=project%2Fsrc",
                include_bytes!("../../../icons/1054.svg"),
            ),
        ];

        for &(uri, expected) in cases {
            let response = get(uri).await;
            assert_eq!(response.status(), http::StatusCode::OK, "{uri}");
            assert_eq!(
                response.headers()[http::header::CONTENT_TYPE],
                "image/svg+xml"
            );

            let body = body::to_bytes(response.into_body(), expected.len())
                .await
                .unwrap();
            assert_eq!(body.as_ref(), expected, "{uri}");
        }
    }

    /// Distinguish malformed queries from valid paths with unknown names on both routes.
    #[tokio::test]
    async fn rejects_invalid_queries_and_distinguishes_lookup_misses() {
        for route in ["/icons/file", "/icons/folder"] {
            for query in ["", "?path=", "?path=%2F"] {
                let uri = format!("{route}{query}");
                assert_eq!(
                    get(&uri).await.status(),
                    http::StatusCode::BAD_REQUEST,
                    "{uri}"
                );
            }

            let uri = format!("{route}?path=unlisted-icon-name");
            assert_eq!(
                get(&uri).await.status(),
                http::StatusCode::NOT_FOUND,
                "{uri}"
            );
        }
    }

    /// Check that a broken bundle yields a generic 500 response.
    #[tokio::test]
    async fn missing_artwork_returns_generic_server_error() {
        use axum::response::IntoResponse;

        let response = svg(u64::MAX).unwrap_err().into_response();
        assert_eq!(response.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            body::to_bytes(response.into_body(), 100).await.unwrap(),
            "icon unavailable"
        );
    }

    /// Catch lookup-table entries whose artwork was omitted from the build.
    #[test]
    fn all_mapped_ids_have_embedded_artwork() {
        for (_, id) in EXT_ICONS.iter().chain(&FILENAME_ICONS).chain(&FOLDER_ICONS) {
            assert!(
                Artwork::get(&format!("{id}.svg")).is_some(),
                "missing {id}.svg"
            );
        }
    }
}
