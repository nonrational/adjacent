//! Collapse ID-shaped path segments to `:id` so per-route metrics don't explode in cardinality.

/// Build a route key from an HTTP method and request path. The query string and fragment are
/// dropped (unbounded, not part of route identity); each path segment that looks like an
/// identifier — all digits, a UUID, or a long hex/hash — becomes `:id`. The method is prefixed
/// so `GET /x` and `POST /x` are distinct routes.
pub fn templatize(method: &str, path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = if path.is_empty() { "/" } else { path };
    let templated = path
        .split('/')
        .map(|seg| if is_id_segment(seg) { ":id" } else { seg })
        .collect::<Vec<_>>()
        .join("/");
    format!("{method} {templated}")
}

fn is_id_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    if seg.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if is_uuid(seg) {
        return true;
    }
    // Long all-hex segments are content hashes / git shas / opaque tokens.
    seg.len() >= 16 && seg.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_numeric_ids() {
        assert_eq!(templatize("GET", "/users/123"), "GET /users/:id");
        assert_eq!(templatize("GET", "/users/1"), "GET /users/:id");
        assert_eq!(
            templatize("GET", "/users/123/posts/456"),
            "GET /users/:id/posts/:id"
        );
    }

    #[test]
    fn collapses_uuid_and_long_hex() {
        assert_eq!(
            templatize("GET", "/items/550e8400-e29b-41d4-a716-446655440000"),
            "GET /items/:id"
        );
        assert_eq!(
            templatize("GET", "/blob/2f1d3a9c8b7e6f5a4d3c2b1a0998877665544332"),
            "GET /blob/:id"
        );
    }

    #[test]
    fn keeps_words_versions_and_short_segments() {
        assert_eq!(templatize("GET", "/v1/users"), "GET /v1/users");
        assert_eq!(templatize("GET", "/api/feed"), "GET /api/feed");
        // A short hex-looking word is not collapsed (< 16 chars, not all-digit, not UUID).
        assert_eq!(templatize("GET", "/face"), "GET /face");
    }

    #[test]
    fn strips_query_and_normalizes_root() {
        assert_eq!(templatize("GET", "/api/feed?page=2"), "GET /api/feed");
        assert_eq!(templatize("GET", "/"), "GET /");
        assert_eq!(templatize("GET", ""), "GET /");
    }

    #[test]
    fn method_distinguishes_routes() {
        assert_ne!(templatize("GET", "/x"), templatize("POST", "/x"));
    }
}
