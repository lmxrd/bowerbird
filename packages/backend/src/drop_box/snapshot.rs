//! A page saved for good (docs/M2_DESIGN.md §8b): a self-contained HTML
//! file, its images, styles and fonts inlined as `data:` URIs, and a
//! Markdown copy of its text.
//!
//! Everything the page needs is fetched through the link's guarded client
//! (`link.rs`: public addresses only, unless `LINKS_ALLOW_PRIVATE`), within
//! limits. Scripts, frames, embedded objects and event handlers are dropped,
//! links made absolute, and a Content-Security-Policy added, so that the
//! saved page loads nothing from the network when it is opened. What could
//! not be fetched is left out, not left pointing at the web.

use super::extract;
use super::link::{self, Fetched};
use lol_html::html_content::{ContentType, Element};
use lol_html::{RewriteStrSettings, element, rewrite_str, text};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

/// The most resources (images, stylesheets, fonts) fetched for one page.
const MAX_RESOURCES: usize = 80;
/// The largest resource.
const MAX_RESOURCE_BYTES: usize = 5_000_000;
/// The most the resources of one page may weigh together.
const MAX_TOTAL_BYTES: usize = 30_000_000;
/// The most stylesheets one stylesheet may import (one level).
const MAX_IMPORTS: usize = 10;

/// What a page is saved as.
#[derive(Debug)]
pub struct Snapshot {
    pub html: String,
    pub markdown: String,
}

/// What the page refers to, found in a first pass.
#[derive(Default)]
struct Found {
    base: Option<String>,
    /// Images, icons, posters: inlined as they are.
    files: Vec<String>,
    /// Stylesheets: fetched, their own references inlined.
    sheets: Vec<String>,
    /// The text of each `<style>` element, in order.
    styles: Vec<String>,
    /// `style` attributes.
    inline_styles: Vec<String>,
}

/// The URLs a stylesheet refers to: `url(...)` and `@import "..."`.
pub fn css_references(css: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = css.to_ascii_lowercase();
    let mut at = 0;
    while let Some(i) = lower[at..].find("url(") {
        let start = at + i + 4;
        let Some(end) = css[start..].find(')') else {
            break;
        };
        let raw = css[start..start + end]
            .trim()
            .trim_matches(['"', '\''])
            .trim();
        if !raw.is_empty() && !raw.starts_with("data:") && !raw.starts_with('#') {
            out.push(raw.to_string());
        }
        at = start + end;
    }
    let mut at = 0;
    while let Some(i) = lower[at..].find("@import") {
        let rest = css[at + i + 7..].trim_start();
        if let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'')
            && let Some(end) = rest[1..].find(quote)
        {
            out.push(rest[1..1 + end].to_string());
        }
        at += i + 7;
    }
    out
}

/// A stylesheet with its references replaced: by their `data:` URI, or by
/// nothing when they could not be fetched. `@import`s are replaced by the
/// imported sheet (already rewritten) when there is one.
pub fn rewrite_css(
    css: &str,
    base: &reqwest::Url,
    inlined: &HashMap<String, String>,
    imports: &HashMap<String, String>,
) -> String {
    let resolve = |raw: &str| base.join(raw).ok().map(|u| u.to_string());
    let mut out = String::with_capacity(css.len());
    let lower = css.to_ascii_lowercase();
    let mut at = 0;
    while at < css.len() {
        let next_url = lower[at..].find("url(").map(|i| at + i);
        let next_import = lower[at..].find("@import").map(|i| at + i);
        match (next_url, next_import) {
            (Some(u), i) if i.is_none_or(|i| u < i) => {
                out.push_str(&css[at..u]);
                let start = u + 4;
                let Some(end) = css[start..].find(')').map(|e| start + e) else {
                    out.push_str(&css[u..]);
                    break;
                };
                let raw = css[start..end].trim().trim_matches(['"', '\'']).trim();
                let replacement = if raw.starts_with("data:") || raw.starts_with('#') {
                    raw.to_string()
                } else {
                    resolve(raw)
                        .and_then(|url| inlined.get(&url).cloned())
                        .unwrap_or_default()
                };
                out.push_str(&format!("url(\"{replacement}\")"));
                at = end + 1;
            }
            (_, Some(i)) => {
                out.push_str(&css[at..i]);
                // The whole rule, to its `;`.
                let end = css[i..].find(';').map(|e| i + e + 1).unwrap_or(css.len());
                let rule = &css[i..end];
                let imported = css_references(rule)
                    .first()
                    .and_then(|raw| resolve(raw))
                    .and_then(|url| imports.get(&url).cloned())
                    .unwrap_or_default();
                out.push_str(&imported);
                at = end;
            }
            _ => {
                out.push_str(&css[at..]);
                break;
            }
        }
    }
    out
}

