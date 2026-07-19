//! Conservative secret detection and default sensitive-app exclusions.
//!
//! Cubby already skips clipboard items tagged with Windows'
//! `ExcludeClipboardContentFromMonitorProcessing`. This module adds:
//! 1. a seeded ignore list of well-known password-manager executables, and
//! 2. high-confidence content heuristics for tokens, keys, and card numbers.
//!
//! Heuristics intentionally prefer false negatives over noisy false positives.
//! Matches never log clipboard content — only a coarse category name.

/// Password-manager (and similar) executables seeded into ignored apps once.
/// Users can remove any entry from Settings; seeding does not re-add them.
pub const DEFAULT_SENSITIVE_APP_EXES: &[&str] = &[
    "1Password.exe",
    "1Password for Windows Desktop.exe",
    "Bitwarden.exe",
    "KeePass.exe",
    "KeePassXC.exe",
    "LastPass.exe",
    "Dashlane.exe",
    "NordPass.exe",
    "Keeper.exe",
    "Keeper Password Manager.exe",
    "Enpass.exe",
    "RoboForm.exe",
    "Proton Pass.exe",
    "ProtonPass.exe",
    "Authy Desktop.exe",
];

/// Coarse category returned when text looks like a secret. Safe for logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    PrivateKey,
    AwsAccessKey,
    GitHubToken,
    SlackToken,
    StripeSecretKey,
    GoogleApiKey,
    Jwt,
    PaymentCard,
}

impl SecretKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrivateKey => "private_key",
            Self::AwsAccessKey => "aws_access_key",
            Self::GitHubToken => "github_token",
            Self::SlackToken => "slack_token",
            Self::StripeSecretKey => "stripe_secret_key",
            Self::GoogleApiKey => "google_api_key",
            Self::Jwt => "jwt",
            Self::PaymentCard => "payment_card",
        }
    }
}

/// Returns a secret category when `text` matches a high-confidence pattern.
/// Empty / whitespace-only input never matches.
pub fn classify_secret(text: &str) -> Option<SecretKind> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.len() > 8_192 {
        // Extremely long pastes are almost never a single secret; skip scanning
        // so we don't burn CPU on multi-megabyte logs.
        return None;
    }

    if looks_like_private_key(trimmed) {
        return Some(SecretKind::PrivateKey);
    }
    if looks_like_aws_access_key(trimmed) {
        return Some(SecretKind::AwsAccessKey);
    }
    if looks_like_github_token(trimmed) {
        return Some(SecretKind::GitHubToken);
    }
    if looks_like_slack_token(trimmed) {
        return Some(SecretKind::SlackToken);
    }
    if looks_like_stripe_secret(trimmed) {
        return Some(SecretKind::StripeSecretKey);
    }
    if looks_like_google_api_key(trimmed) {
        return Some(SecretKind::GoogleApiKey);
    }
    if looks_like_jwt(trimmed) {
        return Some(SecretKind::Jwt);
    }
    if looks_like_payment_card(trimmed) {
        return Some(SecretKind::PaymentCard);
    }
    None
}

fn looks_like_private_key(text: &str) -> bool {
    text.contains("-----BEGIN")
        && (text.contains("PRIVATE KEY-----") || text.contains("OPENSSH PRIVATE KEY-----"))
}

fn looks_like_aws_access_key(text: &str) -> bool {
    // AKIA… is the classic long-term access-key id shape.
    contains_token(text, |candidate| {
        candidate.len() == 20
            && candidate.starts_with("AKIA")
            && candidate.bytes().all(|b| b.is_ascii_alphanumeric())
    })
}

fn looks_like_github_token(text: &str) -> bool {
    contains_token(text, |candidate| {
        (candidate.starts_with("ghp_") && candidate.len() == 40)
            || (candidate.starts_with("gho_") && candidate.len() == 40)
            || (candidate.starts_with("ghu_") && candidate.len() == 40)
            || (candidate.starts_with("ghs_") && candidate.len() == 40)
            || (candidate.starts_with("ghr_") && candidate.len() == 40)
            || (candidate.starts_with("github_pat_") && candidate.len() >= 82)
    })
}

