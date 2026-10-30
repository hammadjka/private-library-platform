//! Axum application layer for the browser page and media endpoint.
//!
//! This module owns routes, application state, request policy, and HTTP
//! responses. It does not accept sockets or parse HTTP/1; those transport
//! responsibilities live in `transport`.

use crate::range::{ByteRange, ParsedRange};
use axum::{
    body::Body,
    extract::{Request, State},
    http::{
        header::{
            ACCEPT_RANGES, CONNECTION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
            RETRY_AFTER, TRANSFER_ENCODING,
        },
        HeaderMap, StatusCode,
    },
    middleware::{self, Next},
    response::{Html, Response},
    routing::get,
    Router,
};
use futures_core::Stream;
use std::{path::PathBuf, sync::Arc};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, SeekFrom},
    sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError},
};
use tokio_util::io::ReaderStream;

/// Browser player embedded into the executable at compile time.
pub(crate) const PLAYER_PAGE: &str = include_str!("../client/index.html");
/// Maximum number of response bodies that may actively hold media capacity.
const MAX_MEDIA_STREAMS: usize = 16;

/// Immutable application resources shared by every cloned Axum router.
#[derive(Clone)]
struct AppState {
    /// Path selected by the operator when the daemon started.
    media_path: Arc<PathBuf>,
    /// Permits for response bodies that can still produce media bytes.
    media_streams: Arc<Semaphore>,
}

/// A response-body stream that owns a media-capacity permit.
///
/// Handlers return before Hyper finishes sending their response bodies. Keeping
/// the permit inside this wrapper makes capacity follow the body lifetime rather
/// than the shorter handler-future lifetime.
struct PermitStream<S> {
    /// Stream that produces the actual body chunks.
    inner: S,
    /// Released automatically when the body completes or is dropped.
    _permit: OwnedSemaphorePermit,
}

impl<S> Stream for PermitStream<S>
where
    S: Stream + Unpin,
{
    type Item = S::Item;

    /// Delegates body production to the inner stream while retaining ownership
    /// of the permit.
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

/// Builds the complete HTTP application for one configured media file.
///
/// The returned router is cheap to clone: its media path and semaphore are
/// reference-counted in `AppState`.
pub(crate) fn router(media_path: PathBuf) -> Router {
    Router::new()
        .route("/", get(player_page))
        .route("/media", get(media).head(head_media))
        .with_state(AppState {
            media_path: Arc::new(media_path),
            media_streams: Arc::new(Semaphore::new(MAX_MEDIA_STREAMS)),
        })
        .layer(middleware::from_fn(reject_request_body))
}

/// Returns the embedded diagnostic player at `GET /`.
async fn player_page() -> Html<&'static str> {
    Html(PLAYER_PAGE)
}

/// Rejects framed request bodies before routing to an application handler.
///
/// No current endpoint accepts uploads. A non-zero or invalid `Content-Length`,
/// or any `Transfer-Encoding`, therefore receives `413 Payload Too Large`. The
/// connection is closed because unread body bytes cannot safely be interpreted
/// as the next HTTP/1 request.
async fn reject_request_body(request: Request, next: Next) -> Response {
    let content_length_is_nonzero_or_invalid = request
        .headers()
        .get(CONTENT_LENGTH)
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                != Some(0)
        })
        .unwrap_or(false);
    let uses_transfer_encoding = request.headers().contains_key(TRANSFER_ENCODING);

    if content_length_is_nonzero_or_invalid || uses_transfer_encoding {
        return Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header(CONTENT_LENGTH, "0")
            .header(CONNECTION, "close")
            .body(Body::empty())
            .expect("static response headers are valid");
    }

    next.run(request).await
}

