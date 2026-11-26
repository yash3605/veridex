//! PDF renderer for research summaries.
//!
//! A small layout engine on top of `printpdf`: A4 pages, word-wrapped
//! paragraphs with hanging indents, section rules, page footers, and a
//! two-pass render so footers can show "page N of M".
//!
//! Text metrics use an approximate Helvetica width table (character-class
//! based), which is accurate enough for wrapping and verified visually.

use crate::summarize::SummaryDoc;
use printpdf::{
    BuiltinFont, Color, Greyscale, IndirectFontRef, Line, Mm, PdfDocument, PdfDocumentReference,
    PdfLayerIndex, PdfPageIndex, Point, Rgb,
};

const PAGE_W: f32 = 210.0;
const PAGE_H: f32 = 297.0;
const MARGIN_L: f32 = 18.0;
const MARGIN_R: f32 = 18.0;
const MARGIN_TOP: f32 = 16.0;
const MARGIN_BOT: f32 = 20.0;
const CONTENT_W: f32 = PAGE_W - MARGIN_L - MARGIN_R;

const PT_TO_MM: f32 = 25.4 / 72.0;

// --- Palette ---------------------------------------------------------------
fn ink() -> Color {
    Color::Greyscale(Greyscale::new(0.10, None))
}
fn accent() -> Color {
    Color::Rgb(Rgb::new(0.09, 0.20, 0.42, None))
}
fn muted() -> Color {
    Color::Greyscale(Greyscale::new(0.42, None))
}
fn rule_color() -> Color {
    Color::Greyscale(Greyscale::new(0.72, None))
}

/// Renders a summary document to PDF bytes.
pub fn render_summary(doc: &SummaryDoc) -> anyhow::Result<Vec<u8>> {
    // First pass to count pages, second pass to print "page N of M".
    let (_, pages) = render(doc, None)?;
    if pages <= 1 {
        let (bytes, _) = render(doc, Some(1))?;
        return Ok(bytes);
    }
    let (bytes, _) = render(doc, Some(pages as u32))?;
    Ok(bytes)
}

#[derive(Clone, Copy)]
enum Weight {
    Regular,
    Bold,
    Italic,
}

struct Fonts {
    regular: IndirectFontRef,
    bold: IndirectFontRef,
    italic: IndirectFontRef,
}

impl Fonts {
    fn get(&self, w: Weight) -> &IndirectFontRef {
        match w {
            Weight::Regular => &self.regular,
            Weight::Bold => &self.bold,
            Weight::Italic => &self.italic,
        }
    }
}

struct Layout<'a> {
    doc: &'a PdfDocumentReference,
    fonts: &'a Fonts,
    page: PdfPageIndex,
    layer: PdfLayerIndex,
    /// Current cursor, mm from the bottom edge.
    y: f32,
    page_no: usize,
    total_pages: Option<u32>,
}

fn render(doc: &SummaryDoc, total_pages: Option<u32>) -> anyhow::Result<(Vec<u8>, usize)> {
    let (pdf, page, layer) =
        PdfDocument::new(sanitize(&doc.title), Mm(PAGE_W), Mm(PAGE_H), "Content");

    let fonts = Fonts {
        regular: pdf
            .add_builtin_font(BuiltinFont::Helvetica)
            .map_err(|e| anyhow::anyhow!("font: {e:?}"))?,
        bold: pdf
            .add_builtin_font(BuiltinFont::HelveticaBold)
            .map_err(|e| anyhow::anyhow!("font: {e:?}"))?,
        italic: pdf
            .add_builtin_font(BuiltinFont::HelveticaOblique)
            .map_err(|e| anyhow::anyhow!("font: {e:?}"))?,
    };

    let mut layout = Layout {
        doc: &pdf,
        fonts: &fonts,
        page,
        layer,
        y: PAGE_H - MARGIN_TOP,
        page_no: 1,
        total_pages,
    };
    layout.draw_footer();
    layout.render_body(doc);

    let page_count = layout.page_no;
    let bytes = pdf
        .save_to_bytes()
        .map_err(|e| anyhow::anyhow!("save: {e:?}"))?;
    Ok((bytes, page_count))
}

