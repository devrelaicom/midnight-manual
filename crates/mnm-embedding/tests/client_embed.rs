//! Tests for `client::embed_code` — both the BYOK (Voyage-direct) and the
//! server (`/v1/embeddings`, `type=code`) resolution modes, against a mocked
//! HTTP endpoint. These pin the shared flat-embed plumbing: retry/backoff
//! classification and the server-proxy wire shape.

#![allow(missing_docs)]

use mnm_embedding::client::{embed_code, EmbedSource};
use mnm_embedding::voyage::{InputType, VoyageEmbedder};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn byok_uses_voyage_directly() {
    let voyage = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{ "embedding": vec![1.0_f32; 4], "index": 0 }],
            "model": "voyage-code-3",
            "usage": { "total_tokens": 3 }
        })))
        .mount(&voyage)
        .await;
    let v = VoyageEmbedder::new("k", "voyage-code-3", 1024, "float").with_base_url(&voyage.uri());
    let out = embed_code(vec!["q".into()], InputType::Query, EmbedSource::Byok(&v))
        .await
        .unwrap();
    assert_eq!(out.vectors.len(), 1);
    assert_eq!(out.total_tokens, 3);
}

#[tokio::test]
async fn byok_retries_transient_503_then_succeeds() {
    let voyage = MockServer::start().await;
    // First call -> 503 (retryable). Higher precedence, exhausted after one hit.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&voyage)
        .await;
    // Subsequent calls -> 200.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [{ "embedding": vec![1.0_f32; 4], "index": 0 }],
            "model": "voyage-code-3",
            "usage": { "total_tokens": 3 }
        })))
        .with_priority(2)
        .mount(&voyage)
        .await;
    let v = VoyageEmbedder::new("k", "voyage-code-3", 1024, "float").with_base_url(&voyage.uri());
    let out = embed_code(vec!["q".into()], InputType::Query, EmbedSource::Byok(&v))
        .await
        .expect("a transient 503 must be retried, not surfaced");
    assert_eq!(out.vectors.len(), 1);
    assert_eq!(out.total_tokens, 3);
}

#[tokio::test]
async fn byok_does_not_retry_400() {
    let voyage = MockServer::start().await;
    // 400 is permanent (e.g. an over-limit batch). `.expect(1)` is verified on
    // server drop: a retry would make it 2+ calls and fail the test.
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(400).set_body_string("too many tokens"))
        .expect(1)
        .mount(&voyage)
        .await;
    let v = VoyageEmbedder::new("k", "voyage-code-3", 1024, "float").with_base_url(&voyage.uri());
    let err = embed_code(vec!["q".into()], InputType::Query, EmbedSource::Byok(&v))
        .await
        .unwrap_err();
    assert!(matches!(err, mnm_embedding::voyage::VoyageError::Status { status: 400, .. }));
}

#[tokio::test]
async fn byok_does_not_retry_client_timeout() {
    // A client-side timeout is not idempotent to retry: the batch may already
    // have reached the server and be consuming tokens, so re-POSTing it would
    // double-count against the shared cap (#164). The server holds the response
    // past the client's 1s deadline; `.expect(1)` (verified on drop) pins that
    // the client fires exactly ONE request — a retry would make it 2 and fail.
    let voyage = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(5))
                .set_body_json(serde_json::json!({
                    "data": [{ "embedding": vec![1.0_f32; 4], "index": 0 }],
                    "model": "voyage-code-3",
                    "usage": { "total_tokens": 3 }
                })),
        )
        .expect(1)
        .mount(&voyage)
        .await;
    let v = VoyageEmbedder::new("k", "voyage-code-3", 1024, "float")
        .with_base_url(&voyage.uri())
        .with_timeout_secs(1);
    let err = embed_code(vec!["q".into()], InputType::Query, EmbedSource::Byok(&v))
        .await
        .unwrap_err();
    assert!(
        matches!(err, mnm_embedding::voyage::VoyageError::Timeout(_)),
        "a client-side timeout must surface as VoyageError::Timeout, got {err:?}",
    );
}

#[tokio::test]
async fn server_mode_calls_v1_embeddings() {
    let srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        // The body must carry the explicit `no_global_limit` flag (false here).
        .and(body_partial_json(serde_json::json!({ "no_global_limit": false })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "voyage-code-3@1",
            "embeddings": [vec![0.5_f32; 4]],
            "usage": { "total_tokens": 2 },
            "rate": {
                "hour": {"limit": 2000, "remaining": 1998, "reset_at": "2030-01-01T00:00:00Z"},
                "day": {"limit": 20000, "remaining": 19998, "reset_at": "2030-01-01T00:00:00Z"}
            }
        })))
        .mount(&srv)
        .await;
    let out = embed_code(
        vec!["q".into()],
        InputType::Query,
        EmbedSource::Server {
            base_url: &srv.uri(),
            bearer: None,
            no_global_limit: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(out.vectors, vec![vec![0.5_f32; 4]]);
    assert_eq!(out.total_tokens, 2);
}

#[tokio::test]
async fn server_mode_sends_no_global_limit_true_on_the_wire() {
    let srv = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        // Prove the admin opt-out travels on the wire: the body must carry
        // `no_global_limit: true`. The mock only matches when it does, so a
        // request that dropped or flipped the flag would 404 and fail the embed.
        .and(body_partial_json(serde_json::json!({ "no_global_limit": true })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "model": "voyage-code-3@1",
            "embeddings": [vec![0.5_f32; 4]],
            "usage": { "total_tokens": 2 },
            "rate": {
                "hour": {"limit": 2000, "remaining": 1998, "reset_at": "2030-01-01T00:00:00Z"},
                "day": {"limit": 20000, "remaining": 19998, "reset_at": "2030-01-01T00:00:00Z"}
            }
        })))
        .mount(&srv)
        .await;
    let out = embed_code(
        vec!["q".into()],
        InputType::Query,
        EmbedSource::Server {
            base_url: &srv.uri(),
            bearer: None,
            no_global_limit: true,
        },
    )
    .await
    .unwrap();
    assert_eq!(out.vectors, vec![vec![0.5_f32; 4]]);
    assert_eq!(out.total_tokens, 2);
}
