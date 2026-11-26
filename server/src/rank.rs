//! Hybrid ranking: FTS5 BM25 relevance re-weighted by source authority.
//!
//! Raw BM25 scores are unbounded and corpus-dependent, so they are squashed
//! into `[0, 1]` with `s / (s + k)`. The final score blends:
//!
//! ```text
//! score = 0.55 · relevance + 0.40 · authority + 0.05 · title_coverage
//! ```
//!
//! Authority dominates on purpose: for scholarly use, two equally relevant
//! pages should always order the peer-reviewed/edu/gov one first.

use crate::authority;
use veridex_core::db::FtsMatch;

/// How many BM25 candidates we fetch before re-ranking. Bounds worst-case
/// work while leaving plenty of headroom for pagination.
pub const CANDIDATE_POOL: i64 = 300;

const W_RELEVANCE: f64 = 0.55;
const W_AUTHORITY: f64 = 0.40;
const W_TITLE: f64 = 0.05;

/// Squashes a raw BM25 magnitude (`s = -bm25`, higher is better) into
/// `[0, 1]` with `s / (s + k)`, `k = 2`.
///
/// SQLite's bm25() collapses to ~1e-6 when a term matches most of the
/// corpus (IDF ≈ 0); with `k = 2` that noise stays dead at 0 instead of
/// being amplified, while genuine matches (|bm25| of 1..30) spread across
/// a useful range. Authority then dominates ties, which is exactly the
/// scholarly-first ordering we want.
pub fn normalize_bm25(raw: f64) -> f64 {
    let s = (-raw).max(0.0);
    s / (s + 2.0)
}

pub struct Scored {
    pub matched: FtsMatch,
    pub relevance: f64,
    pub authority: f64,
    pub grade: &'static str,
    pub signals: Vec<String>,
    pub score: f64,
}

