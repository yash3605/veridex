//! HTML → structured page fields.
//!
//! Shared by the crawler and the server's live-search fetcher so pages
//! indexed from either path produce identical text for ranking, snippets
//! and summarization.

use scraper::{Html, Selector};

/// Fields lifted out of a fetched HTML document.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Extracted {
    /// `<title>` text, trimmed.
    pub title: Option<String>,
    /// `meta[name=description]` content, trimmed.
    pub description: Option<String>,
    /// `meta[name=keywords]` content, trimmed.
    pub keywords: Option<String>,
    /// Visible text of the main content elements, newline-joined.
    pub content: String,
    /// Raw `href` values of every anchor in the document.
    pub links: Vec<String>,
}

/// Parses an HTML document into [`Extracted`].
///
/// Content comes from `h1…h5, p, article, main, section` — the same
/// element set the crawler has always used — joined with newlines.
pub fn from_html(html: &str) -> Extracted {
    let doc = Html::parse_fragment(html);

    let title = text_of(&doc, "title");
    let description = attr_of(&doc, "meta[name='description']", "content");
    let keywords = attr_of(&doc, "meta[name='keywords']", "content");

    let content_selector = selector("h1, h2, h3, h4, h5, p, article, main, section");
    let content = doc
        .select(&content_selector)
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let anchor = selector("a");
    let links = doc
        .select(&anchor)
        .filter_map(|el| el.value().attr("href"))
        .map(|s| s.to_string())
        .collect();

    Extracted {
        title,
        description,
        keywords,
        content,
        links,
    }
}

fn selector(s: &str) -> Selector {
    Selector::parse(s).expect("valid CSS selector")
}

fn text_of(doc: &Html, sel: &str) -> Option<String> {
    doc.select(&selector(sel))
        .next()
        .map(|el| el.text().collect::<String>().trim().to_string())
        .filter(|t| !t.is_empty())
}

fn attr_of(doc: &Html, sel: &str, attr: &str) -> Option<String> {
    doc.select(&selector(sel))
        .next()
        .and_then(|el| el.value().attr(attr))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = r#"
        <html><head>
          <title>  Quantum Biology — Journal  </title>
          <meta name="description" content="Quantum effects in living systems.">
          <meta name="keywords" content="quantum, biology, coherence">
        </head><body>
          <nav><a href="/about">About</a></nav>
          <article>
            <h1>Quantum Biology</h1>
            <p>Coherence in photosynthesis lasts longer than expected.</p>
          </article>
          <a href="https://example.com/paper">Paper</a>
        </body></html>"#;

    #[test]
    fn extracts_title_meta_and_content() {
        let got = from_html(DOC);
        assert_eq!(got.title.as_deref(), Some("Quantum Biology — Journal"));
        assert_eq!(
            got.description.as_deref(),
            Some("Quantum effects in living systems.")
        );
        assert_eq!(got.keywords.as_deref(), Some("quantum, biology, coherence"));
        assert!(got.content.contains("Coherence in photosynthesis"));
        assert!(got.content.contains("Quantum Biology"));
    }

    #[test]
    fn collects_every_link() {
        let got = from_html(DOC);
        assert!(got.links.contains(&"/about".to_string()));
        assert!(got.links.contains(&"https://example.com/paper".to_string()));
    }

    #[test]
    fn empty_document_is_default() {
        assert_eq!(from_html(""), Extracted::default());
    }

    #[test]
    fn missing_meta_fields_stay_none() {
        let got = from_html("<html><head><title>T</title></head><body><p>hi</p></body></html>");
        assert_eq!(got.title.as_deref(), Some("T"));
        assert!(got.description.is_none());
        assert!(got.keywords.is_none());
    }
}
