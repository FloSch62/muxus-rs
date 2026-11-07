//! Fallback SSH gateways and jump routes: the order they are tried in and the failures that
//! stop trying (`server/src/ssh/gateways.ts`, `STOP_FALLBACK` in `connection-manager.ts`).

use crate::protocol::SshGateway;

/// The primary gateway, then its fallbacks, in the order to try them (`gatewayCandidates`).
pub fn gateway_candidates(
    gateway: Option<&SshGateway>,
    fallbacks: Option<&[SshGateway]>,
) -> Vec<SshGateway> {
    let Some(gateway) = gateway else {
        return Vec::new();
    };
    std::iter::once(gateway.clone())
        .chain(fallbacks.unwrap_or_default().iter().cloned())
        .collect()
}

/// Failures that end the attempt instead of trying another route or gateway (`STOP_FALLBACK`):
/// a cancelled prompt is the user's choice, and a rejected or changed host key must be seen,
/// not quietly routed around.
pub fn stops_fallback(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "authentication cancelled",
        "host key verification failed",
        "host denied",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

/// The status line when one gateway failed and the next is tried.
pub fn gateway_retry_status(failed: &str, reason: &str, next: &str) -> String {
    format!("The SSH gateway {failed} failed ({reason}) — trying {next} …")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway(target: &str) -> SshGateway {
        SshGateway {
            target: target.into(),
            profile_id: None,
        }
    }

    #[test]
    fn lists_the_first_gateway_then_its_fallbacks() {
        assert!(gateway_candidates(None, Some(&[gateway("b")])).is_empty());
        assert_eq!(
            gateway_candidates(Some(&gateway("a")), Some(&[gateway("b")])),
            vec![gateway("a"), gateway("b")]
        );
        assert_eq!(
            gateway_candidates(Some(&gateway("a")), None),
            vec![gateway("a")]
        );
    }

    #[test]
    fn stops_at_a_cancelled_sign_in_or_a_host_key_problem() {
        assert!(stops_fallback("authentication cancelled"));
        assert!(stops_fallback("Host key verification failed."));
        assert!(stops_fallback("host denied by the user"));
        assert!(!stops_fallback("connection refused by a:22"));
    }
}
