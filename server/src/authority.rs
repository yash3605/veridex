//! Source authority scoring — what makes this search engine prefer
//! reliable, citable material over popular pages.
//!
//! Every page gets a score in `[0, 1]` composed of:
//!
//! 1. **Host base score** — curated tiers: educational/government domains,
//!    international organisations and scholarly publishers/repos score high;
//!    content farms, essay mills and UGC sinks score low.
//! 2. **Page signals** — DOI in the URL, a references section, inline
//!    citations, long-form content, abstracts, PDF documents, metadata.
//!
//! The score is bucketed into a letter grade (A–D) that the API exposes
//! alongside the numeric score so the frontend can badge results.

/// Exact-or-suffix host matches we trust (scholarly publishers, repos,
/// international bodies, quality references). Matched before TLD rules.
const CURATED_TRUSTED: &[(&str, f64)] = &[
    // Repositories & indexing
    ("arxiv.org", 0.96),
    ("doi.org", 0.97),
    ("pubmed.ncbi.nlm.nih.gov", 0.96),
    ("ncbi.nlm.nih.gov", 0.95),
    ("europepmc.org", 0.95),
    ("jstor.org", 0.95),
    ("projecteuclid.org", 0.94),
    ("papers.ssrn.com", 0.93),
    ("philpapers.org", 0.93),
    ("repec.org", 0.92),
    ("ideas.repec.org", 0.92),
    ("semanticscholar.org", 0.90),
    ("openalex.org", 0.90),
    ("crossref.org", 0.92),
    ("orcid.org", 0.85),
    ("doaj.org", 0.90),
    // Publishers
    ("nature.com", 0.95),
    ("science.org", 0.95),
    ("sciencedirect.com", 0.94),
    ("link.springer.com", 0.94),
    ("springer.com", 0.94),
    ("onlinelibrary.wiley.com", 0.93),
    ("wiley.com", 0.93),
    ("ieeexplore.ieee.org", 0.93),
    ("ieee.org", 0.93),
    ("dl.acm.org", 0.93),
    ("acm.org", 0.93),
    ("tandfonline.com", 0.92),
    ("sagepub.com", 0.91),
    ("journals.plos.org", 0.94),
    ("plos.org", 0.94),
    ("biomedcentral.com", 0.93),
    ("mdpi.com", 0.88),
    ("frontiersin.com", 0.88),
    ("bmj.com", 0.94),
    ("thelancet.com", 0.94),
    ("cell.com", 0.94),
    ("pnas.org", 0.94),
    ("pubs.rsc.org", 0.92),
    ("iopscience.iop.org", 0.92),
    ("cambridge.org", 0.93),
    ("academic.oup.com", 0.94),
    ("muse.jhu.edu", 0.92),
    ("proquest.com", 0.90),
    // International organisations & standards
    ("ipcc.ch", 0.95),
    ("who.int", 0.94),
    ("un.org", 0.93),
    ("unesco.org", 0.92),
    ("oecd.org", 0.93),
    ("imf.org", 0.93),
    ("worldbank.org", 0.93),
    ("wto.org", 0.92),
    ("fao.org", 0.92),
    ("iea.org", 0.90),
    ("undp.org", 0.90),
    ("unicef.org", 0.90),
    ("w3.org", 0.90),
    ("ietf.org", 0.90),
    ("rfc-editor.org", 0.92),
    ("iso.org", 0.90),
    // Quality references & open knowledge
    ("ourworldindata.org", 0.88),
    ("mathworld.wolfram.com", 0.82),
    ("gutenberg.org", 0.82),
    ("archive.org", 0.75),
    ("britannica.com", 0.80),
    ("khanacademy.org", 0.78),
    // Selective quality news (secondary sources — useful but not primary)
    ("reuters.com", 0.62),
    ("apnews.com", 0.62),
    ("bbc.co.uk", 0.60),
    ("bbc.com", 0.60),
];