/// What is fetched for a page, with the limits.
struct Fetcher {
    allow_private: bool,
    total: usize,
    count: usize,
}

impl Fetcher {
    async fn get(&mut self, url: &str) -> Option<Fetched> {
        if self.count >= MAX_RESOURCES || self.total >= MAX_TOTAL_BYTES {
            return None;
        }
        self.count += 1;
        let max = MAX_RESOURCE_BYTES.min(MAX_TOTAL_BYTES - self.total);
        match link::fetch(url, max, self.allow_private).await {
            Ok(fetched) => {
                self.total += fetched.bytes.len();
                Some(fetched)
            }
            Err(e) => {
                tracing::debug!("snapshot: {url} left out: {e}");
                None
            }
        }
    }
}

fn data_uri(fetched: &Fetched, url: &str) -> String {
    let mime = fetched
        .mime_type
        .clone()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| super::mime_type(url));
    format!(
        "data:{mime};base64,{}",
        crate::util::base64_encode(&fetched.bytes)
    )
}

/// Save a fetched page.
pub async fn snapshot(page: &Fetched, title: Option<&str>, allow_private: bool) -> Snapshot {
    let html = String::from_utf8_lossy(&page.bytes).into_owned();
    let page_url = reqwest::Url::parse(&page.url).ok();
    let found = find(&html);
    let base = found
        .base
        .as_deref()
        .and_then(|b| page_url.as_ref().and_then(|u| u.join(b).ok()))
        .or(page_url.clone());

    let mut inlined: HashMap<String, String> = HashMap::new();
    let mut sheets: HashMap<String, String> = HashMap::new();
    if let Some(base) = &base {
        let mut fetcher = Fetcher {
            allow_private,
            total: 0,
            count: 0,
        };
        let resolve = |raw: &str, base: &reqwest::Url| {
            base.join(raw)
                .ok()
                .filter(|u| matches!(u.scheme(), "http" | "https"))
                .map(|u| u.to_string())
        };
        // Stylesheets first: theirs are the references with the most weight.
        let mut sheet_texts: Vec<(String, String)> = Vec::new();
        for raw in &found.sheets {
            let Some(url) = resolve(raw, base) else {
                continue;
            };
            if sheet_texts.iter().any(|(u, _)| *u == url) {
                continue;
            }
            if let Some(fetched) = fetcher.get(&url).await {
                sheet_texts.push((url, String::from_utf8_lossy(&fetched.bytes).into_owned()));
            }
        }
        // One level of imports.
        let mut imported: Vec<(String, String)> = Vec::new();
        for (url, css) in &sheet_texts {
            let Ok(sheet_base) = reqwest::Url::parse(url) else {
                continue;
            };
            for raw in css_references(css).into_iter().take(MAX_IMPORTS) {
                let Some(import) = resolve(&raw, &sheet_base) else {
                    continue;
                };
                if !css.to_ascii_lowercase().contains("@import")
                    || !raw.to_ascii_lowercase().ends_with(".css")
                {
                    continue;
                }
                if let Some(fetched) = fetcher.get(&import).await {
                    imported.push((import, String::from_utf8_lossy(&fetched.bytes).into_owned()));
                }
            }
        }
        // Every file any of them, or the page, refers to.
        let mut wanted: Vec<String> = Vec::new();
        for raw in &found.files {
            wanted.extend(resolve(raw, base));
        }
        for css in found.styles.iter().chain(&found.inline_styles) {
            for raw in css_references(css) {
                wanted.extend(resolve(&raw, base));
            }
        }
        for (url, css) in sheet_texts.iter().chain(&imported) {
            let Ok(sheet_base) = reqwest::Url::parse(url) else {
                continue;
            };
            for raw in css_references(css) {
                if !raw.to_ascii_lowercase().ends_with(".css") {
                    wanted.extend(resolve(&raw, &sheet_base));
                }
            }
        }
        for url in wanted {
            if inlined.contains_key(&url) {
                continue;
            }
            if let Some(fetched) = fetcher.get(&url).await {
                inlined.insert(url.clone(), data_uri(&fetched, &url));
            }
        }
        // The sheets with their references inlined, imports first.
        let mut imports = HashMap::new();
        for (url, css) in &imported {
            if let Ok(sheet_base) = reqwest::Url::parse(url) {
                imports.insert(
                    url.clone(),
                    rewrite_css(css, &sheet_base, &inlined, &HashMap::new()),
                );
            }
        }
        for (url, css) in &sheet_texts {
            if let Ok(sheet_base) = reqwest::Url::parse(url) {
                sheets.insert(
                    url.clone(),
                    rewrite_css(css, &sheet_base, &inlined, &imports),
                );
            }
        }
    }

    let html = match &base {
        Some(base) => rewrite(&html, base, &page.url, &found, &inlined, &sheets),
        None => html,
    };
    let markdown = markdown(page, title).await;
    Snapshot { html, markdown }
}

