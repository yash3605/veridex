//! Local extractive summarizer.
//!
//! Pipeline:
//! 1. Search the index for the query, take the top pages (authority-ranked).
//! 2. Split each page into sentences and score them on query coverage,
//!    page-level salience, position, length quality and source authority.
//! 3. Greedy MMR selection picks diverse, non-redundant sentences across
//!    sources (capped per page so no single source dominates).
//! 4. Selected sentences are grouped per source into a structured brief
//!    that the PDF renderer (or JSON response) consumes.
//!
//! No network calls, no LLM — deterministic and instant.

use crate::authority;
use crate::error::ApiError;
use crate::rank;
use crate::search::extract_domain;
use axum::response::IntoResponse;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use veridex_core::{db, query, SqlitePool};

fn dflt_pages() -> usize {
    6
}
fn dflt_sentences() -> usize {
    10
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SummaryFormat {
    #[default]
    Pdf,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryRequest {
    #[serde(alias = "q")]
    pub query: String,
    #[serde(default = "dflt_pages")]
    pub max_pages: usize,
    #[serde(default = "dflt_sentences")]
    pub max_sentences: usize,
    #[serde(default)]
    pub format: SummaryFormat,
    /// Optional document title (defaults to the query).
    #[serde(default)]
    pub title: Option<String>,
    /// Discover fresh results on the live web before summarizing.
    #[serde(default)]
    pub live: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryStats {
    pub pages_analyzed: usize,
    pub sentences_considered: usize,
    pub sentences_selected: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Finding {
    pub text: String,
    /// 1-based index into `sources`.
    pub source: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceSection {
    pub index: usize,
    pub page_id: i64,
    pub url: String,
    pub host: Option<String>,
    pub title: String,
    pub grade: &'static str,
    pub authority: f64,
    pub crawled_at: String,
    pub sentences: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SummaryDoc {
    pub title: String,
    pub query: String,
    pub generated_at: String,
    pub stats: SummaryStats,
    pub findings: Vec<Finding>,
    pub sources: Vec<SourceSection>,
}

/// A sentence candidate ready for MMR selection.
struct Candidate {
    text: String,
    words: HashSet<String>,
    score: f64,
    page: usize,
    order: usize,
}

pub async fn build_summary(
    pool: &SqlitePool,
    req: &SummaryRequest,
) -> Result<SummaryDoc, ApiError> {
    let max_pages = req.max_pages.clamp(1, 20);
    let max_sentences = req.max_sentences.clamp(3, 30);

    let (clean_q, domain) = extract_domain(req.query.clone(), None);
    let terms = query::query_terms(&clean_q);
    let fts = query::build_fts_query(&clean_q).ok_or_else(|| {
        ApiError::BadRequest("query contains no searchable terms".into())
    })?;

    // Live mode: pull fresh pages into the index first so briefs can cite
    // them like any crawled source. Budgeted like the search endpoint.
    if req.live {
        match tokio::time::timeout(
            crate::live::BUDGET,
            crate::live::expand(pool, &clean_q, domain.as_deref()),
        )
        .await
        {
            Ok(fetched) => tracing::info!("live expansion added {fetched} pages for brief"),
            Err(_) => tracing::warn!("live expansion exceeded budget for brief"),
        }
    }

    let candidates =
        db::search_matches(pool, &fts, domain.as_deref(), max_pages as i64)
            .await
            .map_err(ApiError::internal)?;
    let ranked = rank::rerank(candidates, &terms);
    let top: Vec<_> = ranked.into_iter().take(max_pages).collect();

    if top.is_empty() {
        return Err(ApiError::NotFound(format!(
            "no indexed pages matched '{clean_q}'"
        )));
    }

    // --- Sentence extraction & scoring per page ---------------------------
    let mut per_page_sentences: Vec<Vec<(String, f64)>> = Vec::new();
    let mut pages_analyzed = 0usize;
    let mut sentences_considered = 0usize;

    for s in &top {
        let Some(content) = s.matched.content.as_deref() else {
            per_page_sentences.push(Vec::new());
            continue;
        };
        let raw: Vec<String> = split_sentences(content)
            .into_iter()
            .filter(|sent| {
                let w = sent.split_whitespace().count();
                (6..=70).contains(&w)
            })
            .collect();
        if raw.is_empty() {
            per_page_sentences.push(Vec::new());
            continue;
        }
        pages_analyzed += 1;
        sentences_considered += raw.len();

        let freq = word_frequencies(&raw);
        let scores = score_sentences(&raw, &terms, &freq, s.authority);
        let page_sents: Vec<(String, f64)> =
            raw.into_iter().zip(scores).collect();
        per_page_sentences.push(page_sents);
    }

    if pages_analyzed == 0 {
        return Err(ApiError::NotFound(
            "matched pages contain no usable text".into(),
        ));
    }

    // --- Flatten into MMR candidates --------------------------------------
    let mut cands: Vec<Candidate> = Vec::new();
    for (page_idx, page_sents) in per_page_sentences.iter().enumerate() {
        for (order, (text, score)) in page_sents.iter().enumerate() {
            cands.push(Candidate {
                text: text.clone(),
                words: content_words(text),
                score: *score,
                page: page_idx,
                order,
            });
        }
    }

    let selected = mmr_select(&cands, max_sentences);

    // --- Group by source ---------------------------------------------------
    // Source ordering follows the hybrid search rank of the page (the same
    // ordering `/api/search` would produce), restricted to pages whose
    // sentences were actually selected.
    let mut pages_with_selection: Vec<usize> = selected
        .iter()
        .map(|idx| cands[*idx].page)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    pages_with_selection.sort_by(|a, b| {
        top[*b]
            .score
            .partial_cmp(&top[*a].score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cmp(b))
    });

    // Per-page sentences in original document order.
    let mut sources: Vec<SourceSection> = Vec::new();
    let mut source_index_of_page: HashMap<usize, usize> = HashMap::new();
    for (i, page_idx) in pages_with_selection.iter().enumerate() {
        let s = &top[*page_idx];
        let mut sents: Vec<(usize, String)> = selected
            .iter()
            .filter(|idx| cands[**idx].page == *page_idx)
            .map(|idx| (cands[*idx].order, cands[*idx].text.clone()))
            .collect();
        sents.sort_by_key(|(order, _)| *order);
        source_index_of_page.insert(*page_idx, i + 1);
        sources.push(SourceSection {
            index: i + 1,
            page_id: s.matched.id,
            url: s.matched.url.clone(),
            host: s.matched.host.clone(),
            title: s
                .matched
                .title
                .clone()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| s.matched.url.clone()),
            grade: s.grade,
            authority: s.authority,
            crawled_at: s.matched.crawled_at.clone(),
            sentences: sents.into_iter().map(|(_, t)| t).collect(),
        });
    }

    // Findings in MMR selection order (best first), with source references.
    let findings: Vec<Finding> = selected
        .iter()
        .map(|idx| {
            let c = &cands[*idx];
            Finding {
                text: c.text.clone(),
                source: source_index_of_page[&c.page],
            }
        })
        .collect();

    let title = req
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| req.query.clone());

    Ok(SummaryDoc {
        title,
        query: req.query.clone(),
        generated_at: chrono::Local::now().format("%Y-%m-%d %H:%M").to_string(),
        stats: SummaryStats {
            pages_analyzed,
            sentences_considered,
            sentences_selected: selected.len(),
        },
        findings,
        sources,
    })
}

// ---------------------------------------------------------------------------
// Sentence scoring
// ---------------------------------------------------------------------------

fn score_sentences(
    sentences: &[String],
    terms: &[String],
    freq: &HashMap<String, usize>,
    authority: f64,
) -> Vec<f64> {
    // Salience raw values, normalized per page.
    let raw_info: Vec<f64> = sentences
        .iter()
        .map(|s| {
            content_words(s)
                .iter()
                .map(|w| *freq.get(w).unwrap_or(&0) as f64)
                .sum()
        })
        .collect();
    let max_info = raw_info.iter().cloned().fold(0.0f64, f64::max);

    sentences
        .iter()
        .enumerate()
        .map(|(i, s)| {
            // Query coverage: distinct query terms present.
            let lower = s.to_ascii_lowercase();
            let qcov = if terms.is_empty() {
                0.0
            } else {
                terms.iter().filter(|t| lower.contains(t.as_str())).count() as f64
                    / terms.len() as f64
            };

            // Salience within the page (0..1).
            let info = if max_info > 0.0 {
                (raw_info[i] / max_info).min(1.0)
            } else {
                0.5
            };

            // Factual density: numbers and years.
            let numbers = count_digit_runs(s);
            let num_bonus = 0.12 * (numbers.min(3) as f64) / 3.0;

            // Position: intro bias with exponential decay.
            let pos = (-(i as f64) / 9.0).exp();

            // Length quality: prefer 12..=40 word sentences.
            let wc = s.split_whitespace().count();
            let len = if (12..=40).contains(&wc) {
                1.0
            } else if !(8..=60).contains(&wc) {
                0.0
            } else {
                0.5
            };

            let score = 0.30 * qcov + 0.16 * info + 0.12 * pos + 0.10 * len
                + 0.30 * authority
                + num_bonus;
            score.clamp(0.0, 1.0)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// MMR selection
// ---------------------------------------------------------------------------

/// Maximal Marginal Relevance: greedily pick the sentence that balances
/// relevance against redundancy with what is already selected.
fn mmr_select(cands: &[Candidate], max: usize) -> Vec<usize> {
    const LAMBDA: f64 = 0.55;
    const MIN_GAIN: f64 = 0.12;
    let per_page_cap = (max / 2).clamp(2, 4);

    let mut selected: Vec<usize> = Vec::new();
    let mut page_counts: HashMap<usize, usize> = HashMap::new();

    while selected.len() < max {
        let mut best: Option<(usize, f64)> = None;
        for (i, c) in cands.iter().enumerate() {
            if selected.contains(&i) {
                continue;
            }
            if *page_counts.get(&c.page).unwrap_or(&0) >= per_page_cap {
                continue;
            }
            let redundancy = selected
                .iter()
                .map(|j| jaccard(&c.words, &cands[*j].words))
                .fold(0.0f64, f64::max);
            let gain = LAMBDA * c.score - (1.0 - LAMBDA) * redundancy;
            if best.is_none_or(|(_, b)| gain > b) {
                best = Some((i, gain));
            }
        }
        match best {
            Some((idx, gain)) if gain >= MIN_GAIN => {
                let page = cands[idx].page;
                *page_counts.entry(page).or_insert(0) += 1;
                selected.push(idx);
            }
            _ => break,
        }
    }
    selected
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

// ---------------------------------------------------------------------------
// Text utilities
// ---------------------------------------------------------------------------

/// Splits text into sentences.
///
/// The crawler stores prose hard-wrapped at ~60 columns, so a single
/// `\n` is a *soft* wrap and is joined with a space. Blank lines
/// (`\n\n`) are paragraph breaks, and a line starting with a bullet or
/// heading marker (`-`, `*`, `#`, …) is a boundary too. Footnote anchors
/// (`[46]`), numbered section headings (`4.2 Coherentism`) and a short
/// leading page-title line are dropped rather than merged into prose.
/// Terminal punctuation splits with the usual decimal/abbreviation
/// guards.
pub fn split_sentences(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();

    // The physical line currently being accumulated (used to classify
    // headings and footnote anchors).
    let mut line = String::new();
    // The line currently being accumulated began with a bullet marker?
    let mut line_is_marker = false;
    // The next non-whitespace char begins a new line.
    let mut at_line_start = true;

    // Skip a leading page-title line ("Epistemology\nThe term …").
    let mut i = 0;
    {
        let mut s = 0;
        while s < chars.len() && chars[s].is_whitespace() {
            s += 1;
        }
        if s < chars.len() {
            let mut e = s;
            while e < chars.len() && chars[e] != '\n' {
                e += 1;
            }
            let first: String = chars[s..e].iter().collect();
            if is_title_line(&first) {
                i = e + usize::from(e < chars.len());
            }
        }
    }

    while i < chars.len() {
        let c = chars[i];

        // --- Line structure: paragraph breaks vs soft wraps -----------
        if c == '\n' {
            // Consume the newline run plus surrounding spaces and see
            // what follows.
            let mut j = i;
            let mut newlines = 0usize;
            while j < chars.len() {
                match chars[j] {
                    '\n' => {
                        newlines += 1;
                        j += 1;
                    }
                    ' ' | '\t' | '\r' => j += 1,
                    _ => break,
                }
            }
            let next = chars.get(j).copied();
            let paragraph_break = newlines >= 2;
            let marker_boundary = line_is_marker || next.is_some_and(is_marker_char);
            // Peek the upcoming line: a heading/anchor there must start
            // fresh instead of merging into the text before it.
            let next_is_structural = {
                let mut k = j;
                while k < chars.len() && chars[k] != '\n' {
                    k += 1;
                }
                let peek: String = chars[j..k].iter().collect();
                is_droppable_line(&peek)
            };

            if is_droppable_line(&line) {
                // Footnote anchor / section heading: drop, don't emit.
                cur.clear();
            } else if paragraph_break || marker_boundary || next_is_structural {
                flush_sentence(&mut cur, &mut out);
            } else if !cur.is_empty() && !cur.ends_with(char::is_whitespace) {
                // Soft wrap: keep the sentence going across the line.
                cur.push(' ');
            }
            line.clear();
            line_is_marker = false;
            at_line_start = true;
            i = j;
            continue;
        }

        cur.push(c);
        line.push(c);

        if at_line_start && !c.is_whitespace() {
            line_is_marker = is_marker_char(c);
            at_line_start = false;
        }

        // --- Terminal punctuation --------------------------------------
        if c == '.' || c == '!' || c == '?' {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let mut j = i + 1;
            while j < chars.len() && (chars[j] == ' ' || chars[j] == '\t' || chars[j] == '\r') {
                j += 1;
            }
            let next = chars.get(j).copied();

            // Decimal numbers: "3.5"
            if c == '.'
                && prev.is_some_and(|p| p.is_ascii_digit())
                && next.is_some_and(|n| n.is_ascii_digit())
            {
                i += 1;
                continue;
            }

            let boundary = next.is_none_or(|n| {
                n == '\n'
                    || n.is_uppercase()
                    || n.is_numeric()
                    || n == '"'
                    || n == '\''
                    || n == '('
                    || n == '['
            });

            if boundary && !ends_with_abbreviation(&cur) {
                flush_sentence(&mut cur, &mut out);
            }
        }
        i += 1;
    }
    if is_droppable_line(&line) {
        cur.clear();
    }
    flush_sentence(&mut cur, &mut out);
    out
}

fn flush_sentence(cur: &mut String, out: &mut Vec<String>) {
    let trimmed = cur.trim();
    if !trimmed.is_empty() && !is_droppable_line(trimmed) {
        out.push(trimmed.to_string());
    }
    cur.clear();
}

/// Text that carries no sentence content: bare footnote anchors
/// (`[46]`) and numbered section headings (`4.2 Coherentism`).
fn is_droppable_line(line: &str) -> bool {
    let t = line.trim();
    if t.is_empty() {
        return false;
    }
    let bracket = t.strip_prefix('[').and_then(|s| s.strip_suffix(']'));
    if bracket.is_some_and(|inner| {
        inner.chars().count() <= 8 && !inner.chars().any(char::is_whitespace)
    }) {
        return true;
    }
    is_numbered_heading(t)
}

/// A short numbered section label ("4.2 Coherentism"), but not a
/// wrapped prose line that merely starts with a number ("1978. An even
/// more fundamental …").
fn is_numbered_heading(t: &str) -> bool {
    let mut chars = t.chars().peekable();
    let mut groups = 0u8;
    loop {
        let mut digits = 0u8;
        while digits < 3 && chars.peek().is_some_and(char::is_ascii_digit) {
            chars.next();
            digits += 1;
        }
        if digits == 0 {
            return false;
        }
        groups += 1;
        if groups > 4 {
            return false;
        }
        if chars.peek() == Some(&'.') {
            // Keep consuming while a digit group follows ("4.2.1"),
            // otherwise take the trailing dot ("4.") and stop.
            let mut look = chars.clone();
            look.next();
            if look.peek().is_some_and(char::is_ascii_digit) {
                chars.next();
                continue;
            }
            chars.next();
        }
        break;
    }
    if !chars.peek().is_some_and(|c| c.is_whitespace()) {
        return false;
    }
    let rest: String = chars.collect();
    let rest = rest.trim_start();
    let words = rest.split_whitespace().count();
    (1..=8).contains(&words)
        && rest.chars().next().is_some_and(|c| c.is_uppercase())
        && !rest.contains(['.', '!', '?', ';'])
}

/// A very short leading line in title case with no sentence
/// punctuation — the page's own H1 emitted before the body.
fn is_title_line(line: &str) -> bool {
    let t = line.trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    if words.is_empty() || words.len() > 6 || t.chars().count() > 56 {
        return false;
    }
    if t.chars().next().is_some_and(is_marker_char) || t.contains(['.', '!', '?']) {
        return false;
    }
    words
        .iter()
        .all(|w| w.chars().next().is_some_and(|c| c.is_uppercase()))
}

/// Line-leading characters that mark a structural line (bullet item,
/// heading, block quote) rather than hard-wrapped prose.
fn is_marker_char(c: char) -> bool {
    matches!(c, '-' | '*' | '#' | '>' | '•' | '·')
}

fn ends_with_abbreviation(sentence: &str) -> bool {
    let last_word = sentence
        .split_whitespace()
        .last()
        .unwrap_or_default()
        .trim_end_matches(['"', '\'', ')']);
    if last_word.is_empty() {
        return false;
    }
    // Single-letter initial: "J." or "U.S." fragments handled below.
    let core = last_word.trim_end_matches('.');
    if core.chars().count() == 1 && core.chars().next().is_some_and(|c| c.is_uppercase()) {
        return true;
    }
    authority::is_abbreviation(last_word)
}

fn content_words(text: &str) -> HashSet<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_lowercase()
        })
        .filter(|w| w.len() >= 3 && !query::STOPWORDS.contains(&w.as_str()))
        .collect()
}

fn word_frequencies(sentences: &[String]) -> HashMap<String, usize> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    for s in sentences {
        for w in content_words(s) {
            *freq.entry(w).or_insert(0) += 1;
        }
    }
    freq
}

fn count_digit_runs(text: &str) -> usize {
    let mut count = 0;
    let mut in_run = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_run {
                count += 1;
                in_run = true;
            }
        } else {
            in_run = false;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// HTTP handlers
// ---------------------------------------------------------------------------

/// `POST /api/summarize` (JSON body) and `GET /api/summarize?query=...`
/// both land here. Returns either the summary as JSON or as a PDF download
/// depending on `format`.
pub async fn run_summarize(
    state: crate::state::AppState,
    req: SummaryRequest,
) -> Result<axum::response::Response, ApiError> {
    let doc = build_summary(&state.pool, &req).await?;
    match req.format {
        SummaryFormat::Json => Ok(axum::Json(doc).into_response()),
        SummaryFormat::Pdf => {
            let bytes = crate::pdf::render_summary(&doc).map_err(ApiError::internal)?;
            let filename = pdf_filename(&req.query);
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/pdf"),
            );
            if let Ok(value) =
                axum::http::HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
            {
                headers.insert(axum::http::header::CONTENT_DISPOSITION, value);
            }
            Ok((headers, bytes).into_response())
        }
    }
}

pub async fn summarize_post(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::Json(req): axum::Json<SummaryRequest>,
) -> Result<axum::response::Response, ApiError> {
    run_summarize(state, req).await
}

pub async fn summarize_get(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    axum::extract::Query(mut req): axum::extract::Query<SummaryRequest>,
) -> Result<axum::response::Response, ApiError> {
    req.format = SummaryFormat::Pdf;
    run_summarize(state, req).await
}

fn pdf_filename(query: &str) -> String {
    let slug: String = query
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = if slug.is_empty() {
        "summary".to_string()
    } else {
        slug.chars().take(40).collect()
    };
    format!("veridex-summary-{slug}.pdf")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn splits_plain_sentences() {
        let out = split_sentences("First sentence here. Second one! Third?");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], "First sentence here.");
    }

    #[test]
    fn keeps_decimals_intact() {
        let out = split_sentences("The value is 3.5 exactly. Next sentence.");
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("3.5"));
    }

    #[test]
    fn respects_abbreviations() {
        let out = split_sentences("Smith et al. continued the work. Done.");
        assert_eq!(out.len(), 2);
        assert!(out[0].contains("et al."));
    }

    #[test]
    fn blank_line_is_a_boundary() {
        let out = split_sentences("Heading line\n\nBody text follows here.");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "Heading line");
        assert_eq!(out[1], "Body text follows here.");
    }

    #[test]
    fn soft_wrapped_lines_are_joined() {
        // Crawler content is hard-wrapped at ~60 columns mid-sentence.
        let out = split_sentences(
            "Plato's epistemology was\nan attempt to understand what it was\nto know, and how knowledge is good.",
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            "Plato's epistemology was an attempt to understand what it was to know, and how knowledge is good."
        );
    }

    #[test]
    fn bullet_lines_are_boundaries() {
        let out = split_sentences("- first item\n- second item\nprose after the list.");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], "- first item");
        assert_eq!(out[1], "- second item");
        assert_eq!(out[2], "prose after the list.");
    }

    #[test]
    fn section_abbreviations_do_not_end_sentences() {
        // SEP cross-references use "Sect. 4" — must not split before "4".
        let out = split_sentences("See the entry, Sect. 4 for a survey of the relevant disorders.");
        assert_eq!(out.len(), 1, "{out:?}");
    }

    #[test]
    fn leading_title_line_is_skipped() {
        let out = split_sentences(
            "Epistemology\nThe term \u{201c}epistemology\u{201d} comes from the Greek words \u{201c}episteme\u{201d} and \u{201c}logos\u{201d}.",
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("The term"), "{out:?}");
    }

    #[test]
    fn numbered_heading_and_anchor_lines_are_dropped() {
        let text = "however such rationality is to be understood.[46]\n4.2 Coherentism\nFoundationalism says that knowledge is structured like a building.\nNext sentence follows.";
        let out = split_sentences(text);
        assert!(
            out.iter().all(|s| !s.contains("4.2") && !s.contains("[46]")),
            "{out:?}"
        );
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(
            out[0],
            "however such rationality is to be understood."
        );
        assert!(out[1].starts_with("Foundationalism says"), "{out:?}");
        assert_eq!(out[2], "Next sentence follows.");
    }

    #[test]
    fn multi_level_section_headings_are_dropped() {
        let out = split_sentences(
            "4.4.1 Conceptual issues and analytic functionalism\nOne such view is tied to functionalism, although this connection is mainly implicit.",
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].starts_with("One such view"), "{out:?}");
    }

    #[test]
    fn wrapped_prose_starting_with_a_number_is_not_a_heading() {
        let out = split_sentences("become so dominant (Feyerabend\n1978).\nAn even more fundamental criticism was offered.");
        assert!(out.iter().any(|s| s.contains("1978")), "{out:?}");
        assert!(out.iter().any(|s| s.starts_with("An even")), "{out:?}");
    }

    #[test]
    fn does_not_split_single_letter_initials() {
        let out = split_sentences("Written by J. Smith in 2020. It is fine.");
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn mmr_prefers_diverse_sentences() {
        // Two near-identical sentences + one distinct one.
        let dup = "The quick brown fox jumps over the lazy dog repeatedly.";
        let other = "Quantum entanglement enables secure key distribution today.";
        let text = [dup.to_string(), dup.to_string(), other.to_string()];
        let cands: Vec<Candidate> = text
            .iter()
            .enumerate()
            .map(|(i, t)| Candidate {
                text: t.clone(),
                words: content_words(t),
                score: if i == 2 { 0.8 } else { 0.9 },
                page: 0,
                order: i,
            })
            .collect();
        let picked = mmr_select(&cands, 2);
        assert_eq!(picked.len(), 2);
        assert_ne!(
            cands[picked[0]].text, cands[picked[1]].text,
            "redundant duplicates must not both be selected"
        );
    }

    #[test]
    fn mmr_caps_per_page() {
        let cands: Vec<Candidate> = (0..10)
            .map(|i| Candidate {
                text: format!("Sentence number {i} with enough words to be valid here ok."),
                words: content_words(&format!("Sentence number {i} with enough words to be valid here ok.")),
                score: 0.9,
                page: 0,
                order: i,
            })
            .collect();
        let picked = mmr_select(&cands, 6);
        assert!(picked.len() <= 4, "per-page cap is 4, got {}", picked.len());
    }

    #[tokio::test]
    async fn end_to_end_summary() {
        let pool = db::connect(":memory:").await.expect("db");
        db::init_schema(&pool).await.expect("schema");

        let edu_content = "Photosynthesis converts light energy into chemical energy stored in glucose molecules. \
            The light-dependent reactions occur across the thylakoid membrane of the chloroplast. \
            Chlorophyll absorbs photons at wavelengths near 430 and 662 nanometres under normal daylight. \
            The Calvin cycle then fixes carbon dioxide into glyceraldehyde 3-phosphate using rubisco enzymes. \
            Researchers measured a quantum yield of 0.125 under carefully controlled laboratory conditions. \
            Water photolysis supplies the electrons that replenish photosystem II reaction centres. \
            C4 plants spatially separate carbon fixation from the calvin cycle to reduce photorespiration losses. \
            See the supplementary materials for further details on measurement methodology and error bars. \
            Global crop yields depend on the efficiency of these reactions across varying light regimes. \
            Computational models estimate theoretical maximum efficiency near eleven percent for c3 species. \
            Field measurements consistently report six to eight percent conversion for major cereal crops. \
            Temperature stress deactivates rubisco activase and lowers overall carbon fixation rates. \
            Isotope tracing experiments confirm the temporal ordering of intermediate metabolites in vivo. \
            The full pathway was elucidated through decades of coordinated biochemical investigation. \
            Recent advances in cryo electron microscopy reveal conformational states of photosystem one. \
            These structural insights explain how excitation energy transfers with minimal wasteful dissipation.";
        let farm_content = "Photosynthesis is when plants eat sunlight maybe. \
            Anyway here are ten amazing photosynthesis facts that will blow your mind completely. \
            Number seven is absolutely wild and totally scientific for sure my friends.";

        db::insert_page(
            &pool,
            "https://stanford.edu/photo",
            Some("stanford.edu"),
            Some("Photosynthesis overview".into()),
            None,
            None,
            Some(edu_content.to_string()),
            "<html></html>".into(),
        )
        .await
        .unwrap();
        db::insert_page(
            &pool,
            "https://wikihow.com/photo",
            Some("wikihow.com"),
            Some("How plants do photosynthesis".into()),
            None,
            None,
            Some(farm_content.to_string()),
            "<html></html>".into(),
        )
        .await
        .unwrap();

        let req = SummaryRequest {
            query: "photosynthesis".into(),
            max_pages: 5,
            max_sentences: 6,
            format: SummaryFormat::Json,
            title: None,
            live: false,
        };
        let doc = build_summary(&pool, &req).await.expect("summary");
        assert!(!doc.findings.is_empty());
        assert!(!doc.sources.is_empty());
        assert_eq!(doc.sources[0].index, 1);
        // The .edu source must rank first.
        assert_eq!(doc.sources[0].grade, "A");
        assert!(doc.sources[0].url.contains("stanford.edu"));
        assert!(doc.stats.pages_analyzed >= 1);
        // Every finding references a valid source index.
        for f in &doc.findings {
            assert!(f.source >= 1 && f.source <= doc.sources.len());
        }
    }

    #[tokio::test]
    async fn unknown_query_errors() {
        let pool = db::connect(":memory:").await.expect("db");
        db::init_schema(&pool).await.expect("schema");
        let req = SummaryRequest {
            query: "zzzznotfound".into(),
            max_pages: 5,
            max_sentences: 6,
            format: SummaryFormat::Json,
            title: None,
            live: false,
        };
        let err = build_summary(&pool, &req).await.unwrap_err();
        assert!(matches!(err, ApiError::NotFound(_)));
    }
}
