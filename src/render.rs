//! Renders the design package and the review pack (Markdown) as self-contained HTML.
//!
//! - **Marp decks** (`marp: true` front matter, slides split by `---`, speaker notes in
//!   HTML comments) become slide shows: keyboard navigation, speaker notes, all-slides
//!   view, and print to PDF (one slide per page). The same files open in the Marp
//!   tools (`npx @marp-team/marp-cli deck.md --pptx` makes a PowerPoint).
//! - **Other documents** become plain pages; Mermaid diagrams are shown as their source
//!   (GitHub and VS Code draw them).
//!
//! Models and peers write these files, so raw HTML in them is shown as text, never
//! run, and links can't use `javascript:` and similar schemes.

use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};

/// One slide of a deck: its Markdown and speaker notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slide {
    pub markdown: String,
    pub notes: Vec<String>,
}

/// Marp directives, which are HTML comments too but not speaker notes.
const DIRECTIVES: &[&str] = &[
    "theme",
    "paginate",
    "header",
    "footer",
    "class",
    "backgroundColor",
    "backgroundImage",
    "backgroundPosition",
    "backgroundRepeat",
    "backgroundSize",
    "color",
    "size",
    "style",
    "headingDivider",
    "math",
    "lang",
    "title",
    "description",
    "author",
    "transition",
];

fn is_directive(comment: &str) -> bool {
    !comment.contains('\n')
        && comment.split_once(':').is_some_and(|(key, _)| {
            let key = key.trim().trim_start_matches('_');
            DIRECTIVES.contains(&key)
        })
}

/// Splits HTML comments (speaker notes, unless Marp directives) from Markdown.
pub fn split_notes(text: &str) -> Slide {
    let (mut markdown, mut notes, mut rest) = (String::new(), Vec::new(), text);
    while let Some(start) = rest.find("<!--") {
        markdown.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let (comment, next) = match after.find("-->") {
            Some(end) => (&after[..end], &after[end + 3..]),
            None => (after, ""),
        };
        let comment = comment.trim();
        if !comment.is_empty() && !is_directive(comment) {
            notes.push(comment.to_owned());
        }
        rest = next;
    }
    markdown.push_str(rest);
    Slide { markdown, notes }
}

/// Lines outside fenced code blocks, and the number of code blocks.
fn outside_code(markdown: &str) -> (Vec<&str>, usize) {
    let (mut lines, mut blocks, mut fence) = (Vec::new(), 0, None::<&str>);
    for line in markdown.lines() {
        let t = line.trim_start();
        match fence {
            Some(f) if t.starts_with(f) => fence = None,
            Some(_) => {}
            None if t.starts_with("```") || t.starts_with("~~~") => {
                fence = Some(&t[..3]);
                blocks += 1;
            }
            None => lines.push(line),
        }
    }
    (lines, blocks)
}

impl Slide {
    /// The slide's first heading.
    pub fn title(&self) -> Option<String> {
        outside_code(&self.markdown)
            .0
            .into_iter()
            .map(str::trim_start)
            .find(|l| l.starts_with('#'))
            .map(|l| l.trim_start_matches('#').trim().to_owned())
            .filter(|t| !t.is_empty())
    }

    /// Words on the slide (speaker notes and code left out).
    pub fn words(&self) -> usize {
        outside_code(&self.markdown)
            .0
            .iter()
            .flat_map(|l| l.split_whitespace())
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .count()
    }

    pub fn code_blocks(&self) -> usize {
        outside_code(&self.markdown).1
    }
}

