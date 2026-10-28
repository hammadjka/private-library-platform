use axum::{response::Html, routing::get, Router};

pub(crate) const PLAYER_PAGE: &str = include_str!("../client/index.html");

// Milestone 0.1.6 will make this the executable's production router.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn router() -> Router {
    Router::new().route("/", get(player_page))
}

// This is reached through `router`; the raw TCP server remains active for now.
#[cfg_attr(not(test), allow(dead_code))]
async fn player_page() -> Html<&'static str> {
    Html(PLAYER_PAGE)
}

#[cfg(test)]
mod tests {
    use super::{router, PLAYER_PAGE};
    use axum::{
        body::{to_bytes, Body},
        http::{header::CONTENT_TYPE, Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn player_route_returns_the_embedded_page() {
        let response = router()
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
}
