use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Retry and timeout configuration for webhook delivery.
#[derive(Debug, Clone, Copy)]
pub struct WebhookRetryConfig {
    max_attempts: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl WebhookRetryConfig {
    pub fn new(max_attempts: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_attempts: max_attempts.max(1),
            base_delay,
            max_delay,
        }
    }

    pub fn single_attempt() -> Self {
        Self::new(1, Duration::ZERO, Duration::ZERO)
    }

    pub fn max_attempts(&self) -> u32 {
        self.max_attempts
    }

    pub fn base_delay(&self) -> Duration {
        self.base_delay
    }

    pub fn max_delay(&self) -> Duration {
        self.max_delay
    }

    fn retry_delay(&self, attempts: u32) -> Duration {
        let multiplier = 2_u32.saturating_pow(attempts.saturating_sub(1));
        let delay = self
            .base_delay
            .checked_mul(multiplier)
            .unwrap_or(self.max_delay);
        delay.min(self.max_delay)
    }
}

impl Default for WebhookRetryConfig {
    fn default() -> Self {
        Self::new(3, Duration::from_millis(500), Duration::from_secs(5))
    }
}

#[derive(Debug, Clone)]
pub struct WebhookDeliveryError {
    attempts: u32,
    message: String,
}

impl WebhookDeliveryError {
    fn new(attempts: u32, message: String) -> Self {
        Self { attempts, message }
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for WebhookDeliveryError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "webhook delivery failed after {} attempt(s): {}",
            self.attempts, self.message
        )
    }
}

impl Error for WebhookDeliveryError {}

/// Shared webhook client used across services.
#[derive(Debug, Clone)]
pub struct WebhookClient {
    client: reqwest::Client,
    retry_config: WebhookRetryConfig,
}

impl WebhookClient {
    pub fn new(
        timeout: Duration,
        retry_config: WebhookRetryConfig,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self {
            client,
            retry_config,
        })
    }

    /// POST JSON to a webhook URL, retrying according to configured policy.
    pub async fn post_json<T: Serialize + ?Sized>(
        &self,
        url: &str,
        payload: &T,
        context: &str,
    ) -> Result<(), WebhookDeliveryError> {
        let mut attempts = 0_u32;

        loop {
            attempts += 1;

            let last_error = match self.client.post(url).json(payload).send().await {
                Ok(response) if response.status().is_success() => return Ok(()),
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    if body.is_empty() {
                        format!("HTTP {}", status)
                    } else {
                        format!("HTTP {}: {}", status, body)
                    }
                }
                Err(error) => format!("request failed: {}", error),
            };

            if attempts >= self.retry_config.max_attempts {
                return Err(WebhookDeliveryError::new(attempts, last_error));
            }

            let delay = self.retry_config.retry_delay(attempts);
            warn!(
                "Webhook attempt {} failed for {}. Retrying in {:?}: {}",
                attempts, context, delay, last_error
            );

            if delay > Duration::ZERO {
                sleep(delay).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_config_clamps_attempts_to_one() {
        let config = WebhookRetryConfig::new(0, Duration::from_millis(10), Duration::from_secs(1));
        assert_eq!(config.max_attempts(), 1);
    }

    #[test]
    fn retry_delay_exponential_backoff() {
        let config =
            WebhookRetryConfig::new(5, Duration::from_millis(100), Duration::from_secs(10));
        // attempt 1: base * 2^0 = 100ms
        assert_eq!(config.retry_delay(1), Duration::from_millis(100));
        // attempt 2: base * 2^1 = 200ms
        assert_eq!(config.retry_delay(2), Duration::from_millis(200));
        // attempt 3: base * 2^2 = 400ms
        assert_eq!(config.retry_delay(3), Duration::from_millis(400));
    }

    #[test]
    fn retry_delay_capped_at_max() {
        let config = WebhookRetryConfig::new(10, Duration::from_secs(1), Duration::from_secs(5));
        // attempt 5: base * 2^4 = 16s, but capped at 5s
        assert_eq!(config.retry_delay(5), Duration::from_secs(5));
    }

    // ── post_json retry logic (wiremock) ──────────────────────────────────────

    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    #[derive(Serialize)]
    struct TestPayload {
        msg: String,
    }

    #[tokio::test]
    async fn post_json_success_on_first_attempt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            WebhookClient::new(Duration::from_secs(5), WebhookRetryConfig::single_attempt())
                .unwrap();

        let result = client
            .post_json(&server.uri(), &TestPayload { msg: "hi".into() }, "test")
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn post_json_retries_then_succeeds() {
        let server = MockServer::start().await;

        // First attempt: 500, second attempt: 200
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let client = WebhookClient::new(
            Duration::from_secs(5),
            // 2 attempts, no delay for test speed
            WebhookRetryConfig::new(2, Duration::ZERO, Duration::ZERO),
        )
        .unwrap();

        let result = client
            .post_json(
                &server.uri(),
                &TestPayload {
                    msg: "retry".into(),
                },
                "test",
            )
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn post_json_exhausts_retries() {
        let server = MockServer::start().await;

        // Always return 500
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_string("server error"))
            .expect(3)
            .mount(&server)
            .await;

        let client = WebhookClient::new(
            Duration::from_secs(5),
            WebhookRetryConfig::new(3, Duration::ZERO, Duration::ZERO),
        )
        .unwrap();

        let result = client
            .post_json(
                &server.uri(),
                &TestPayload { msg: "fail".into() },
                "test-ctx",
            )
            .await;
        let err = result.unwrap_err();
        assert_eq!(err.attempts(), 3);
        assert!(err.message().contains("500"));
        assert!(err.message().contains("server error"));
    }

    #[tokio::test]
    async fn post_json_single_attempt_no_retry() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            WebhookClient::new(Duration::from_secs(5), WebhookRetryConfig::single_attempt())
                .unwrap();

        let result = client
            .post_json(&server.uri(), &TestPayload { msg: "once".into() }, "test")
            .await;
        let err = result.unwrap_err();
        assert_eq!(err.attempts(), 1);
    }

    #[tokio::test]
    async fn post_json_empty_error_body() {
        let server = MockServer::start().await;

        // 502 with no body
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(502))
            .expect(1)
            .mount(&server)
            .await;

        let client =
            WebhookClient::new(Duration::from_secs(5), WebhookRetryConfig::single_attempt())
                .unwrap();

        let result = client
            .post_json(&server.uri(), &TestPayload { msg: "x".into() }, "test")
            .await;
        let err = result.unwrap_err();
        // Empty body → "HTTP 502 Bad Gateway" (no colon-separated body)
        assert!(err.message().contains("HTTP 502"), "got: {}", err.message());
        assert!(!err.message().contains(": ") || err.message().ends_with("Gateway"));
    }

    #[tokio::test]
    async fn post_json_connection_refused() {
        // No server running on this port
        let client =
            WebhookClient::new(Duration::from_secs(1), WebhookRetryConfig::single_attempt())
                .unwrap();

        let result = client
            .post_json(
                "http://127.0.0.1:1",
                &TestPayload { msg: "nope".into() },
                "test",
            )
            .await;
        let err = result.unwrap_err();
        assert_eq!(err.attempts(), 1);
        assert!(
            err.message().contains("request failed"),
            "got: {}",
            err.message()
        );
    }
}