/// The first pass: what the page refers to.
fn find(html: &str) -> Found {
    let found = Rc::new(RefCell::new(Found::default()));
    let (f1, f2, f3, f4, f5, f6, f7) = (
        found.clone(),
        found.clone(),
        found.clone(),
        found.clone(),
        found.clone(),
        found.clone(),
        found.clone(),
    );
    let handlers = vec![
        element!("base[href]", move |el| {
            let mut f = f1.borrow_mut();
            if f.base.is_none() {
                f.base = el.get_attribute("href");
            }
            Ok(())
        }),
        element!(
            "img[src], video[poster], input[type=image][src]",
            move |el| {
                let attr = if el.tag_name() == "video" {
                    "poster"
                } else {
                    "src"
                };
                if let Some(v) = el.get_attribute(attr) {
                    f2.borrow_mut().files.push(v);
                }
                Ok(())
            }
        ),
        element!("link[href]", move |el| {
            let rel = el.get_attribute("rel").unwrap_or_default().to_lowercase();
            if let Some(href) = el.get_attribute("href") {
                if rel.split_whitespace().any(|r| r == "stylesheet") {
                    f3.borrow_mut().sheets.push(href);
                } else if rel.split_whitespace().any(|r| r == "icon") {
                    f3.borrow_mut().files.push(href);
                }
            }
            Ok(())
        }),
        element!("style", move |_| {
            f4.borrow_mut().styles.push(String::new());
            Ok(())
        }),
        text!("style", move |t| {
            if let Some(last) = f5.borrow_mut().styles.last_mut() {
                last.push_str(t.as_str());
            }
            Ok(())
        }),
        element!("[style]", move |el| {
            if let Some(style) = el.get_attribute("style") {
                f6.borrow_mut().inline_styles.push(style);
            }
            Ok(())
        }),
    ];
    let settings = handlers
        .into_iter()
        .fold(RewriteStrSettings::new(), |s, h| {
            s.append_element_content_handler(h)
        });
    let _ = rewrite_str(html, settings);
    drop(f7);
    Rc::try_unwrap(found)
        .map(RefCell::into_inner)
        .unwrap_or_default()
}