/// The slides of a Marp deck, or None if `text` isn't one (no `marp: true` front
/// matter).
pub fn deck(text: &str) -> Option<Vec<Slide>> {
    let text = text.replace("\r\n", "\n");
    let mut lines = text.lines().skip_while(|l| l.trim().is_empty());
    if lines.next()?.trim() != "---" {
        return None;
    }
    let mut marp = false;
    for line in lines.by_ref() {
        let l = line.trim();
        if l == "---" {
            break;
        }
        marp |= l.replace(' ', "").eq_ignore_ascii_case("marp:true");
    }
    if !marp {
        return None;
    }
    let (mut slides, mut current, mut fence) = (Vec::new(), String::new(), None::<&str>);
    for line in lines {
        let t = line.trim_start();
        match fence {
            Some(f) if t.starts_with(f) => fence = None,
            Some(_) => {}
            None if t.starts_with("```") || t.starts_with("~~~") => fence = Some(&t[..3]),
            None if line.trim() == "---" => {
                slides.push(std::mem::take(&mut current));
                continue;
            }
            None => {}
        }
        current.push_str(line);
        current.push('\n');
    }
    slides.push(current);
    Some(
        slides
            .iter()
            .filter(|s| !s.trim().is_empty())
            .map(|s| split_notes(s))
            .collect(),
    )
}

/// `url` if it is safe to follow (http(s), mailto, relative, or `data:` for images);
/// links to Markdown files point to their rendered pages.
fn safe_url(url: CowStr<'_>, image: bool) -> CowStr<'_> {
    let lower = url.trim().to_ascii_lowercase();
    match lower.split_once(':').map(|(scheme, _)| scheme) {
        Some(scheme) if !scheme.contains(['/', '#', '?']) => {
            if matches!(scheme, "http" | "https" | "mailto") || (image && scheme == "data") {
                url
            } else {
                CowStr::Borrowed("#")
            }
        }
        _ => {
            let (path, fragment) = url.split_once('#').unwrap_or((&url, ""));
            match path.strip_suffix(".md") {
                Some(stem) if fragment.is_empty() => format!("{stem}.html").into(),
                Some(stem) => format!("{stem}.html#{fragment}").into(),
                None => url,
            }
        }
    }
}

/// Markdown to HTML: raw HTML is shown as text, Mermaid diagrams as their source.
pub fn markdown_html(markdown: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut in_diagram = false;
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        Event::Html(html) | Event::InlineHtml(html) => Event::Text(html),
        Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(lang)))
            if lang.trim().eq_ignore_ascii_case("mermaid") =>
        {
            in_diagram = true;
            Event::Html(CowStr::Borrowed(
                "<figure class=\"diagram\"><figcaption>Diagram (Mermaid source; GitHub and VS Code draw it)</figcaption><pre><code>",
            ))
        }
        Event::End(TagEnd::CodeBlock) if in_diagram => {
            in_diagram = false;
            Event::Html(CowStr::Borrowed("</code></pre></figure>\n"))
        }
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url, false),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_url(dest_url, true),
            title,
            id,
        }),
        other => other,
    });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    html
}

/// No network, no plugins: only this page's own inline style and script, and inline
/// images.
const PAGE_CSP: &str =
    "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:";