fn looks_like_slack_token(text: &str) -> bool {
    contains_token(text, |candidate| {
        let bytes = candidate.as_bytes();
        if bytes.len() < 20 {
            return false;
        }
        matches!(&bytes[..4], b"xoxb" | b"xoxp" | b"xoxa" | b"xoxr" | b"xoxs")
            && bytes.get(4) == Some(&b'-')
            && candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

fn looks_like_stripe_secret(text: &str) -> bool {
    contains_token(text, |candidate| {
        (candidate.starts_with("sk_live_") || candidate.starts_with("sk_test_"))
            && candidate.len() >= 20
            && candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

fn looks_like_google_api_key(text: &str) -> bool {
    contains_token(text, |candidate| {
        candidate.starts_with("AIza")
            && candidate.len() == 39
            && candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    })
}

fn looks_like_jwt(text: &str) -> bool {
    // Require the compact JWS shape starting with the common base64url header
    // prefix for `{"alg":...}` (`eyJ`) and exactly three segments.
    let candidate = text.trim();
    if !candidate.starts_with("eyJ") {
        return false;
    }
    let mut parts = candidate.split('.');
    let Some(header) = parts.next() else {
        return false;
    };
    let Some(payload) = parts.next() else {
        return false;
    };
    let Some(signature) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    is_base64url(header)
        && is_base64url(payload)
        && is_base64url(signature)
        && header.len() >= 10
        && payload.len() >= 10
        && signature.len() >= 10
}

fn looks_like_payment_card(text: &str) -> bool {
    // Require grouped digits (spaces or dashes). A bare 16-digit integer is too
    // often an order id / tracking number to treat as a card by default.
    let has_separator = text.chars().any(|c| c == ' ' || c == '-');
    if !has_separator {
        return false;
    }

    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    // Reject strings that are mostly non-digit noise around a digit run.
    let non_digits = text
        .chars()
        .filter(|c| !c.is_ascii_digit() && !c.is_whitespace() && *c != '-')
        .count();
    if non_digits > 2 {
        return false;
    }
    luhn_ok(&digits)
}

fn luhn_ok(digits: &str) -> bool {
    let mut sum = 0_u32;
    let mut double = false;
    for ch in digits.chars().rev() {
        let Some(mut digit) = ch.to_digit(10) else {
            return false;
        };
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    sum.is_multiple_of(10)
}

fn is_base64url(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn contains_token(text: &str, predicate: impl Fn(&str) -> bool) -> bool {
    // Split on common clipboard delimiters without allocating a regex engine.
    let mut start = 0_usize;
    let bytes = text.as_bytes();
    for (idx, byte) in bytes.iter().enumerate() {
        if is_token_separator(*byte) {
            if idx > start && predicate(&text[start..idx]) {
                return true;
            }
            start = idx + 1;
        }
    }
    if start < text.len() && predicate(&text[start..]) {
        return true;
    }
    false
}

fn is_token_separator(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t'
            | b'\n'
            | b'\r'
            | b'"'
            | b'\''
            | b'`'
            | b','
            | b';'
            | b':'
            | b'='
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'('
            | b')'
            | b'|'
            | b'\\'
            | b'/'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_ordinary_text_and_codes() {
        assert_eq!(classify_secret("hello world"), None);
        assert_eq!(classify_secret("123456"), None);
        assert_eq!(classify_secret("order 4111111111111111"), None);
        assert_eq!(classify_secret("sk-not-a-real-openai-key"), None);
    }

    #[test]
    fn detects_private_keys() {
        let pem =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
        assert_eq!(classify_secret(pem), Some(SecretKind::PrivateKey));
    }

    #[test]
    fn detects_aws_and_github_tokens() {
        // Assemble fixtures at runtime so source scanners do not treat the
        // test strings as live credentials.
        let aws = format!("AKIA{}{}", "IOSFODNN7", "EXAMPLE");
        let github = format!("ghp_{}", "abcdefghijklmnopqrstuvwxyz0123456789");
        assert_eq!(classify_secret(&aws), Some(SecretKind::AwsAccessKey));
        assert_eq!(classify_secret(&github), Some(SecretKind::GitHubToken));
    }

    #[test]
    fn detects_slack_stripe_and_google_keys() {
        let slack = format!("xox{}-{}-{}", "b", "123456789012", "abcdefghijklmnop");
        let stripe = format!("sk_{}_{}", "live", "51AbcdefGhIjKlMnOpQrStUv");
        let google = format!("AIza{}{}", "SyA-", "abcdefghijklmnopqrstuvwxyz01234");
        assert_eq!(classify_secret(&slack), Some(SecretKind::SlackToken));
        assert_eq!(classify_secret(&stripe), Some(SecretKind::StripeSecretKey));
        assert_eq!(classify_secret(&google), Some(SecretKind::GoogleApiKey));
    }

    #[test]
    fn detects_jwt_and_grouped_cards() {
        let jwt = [
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            "signaturevalue12",
        ]
        .join(".");
        assert_eq!(classify_secret(&jwt), Some(SecretKind::Jwt));
        let card_spaces = ["4111", "1111", "1111", "1111"].join(" ");
        let card_dashes = ["4111", "1111", "1111", "1111"].join("-");
        assert_eq!(
            classify_secret(&card_spaces),
            Some(SecretKind::PaymentCard)
        );
        assert_eq!(
            classify_secret(&card_dashes),
            Some(SecretKind::PaymentCard)
        );
    }

    #[test]
    fn default_sensitive_apps_cover_major_password_managers() {
        assert!(DEFAULT_SENSITIVE_APP_EXES.contains(&"Bitwarden.exe"));
        assert!(DEFAULT_SENSITIVE_APP_EXES.contains(&"1Password.exe"));
        assert!(DEFAULT_SENSITIVE_APP_EXES.contains(&"KeePassXC.exe"));
    }

    #[test]
    fn secret_kind_labels_are_stable_for_logs() {
        assert_eq!(SecretKind::GitHubToken.as_str(), "github_token");
    }
}
