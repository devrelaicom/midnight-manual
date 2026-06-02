#![allow(missing_docs)]
use mn_embedding::voyage::{InputType, VoyageEmbedder};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn embeds_and_reports_tokens() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [{ "object": "embedding", "embedding": [0.1, 0.2, 0.3], "index": 0 }],
            "model": "voyage-code-3",
            "usage": { "total_tokens": 7 }
        })))
        .mount(&server)
        .await;

    let emb = VoyageEmbedder::new("test-key", "voyage-code-3", 1024, "float")
        .with_base_url(&server.uri());
    let out = emb
        .embed(vec!["hello".into()], InputType::Query)
        .await
        .unwrap();
    assert_eq!(out.vectors, vec![vec![0.1_f32, 0.2, 0.3]]);
    assert_eq!(out.total_tokens, 7);
    assert_eq!(out.model, "voyage-code-3");
}

#[tokio::test]
async fn errors_on_embedding_count_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "object": "list",
            "data": [
                { "embedding": [0.1, 0.2], "index": 0 },
                { "embedding": [0.3, 0.4], "index": 1 }
            ],
            "model": "voyage-code-3",
            "usage": { "total_tokens": 4 }
        })))
        .mount(&server)
        .await;
    let emb = VoyageEmbedder::new("k", "voyage-code-3", 1024, "float").with_base_url(&server.uri());
    // one input, two embeddings returned -> Decode error
    let err = emb
        .embed(vec!["only-one".into()], InputType::Query)
        .await
        .unwrap_err();
    assert!(matches!(err, mn_embedding::voyage::VoyageError::Decode(_)));
}
