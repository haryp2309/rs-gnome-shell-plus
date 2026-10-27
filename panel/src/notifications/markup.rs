//! Notification body markup, as GNOME Shell handles it: `<b>`, `<i>`,
//! `<u>` and `<a href>` are kept, anything else is escaped so it shows up
//! as text. If the result still isn't valid Pango markup (unbalanced tags,
//! say), the whole body is shown as plain text.

use gtk::{glib, pango};

const SIMPLE_TAGS: &[&str] = &["<b>", "</b>", "<i>", "</i>", "<u>", "</u>", "</a>"];

pub fn sanitize(body: &str) -> String {
    let markup = fix_markup(body);
    if pango::parse_markup(&links_as_spans(&markup), '\0').is_ok() {
        markup
    } else {
        glib::markup_escape_text(body).to_string()
    }
}

fn fix_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let consumed = match c {
            '<' => tag(rest, &mut out),
            '&' => entity(rest, &mut out),
            '>' => {
                out.push_str("&gt;");
                1
            }
            '"' => {
                out.push_str("&quot;");
                1
            }
            _ => {
                out.push(c);
                c.len_utf8()
            }
        };
        rest = &rest[consumed..];
    }
    out
}

/// Copies an allowed tag at the start of `text`, or escapes the `<`.
fn tag(text: &str, out: &mut String) -> usize {
    if let Some(tag) = SIMPLE_TAGS.iter().find(|tag| text.starts_with(*tag)) {
        out.push_str(tag);
        return tag.len();
    }
    if let Some((href, len)) = link(text) {
        out.push_str(&format!("<a href=\"{}\">", glib::markup_escape_text(&href)));
        return len;
    }
    out.push_str("&lt;");
    1
}

/// Parses `<a href="...">` (either quote style) into the URL and the
/// length of the tag.
fn link(text: &str) -> Option<(String, usize)> {
    let after = text.strip_prefix("<a href=")?;
    let quote = after.chars().next().filter(|q| *q == '"' || *q == '\'')?;
    let value = &after[1..];
    let end = value.find(quote)?;
    let tail = value[end + 1..].strip_prefix('>')?;
    let href = unescape(&value[..end]);
    Some((href, text.len() - tail.len()))
}

/// Keeps a valid entity at the start of `text`, or escapes the `&`.
fn entity(text: &str, out: &mut String) -> usize {
    let candidate = text[1..].split(';').next().filter(|_| text[1..].contains(';'));
    let valid = candidate.is_some_and(|name| match name.strip_prefix('#') {
        Some(num) => match num.strip_prefix(['x', 'X']) {
            Some(hex) => !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()),
            None => !num.is_empty() && num.chars().all(|c| c.is_ascii_digit()),
        },
        None => matches!(name, "amp" | "lt" | "gt" | "quot" | "apos"),
    });
    match candidate {
        Some(name) if valid => {
            let len = name.len() + 2;
            out.push_str(&text[..len]);
            len
        }
        _ => {
            out.push_str("&amp;");
            1
        }
    }
}

/// Links are GtkLabel's addition to Pango markup; Pango itself only
/// validates them as spans.
fn links_as_spans(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(start) = rest.find("<a href=\"") {
        out.push_str(&rest[..start]);
        // fix_markup escaped the URL, so the first '>' ends the tag.
        let end = rest[start..].find('>').map_or(rest.len(), |i| start + i + 1);
        out.push_str("<span>");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out.replace("</a>", "</span>")
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_supported_tags() {
        assert_eq!(fix_markup("<b>bold</b> <i>x</i>"), "<b>bold</b> <i>x</i>");
        assert_eq!(fix_markup("<a href='https://a.b/?x=1&amp;y'>l</a>"), "<a href=\"https://a.b/?x=1&amp;y\">l</a>");
    }

    #[test]
    fn escapes_everything_else() {
        assert_eq!(fix_markup("<span color='red'>x</span>"), "&lt;span color='red'&gt;x&lt;/span&gt;");
        assert_eq!(fix_markup("a & b &amp; &#39; &bogus;"), "a &amp; b &amp; &#39; &amp;bogus;");
        assert_eq!(fix_markup("1 < 2 > 0"), "1 &lt; 2 &gt; 0");
    }

    #[test]
    fn validates_links() {
        let body = "Open <a href=\"https://gnome.org\">gnome.org</a> <b>now</b>";
        assert_eq!(sanitize(body), body);
    }

    #[test]
    fn falls_back_to_plain_text() {
        assert_eq!(sanitize("<b>unclosed"), "&lt;b&gt;unclosed");
    }
}
