use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

/// Shared Win+V helper <-> main-process datagram (SBS-809).
///
/// The helper used to send the literal body `activate` to a loopback UDP port.
/// Any local process that could guess or scan that port could open the flyout.
/// The channel now requires a per-session token, a loopback source, and a
/// rate limit. The token is passed to the helper on its command line; a
/// same-user process that can read Cubby's arguments can still forge a
/// packet. That is a weaker attacker than one who can decrypt `storage.key`,
/// but it is no longer "any datagram whose body is activate".

pub const TOKEN_BYTE_LEN: usize = 16;
pub const TOKEN_HEX_LEN: usize = TOKEN_BYTE_LEN * 2;
pub const ACTIVATE_PREFIX: &[u8] = b"activate ";
pub const ACTIVATE_MESSAGE_LEN: usize = ACTIVATE_PREFIX.len() + TOKEN_HEX_LEN;
/// Recv buffer is larger than a valid message so a trailing suffix is visible
/// and rejected instead of silently truncating into a match.
pub const RECV_BUFFER_LEN: usize = ACTIVATE_MESSAGE_LEN + 16;

const RATE_WINDOW: Duration = Duration::from_secs(1);
const RATE_MAX_ACCEPTS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationDecision {
    Accept,
    /// Missing token, wrong token, or the legacy bare `activate` body.
    RejectUnauthenticated,
    RejectOrigin,
    RejectRateLimited,
}

pub struct ActivationRateLimit {
    stamps: VecDeque<Instant>,
}

impl Default for ActivationRateLimit {
    fn default() -> Self {
        Self {
            stamps: VecDeque::with_capacity(RATE_MAX_ACCEPTS),
        }
    }
}

impl ActivationRateLimit {
    pub fn allow(&mut self, now: Instant) -> bool {
        while let Some(front) = self.stamps.front() {
            if now.duration_since(*front) >= RATE_WINDOW {
                self.stamps.pop_front();
            } else {
                break;
            }
        }
        if self.stamps.len() >= RATE_MAX_ACCEPTS {
            return false;
        }
        self.stamps.push_back(now);
        true
    }
}

pub fn generate_token() -> Result<String, String> {
    let mut bytes = [0_u8; TOKEN_BYTE_LEN];
    getrandom::fill(&mut bytes)
        .map_err(|error| format!("Could not generate the Cubby shortcut token: {error}"))?;
    Ok(hex_encode(&bytes))
}

pub fn is_well_formed_token(token: &str) -> bool {
    token.len() == TOKEN_HEX_LEN
        && token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

pub fn encode_activate(token: &str) -> Option<Vec<u8>> {
    if !is_well_formed_token(token) {
        return None;
    }
    let mut message = Vec::with_capacity(ACTIVATE_MESSAGE_LEN);
    message.extend_from_slice(ACTIVATE_PREFIX);
    message.extend_from_slice(token.as_bytes());
    Some(message)
}

pub fn token_from_args<'a, I>(args: I) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let args: Vec<&str> = args.into_iter().collect();
    args.windows(2)
        .find(|pair| pair[0] == "--activation-token")
        .map(|pair| pair[1].to_string())
        .filter(|token| is_well_formed_token(token))
}

pub fn token_matches(payload: &[u8], token: &str) -> bool {
    let Some(expected) = encode_activate(token) else {
        return false;
    };
    constant_time_eq(payload, &expected)
}