/// Re-ranks FTS5 candidates by the hybrid formula, best first.
pub fn rerank(candidates: Vec<FtsMatch>, terms: &[String]) -> Vec<Scored> {
    let mut scored: Vec<Scored> = candidates
        .into_iter()
        .map(|matched| {
            let auth = authority::score_page(
                &matched.url,
                matched.host.as_deref(),
                matched.title.as_deref(),
                matched.description.as_deref(),
                matched.keywords.as_deref(),
                matched.content.as_deref(),
            );
            let relevance = normalize_bm25(matched.score);
            let title_cov = title_coverage(matched.title.as_deref(), terms);
            let score =
                W_RELEVANCE * relevance + W_AUTHORITY * auth.score + W_TITLE * title_cov;
            Scored {
                grade: authority::grade(auth.score),
                score,
                relevance,
                authority: auth.score,
                signals: auth.signals,
                matched,
            }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.relevance
                    .partial_cmp(&a.relevance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
    });
    scored
}

/// Fraction of query terms that appear in the title (case-insensitive).
fn title_coverage(title: Option<&str>, terms: &[String]) -> f64 {
    let Some(title) = title else {
        return 0.0;
    };
    if terms.is_empty() {
        return 0.0;
    }
    let lower = title.to_ascii_lowercase();
    let hits = terms.iter().filter(|t| lower.contains(t.as_str())).count();
    hits as f64 / terms.len() as f64
}

/// Extracts a readable window of `content` around the densest cluster of
/// query terms — the classic search-result snippet.
pub fn make_snippet(content: &str, terms: &[String], max_chars: usize) -> String {
    let flat = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return String::new();
    }
    if flat.chars().count() <= max_chars {
        return flat;
    }

    let lower = flat.to_ascii_lowercase();
    let byte_len = lower.len();
    // Match against the ASCII-lowered copy so byte offsets stay aligned.
    let terms: Vec<String> = terms.iter().map(|t| t.to_ascii_lowercase()).collect();

    let snap = |mut start: usize, end_limit: usize| -> (usize, usize) {
        while start < end_limit && !lower.is_char_boundary(start) {
            start += 1;
        }
        let mut end = (start + max_chars).min(end_limit);
        while end > start && !lower.is_char_boundary(end) {
            end -= 1;
        }
        (start, end)
    };

    // Best window = start byte that covers the most distinct query terms
    // (ties → earliest window).
    let mut best_start = 0usize;
    let mut best_score = -1i32;
    for term in &terms {
        if term.is_empty() {
            continue;
        }
        let mut from = 0usize;
        while from < byte_len {
            let Some(rel) = lower[from..].find(term.as_str()) else {
                break;
            };
            let pos = from + rel;
            let anchor = pos.saturating_sub(max_chars / 4);
            let (start, end) = snap(anchor, byte_len);
            let window = &lower[start..end];
            let distinct = terms
                .iter()
                .filter(|t| window.contains(t.as_str()))
                .count() as i32;
            let occurrences = terms
                .iter()
                .map(|t| window.matches(t.as_str()).count().min(3))
                .sum::<usize>() as i32;
            let score = distinct * 10 + occurrences;
            if score > best_score {
                best_score = score;
                best_start = start;
            }
            from = pos + term.len().max(1);
        }
    }

    let (mut start, mut end) = snap(best_start, byte_len);
    // Snap to word boundaries within the window.
    if start > 0
        && let Some(space) = flat[start..end].find(' ')
    {
        start += space + 1;
    }
    if end < byte_len
        && let Some(space) = flat[start..end].rfind(' ')
    {
        end = start + space;
    }
    while start < end && !flat.is_char_boundary(start) {
        start += 1;
    }
    while end > start && !flat.is_char_boundary(end) {
        end -= 1;
    }

    let mut out = String::new();
    if best_start > 0 {
        out.push('…');
    }
    out.push_str(&flat[start..end]);
    if end < byte_len {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use veridex_core::db::FtsMatch;

    fn match_with(content: &str) -> FtsMatch {
        FtsMatch {
            id: 1,
            url: "https://example.org/a".into(),
            host: Some("example.org".into()),
            title: Some("Test".into()),
            description: None,
            keywords: None,
            content: Some(content.into()),
            crawled_at: "2026-01-01 00:00:00".into(),
            score: -10.0,
        }
    }

    #[test]
    fn bm25_normalization_is_bounded_and_noise_dead() {
        // SQLite's IDF≈0 epsilon must stay at ~0, not get amplified.
        // Squash gives s/(s+2) ≤ s/2, so 1e-6 noise → ~5e-7: three
        // orders of magnitude below a mild genuine match (1/3).
        let epsilon = normalize_bm25(-1e-6);
        let mild = normalize_bm25(-1.0);
        assert!(epsilon < 1e-6, "epsilon noise must be ~0, got {epsilon}");
        assert!(
            epsilon < mild * 1e-3,
            "noise must be dead vs real signal: {epsilon} vs {mild}"
        );
        assert!(normalize_bm25(-2e-6) < 1e-6);
        assert_eq!(normalize_bm25(0.0), 0.0);
        // Genuine matches spread across a useful range.
        let a = normalize_bm25(-1.0);
        let b = normalize_bm25(-6.0);
        let c = normalize_bm25(-30.0);
        assert!(a > 0.0 && a < b && b < c && c < 1.0);
        // Monotonic: better BM25 → higher relevance.
        assert!(normalize_bm25(-12.0) > normalize_bm25(-4.0));
    }

    #[test]
    fn authority_outranks_equal_relevance() {
        let filler = "photosynthesis research evidence. ".repeat(70); // ~210 words
        let mut edu = match_with(&filler);
        edu.url = "https://stanford.edu/x".into();
        edu.host = Some("stanford.edu".into());
        edu.score = -10.0;

        let mut farm = match_with("photosynthesis facts maybe totally wild");
        farm.url = "https://wikihow.com/x".into();
        farm.host = Some("wikihow.com".into());
        farm.score = -10.0; // identical relevance

        let ranked = rerank(vec![farm, edu], &["photosynthesis".to_string()]);
        assert!(ranked[0].authority > ranked[1].authority);
        assert_eq!(ranked[0].grade, "A");
        assert_eq!(ranked[1].grade, "D");
    }

    #[test]
    fn snippet_prefers_dense_term_cluster() {
        let content = "alpha filler text here. \
            Now we discuss transformer attention mechanisms in depth. \
            transformer attention is critical. \
            Unrelated closing words about gardening and weather.";
        let snippet = make_snippet(content, &["transformer".to_string()], 120);
        assert!(snippet.contains("transformer attention"), "got: {snippet}");
        assert!(snippet.chars().count() <= 125);
    }

    #[test]
    fn snippet_of_short_content_is_whole_content() {
        assert_eq!(
            make_snippet("short text", &["short".into()], 100),
            "short text"
        );
    }

    #[test]
    fn snippet_handles_no_term_match() {
        let content = "a b c d e f g h i j k l m n o p q r s t u v w x y z";
        let s = make_snippet(content, &["missing".into()], 30);
        assert!(s.chars().count() <= 32);
        assert!(s.starts_with("a b"));
    }

    #[test]
    fn snippet_is_utf8_safe() {
        let content = "héllo wörld ünïcode — ".repeat(50);
        let s = make_snippet(&content, &["wörld".to_string()], 60);
        assert!(!s.is_empty());
    }
}