/// Hosts we actively demote: content farms, essay mills, UGC sinks.
const CURATED_DEMOTED: &[(&str, f64)] = &[
    ("essayshark.com", 0.05),
    ("ukessays.com", 0.05),
    ("123helpme.com", 0.05),
    ("echeat.com", 0.10),
    ("phdessay.com", 0.10),
    ("coursehero.com", 0.15),
    ("chegg.com", 0.15),
    ("bartleby.com", 0.15),
    ("brainly.com", 0.15),
    ("studocu.com", 0.15),
    ("pinterest.com", 0.15),
    ("facebook.com", 0.10),
    ("instagram.com", 0.10),
    ("tiktok.com", 0.10),
    ("twitter.com", 0.10),
    ("x.com", 0.12),
    ("wikihow.com", 0.18),
    ("answers.com", 0.20),
    ("gradesaver.com", 0.20),
    ("quizlet.com", 0.25),
    ("docsity.com", 0.25),
    ("buzzfeed.com", 0.25),
    ("listverse.com", 0.25),
    ("toppr.com", 0.25),
    ("quora.com", 0.28),
    ("sparknotes.com", 0.30),
    ("shmoop.com", 0.30),
    ("enotes.com", 0.30),
    ("slideshare.net", 0.30),
    ("scribd.com", 0.30),
    ("reddit.com", 0.30),
    ("medium.com", 0.40),
    ("stackoverflow.com", 0.42),
];

/// Educational TLDs (country-specific academic namespaces).
const EDUCATIONAL_TLDS: &[&str] = &[
    "edu", "ac.uk", "ac.jp", "ac.nz", "ac.au", "edu.au", "edu.cn", "edu.br", "edu.sg", "edu.hk",
    "edu.tw", "ac.in", "edu.in", "ac.kr", "ac.il", "ac.za", "ac.th", "ac.at", "ac.be", "ac.ch",
    "ac.id", "edu.mx",
];

/// Government / military TLDs.
const GOVERNMENT_TLDS: &[&str] = &[
    "gov", "mil", "gc.ca", "gov.uk", "gov.au", "go.jp", "gouv.fr", "gov.br", "gov.in", "gov.sg",
    "gov.hk", "gov.za", "gov.tr", "gov.nz", "gov.cn", "gov.il", "gov.my", "gov.ph", "gov.pl",
    "go.th", "gob.es",
];

const ABBREV: &[&str] = &[
    "e", "g", "i", "etc", "vs", "al", "dr", "mr", "mrs", "ms", "prof", "no", "approx", "cf", "ca",
    "st", "fig", "eq", "vol", "dept", "est", "inc", "ltd", "jr", "sr", "u.s", "u.k", "ref",
    "sect", "sec", "ch", "pp", "para",
];

/// Extract the host from a URL string, lowercased, without `www.`.
pub fn extract_host(url: &str) -> Option<String> {
    let after_scheme = match url.find("://") {
        Some(i) => &url[i + 3..],
        None => url,
    };
    let end = after_scheme
        .find(['/', '?', '#', ':'])
        .unwrap_or(after_scheme.len());
    let host = &after_scheme[..end];
    if host.is_empty() {
        return None;
    }
    Some(strip_www(&host.to_ascii_lowercase()).to_string())
}

fn strip_www(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

/// Normalizes a user-supplied `domain:` filter to a bare host.
pub fn normalize_domain_filter(raw: &str) -> String {
    let trimmed = raw
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let end = trimmed
        .find(['/', '?', '#'])
        .unwrap_or(trimmed.len());
    strip_www(&trimmed[..end].to_ascii_lowercase()).to_string()
}

fn matches_host(host: &str, entry: &str) -> bool {
    host == entry || host.ends_with(&format!(".{entry}"))
}

/// Base score for a host from the curated tiers and TLD rules.
/// Returns `(score, signal)` where signal describes why.
pub fn host_base_score(host: &str) -> (f64, &'static str) {
    let lowered = host.to_ascii_lowercase();
    let host = strip_www(&lowered);

    for (entry, score) in CURATED_DEMOTED {
        if matches_host(host, entry) {
            return (*score, "demoted_source");
        }
    }
    for (entry, score) in CURATED_TRUSTED {
        if matches_host(host, entry) {
            return (*score, "trusted_source");
        }
    }
    // Encyclopedias: useful starting point, but not itself citable.
    if matches_host(host, "wikipedia.org") {
        return (0.62, "encyclopedia");
    }
    for tld in EDUCATIONAL_TLDS {
        if host == *tld || host.ends_with(&format!(".{tld}")) {
            return (0.92, "educational_domain");
        }
    }
    for tld in GOVERNMENT_TLDS {
        if host == *tld || host.ends_with(&format!(".{tld}")) {
            return (0.94, "government_domain");
        }
    }
    (0.50, "neutral_domain")
}

/// Letter grade for an authority score.
pub fn grade(score: f64) -> &'static str {
    if score >= 0.85 {
        "A"
    } else if score >= 0.68 {
        "B"
    } else if score >= 0.50 {
        "C"
    } else {
        "D"
    }
}