/// The second pass: the page made self-contained.
fn rewrite(
    html: &str,
    base: &reqwest::Url,
    source: &str,
    found: &Found,
    inlined: &HashMap<String, String>,
    sheets: &HashMap<String, String>,
) -> String {
    let resolve = |raw: &str| base.join(raw).ok().map(|u| u.to_string());
    let inline = |raw: &str| -> String {
        if raw.starts_with("data:") {
            return raw.to_string();
        }
        resolve(raw)
            .and_then(|u| inlined.get(&u).cloned())
            .unwrap_or_default()
    };
    let css = |text: &str| rewrite_css(text, base, inlined, &HashMap::new()).replace("</", "<\\/");
    let style_index = RefCell::new(0usize);
    let now = jiff::Timestamp::now().strftime("%Y-%m-%d").to_string();
    let head = format!(
        "<meta charset=\"utf-8\">\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; img-src data:; \
         style-src 'unsafe-inline' data:; font-src data:; media-src data:\">\
         <meta name=\"generator\" content=\"Organizer snapshot of {} on {now}\">",
        escape_attribute(source)
    );
    let handlers = vec![
        // Nothing that runs or loads from elsewhere.
        element!(
            "script, iframe, frame, frameset, object, embed, applet, base, \
                 meta[http-equiv=refresh], meta[http-equiv=Content-Security-Policy], \
                 picture > source, audio > source, video > source, template, portal",
            |el| {
                el.remove();
                Ok(())
            }
        ),
        element!("noscript", |el| {
            el.remove_and_keep_content();
            Ok(())
        }),
        element!("*", |el| {
            let names: Vec<String> = el.attributes().iter().map(|a| a.name()).collect();
            for name in names {
                let value = el.get_attribute(&name).unwrap_or_default();
                if name.starts_with("on")
                    || matches!(name.as_str(), "srcset" | "integrity" | "ping" | "nonce")
                    || value.trim_start().to_lowercase().starts_with("javascript:")
                {
                    el.remove_attribute(&name);
                }
            }
            Ok(())
        }),
        element!("img[src], input[type=image][src]", |el| {
            let src = el.get_attribute("src").unwrap_or_default();
            el.set_attribute("src", &inline(&src))?;
            Ok(())
        }),
        element!("video[poster]", |el| {
            let poster = el.get_attribute("poster").unwrap_or_default();
            el.set_attribute("poster", &inline(&poster))?;
            Ok(())
        }),
        element!("link[href]", |el: &mut Element| {
            let rel = el.get_attribute("rel").unwrap_or_default().to_lowercase();
            let href = el.get_attribute("href").unwrap_or_default();
            if rel.split_whitespace().any(|r| r == "stylesheet") {
                match resolve(&href).and_then(|u| sheets.get(&u)) {
                    Some(sheet) => el.replace(
                        &format!("<style>{}</style>", sheet.replace("</", "<\\/")),
                        ContentType::Html,
                    ),
                    None => el.remove(),
                }
            } else if rel.split_whitespace().any(|r| r == "icon") {
                el.set_attribute("href", &inline(&href))?;
            } else {
                el.remove();
            }
            Ok(())
        }),
        element!("style", |el| {
            let mut i = style_index.borrow_mut();
            let text = found.styles.get(*i).map(|s| css(s)).unwrap_or_default();
            *i += 1;
            el.set_inner_content(&text, ContentType::Html);
            Ok(())
        }),
        element!("[style]", |el| {
            let style = el.get_attribute("style").unwrap_or_default();
            el.set_attribute("style", &css(&style))?;
            Ok(())
        }),
        element!("a[href], area[href]", |el| {
            let href = el.get_attribute("href").unwrap_or_default();
            if !href.starts_with('#')
                && let Some(absolute) = resolve(&href)
            {
                el.set_attribute("href", &absolute)?;
            }
            Ok(())
        }),
        element!("form", |el| {
            el.remove_attribute("action");
            Ok(())
        }),
        element!("head", |el| {
            el.prepend(&head, ContentType::Html);
            Ok(())
        }),
    ];
    let settings = handlers
        .into_iter()
        .fold(RewriteStrSettings::new(), |s, h| {
            s.append_element_content_handler(h)
        });
    match rewrite_str(html, settings) {
        Ok(out) if out.to_lowercase().contains("<head") => out,
        Ok(out) => format!("<!doctype html><html><head>{head}</head><body>{out}</body></html>"),
        // A page lol_html cannot rewrite is not saved as it is: its text,
        // escaped, keeps it harmless.
        Err(e) => {
            tracing::info!("snapshot of {source}: {e}");
            format!(
                "<!doctype html><html><head>{head}</head><body><pre>{}</pre></body></html>",
                escape_text(html)
            )
        }
    }
}

