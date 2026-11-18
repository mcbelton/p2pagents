//! Secret redaction: results leave the worker for other peers (and their run
//! reports, knowledge bases and alerts), so tokens, keys and passwords a model or a
//! check happened to print are replaced with `[REDACTED:kind]` first. Only
//! high-confidence formats are matched (well-known token prefixes, key blocks,
//! connection-string secrets), so ordinary code and text pass unchanged.

use std::sync::LazyLock;

use regex::Regex;

/// (kind, pattern, capture group holding the secret: 0 = the whole match).
const PATTERNS: &[(&str, &str, usize)] = &[
    (
        "private-key",
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        0,
    ),
    ("github-token", r"\bgithub_pat_[A-Za-z0-9_]{40,}", 0),
    ("github-token", r"\bgh[pousr]_[A-Za-z0-9]{30,}", 0),
    (
        "jwt",
        r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        0,
    ),
    ("aws-access-key", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", 0),
    ("slack-token", r"\bxox[abprs]-[A-Za-z0-9-]{10,}", 0),
    ("anthropic-key", r"\bsk-ant-[A-Za-z0-9_-]{20,}", 0),
    (
        "openai-key",
        r"\bsk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{32,}",
        0,
    ),
    ("google-api-key", r"\bAIza[A-Za-z0-9_-]{35}", 0),
    ("npm-token", r"\bnpm_[A-Za-z0-9]{36}\b", 0),
    (
        "azure-key",
        r"(?i)\b(?:AccountKey|SharedAccessKey)=([A-Za-z0-9+/]{30,}={0,2})",
        1,
    ),
    ("azure-sas", r"[?&]sig=([A-Za-z0-9%+/]{30,}={0,2})", 1),
    (
        "bearer-token",
        r"(?i)\b(?:authorization:\s*)?bearer\s+([A-Za-z0-9._~+/-]{24,}=*)",
        1,
    ),
    (
        "password",
        r"(?i)\b(?:password|pwd)\s*=\s*([^;\s'\x22]{4,})",
        1,
    ),
    (
        "url-password",
        r"\b[a-z][a-z0-9+.-]*://[^/\s:@]+:([^@\s/]{3,})@",
        1,
    ),
];

static COMPILED: LazyLock<Vec<(&'static str, Regex, usize)>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .map(|(kind, pattern, group)| (*kind, Regex::new(pattern).expect("valid regex"), *group))
        .collect()
});

/// `text` with every secret replaced by `[REDACTED:kind]`, and how many there were.
pub fn redact(text: &str) -> (String, usize) {
    let mut out = text.to_owned();
    let mut count = 0;
    for (kind, re, group) in COMPILED.iter() {
        if !re.is_match(&out) {
            continue;
        }
        let mut result = String::with_capacity(out.len());
        let mut last = 0;
        for caps in re.captures_iter(&out) {
            let Some(secret) = caps.get(*group) else {
                continue;
            };
            result.push_str(&out[last..secret.start()]);
            result.push_str(&format!("[REDACTED:{kind}]"));
            last = secret.end();
            count += 1;
        }
        result.push_str(&out[last..]);
        out = result;
    }
    (out, count)
}

/// `text` redacted, with a note saying so when anything was removed.
pub fn redact_with_note(text: &str, who: &str) -> String {
    let (clean, count) = redact(text);
    if count == 0 {
        return clean;
    }
    format!(
        "{clean}\n\n_{who} redacted {count} secret{} from this result before it left the machine._",
        if count == 1 { "" } else { "s" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_secret_formats_are_redacted_and_ordinary_text_is_not() {
        let gh = format!("ghp_{}", "a1B2".repeat(9));
        let text = format!(
            "token {gh} and AKIAABCDEFGHIJKLMNOP\n\
             Server=db;User Id=sa;Password=Hunter2!x;\n\
             curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz0123' https://api\n\
             https://acct.blob.core.windows.net/c?sv=1&sig={}\n\
             git clone https://bob:s3cr3tpw@github.com/x/y\n\
             -----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----\n\
             sk-ant-{}",
            "Q".repeat(44),
            "x".repeat(30)
        );
        let (clean, count) = redact(&text);
        assert_eq!(count, 8, "{clean}");
        for gone in [
            gh.as_str(),
            "AKIAABCDEFGHIJKLMNOP",
            "Hunter2!x",
            "abcdefghijklmnopqrstuvwxyz0123",
            "QQQQ",
            "s3cr3tpw",
            "b3BlbnNzaC1rZXk",
            "sk-ant-",
        ] {
            assert!(!clean.contains(gone), "{gone} in {clean}");
        }
        assert!(clean.contains("Password=[REDACTED:password];"));
        assert!(clean.contains("https://bob:[REDACTED:url-password]@github.com"));

        let code = "let token = get_token();\nfn password_ok(p: &str) -> bool { p.len() > 8 }\n\
                    See https://github.com/acme/shop and commit 3f2a9c1d8e7b6a5f4e3d2c1b0a9f8e7d6c5b4a3f.";
        assert_eq!(redact(code), (code.to_owned(), 0));
        assert_eq!(redact_with_note(code, "worker"), code);
        assert!(
            redact_with_note(&gh, "worker").ends_with(
                "_worker redacted 1 secret from this result before it left the machine._"
            )
        );
    }
}
