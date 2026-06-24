//! Hosted Hugging Face model-detector client (issue #103).
//!
//! Wraps a self-hosted HF text-classification endpoint (Llama-Prompt-Guard-2)
//! used as the optional second leg of the ingest-time injection scan. The
//! endpoint returns per-label scores; [`HfClient::score`] splits the input into
//! ≤512-token windows, classifies each, and assembles a
//! [`mnm_core::injection::ModelReport`].
//!
//! HF Inference Endpoints "scale to zero": the first request after an idle
//! period returns HTTP 502/503 while the endpoint cold-starts. Both
//! [`HfClient::classify_window`] (a small retry budget) and
//! [`HfClient::service_start`] (a long warm-and-wait) treat 502/503 as a
//! transient cold-start signal and back off.

use std::time::{Duration, Instant};

use mnm_core::injection::{FlaggedWindow, ModelReport};

/// Approximate window size in CHARACTERS. The model's context is ~512 tokens;
/// at roughly 4 chars/token that is ~2000 chars. We split on char boundaries
/// (an over-estimate of tokens is safe — the endpoint truncates internally).
const WINDOW_CHARS: usize = 2_000;

/// Max retry attempts for a single [`HfClient::classify_window`] call on a
/// 502/503 cold-start response.
const MAX_RETRY_ATTEMPTS: u32 = 5;

/// Wall-clock ceiling for one [`HfClient::classify_window`]'s internal retry
/// loop. `service_start` extends patience beyond this via its own outer loop.
const RETRY_BUDGET: Duration = Duration::from_secs(20);

/// Lowercased label names treated as benign by [`parse_malicious_prob`].
const BENIGN_LABELS: [&str; 3] = ["label_0", "benign", "negative"];

/// Lowercased label names treated as explicitly malicious by
/// [`parse_malicious_prob`] (used for the single-label disambiguation).
const MALICIOUS_LABELS: [&str; 4] = ["label_1", "injection", "jailbreak", "malicious"];

/// Connect timeout for the HF client.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-request timeout for the HF client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Hosted model-detector HTTP client.
///
/// Cheap to clone is NOT required — held behind an `Arc` in
/// [`crate::injection::scan::InjectionState`].
#[derive(Debug)]
pub struct HfClient {
    client: reqwest::Client,
    endpoint: String,
    token: String,
    model: Option<String>,
}

