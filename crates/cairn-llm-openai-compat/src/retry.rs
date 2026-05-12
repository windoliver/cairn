//! Bounded exponential-backoff retry for transient HTTP errors.
//!
//! The closure pre-classifies each failure as transient or terminal via
//! [`Retryable`]; this module is purely about pacing.

use std::time::Duration;

use cairn_core::contract::LlmError;

/// Outcome of a single attempt that did not succeed.
pub(crate) struct Retryable {
    pub err: LlmError,
    pub retryable: bool,
}

/// Retry policy for transient endpoint errors.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetryPolicy {
    /// Maximum number of retries (excluding the initial attempt).
    pub max_retries: u32,
    /// Base delay between attempts; doubles every retry, clamped at `max_delay`.
    pub base_delay: Duration,
    /// Upper bound on a single retry's sleep.
    pub max_delay: Duration,
}

impl RetryPolicy {
    /// Conservative default — 3 retries, 200ms→400ms→800ms.
    pub(crate) const fn standard() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(5),
        }
    }

    /// Compute the delay for the n-th retry (0-indexed).
    fn delay_for(&self, attempt: u32) -> Duration {
        // Cap the shift at 31 so attempt > 31 doesn't overflow.
        let factor = 1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        let scaled = self.base_delay.saturating_mul(factor);
        if scaled > self.max_delay {
            self.max_delay
        } else {
            scaled
        }
    }
}

/// Run `op` with bounded retry on transient errors.
pub(crate) async fn with_retries<F, Fut, T>(policy: RetryPolicy, mut op: F) -> Result<T, LlmError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Retryable>>,
{
    let mut attempt: u32 = 0;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(c) => {
                if attempt >= policy.max_retries || !c.retryable {
                    return Err(c.err);
                }
                let delay = policy.delay_for(attempt);
                let delay_ms = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX);
                tracing::debug!(
                    attempt = attempt + 1,
                    max = policy.max_retries,
                    delay_ms,
                    "transient LLM error; retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_doubles_until_clamp() {
        let p = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        assert_eq!(p.delay_for(0), Duration::from_millis(100));
        assert_eq!(p.delay_for(1), Duration::from_millis(200));
        assert_eq!(p.delay_for(2), Duration::from_millis(400));
        // 800ms would exceed max_delay of 500.
        assert_eq!(p.delay_for(3), Duration::from_millis(500));
        assert_eq!(p.delay_for(99), Duration::from_millis(500));
    }
}