/// Full page-level authority score: host tier + page signals.
pub struct PageAuthority {
    pub score: f64,
    pub signals: Vec<String>,
}

pub fn score_page(
    url: &str,
    host: Option<&str>,
    title: Option<&str>,
    description: Option<&str>,
    keywords: Option<&str>,
    content: Option<&str>,
) -> PageAuthority {
    let host = host
        .map(|h| h.to_string())
        .or_else(|| extract_host(url))
        .unwrap_or_default();

    let (base, base_signal) = host_base_score(&host);
    let mut signals = vec![base_signal.to_string()];
    let mut score = base;
    let lower_url = url.to_ascii_lowercase();

    // DOI links are the gold standard for citable material.
    if lower_url.contains("doi.org/") || lower_url.contains("/10.1") {
        score += 0.12;
        signals.push("doi".into());
    }
    // Direct PDF documents are usually primary material.
    if lower_url.split(['?', '#']).next().is_some_and(|p| p.ends_with(".pdf")) {
        score += 0.06;
        signals.push("pdf_document".into());
    }

    if let Some(content) = content {
        let word_count = content.split_whitespace().count();
        if word_count >= 700 {
            score += 0.07;
            signals.push("long_form".into());
        } else if word_count < 120 {
            score -= 0.12;
            signals.push("thin_content".into());
        }

        let lower = content.to_ascii_lowercase();
        // A references section near the end is a strong scholarly signal.
        let mut tail_start = lower.len().saturating_sub(lower.len() / 2 + 1);
        while tail_start < lower.len() && !lower.is_char_boundary(tail_start) {
            tail_start += 1;
        }
        let tail = &lower[tail_start..];
        if tail.contains("references")
            || tail.contains("bibliography")
            || tail.contains("works cited")
        {
            score += 0.07;
            signals.push("references_section".into());
        }
        // Inline citations like [1], [23].
        let mut bracketed = 0usize;
        let bytes = lower.as_bytes();
        let mut i = 0;
        while i + 2 < bytes.len() {
            if bytes[i] == b'[' && bytes[i + 1].is_ascii_digit() {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b']' {
                    bracketed += 1;
                    i = j;
                }
            }
            i += 1;
        }
        if bracketed >= 3 {
            score += 0.05;
            signals.push("inline_citations".into());
        }
        // Abstract-style opening.
        if lower.chars().take(400).collect::<String>().contains("abstract") {
            score += 0.04;
            signals.push("has_abstract".into());
        }
    }

    if title.is_some_and(|t| t.trim().len() >= 8) {
        score += 0.03;
        signals.push("has_title".into());
    }
    if description.is_some_and(|d| !d.trim().is_empty()) {
        score += 0.03;
        signals.push("has_description".into());
    }
    if keywords.is_some_and(|k| !k.trim().is_empty()) {
        score += 0.02;
        signals.push("has_keywords".into());
    }

    PageAuthority {
        score: score.clamp(0.0, 1.0),
        signals,
    }
}

