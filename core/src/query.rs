//! Turns raw user input into safe FTS5 queries.
//!
//! FTS5 has its own query grammar; feeding raw user input to `MATCH` causes
//! syntax errors on quotes, colons, asterisks, parentheses and other
//! characters. This module tokenizes the input, drops English stopwords,
//! escapes each remaining term and joins them with an explicit boolean
//! operator, so any user string produces a valid query.

/// Common English stopwords removed from queries (unless the query consists
/// only of stopwords, in which case the original terms are kept so the
/// search still returns something).
pub const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "been", "but", "by", "for", "from", "has", "have",
    "he", "her", "his", "i", "if", "in", "into", "is", "it", "its", "of", "on", "or", "our", "s",
    "she", "so", "such", "t", "that", "the", "their", "them", "then", "there", "these", "they",
    "this", "to", "was", "we", "were", "what", "when", "which", "who", "will", "with", "you",
    "your",
];

/// Builds a safe FTS5 query string from raw user input.
///
/// * Terms are stripped of leading/trailing punctuation.
/// * Stopwords are dropped (unless everything would be dropped).
/// * Terms are joined with `AND`, or `OR`/`NOT` if the user wrote explicit
///   uppercase boolean operators.
/// * The final term gets a `*` prefix wildcard when it is alphanumeric, so
///   typing a prefix of a word still matches (FTS5 without a prefix index
///   handles this fine at this corpus size).
///
/// Returns `None` when the input contains no searchable terms.
pub fn build_fts_query(raw: &str) -> Option<String> {
    let tokens: Vec<&str> = raw.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }

    // Explicit boolean operators, kept verbatim (only uppercase FTS5 keywords).
    let is_operator = |t: &str| matches!(t, "AND" | "OR" | "NOT");

    // Clean each token: trim punctuation from both ends.
    let cleaned: Vec<String> = tokens
        .iter()
        .filter(|t| !is_operator(t))
        .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_').to_string())
        .filter(|t| !t.is_empty())
        .collect();

    if cleaned.is_empty() {
        return None;
    }

    // Drop stopwords, but keep them if that would leave nothing behind.
    let kept: Vec<String> = cleaned
        .iter()
        .filter(|t| !STOPWORDS.contains(&t.to_lowercase().as_str()))
        .cloned()
        .collect();
    let terms = if kept.is_empty() { cleaned } else { kept };

    // Detect an explicit operator written by the user; default is AND.
    let mut op = "AND";
    for t in &tokens {
        match *t {
            "OR" => op = "OR",
            "NOT" => op = "NOT",
            _ => {}
        }
    }

    let n = terms.len();
    let mut parts: Vec<String> = Vec::with_capacity(n);
    for (i, term) in terms.iter().enumerate() {
        let escaped = term.replace('"', "\"\"");
        let quoted = format!("\"{}\"", escaped);
        // Wildcard the last term only, and only when it is alphanumeric
        // (a `*` must directly follow a bare token in FTS5).
        if i == n - 1 && term.chars().all(|c| c.is_alphanumeric()) {
            parts.push(format!("{}*", quoted));
        } else {
            parts.push(quoted);
        }
    }

    Some(parts.join(&format!(" {} ", op)))
}

/// Splits a query into the content terms used for snippet highlighting and
/// summarizer scoring (lowercased, stopwords removed, deduplicated).
pub fn query_terms(raw: &str) -> Vec<String> {
    let mut terms: Vec<String> = raw
        .split_whitespace()
        .map(|t| {
            t.trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_lowercase()
        })
        .filter(|t| !t.is_empty() && !STOPWORDS.contains(&t.as_str()))
        .collect();
    terms.dedup();
    if terms.is_empty() {
        // Fall back to the raw tokens so an all-stopword query still works.
        terms = raw
            .split_whitespace()
            .map(|t| t.to_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
        terms.dedup();
    }
    terms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_query_becomes_anded_phrases() {
        assert_eq!(build_fts_query("neural networks"), Some("\"neural\" AND \"networks\"*".into()));
    }

    #[test]
    fn dangerous_characters_are_escaped() {
        let q = build_fts_query(r#"foo"bar"#).unwrap();
        assert_eq!(q, r#""foo""bar""#);
    }

    #[test]
    fn punctuation_only_query_returns_none() {
        assert_eq!(build_fts_query("!! ??? ..."), None);
        assert_eq!(build_fts_query("   "), None);
    }

    #[test]
    fn stopwords_are_dropped() {
        let q = build_fts_query("the theory of relativity").unwrap();
        assert_eq!(q, "\"theory\" AND \"relativity\"*");
    }

    #[test]
    fn all_stopword_query_still_searches() {
        let q = build_fts_query("the and or").unwrap();
        assert!(!q.is_empty());
        assert!(q.contains("\"the\""));
    }

    #[test]
    fn explicit_or_is_respected() {
        let q = build_fts_query("photosynthesis OR respiration").unwrap();
        assert_eq!(q, "\"photosynthesis\" OR \"respiration\"*");
    }

    #[test]
    fn trailing_punctuation_is_trimmed() {
        // The '!' is trimmed; no wildcard on hyphenated terms (the phrase
        // itself already covers both tokens).
        assert_eq!(build_fts_query("climate-change!"), Some("\"climate-change\"".into()));
        assert_eq!(build_fts_query("neural!"), Some("\"neural\"*".into()));
    }

    #[test]
    fn query_terms_filters_and_lowercases() {
        assert_eq!(query_terms("The Theory of Relativity"), vec!["theory", "relativity"]);
    }
}