const BASE_CSS: &str = r#"
:root { color-scheme: light dark; --bg: #f4f5f7; --card: #ffffff; --fg: #1b1d21; --muted: #5a616d; --accent: #2457d6; --line: #d9dce3; --code: #eef0f4; }
@media (prefers-color-scheme: dark) { :root { --bg: #111317; --card: #1b1e24; --fg: #eceef2; --muted: #a6acb8; --accent: #8fb0ff; --line: #343943; --code: #252a31; } }
* { box-sizing: border-box; }
html, body { margin: 0; background: var(--bg); color: var(--fg); font: 17px/1.5 system-ui, -apple-system, "Segoe UI", Roboto, sans-serif; }
a { color: var(--accent); }
:focus-visible { outline: 3px solid var(--accent); outline-offset: 2px; }
code { font-family: ui-monospace, "Cascadia Code", Consolas, monospace; font-size: .9em; background: var(--code); padding: .1em .35em; border-radius: 4px; }
pre { background: var(--code); padding: .8em 1em; border-radius: 8px; overflow: auto; }
pre code { background: none; padding: 0; }
table { border-collapse: collapse; width: 100%; margin: .6em 0; font-size: .92em; }
th, td { border: 1px solid var(--line); padding: .35em .6em; text-align: left; vertical-align: top; }
th { background: var(--code); }
figure.diagram { margin: .8em 0; }
figure.diagram figcaption { font-size: .8em; color: var(--muted); margin-bottom: .3em; }
blockquote { margin: .6em 0; padding: .2em 1em; border-left: 4px solid var(--accent); color: var(--muted); }
"#;

const DOC_CSS: &str = r#"
header { max-width: 60rem; margin: 0 auto; padding: 1rem 1.5rem 0; font-size: .9em; }
main.doc { max-width: 60rem; margin: 1rem auto 3rem; padding: 2rem 2.5rem; background: var(--card); border: 1px solid var(--line); border-radius: 12px; }
main.doc h1 { margin-top: 0; }
@media print { header { display: none; } main.doc { border: 0; margin: 0; max-width: none; } }
"#;

const DECK_CSS: &str = r#"
#deck { height: calc(100vh - 56px); display: grid; place-items: center; padding: 16px; }
.slide { display: none; position: relative; container-type: inline-size; width: min(100%, calc((100vh - 88px) * 16 / 9)); aspect-ratio: 16 / 9; background: var(--card); border: 1px solid var(--line); border-radius: 12px; box-shadow: 0 6px 24px rgb(0 0 0 / .12); overflow: auto; }
.slide.current { display: block; }
.slide .content { padding: 4.5cqi 6cqi 5cqi; font-size: 2.3cqi; }
.slide h1 { font-size: 2.1em; margin: 0 0 .35em; color: var(--accent); line-height: 1.15; }
.slide.title .content { height: 100%; display: flex; flex-direction: column; justify-content: center; }
.slide.title h1 { font-size: 2.8em; }
.slide.title h2 { color: var(--muted); font-weight: 500; }
.slide h2 { font-size: 1.5em; margin: 0 0 .5em; line-height: 1.2; }
.slide h3 { font-size: 1.15em; margin: .6em 0 .3em; }
.slide li + li { margin-top: .25em; }
.page { position: absolute; right: 2.5cqi; bottom: 1.6cqi; color: var(--muted); font-size: 1.4cqi; }
.notes { display: none; }
body.show-notes .notes { display: block; margin: 0 6cqi 5cqi; padding-top: .6em; border-top: 1px dashed var(--line); color: var(--muted); font-size: 1.7cqi; }
body.show-notes .notes::before { content: "Speaker notes"; display: block; font-weight: 600; margin-bottom: .3em; }
body.all #deck { display: block; height: auto; padding-bottom: 72px; }
body.all .slide { display: block; margin: 0 auto 24px; }
.bar { position: fixed; left: 0; right: 0; bottom: 0; height: 56px; display: flex; gap: 8px; align-items: center; justify-content: center; background: var(--card); border-top: 1px solid var(--line); }
.bar button { font: inherit; font-size: 15px; padding: 6px 12px; border-radius: 8px; border: 1px solid var(--line); background: var(--bg); color: var(--fg); cursor: pointer; }
.bar button[aria-pressed="true"] { background: var(--accent); border-color: var(--accent); color: var(--card); }
.bar .where { min-width: 5em; text-align: center; color: var(--muted); font-size: 15px; }
.home { position: absolute; left: 16px; font-size: 15px; }
@media print {
  @page { size: 13.333in 7.5in; margin: 0; }
  body { background: #fff; }
  .bar { display: none; }
  #deck { display: block; height: auto; padding: 0; }
  .slide { display: block !important; width: 13.333in; height: 7.5in; aspect-ratio: auto; border: 0; border-radius: 0; box-shadow: none; overflow: hidden; break-after: page; }
}
"#;

const DECK_JS: &str = r#"(() => {
  const slides = [...document.querySelectorAll(".slide")];
  const where = document.getElementById("where");
  const notes = document.getElementById("notes-btn");
  const all = document.getElementById("all-btn");
  let i = Math.min(Math.max((parseInt(location.hash.slice(1), 10) || 1) - 1, 0), slides.length - 1);
  const show = (n) => {
    i = Math.min(Math.max(n, 0), slides.length - 1);
    slides.forEach((s, k) => s.classList.toggle("current", k === i));
    where.textContent = `${i + 1} / ${slides.length}`;
    try { history.replaceState(null, "", `#${i + 1}`); } catch { /* opaque origin */ }
  };
  const toggle = (cls, button) => button.setAttribute("aria-pressed", String(document.body.classList.toggle(cls)));
  document.querySelectorAll("[data-go]").forEach((b) => b.addEventListener("click", () => show(i + Number(b.dataset.go))));
  notes.addEventListener("click", () => toggle("show-notes", notes));
  all.addEventListener("click", () => toggle("all", all));
  document.getElementById("print-btn").addEventListener("click", () => window.print());
  document.addEventListener("keydown", (e) => {
    if (e.altKey || e.ctrlKey || e.metaKey) return;
    if (e.target instanceof HTMLButtonElement && (e.key === " " || e.key === "Enter")) return;
    if (["ArrowRight", "PageDown", " "].includes(e.key)) { e.preventDefault(); show(i + 1); }
    else if (["ArrowLeft", "PageUp"].includes(e.key)) { e.preventDefault(); show(i - 1); }
    else if (e.key === "Home") show(0);
    else if (e.key === "End") show(slides.length - 1);
    else if (e.key.toLowerCase() === "n") toggle("show-notes", notes);
    else if (e.key.toLowerCase() === "a") toggle("all", all);
  });
  show(i);
})();"#;

fn page(title: &str, css: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<meta http-equiv=\"Content-Security-Policy\" content=\"{PAGE_CSP}\">\n<title>{}</title>\n<style>{BASE_CSS}{css}</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        crate::spec::escape_html(title)
    )
}

/// A deck as a slide show; `home` links back to the list of documents.
pub fn deck_html(title: &str, slides: &[Slide], home: &str) -> String {
    let count = slides.len();
    let mut body = String::from("<main id=\"deck\">\n");
    for (i, slide) in slides.iter().enumerate() {
        body += &format!(
            "<section class=\"slide{}\" aria-label=\"Slide {} of {count}\"><div class=\"content\">{}</div>",
            if i == 0 { " title" } else { "" },
            i + 1,
            markdown_html(&slide.markdown)
        );
        if !slide.notes.is_empty() {
            body += &format!(
                "<aside class=\"notes\">{}</aside>",
                markdown_html(&slide.notes.join("\n\n"))
            );
        }
        body += &format!(
            "<footer class=\"page\">{} / {count}</footer></section>\n",
            i + 1
        );
    }
    body += &format!(
        "</main>\n<nav class=\"bar\" aria-label=\"Slides\">\
        <a class=\"home\" href=\"{}\">All documents</a>\
        <button type=\"button\" data-go=\"-1\" aria-label=\"Previous slide\">&#8249;</button>\
        <span class=\"where\" id=\"where\" aria-live=\"polite\"></span>\
        <button type=\"button\" data-go=\"1\" aria-label=\"Next slide\">&#8250;</button>\
        <button type=\"button\" id=\"notes-btn\" aria-pressed=\"false\" title=\"N\">Speaker notes</button>\
        <button type=\"button\" id=\"all-btn\" aria-pressed=\"false\" title=\"A\">All slides</button>\
        <button type=\"button\" id=\"print-btn\">Print / PDF</button></nav>\n",
        crate::spec::escape_html(home)
    );
    body += &format!("<script>{DECK_JS}</script>");
    page(title, DECK_CSS, &body)
}

/// A document as a page; `home` links back to the list of documents.
pub fn doc_html(title: &str, markdown: &str, home: &str) -> String {
    let body = format!(
        "<header><a href=\"{}\">All documents</a></header>\n<main class=\"doc\">\n{}</main>",
        crate::spec::escape_html(home),
        markdown_html(&split_notes(markdown).markdown)
    );
    page(title, DOC_CSS, &body)
}

/// The title of a Markdown document or deck: its first heading.
pub fn title_of(markdown: &str) -> Option<String> {
    match deck(markdown) {
        Some(slides) => slides.first().and_then(Slide::title),
        None => split_notes(markdown).title(),
    }
}

/// A landing page: groups of (label, link).
pub fn index_html(title: &str, groups: &[(&str, Vec<(String, String)>)]) -> String {
    let mut body = format!(
        "<main class=\"doc\">\n<h1>{}</h1>\n",
        crate::spec::escape_html(title)
    );
    for (heading, links) in groups.iter().filter(|(_, l)| !l.is_empty()) {
        body += &format!("<h2>{}</h2>\n<ul>\n", crate::spec::escape_html(heading));
        for (label, href) in links {
            body += &format!(
                "<li><a href=\"{}\">{}</a></li>\n",
                crate::spec::escape_html(href),
                crate::spec::escape_html(label)
            );
        }
        body += "</ul>\n";
    }
    body += "</main>";
    page(title, DOC_CSS, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DECK: &str = "---\nmarp: true\npaginate: true\n---\n\n# Wishlist\n## Leadership review\n<!-- _class: lead -->\n<!-- Open with the customer quote. -->\n\n---\n\n## Summary\n- Ship in Q3\n\n```text\n---\nnot a slide break\n```\n\n---\n\n## Ask\nApprove the plan.\n";

    #[test]
    fn decks_render_to_self_contained_slides_with_notes() {
        let slides = deck(DECK).unwrap();
        assert_eq!(slides.len(), 3, "--- inside code is not a slide break");
        assert_eq!(slides[0].title().as_deref(), Some("Wishlist"));
        assert_eq!(
            slides[0].notes,
            ["Open with the customer quote."],
            "directives aren't notes"
        );
        assert_eq!(slides[1].code_blocks(), 1);
        assert_eq!(slides[2].words(), 4);
        assert!(deck("# Not a deck\n").is_none());
        assert!(deck("---\ntitle: x\n---\n# No marp\n").is_none());

        let html = deck_html("Wishlist", &slides, "../index.html");
        assert_eq!(html.matches("<section class=\"slide").count(), 3);
        assert!(html.contains("<aside class=\"notes\"><p>Open with the customer quote.</p>"));
        assert!(html.contains("Content-Security-Policy\" content=\"default-src 'none'"));
        assert!(html.contains("@media print"));
        assert!(!html.contains("_class"));
    }

    #[test]
    fn markdown_is_rendered_without_raw_html_or_script_links() {
        let html = markdown_html(
            "# Doc\n<script>alert(1)</script>\n\n<img src=x onerror=alert(1)>\n\n[bad](javascript:alert(1)) [web](https://example.com) [arch](../design/architecture.md#context)\n\n```mermaid\nflowchart LR\n  a --> b\n```\n\n| A | B |\n| --- | --- |\n| 1 | 2 |\n",
        );
        assert!(
            !html.contains("<script>") && html.contains("&lt;script&gt;"),
            "{html}"
        );
        assert!(!html.contains("<img"), "{html}");
        assert!(
            html.contains("href=\"#\"") && !html.contains("javascript:"),
            "{html}"
        );
        assert!(html.contains("href=\"https://example.com\""));
        assert!(html.contains("href=\"../design/architecture.html#context\""));
        assert!(html.contains("<figure class=\"diagram\">") && html.contains("a --&gt; b"));
        assert!(html.contains("<table>"));
        let page = doc_html("Doc", "# Doc\n<!-- a note -->\ntext\n", "index.html");
        assert!(!page.contains("a note"));
        assert_eq!(title_of(DECK).as_deref(), Some("Wishlist"));
        assert_eq!(
            title_of("intro\n# Architecture\n").as_deref(),
            Some("Architecture")
        );
    }
}