/// Detects a "references"-style heading word used by the summarizer.
pub fn is_abbreviation(word: &str) -> bool {
    let cleaned = word.trim_end_matches('.').to_ascii_lowercase();
    ABBREV.contains(&cleaned.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn educational_domains_score_high() {
        assert!(host_base_score("stanford.edu").0 >= 0.9);
        assert_eq!(host_base_score("mit.edu").1, "educational_domain");
        assert!(host_base_score("www.ox.ac.uk").0 >= 0.9);
        assert!(host_base_score("plato.stanford.edu").0 >= 0.9);
    }

    #[test]
    fn government_domains_score_high() {
        assert!(host_base_score("nih.gov").0 >= 0.9);
        assert!(host_base_score("nasa.gov").0 >= 0.9);
        assert_eq!(host_base_score("gov.uk").1, "government_domain");
    }

    #[test]
    fn trusted_publishers_are_curated() {
        assert!(host_base_score("arxiv.org").0 >= 0.9);
        assert!(host_base_score("www.nature.com").0 >= 0.9);
        assert!(host_base_score("doi.org").0 >= 0.9);
        assert_eq!(host_base_score("jstor.org").1, "trusted_source");
    }

    #[test]
    fn content_farms_are_demoted() {
        assert!(host_base_score("wikihow.com").0 < 0.3);
        assert!(host_base_score("coursehero.com").0 < 0.3);
        assert!(host_base_score("essayshark.com").0 < 0.1);
        assert!(host_base_score("www.reddit.com").0 <= 0.3);
    }

    #[test]
    fn neutral_hosts_get_mid_score() {
        let (s, signal) = host_base_score("example.com");
        assert_eq!(s, 0.50);
        assert_eq!(signal, "neutral_domain");
    }

    #[test]
    fn wikipedia_is_slightly_above_neutral() {
        let (s, signal) = host_base_score("en.wikipedia.org");
        assert_eq!(signal, "encyclopedia");
        assert!((0.6..0.7).contains(&s));
    }

    #[test]
    fn doi_urls_boost_score() {
        let plain = score_page(
            "https://example.org/paper",
            None,
            None,
            None,
            None,
            Some("word ".repeat(100).as_str()),
        );
        let with_doi = score_page(
            "https://doi.org/10.1038/nature12373",
            None,
            None,
            None,
            None,
            Some("word ".repeat(100).as_str()),
        );
        assert!(with_doi.score > plain.score);
        assert!(with_doi.signals.contains(&"doi".into()));
    }

    #[test]
    fn references_and_citations_boost_score() {
        let content = format!(
            "Some body text. {} [1] [2] [3] More text.\n\nReferences\n[1] Smith 2020",
            "filler sentence here. ".repeat(50)
        );
        let a = score_page(
            "https://example.org/x",
            None,
            Some("A proper title"),
            Some("desc"),
            None,
            Some(&content),
        );
        assert!(a.signals.contains(&"references_section".into()));
        assert!(a.signals.contains(&"inline_citations".into()));
        assert!(a.score > 0.5);
    }

    #[test]
    fn thin_content_is_penalised() {
        let thin = score_page("https://example.org/x", None, None, None, None, Some("tiny"));
        assert!(thin.signals.contains(&"thin_content".into()));
        assert!(thin.score < 0.5);
    }

    #[test]
    fn grades_map_to_bands() {
        assert_eq!(grade(0.95), "A");
        assert_eq!(grade(0.7), "B");
        assert_eq!(grade(0.55), "C");
        assert_eq!(grade(0.3), "D");
    }

    #[test]
    fn host_extraction() {
        assert_eq!(
            extract_host("https://www.Stanford.EDU/foo/bar").as_deref(),
            Some("stanford.edu")
        );
        assert_eq!(extract_host("example.org?x=1").as_deref(), Some("example.org"));
        assert_eq!(extract_host("http://localhost:8080/x").as_deref(), Some("localhost"));
        assert_eq!(extract_host(""), None);
        assert_eq!(extract_host("///"), None);
    }

    #[test]
    fn domain_filter_normalization() {
        assert_eq!(
            normalize_domain_filter("https://www.Stanford.edu/entries/"),
            "stanford.edu"
        );
        assert_eq!(normalize_domain_filter("arxiv.org"), "arxiv.org");
    }

    #[test]
    fn abbreviation_detection() {
        assert!(is_abbreviation("al."));
        assert!(is_abbreviation("etc"));
        assert!(!is_abbreviation("attention"));
    }
}