fn escape_attribute(s: &str) -> String {
    escape_text(s).replace('"', "&quot;")
}

fn escape_text(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The page's text as Markdown, under its title and source.
async fn markdown(page: &Fetched, title: Option<&str>) -> String {
    let body = extract::markdown(page.bytes.clone(), "text/html")
        .await
        .unwrap_or_default();
    let mut out = String::new();
    if let Some(title) = title {
        out.push_str(&format!("# {title}\n\n"));
    }
    out.push_str(&format!("Source: <{}>\n\n", page.url));
    out.push_str(&body);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_references_and_their_rewriting() {
        let css = "@import \"theme.css\";\nbody{background:url( 'bg.png' )}\n\
                   .a{background:url(data:image/png;base64,AA==)}\n@font-face{src:url(f.woff2)}";
        assert_eq!(css_references(css), ["bg.png", "f.woff2", "theme.css"]);
        let base = reqwest::Url::parse("https://example.com/css/site.css").unwrap();
        let inlined = HashMap::from([(
            "https://example.com/css/bg.png".to_string(),
            "data:image/png;base64,BG==".to_string(),
        )]);
        let imports = HashMap::from([(
            "https://example.com/css/theme.css".to_string(),
            "h1{color:red}".to_string(),
        )]);
        let out = rewrite_css(css, &base, &inlined, &imports);
        assert!(out.starts_with("h1{color:red}"), "{out}");
        assert!(out.contains("url(\"data:image/png;base64,BG==\")"));
        assert!(out.contains("url(\"data:image/png;base64,AA==\")"));
        // Not fetched: left out, not left pointing at the web.
        assert!(out.contains("src:url(\"\")"));
    }

    #[test]
    fn the_page_is_made_self_contained() {
        let html = r#"<html><head><base href="/sub/"><link rel="stylesheet" href="a.css"><link rel="preload" href="x.js"><script>alert(1)</script></head>
<body onload="x()"><img src="i.png" srcset="i2.png 2x"><a href="page.html">p</a><a href="javascript:alert(1)">j</a><style>p{background:url(b.png)}</style><iframe src="https://evil.example/"></iframe><noscript><p>kept</p></noscript></body></html>"#;
        let found = find(html);
        assert_eq!(found.base.as_deref(), Some("/sub/"));
        assert_eq!(found.sheets, ["a.css"]);
        assert_eq!(found.files, ["i.png"]);
        assert_eq!(found.styles, ["p{background:url(b.png)}"]);
        let base = reqwest::Url::parse("https://example.com/sub/").unwrap();
        let inlined = HashMap::from([
            (
                "https://example.com/sub/i.png".to_string(),
                "data:image/png;base64,I==".to_string(),
            ),
            (
                "https://example.com/sub/b.png".to_string(),
                "data:image/png;base64,B==".to_string(),
            ),
        ]);
        let sheets = HashMap::from([(
            "https://example.com/sub/a.css".to_string(),
            "body{margin:0}".to_string(),
        )]);
        let out = rewrite(
            html,
            &base,
            "https://example.com/page",
            &found,
            &inlined,
            &sheets,
        );
        for gone in [
            "<script",
            "alert(1)</script>",
            "onload",
            "srcset",
            "x.js",
            "<iframe",
            "javascript:",
            "<base",
        ] {
            assert!(!out.contains(gone), "{gone} in {out}");
        }
        for kept in [
            "Content-Security-Policy",
            "<style>body{margin:0}</style>",
            "src=\"data:image/png;base64,I==\"",
            "href=\"https://example.com/sub/page.html\"",
            "url(\"data:image/png;base64,B==\")",
            "<p>kept</p>",
            "Organizer snapshot of https://example.com/page",
        ] {
            assert!(out.contains(kept), "{kept} not in {out}");
        }
    }
}
