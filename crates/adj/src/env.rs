use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Parse a dotenv-format file into key/value pairs.
///
/// Intentionally minimal: `KEY=VALUE` lines, `#` comments, blank lines, surrounding whitespace
/// trimmed, and matching outer single or double quotes stripped from the value. No shell
/// substitution, no `export` prefix, no escapes. If users need more, they can pre-render to a
/// committed file or shell-source it outside Adjacent.
pub fn parse_dotenv(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for (lineno, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some(eq_idx) = trimmed.find('=') else {
            return Err(anyhow!(
                "line {}: expected `KEY=VALUE`, got `{}`",
                lineno + 1,
                trimmed
            ));
        };
        let key = trimmed[..eq_idx].trim();
        let value_raw = trimmed[eq_idx + 1..].trim();
        if key.is_empty() {
            return Err(anyhow!("line {}: empty key", lineno + 1));
        }
        let value = strip_matching_quotes(value_raw);
        out.insert(key.to_string(), value);
    }
    Ok(out)
}

fn strip_matching_quotes(s: &str) -> String {
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Load and parse a dotenv file at `path`. Returns a clear error if the file is missing — this
/// is the startup-error case the CLI surfaces as "adj: ...".
pub fn load_env_file(path: &Path) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        return Err(anyhow!("env_file not found at {}", path.display()));
    }
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("reading env_file {}", path.display()))?;
    parse_dotenv(&raw).with_context(|| format!("parsing env_file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_pairs() {
        let raw = "FOO=bar\nBAZ=qux\n";
        let got = parse_dotenv(raw).unwrap();
        assert_eq!(got.get("FOO").map(String::as_str), Some("bar"));
        assert_eq!(got.get("BAZ").map(String::as_str), Some("qux"));
    }

    #[test]
    fn ignores_comments_and_blanks() {
        let raw = "# leading comment\n\nFOO=bar\n  # indented comment\nBAZ=qux\n";
        let got = parse_dotenv(raw).unwrap();
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn strips_matching_quotes() {
        let raw = "A=\"hello world\"\nB='single'\nC=\"mismatched'\n";
        let got = parse_dotenv(raw).unwrap();
        assert_eq!(got.get("A").map(String::as_str), Some("hello world"));
        assert_eq!(got.get("B").map(String::as_str), Some("single"));
        // Mismatched quotes are kept verbatim — we only strip matching outer pairs.
        assert_eq!(got.get("C").map(String::as_str), Some("\"mismatched'"));
    }

    #[test]
    fn allows_equals_in_value() {
        let raw = "URL=postgres://user:pass@host/db?x=1\n";
        let got = parse_dotenv(raw).unwrap();
        assert_eq!(
            got.get("URL").map(String::as_str),
            Some("postgres://user:pass@host/db?x=1")
        );
    }

    #[test]
    fn rejects_line_without_equals() {
        let raw = "NOT_AN_ASSIGNMENT\n";
        assert!(parse_dotenv(raw).is_err());
    }

    #[test]
    fn rejects_empty_key() {
        let raw = "=value\n";
        assert!(parse_dotenv(raw).is_err());
    }
}
