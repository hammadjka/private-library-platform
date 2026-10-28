use crate::range::{ByteRange, ParsedRange};
use axum::{
    body::Body,
    extract::State,
    http::{
        header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE},
        HeaderMap, StatusCode,
    },
    response::{Html, Response},
    routing::get,
    Router,
};
use std::{path::PathBuf, sync::Arc};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, SeekFrom},
};
use tokio_util::io::ReaderStream;

pub(crate) const PLAYER_PAGE: &str = include_str!("../client/index.html");

#[derive(Clone)]
struct AppState {
    media_path: Arc<PathBuf>,
}

pub(crate) fn router(media_path: PathBuf) -> Router {
    Router::new()
        .route("/", get(player_page))
        .route("/media", get(media).head(head_media))
        .with_state(AppState {
            media_path: Arc::new(media_path),
        })
}

async fn player_page() -> Html<&'static str> {
    Html(PLAYER_PAGE)
}

async fn media(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, StatusCode> {
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
    let body = Body::from_stream(ReaderStream::new(file.take(content_length)));
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

fn range_not_satisfiable(total_length: u64) -> Result<Response, StatusCode> {
    Response::builder()
        .status(StatusCode::RANGE_NOT_SATISFIABLE)
        .header(CONTENT_LENGTH, "0")
        .header(CONTENT_RANGE, format!("bytes */{total_length}"))
        .header(ACCEPT_RANGES, "bytes")
        .body(Body::empty())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

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
            header::{ACCEPT_RANGES, CONTENT_RANGE, CONTENT_TYPE, RANGE},
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
}
