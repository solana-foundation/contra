use serde::Serialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::time::Duration;
use tokio::time::sleep;
use tracing::warn;

/// Retry and timeout configuration for webhook delivery.
#[derive(Debug, Clone, Copy)]
pub struct WebhookRetryConfig {
    pub max_attempts: u32,
    pub base_delay: Duration,
    pub max_delay: Duration,
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
        assert_eq!(config.max_attempts, 1);
    }
}