impl Layout<'_> {
    // --- primitives --------------------------------------------------------

    fn layer(&self) -> printpdf::PdfLayerReference {
        self.doc.get_page(self.page).get_layer(self.layer)
    }

    fn ensure(&mut self, h: f32) {
        if self.y - h < MARGIN_BOT {
            self.new_page();
        }
    }

    fn new_page(&mut self) {
        let (page, layer) = self.doc.add_page(Mm(PAGE_W), Mm(PAGE_H), "Content");
        self.page = page;
        self.layer = layer;
        self.page_no += 1;
        self.y = PAGE_H - MARGIN_TOP;
        self.draw_footer();
    }

    fn draw_footer(&self) {
        let label = match self.total_pages {
            Some(t) => format!(
                "Veridex Research Brief  ·  page {} of {}",
                self.page_no, t
            ),
            None => format!("Veridex Research Brief  ·  page {}", self.page_no),
        };
        let w = text_width_mm(&label, 7.5, false);
        let x = ((PAGE_W - w) / 2.0).max(MARGIN_L);
        self.text(&label, 7.5, x, 10.5, Weight::Regular, muted());
    }

    fn text(&self, s: &str, size: f32, x: f32, y: f32, weight: Weight, color: Color) {
        if y < 6.0 {
            return; // never draw below the page
        }
        let layer = self.layer();
        layer.set_fill_color(color);
        layer.use_text(s, size, Mm(x), Mm(y), self.fonts.get(weight));
    }

    fn rule(&self, y: f32) {
        let layer = self.layer();
        layer.set_outline_color(rule_color());
        layer.set_outline_thickness(0.6);
        layer.add_line(Line {
            points: vec![
                (Point::new(Mm(MARGIN_L), Mm(y)), false),
                (Point::new(Mm(PAGE_W - MARGIN_R), Mm(y)), false),
            ],
            is_closed: false,
        });
    }

    fn spacer(&mut self, mm: f32) {
        self.y -= mm;
    }

    /// Draws `text` word-wrapped within the content column.
    /// `hanging` indents continuation lines (for bullets / citation markers).
    #[allow(clippy::too_many_arguments)]
    fn block(
        &mut self,
        text: &str,
        size: f32,
        weight: Weight,
        color: Color,
        indent: f32,
        hanging: f32,
        leading: f32,
    ) {
        let max_w = CONTENT_W - indent - hanging;
        let lines = wrap(&sanitize(text), size, max_w, matches!(weight, Weight::Bold));
        for (i, line) in lines.iter().enumerate() {
            self.ensure(leading);
            let x = MARGIN_L + indent + if i == 0 { 0.0 } else { hanging };
            self.text(line, size, x, self.y - leading * 0.8, weight, color.clone());
            self.y -= leading;
        }
    }

    fn section_header(&mut self, title: &str) {
        self.ensure(14.0);
        self.spacer(5.0);
        self.text(title, 11.0, MARGIN_L, self.y - 4.0, Weight::Bold, accent());
        self.y -= 5.5;
        self.rule(self.y);
        self.y -= 4.5;
    }

    // --- document body ----------------------------------------------------

    fn render_body(&mut self, doc: &SummaryDoc) {
        // Masthead
        self.text(
            "VERIDEX RESEARCH BRIEF",
            9.0,
            MARGIN_L,
            self.y - 3.5,
            Weight::Bold,
            accent(),
        );
        self.y -= 6.0;
        self.rule(self.y);
        self.y -= 8.0;

        // Title (the query or a user-supplied title)
        self.block(&doc.title, 17.0, Weight::Bold, ink(), 0.0, 0.0, 9.0);
        self.spacer(3.0);

        // Meta line
        let mut meta = format!(
            "Generated {}  ·  {} sources analyzed  ·  {} findings",
            doc.generated_at, doc.stats.pages_analyzed, doc.stats.sentences_selected
        );
        if doc.title != doc.query {
            meta = format!("Query: \"{}\"  ·  {meta}", sanitize(&doc.query));
        }
        self.block(&meta, 8.5, Weight::Italic, muted(), 0.0, 0.0, 4.8);
        self.spacer(4.0);
        self.rule(self.y);
        self.spacer(2.0);

        // Key findings
        self.section_header("KEY FINDINGS");
        for finding in &doc.findings {
            let text = format!("- {} [{}]", finding.text, finding.source);
            self.block(&text, 9.5, Weight::Regular, ink(), 0.0, 5.0, 5.2);
            self.spacer(2.5);
        }

        // Source breakdown
        self.section_header("SOURCE BREAKDOWN");
        for source in &doc.sources {
            let head = format!("[{}] {}", source.index, source.title);
            // Reserve room for the heading + meta so they don't orphan.
            self.ensure(24.0);
            self.block(&head, 10.5, Weight::Bold, ink(), 0.0, 7.0, 6.0);

            let meta = format!(
                "Grade {}  ·  authority {:.2}  ·  {}  ·  crawled {}",
                source.grade,
                source.authority,
                source.host.as_deref().unwrap_or("-"),
                source.crawled_at
            );
            self.block(&meta, 8.0, Weight::Regular, muted(), 7.0, 0.0, 4.6);

            let url = wrap(&sanitize(&source.url), 7.5, CONTENT_W - 7.0, false);
            for line in url {
                self.ensure(4.5);
                self.text(&line, 7.5, MARGIN_L + 7.0, self.y - 3.8, Weight::Italic, accent());
                self.y -= 4.4;
            }
            self.spacer(2.0);

            for sentence in &source.sentences {
                let text = format!("- {}", sentence);
                self.block(&text, 9.5, Weight::Regular, ink(), 7.0, 4.5, 5.2);
                self.spacer(1.8);
            }
            self.spacer(4.0);
        }

        // References
        self.section_header("REFERENCES");
        for source in &doc.sources {
            let text = format!(
                "[{}] {}. {}",
                source.index,
                source.title,
                source.url
            );
            self.block(&text, 8.5, Weight::Regular, ink(), 0.0, 6.0, 4.8);
            self.spacer(2.5);
        }
    }
}