/// Serves the complete media file or one satisfiable byte range.
///
/// File bytes are streamed with backpressure. The function constructs and
/// returns a response; Hyper polls its body after this handler future finishes.
async fn media(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, StatusCode> {
    let permit = match Arc::clone(&state.media_streams).try_acquire_owned() {
        Ok(permit) => permit,
        Err(TryAcquireError::NoPermits) => return overloaded_response(),
        Err(TryAcquireError::Closed) => return Err(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let mut file = File::open(state.media_path.as_ref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total_length = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();
    let byte_range = match headers.get(RANGE) {
        Some(value) => match value.to_str() {
            Ok(value) => match ByteRange::parse(value, total_length) {
                ParsedRange::Selected(range) => Some(range),
                // An unknown unit is ignored, so the response describes the
                // complete representation instead of interpreting it as bytes.
                ParsedRange::UnsupportedUnit => None,
                ParsedRange::Invalid => return range_not_satisfiable(total_length),
            },
            Err(_) => return range_not_satisfiable(total_length),
        },
        None => None,
    };

    let (status, content_length, content_range) = match byte_range {
        Some(range) => {
            file.seek(SeekFrom::Start(range.start()))
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            (
                StatusCode::PARTIAL_CONTENT,
                range.length(),
                Some(format!(
                    "bytes {}-{}/{}",
                    range.start(),
                    range.end(),
                    total_length
                )),
            )
        }
        None => (StatusCode::OK, total_length, None),
    };

    let content_type = content_type_for(state.media_path.as_ref());
    // `take` exposes EOF after exactly `content_length` bytes. ReaderStream
    // converts those bounded async reads into chunks Hyper can poll without
    // loading the full file into memory.
    let body = Body::from_stream(PermitStream {
        inner: ReaderStream::new(file.take(content_length)),
        _permit: permit,
    });
    let mut response = Response::builder()
        .status(status)
        .header(CONTENT_LENGTH, content_length.to_string())
        .header(CONTENT_TYPE, content_type)
        .header(ACCEPT_RANGES, "bytes");
    if let Some(content_range) = content_range {
        response = response.header(CONTENT_RANGE, content_range);
    }

    response
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Builds the retryable response returned when all media permits are occupied.
fn overloaded_response() -> Result<Response, StatusCode> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(CONTENT_LENGTH, "0")
        .header(RETRY_AFTER, "1")
        .body(Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Describes the complete media representation without constructing a body
/// stream or consuming media-stream capacity.
async fn head_media(State(state): State<AppState>) -> Result<Response, StatusCode> {
    let file = File::open(state.media_path.as_ref())
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let total_length = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();

    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_LENGTH, total_length.to_string())
        .header(CONTENT_TYPE, content_type_for(state.media_path.as_ref()))
        .header(ACCEPT_RANGES, "bytes")
        .body(Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Builds the empty `416 Range Not Satisfiable` response for this file length.
fn range_not_satisfiable(total_length: u64) -> Result<Response, StatusCode> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(CONTENT_LENGTH, "0")
        .header(CONTENT_RANGE, format!("bytes */{total_length}"))
        .header(ACCEPT_RANGES, "bytes")
        .body(Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// Maps the small set of currently supported filename extensions to media
/// types, falling back to opaque binary data.
fn content_type_for(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::{router, PLAYER_PAGE};
    use axum::{
        body::{to_bytes, Body},
        http::{
            header::{
                ACCEPT_RANGES, CONNECTION, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
                RETRY_AFTER, TRANSFER_ENCODING,
            },
            Method, Request, StatusCode,
        },
    };
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };
    use tower::ServiceExt;

    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

    struct TestMedia {
        path: PathBuf,
    }

    impl TestMedia {
        fn new(bytes: &[u8]) -> Self {
            let id = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("plpd-axum-media-{}-{id}.mp4", std::process::id()));
            fs::write(&path, bytes).expect("write test media");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestMedia {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn player_route_returns_the_embedded_page() {
        let response = router(PathBuf::new())
            .oneshot(
                Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE),
            Some(&"text/html; charset=utf-8".parse().expect("valid header"))
        );

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert_eq!(body.as_ref(), PLAYER_PAGE.as_bytes());
    }

    #[tokio::test]
    async fn media_route_streams_the_complete_file() {
        let media = TestMedia::new(b"video-bytes");

        let response = router(media.path().to_owned())
            .oneshot(
                Request::builder()
                    .uri("/media")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-length").unwrap(), "11");
        assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "video/mp4");
        assert_eq!(response.headers().get("accept-ranges").unwrap(), "bytes");

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert_eq!(body.as_ref(), b"video-bytes");
    }

    #[tokio::test]
    async fn media_route_streams_supported_single_byte_ranges() {
        let media = TestMedia::new(b"video-bytes");
        let cases = [
            ("bytes=2-5", "bytes 2-5/11", b"deo-".as_slice()),
            ("bytes=6-", "bytes 6-10/11", b"bytes".as_slice()),
            ("bytes=-5", "bytes 6-10/11", b"bytes".as_slice()),
            ("bytes=6-999", "bytes 6-10/11", b"bytes".as_slice()),
            ("bytes=-999", "bytes 0-10/11", b"video-bytes".as_slice()),
            ("Bytes=6-10", "bytes 6-10/11", b"bytes".as_slice()),
        ];

        for (range, expected_content_range, expected_body) in cases {
            let response = router(media.path().to_owned())
                .oneshot(
                    Request::builder()
                        .uri("/media")
                        .header(RANGE, range)
                        .body(Body::empty())
                        .expect("build request"),
                )
                .await
                .expect("route request");

            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
            assert_eq!(
                response.headers().get("content-length").unwrap(),
                expected_body.len().to_string().as_str(),
                "{range}"
            );
            assert_eq!(
                response.headers().get(CONTENT_RANGE).unwrap(),
                expected_content_range,
                "{range}"
            );

            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read response body");
            assert_eq!(body.as_ref(), expected_body, "{range}");
        }
    }

    #[tokio::test]
    async fn media_head_describes_the_full_file_and_sends_no_bytes() {
        let media = TestMedia::new(b"video-bytes");

        for range in [
            None,
            Some("bytes=6-"),
            Some("bytes=0-1,4-5"),
            Some("bytes=not-a-range"),
        ] {
            let mut request = Request::builder().method(Method::HEAD).uri("/media");
            if let Some(range) = range {
                request = request.header(RANGE, range);
            }
            let response = router(media.path().to_owned())
                .oneshot(request.body(Body::empty()).expect("build request"))
                .await
                .expect("route request");

            assert_eq!(response.status(), StatusCode::OK, "{range:?}");
            assert_eq!(response.headers().get("content-length").unwrap(), "11");
            assert_eq!(response.headers().get(CONTENT_TYPE).unwrap(), "video/mp4");
            assert_eq!(response.headers().get(ACCEPT_RANGES).unwrap(), "bytes");
            assert!(response.headers().get(CONTENT_RANGE).is_none(), "{range:?}");

            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read response body");
            assert!(body.is_empty(), "{range:?}");
        }
    }

    #[tokio::test]
    async fn media_route_rejects_invalid_unsatisfiable_or_multiple_ranges() {
        let media = TestMedia::new(b"video-bytes");
        let ranges = [
            "bytes=8-2",
            "bytes=not-a-range",
            "bytes=1-2-3",
            "bytes=-",
            "bytes=11-12",
            "bytes=-0",
            "bytes=18446744073709551616-",
            "bytes=0-1,4-5",
        ];

        for range in ranges {
            let response = router(media.path().to_owned())
                .oneshot(
                    Request::builder()
                        .uri("/media")
                        .header(RANGE, range)
                        .body(Body::empty())
                        .expect("build request"),
                )
                .await
                .expect("route request");

            assert_eq!(
                response.status(),
                StatusCode::RANGE_NOT_SATISFIABLE,
                "{range}"
            );
            assert_eq!(response.headers().get("content-length").unwrap(), "0");
            assert_eq!(
                response.headers().get(CONTENT_RANGE).unwrap(),
                "bytes */11",
                "{range}"
            );
            assert_eq!(response.headers().get("accept-ranges").unwrap(), "bytes");

            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read response body");
            assert!(body.is_empty(), "{range}");
        }
    }

    #[tokio::test]
    async fn media_route_ignores_an_unsupported_range_unit() {
        let media = TestMedia::new(b"video-bytes");

        let response = router(media.path().to_owned())
            .oneshot(
                Request::builder()
                    .uri("/media")
                    .header(RANGE, "items=0-1")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get("content-length").unwrap(), "11");
        assert!(response.headers().get(CONTENT_RANGE).is_none());

        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert_eq!(body.as_ref(), b"video-bytes");
    }

    #[tokio::test]
    async fn media_route_handles_an_empty_file() {
        let media = TestMedia::new(b"");

        let full_response = router(media.path().to_owned())
            .oneshot(
                Request::builder()
                    .uri("/media")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        assert_eq!(full_response.status(), StatusCode::OK);
        assert_eq!(full_response.headers().get("content-length").unwrap(), "0");
        let body = to_bytes(full_response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert!(body.is_empty());

        let range_response = router(media.path().to_owned())
            .oneshot(
                Request::builder()
                    .uri("/media")
                    .header(RANGE, "bytes=0-0")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        assert_eq!(range_response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(
            range_response.headers().get(CONTENT_RANGE).unwrap(),
            "bytes */0"
        );
        let body = to_bytes(range_response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn unknown_route_returns_not_found_without_details() {
        let response = router(PathBuf::new())
            .oneshot(
                Request::builder()
                    .uri("/unknown")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn known_route_with_unsupported_method_returns_method_not_allowed() {
        let response = router(PathBuf::new())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/media")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn media_stream_capacity_is_held_until_the_response_body_is_dropped() {
        let media = TestMedia::new(b"video-bytes");
        let app = router(media.path().to_owned());
        let mut active_responses = Vec::new();

        for _ in 0..16 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/media")
                        .body(Body::empty())
                        .expect("build request"),
                )
                .await
                .expect("route request");
            assert_eq!(response.status(), StatusCode::OK);
            active_responses.push(response);
        }

        let overloaded = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/media")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        assert_eq!(overloaded.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(overloaded.headers().get(RETRY_AFTER).unwrap(), "1");
        let body = to_bytes(overloaded.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert!(body.is_empty());

        active_responses.pop();
        let recovered = app
            .oneshot(
                Request::builder()
                    .uri("/media")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("route request");
        assert_eq!(recovered.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn request_bodies_are_rejected_and_the_connection_is_closed() {
        for (header, value) in [(CONTENT_LENGTH, "1"), (TRANSFER_ENCODING, "chunked")] {
            let response = router(PathBuf::new())
                .oneshot(
                    Request::builder()
                        .uri("/")
                        .header(header, value)
                        .body(Body::from("x"))
                        .expect("build request"),
                )
                .await
                .expect("route request");

            assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
            assert_eq!(response.headers().get(CONTENT_LENGTH).unwrap(), "0");
            assert_eq!(response.headers().get(CONNECTION).unwrap(), "close");
            let body = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("read response body");
            assert!(body.is_empty());
        }
    }
}