pub fn decide_activation(
    payload: &[u8],
    source: SocketAddr,
    token: &str,
    rate: &mut ActivationRateLimit,
    now: Instant,
) -> ActivationDecision {
    if !source.ip().is_loopback() {
        return ActivationDecision::RejectOrigin;
    }
    if !token_matches(payload, token) {
        return ActivationDecision::RejectUnauthenticated;
    }
    if !rate.allow(now) {
        return ActivationDecision::RejectRateLimited;
    }
    ActivationDecision::Accept
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0_u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn loopback() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9)
    }

    fn remote() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 9)
    }

    /// SBS-809: a scanner that only knows the old body must not open the flyout.
    #[test]
    fn bare_activate_datagram_is_rejected() {
        let mut rate = ActivationRateLimit::default();
        assert_eq!(
            decide_activation(b"activate", loopback(), TOKEN, &mut rate, Instant::now()),
            ActivationDecision::RejectUnauthenticated
        );
    }

    #[test]
    fn matching_token_from_loopback_is_accepted() {
        let mut rate = ActivationRateLimit::default();
        let payload = encode_activate(TOKEN).expect("fixture token");
        assert_eq!(
            decide_activation(&payload, loopback(), TOKEN, &mut rate, Instant::now()),
            ActivationDecision::Accept
        );
    }

    #[test]
    fn wrong_token_is_rejected() {
        let mut rate = ActivationRateLimit::default();
        let other = "fedcba9876543210fedcba9876543210";
        let payload = encode_activate(other).expect("fixture token");
        assert_eq!(
            decide_activation(&payload, loopback(), TOKEN, &mut rate, Instant::now()),
            ActivationDecision::RejectUnauthenticated
        );
    }

    #[test]
    fn trailing_suffix_is_rejected() {
        let mut rate = ActivationRateLimit::default();
        let mut payload = encode_activate(TOKEN).expect("fixture token");
        payload.push(b'x');
        assert_eq!(
            decide_activation(&payload, loopback(), TOKEN, &mut rate, Instant::now()),
            ActivationDecision::RejectUnauthenticated
        );
    }

    #[test]
    fn empty_or_malformed_token_never_matches() {
        assert!(encode_activate("").is_none());
        assert!(encode_activate("activate").is_none());
        assert!(encode_activate(&"A".repeat(TOKEN_HEX_LEN)).is_none());
        assert!(!token_matches(b"activate ", ""));
    }

    #[test]
    fn non_loopback_source_is_rejected_even_with_a_valid_token() {
        let mut rate = ActivationRateLimit::default();
        let payload = encode_activate(TOKEN).expect("fixture token");
        assert_eq!(
            decide_activation(&payload, remote(), TOKEN, &mut rate, Instant::now()),
            ActivationDecision::RejectOrigin
        );
    }

    /// A flood of authorized packets must not keep toggling the flyout.
    #[test]
    fn rate_limit_rejects_a_flood_then_recovers() {
        let mut rate = ActivationRateLimit::default();
        let payload = encode_activate(TOKEN).expect("fixture token");
        let start = Instant::now();
        for offset_ms in 0..10 {
            assert_eq!(
                decide_activation(
                    &payload,
                    loopback(),
                    TOKEN,
                    &mut rate,
                    start + Duration::from_millis(offset_ms),
                ),
                ActivationDecision::Accept,
                "accept #{offset_ms}"
            );
        }
        assert_eq!(
            decide_activation(
                &payload,
                loopback(),
                TOKEN,
                &mut rate,
                start + Duration::from_millis(11),
            ),
            ActivationDecision::RejectRateLimited
        );
        assert_eq!(
            decide_activation(
                &payload,
                loopback(),
                TOKEN,
                &mut rate,
                start + Duration::from_secs(1),
            ),
            ActivationDecision::Accept
        );
    }

    #[test]
    fn generate_token_is_32_lowercase_hex_and_unique() {
        let first = generate_token().expect("entropy");
        let second = generate_token().expect("entropy");
        assert!(is_well_formed_token(&first));
        assert!(is_well_formed_token(&second));
        assert_ne!(first, second);
    }

    #[test]
    fn token_from_args_requires_a_well_formed_value() {
        assert_eq!(
            token_from_args(["--activation-token", TOKEN]),
            Some(TOKEN.to_string())
        );
        assert_eq!(token_from_args(["--activation-token", "activate"]), None);
        assert_eq!(token_from_args(["--activation-port", "1234"]), None);
        assert_eq!(token_from_args(["--activation-token"]), None);
    }
}