// --- text metrics & wrapping ------------------------------------------------

/// Approximate width of a character in em units (Helvetica).
fn char_em(c: char) -> f32 {
    match c {
        ' ' => 0.278,
        '!' | '"' | '\'' | ',' | '.' | ':' | ';' | '|' | '`' => 0.278,
        '(' | ')' | '[' | ']' | '{' | '}' => 0.333,
        '-' | '/' => 0.278,
        '_' => 0.556,
        '+' | '=' | '<' | '>' | '~' => 0.584,
        '*' => 0.389,
        '#' | '$' | '%' => 0.556,
        '&' => 0.667,
        '@' => 1.015,
        '?' => 0.556,
        '0'..='9' => 0.556,
        // Wide letters (before the general ranges)
        'm' | 'M' => 0.833,
        'w' => 0.722,
        'W' => 0.944,
        'A'..='Z' => 0.667,
        'a'..='z' => 0.556,
        _ => 0.55,
    }
}

pub fn text_width_mm(s: &str, size_pt: f32, bold: bool) -> f32 {
    let em: f32 = s.chars().map(char_em).sum();
    let factor = if bold { 1.06 } else { 1.0 };
    em * factor * size_pt * PT_TO_MM
}

/// Greedy word wrap; hard-splits tokens that are longer than the column
/// (long URLs, identifiers).
pub fn wrap(text: &str, size_pt: f32, max_w_mm: f32, bold: bool) -> Vec<String> {
    let fits = |s: &str| text_width_mm(s, size_pt, bold) <= max_w_mm;

    // Stage 1: turn the text into atoms — whole words, or hard-split chunks
    // of overlong words (glued together without spaces).
    struct Atom {
        text: String,
        glue: bool,
    }
    let mut atoms: Vec<Atom> = Vec::new();
    for word in text.split_whitespace() {
        if fits(word) {
            atoms.push(Atom {
                text: word.to_string(),
                glue: false,
            });
            continue;
        }
        let mut rest = word;
        let mut first = true;
        while !rest.is_empty() {
            let mut end = 0;
            for (i, ch) in rest.char_indices() {
                let next = i + ch.len_utf8();
                if text_width_mm(&rest[..next], size_pt, bold) > max_w_mm && i > 0 {
                    break;
                }
                end = next;
            }
            if end == 0 {
                // Single character wider than the column: take it anyway.
                end = rest.chars().next().map_or(0, |c| c.len_utf8());
            }
            atoms.push(Atom {
                text: rest[..end].to_string(),
                glue: !first,
            });
            rest = &rest[end..];
            first = false;
        }
    }

    // Stage 2: fill lines.
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for atom in atoms {
        if line.is_empty() {
            line = atom.text;
            continue;
        }
        let candidate = if atom.glue {
            format!("{line}{}", atom.text)
        } else {
            format!("{line} {}", atom.text)
        };
        if fits(&candidate) {
            line = candidate;
        } else {
            lines.push(std::mem::take(&mut line));
            line = atom.text;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Maps typographic characters to ASCII so the built-in Helvetica fonts
/// (WinAnsi) render them correctly; anything else becomes '?'.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '–' | '—' | '−' => out.push('-'),
            '‘' | '’' | '‚' | '‛' => out.push('\''),
            '“' | '”' | '„' => out.push('"'),
            '…' => out.push_str("..."),
            '•' => out.push_str("- "),
            '\u{a0}' | '\u{2000}'..='\u{200b}' | '\u{202f}' => out.push(' '),
            '\t' => out.push(' '),
            '\r' => {}
            c if (c as u32) < 128 && !c.is_control() => out.push(c),
            // Common Latin-1 accents → ASCII approximations.
            'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => out.push('A'),
            'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => out.push('a'),
            'È' | 'É' | 'Ê' | 'Ë' => out.push('E'),
            'è' | 'é' | 'ê' | 'ë' => out.push('e'),
            'Ì' | 'Í' | 'Î' | 'Ï' => out.push('I'),
            'ì' | 'í' | 'î' | 'ï' => out.push('i'),
            'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => out.push('O'),
            'ò' | 'ó' | 'ô' | 'õ' | 'ö' => out.push('o'),
            'Ù' | 'Ú' | 'Û' | 'Ü' => out.push('U'),
            'ù' | 'ú' | 'û' | 'ü' => out.push('u'),
            'Ç' => out.push('C'),
            'ç' => out.push('c'),
            'Ñ' => out.push('N'),
            'ñ' => out.push('n'),
            'ß' => out.push_str("ss"),
            'Æ' => out.push_str("AE"),
            'æ' => out.push_str("ae"),
            'Ø' | 'ø' => out.push('O'),
            '°' => out.push_str(" deg"),
            '×' => out.push('x'),
            '±' => out.push('+'),
            '·' => out.push('-'),
            _ => out.push('?'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarize::{Finding, SourceSection, SummaryStats};

    fn fixture_doc() -> SummaryDoc {
        SummaryDoc {
            title: "Photosynthesis".into(),
            query: "photosynthesis".into(),
            generated_at: "2026-11-25 12:00".into(),
            stats: SummaryStats {
                pages_analyzed: 2,
                sentences_considered: 40,
                sentences_selected: 3,
            },
            findings: vec![
                Finding {
                    text: "Chlorophyll absorbs photons at wavelengths near 430 and 662 nanometres.".into(),
                    source: 1,
                },
                Finding {
                    text: "The Calvin cycle fixes carbon dioxide into glyceraldehyde 3-phosphate.".into(),
                    source: 2,
                },
            ],
            sources: vec![
                SourceSection {
                    index: 1,
                    page_id: 1,
                    url: "https://stanford.edu/entries/photosynthesis".into(),
                    host: Some("stanford.edu".into()),
                    title: "Photosynthesis — Stanford Encyclopedia".into(),
                    grade: "A",
                    authority: 0.95,
                    crawled_at: "2026-11-25 10:00:00".into(),
                    sentences: vec![
                        "Photosynthesis converts light energy into chemical energy.".into(),
                        "The light-dependent reactions occur in the thylakoid membrane (see [1], [2], [3] for details on the electron transport chain).".into(),
                    ],
                },
                SourceSection {
                    index: 2,
                    page_id: 2,
                    url: "https://doi.org/10.1038/nature12373".into(),
                    host: Some("doi.org".into()),
                    title: "A very long title about the Calvin cycle and carbon fixation mechanisms in plants".into(),
                    grade: "A",
                    authority: 0.97,
                    crawled_at: "2026-11-25 10:00:01".into(),
                    sentences: vec![
                        "Researchers measured a quantum yield of 0.125 under controlled conditions.".into(),
                    ],
                },
            ],
        }
    }

    #[test]
    fn wrap_respects_column_width() {
        let text = "The quick brown fox jumps over the lazy dog while \
                    researching photosynthesis under controlled laboratory conditions daily.";
        let lines = wrap(text, 9.5, 120.0, false);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(text_width_mm(l, 9.5, false) <= 120.5, "line too wide: {l}");
        }
        // No words lost.
        let rejoined = lines.join(" ");
        assert_eq!(
            rejoined.split_whitespace().count(),
            text.split_whitespace().count()
        );
    }

    #[test]
    fn wrap_hard_splits_overlong_tokens() {
        let url = "https://example.org/very/long/path/that/never/fits/in/a/narrow/column/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let lines = wrap(url, 7.5, 60.0, false);
        assert!(lines.len() > 1);
        for l in &lines {
            assert!(text_width_mm(l, 7.5, false) <= 60.5, "line too wide: {l}");
        }
        assert_eq!(
            lines.concat(),
            url.chars().filter(|c| !c.is_whitespace()).collect::<String>()
        );
    }

    #[test]
    fn wrap_handles_empty_input() {
        assert_eq!(wrap("", 10.0, 100.0, false), vec![String::new()]);
    }

    #[test]
    fn sanitize_replaces_typography() {
        assert_eq!(sanitize("“smart” – quotes…"), "\"smart\" - quotes...");
        assert_eq!(sanitize("café naïve"), "cafe naive");
        assert_eq!(sanitize("plain ascii"), "plain ascii");
    }

    #[test]
    fn renders_valid_pdf() {
        let doc = fixture_doc();
        let bytes = render_summary(&doc).expect("render");
        assert!(bytes.starts_with(b"%PDF"), "missing PDF header");
        assert!(bytes.len() > 2_000, "PDF suspiciously small: {} bytes", bytes.len());
        // Trailer must be present.
        assert!(bytes.windows(5).any(|w| w == b"%%EOF"));
    }

    #[test]
    fn renders_multi_page_summary_without_panic() {
        let mut doc = fixture_doc();
        // Many sources → multiple pages.
        doc.sources = (0..30)
            .map(|i| SourceSection {
                index: i + 1,
                page_id: i as i64,
                url: format!("https://example.org/entry/{i}"),
                host: Some("example.org".into()),
                title: format!("Source entry number {i} with a reasonably long descriptive title"),
                grade: "B",
                authority: 0.7,
                crawled_at: "2026-11-25 10:00:00".into(),
                sentences: (0..4)
                    .map(|j| format!("Sentence {j} from source {i} contains enough words to wrap across lines nicely ok."))
                    .collect(),
            })
            .collect();
        doc.findings = (0..25)
            .map(|i| Finding {
                text: format!("Finding {i}: a sufficiently long sentence that will wrap over multiple lines in the narrow column."),
                source: (i % 30) + 1,
            })
            .collect();
        let bytes = render_summary(&doc).expect("render");
        assert!(bytes.starts_with(b"%PDF"));
        // Two passes over ~30 sources must produce several pages of content.
        assert!(bytes.len() > 10_000);
    }

    #[test]
    fn unicode_content_does_not_panic() {
        let mut doc = fixture_doc();
        doc.title = "Ünïcödé “tëst” — café & naïve".into();
        doc.findings[0].text = "Élan vital, Straße, Ω ≈ 3.14, 中文.".into();
        let bytes = render_summary(&doc).expect("render");
        assert!(bytes.starts_with(b"%PDF"));
    }
}
