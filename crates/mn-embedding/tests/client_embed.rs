//! Tests for `client::embed` — both the BYOK (Voyage-direct) and the server
//! (`/v1/embeddings`) resolution modes, against a mocked HTTP endpoint.

#![allow(missing_docs)]

use mn_embedding::client::{embed, EmbedSource};
use mn_embedding::voyage::{InputType, VoyageEmbedder};
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
    let out = embed(vec!["q".into()], InputType::Query, EmbedSource::Byok(&v))
        .await
        .unwrap();
    assert_eq!(out.vectors.len(), 1);
    assert_eq!(out.total_tokens, 3);
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
    let out = embed(
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