impl HfClient {
    /// Build a client for the given HF endpoint URL + bearer token.
    ///
    /// Unlike the Voyage client this does NOT force HTTP/1.1 — that was a
    /// Voyage-specific workaround; HF endpoints behave on reqwest's default
    /// HTTP/2 negotiation.
    ///
    /// # Errors
    ///
    /// Returns [`HfError::Build`] when the underlying reqwest client cannot be
    /// constructed (e.g. TLS init failure).
    pub fn new(
        endpoint: impl Into<String>,
        token: impl Into<String>,
        model: Option<String>,
    ) -> Result<Self, HfError> {
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| HfError::Build(e.to_string()))?;
        Ok(Self {
            client,
            endpoint: endpoint.into(),
            token: token.into(),
            model,
        })
    }

    /// Score `text` by splitting it into ≤[`WINDOW_CHARS`]-char windows on char
    /// boundaries and classifying each. Returns the max malicious probability
    /// across windows and per-window detail for windows that met
    /// `model_threshold` (spans are byte offsets into the ORIGINAL input).
    ///
    /// Empty `text` returns an available report with score `0.0` and no
    /// windows (no request is sent).
    ///
    /// # Errors
    ///
    /// Returns the first [`HfError`] any window classification produced.
    pub async fn score(&self, text: &str, model_threshold: f64) -> Result<ModelReport, HfError> {
        if text.is_empty() {
            return Ok(ModelReport {
                available: true,
                score: 0.0,
                flagged_windows: vec![],
            });
        }
        let mut max_score = 0.0_f64;
        let mut flagged_windows = Vec::new();
        for (start, end) in char_windows(text, WINDOW_CHARS) {
            let window = &text[start..end];
            let score = self.classify_window(window).await?;
            if score > max_score {
                max_score = score;
            }
            if score >= model_threshold {
                // `span` is a `[usize; 2]`; an array literal of the two byte
                // offsets is the natural fill. The nursery lint mistakes this
                // for a tuple→array conversion.
                #[allow(clippy::tuple_array_conversions)]
                let span = [start, end];
                flagged_windows.push(FlaggedWindow { span, score });
            }
        }
        Ok(ModelReport {
            available: true,
            score: max_score,
            flagged_windows,
        })
    }

    /// Classify a single window, returning its malicious probability.
    ///
    /// HTTP 502/503 means the HF endpoint is cold-starting; this retries with
    /// jittered exponential backoff up to [`MAX_RETRY_ATTEMPTS`] / the
    /// [`RETRY_BUDGET`] wall-clock ceiling. Transport errors are NOT retried
    /// here (kept simple — only cold-start status codes warrant a wait). A
    /// non-2xx that is not 502/503 returns [`HfError::Status`]; a 2xx whose body
    /// has no parseable label/score pairs returns [`HfError::Decode`].
    ///
    /// # Errors
    ///
    /// Returns [`HfError::Http`] on a transport error, [`HfError::Status`] on a
    /// non-retryable non-2xx (including a 502/503 that outlived the retry
    /// budget), or [`HfError::Decode`] when the body shape is unrecognizable.
    async fn classify_window(&self, text: &str) -> Result<f64, HfError> {
        let body = self.payload(text);
        let deadline = Instant::now() + RETRY_BUDGET;
        let mut last_status: Option<(u16, String)> = None;
        for attempt in 0..MAX_RETRY_ATTEMPTS {
            let resp = self
                .client
                .post(&self.endpoint)
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
                .map_err(|e| HfError::Http(e.to_string()))?;
            let status = resp.status();
            if status.is_success() {
                let value: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| HfError::Decode(e.to_string()))?;
                return parse_malicious_prob(&value)
                    .ok_or_else(|| HfError::Decode(format!("unrecognized HF response: {value}")));
            }
            let code = status.as_u16();
            let text_body = resp.text().await.unwrap_or_default();
            if code == 502 || code == 503 {
                last_status = Some((code, text_body));
                // Cold start: back off and retry while the budget allows.
                if attempt + 1 >= MAX_RETRY_ATTEMPTS {
                    break;
                }
                let backoff = backoff_for_attempt(attempt);
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                tokio::time::sleep(backoff.min(remaining)).await;
            } else {
                return Err(HfError::Status { status: code, body: text_body });
            }
        }
        let (status, body) = last_status
            .unwrap_or_else(|| (503, "HF endpoint cold-start retry budget exhausted".to_owned()));
        Err(HfError::Status { status, body })
    }

    /// Idempotent warm-and-wait. Repeatedly pings the endpoint until it answers
    /// (returns `Ok(true)`) or `deadline` elapses (returns `Ok(false)`).
    ///
    /// [`classify_window`](Self::classify_window) already retries 502/503 over
    /// its own short budget then surfaces the 502/503 as [`HfError::Status`];
    /// this outer loop catches that condition and keeps retrying for the full
    /// `deadline`, so service-start gets longer patience than an inline scan.
    /// Any other error propagates immediately.
    ///
    /// # Errors
    ///
    /// Returns the first non-cold-start [`HfError`] encountered.
    pub async fn service_start(&self, deadline: Duration) -> Result<bool, HfError> {
        let until = Instant::now() + deadline;
        let mut attempt = 0_u32;
        loop {
            match self.classify_window("ping").await {
                Ok(_) => return Ok(true),
                Err(HfError::Status { status: 502 | 503, .. }) => {
                    if Instant::now() >= until {
                        return Ok(false);
                    }
                    let backoff = backoff_for_attempt(attempt);
                    let remaining = until.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        return Ok(false);
                    }
                    tokio::time::sleep(backoff.min(remaining)).await;
                    attempt = attempt.saturating_add(1);
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Build the HF request payload. Includes the optional model id when set.
    fn payload(&self, text: &str) -> serde_json::Value {
        self.model.as_ref().map_or_else(
            || serde_json::json!({ "inputs": text }),
            |model| serde_json::json!({ "inputs": text, "model": model }),
        )
    }
}

/// Yield `[start, end)` byte spans for each ≤`window_chars`-char window of
/// `text`, split on char boundaries. The returned spans tile `text` exactly.
fn char_windows(text: &str, window_chars: usize) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = 0_usize;
    let mut count = 0_usize;
    for (idx, ch) in text.char_indices() {
        count += 1;
        if count >= window_chars {
            let end = idx + ch.len_utf8();
            spans.push((start, end));
            start = end;
            count = 0;
        }
    }
    if start < text.len() {
        spans.push((start, text.len()));
    }
    spans
}

/// Parse the malicious probability out of an HF text-classification response.
///
/// HF endpoints return either `[[{"label","score"},...]]` (nested) or
/// `[{"label","score"},...]` (flat). The "malicious probability" is the score
/// of the first label whose lowercased name is NOT a known benign label
/// (`label_0`, `benign`, `negative`). A single benign label is treated as its
/// complement (`1.0 - score`); a single explicitly-malicious label
/// (`label_1`, `injection`, `jailbreak`, `malicious`) is taken at face value.
///
/// Returns `None` only when the JSON has no parseable label/score pairs.
pub(crate) fn parse_malicious_prob(v: &serde_json::Value) -> Option<f64> {
    let outer = v.as_array()?;
    // Unwrap one level of nesting if the first element is itself an array.
    let labels = match outer.first() {
        Some(first) if first.is_array() => first.as_array()?,
        _ => outer,
    };

    let pairs: Vec<(String, f64)> = labels
        .iter()
        .filter_map(|item| {
            let label = item.get("label")?.as_str()?.to_lowercase();
            let score = item.get("score")?.as_f64()?;
            Some((label, score))
        })
        .collect();

    if pairs.is_empty() {
        return None;
    }

    if pairs.len() == 1 {
        let (label, score) = &pairs[0];
        if MALICIOUS_LABELS.contains(&label.as_str()) {
            return Some(*score);
        }
        if BENIGN_LABELS.contains(&label.as_str()) {
            return Some(1.0 - score);
        }
        // A single unknown label: take it at face value as the malicious score.
        return Some(*score);
    }

    // Multiple labels: the first non-benign score is the malicious probability.
    if let Some((_, score)) = pairs
        .iter()
        .find(|(label, _)| !BENIGN_LABELS.contains(&label.as_str()))
    {
        return Some(*score);
    }

    // Every label was benign (e.g. a `negative`/`neutral`-only response from a
    // differently-labeled model). Treat the malicious probability as the
    // complement of the strongest benign score rather than `None` — returning
    // `None` would surface as a decode error and, under fail-closed, wrongly
    // reject benign content.
    let max_benign = pairs
        .iter()
        .map(|(_, score)| *score)
        .fold(0.0_f64, f64::max);
    Some((1.0 - max_benign).clamp(0.0, 1.0))
}

/// Backoff schedule: 100ms, 500ms, 2s. Each value gets +/- 25% jitter derived
/// from the wall-clock subsec nanos, so we don't pull a full RNG dep just for
/// jitter. Lifted from `mnm-telemetry`'s client.
fn backoff_for_attempt(attempt: u32) -> Duration {
    let base_ms: u64 = match attempt {
        0 => 100,
        1 => 500,
        _ => 2_000,
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    let jitter_pct: i64 = i64::try_from(nanos % 51).unwrap_or(0) - 25; // -25..=25
    let base_i: i64 = i64::try_from(base_ms).unwrap_or(i64::MAX);
    let adjusted = jitter_pct.saturating_mul(base_i) / 100;
    let final_i = base_i.saturating_add(adjusted).max(0);
    Duration::from_millis(u64::try_from(final_i).unwrap_or(0))
}

/// Errors the HF model client can produce.
#[derive(Debug, thiserror::Error)]
pub enum HfError {
    /// The underlying reqwest client could not be built.
    #[error("hf client build failed: {0}")]
    Build(String),
    /// A transport-level error (connection refused, timeout, …).
    #[error("hf http error: {0}")]
    Http(String),
    /// The endpoint returned a non-2xx status (502/503 ⇒ cold start).
    #[error("hf returned status {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Response body, if readable.
        body: String,
    },
    /// The 2xx response body could not be decoded into label/score pairs.
    #[error("hf response decode error: {0}")]
    Decode(String),
}

#[cfg(test)]
mod tests {
    use super::{backoff_for_attempt, char_windows, parse_malicious_prob};

    const EPS: f64 = 1e-9;

    #[test]
    fn parse_nested_picks_non_benign_label() {
        let v = serde_json::json!([[
            {"label": "INJECTION", "score": 0.97},
            {"label": "LABEL_0", "score": 0.03}
        ]]);
        let got = parse_malicious_prob(&v).unwrap();
        assert!((got - 0.97).abs() < EPS, "got {got}");
    }

    #[test]
    fn parse_flat_single_benign_is_complement() {
        let v = serde_json::json!([{"label": "BENIGN", "score": 0.92}]);
        let got = parse_malicious_prob(&v).unwrap();
        assert!((got - 0.08).abs() < EPS, "got {got}");
    }

    #[test]
    fn parse_flat_two_label_picks_malicious() {
        let v = serde_json::json!([
            {"label": "LABEL_0", "score": 0.2},
            {"label": "LABEL_1", "score": 0.8}
        ]);
        let got = parse_malicious_prob(&v).unwrap();
        assert!((got - 0.8).abs() < EPS, "got {got}");
    }

    #[test]
    fn parse_single_malicious_label_taken_at_face_value() {
        let v = serde_json::json!([{"label": "LABEL_1", "score": 0.73}]);
        let got = parse_malicious_prob(&v).unwrap();
        assert!((got - 0.73).abs() < EPS, "got {got}");
    }

    #[test]
    fn parse_unrecognizable_shapes_are_none() {
        assert!(parse_malicious_prob(&serde_json::json!({"foo": 1})).is_none());
        assert!(parse_malicious_prob(&serde_json::json!([])).is_none());
        assert!(parse_malicious_prob(&serde_json::json!([{"no_label": 1}])).is_none());
    }

    #[test]
    fn parse_all_benign_multilabel_is_complement_not_error() {
        // A differently-labeled model that emits only benign labels must NOT
        // decode to None (which would fail-closed-reject benign content); it
        // resolves to 1 - max(benign score).
        let v = serde_json::json!([
            {"label": "negative", "score": 0.9},
            {"label": "neutral", "score": 0.1}
        ]);
        let got = parse_malicious_prob(&v).unwrap();
        assert!((got - 0.1).abs() < EPS, "got {got}");
    }

    #[test]
    fn backoff_within_jitter_window() {
        for _ in 0..50 {
            let a0 = backoff_for_attempt(0).as_millis();
            assert!((75..=125).contains(&a0), "attempt 0: {a0}ms outside [75, 125]");
            let a2 = backoff_for_attempt(2).as_millis();
            assert!((1500..=2500).contains(&a2), "attempt 2: {a2}ms outside [1500, 2500]");
        }
    }

    #[test]
    fn char_windows_tile_input_exactly() {
        let text = "abcdefghij"; // 10 ascii chars
        let spans = char_windows(text, 4);
        assert_eq!(spans, vec![(0, 4), (4, 8), (8, 10)]);
        // Reassembling the spans reproduces the input.
        let joined: String = spans.iter().map(|&(s, e)| &text[s..e]).collect();
        assert_eq!(joined, text);
    }

    #[test]
    fn char_windows_respects_multibyte_boundaries() {
        let text = "héllo wörld"; // contains 2-byte chars
        let spans = char_windows(text, 3);
        // Every span must be a valid char boundary slice (no panic).
        let joined: String = spans.iter().map(|&(s, e)| &text[s..e]).collect();
        assert_eq!(joined, text);
    }
}
