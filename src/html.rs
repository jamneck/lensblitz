//! Just enough HTML to make real-world READMEs look right: a forgiving tag tokenizer and
//! entity decoder. Not a parser — Markdown files use a small, predictable subset.

use std::borrow::Cow;

pub enum Tok<'a> {
    Text(&'a str),
    Open { name: String, attrs: Vec<(String, String)> },
    Close(String),
}

/// Split raw HTML into text, opening tags and closing tags. Comments, doctypes and
/// processing instructions are dropped.
pub fn tokens(src: &str) -> Vec<Tok<'_>> {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    let mut text_start = 0;
    while i < n {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &src[i..];
        let end = if rest.starts_with("<!--") {
            Some(rest.find("-->").map_or(n, |p| i + p + 3))
        } else if rest.starts_with("<!") || rest.starts_with("<?") {
            Some(rest.find('>').map_or(n, |p| i + p + 1))
        } else {
            None
        };
        if let Some(end) = end {
            if text_start < i {
                out.push(Tok::Text(&src[text_start..i]));
            }
            i = end;
            text_start = i;
            continue;
        }
        let closing = b.get(i + 1) == Some(&b'/');
        let name_start = i + 1 + closing as usize;
        if !b.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            i += 1;
            continue;
        }
        let Some(tag_end) = tag_end(b, name_start) else {
            i += 1;
            continue;
        };
        if text_start < i {
            out.push(Tok::Text(&src[text_start..i]));
        }
        let inner = &src[name_start..tag_end];
        let name_len =
            inner.find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':')).unwrap_or(inner.len());
        let name = inner[..name_len].to_ascii_lowercase();
        if closing {
            out.push(Tok::Close(name));
        } else {
            out.push(Tok::Open { name, attrs: attrs(&inner[name_len..]) });
        }
        i = tag_end + 1;
        text_start = i;
    }
    if text_start < n {
        out.push(Tok::Text(&src[text_start..]));
    }
    out
}

/// Index of the `>` that closes a tag, skipping quoted attribute values.
fn tag_end(b: &[u8], from: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (i, &c) in b.iter().enumerate().skip(from) {
        if quote != 0 {
            if c == quote {
                quote = 0;
            }
        } else if c == b'"' || c == b'\'' {
            quote = c;
        } else if c == b'>' {
            return Some(i);
        }
    }
    None
}

fn attrs(s: &str) -> Vec<(String, String)> {
    let b = s.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        while i < n && (b[i].is_ascii_whitespace() || b[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < n && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' {
            i += 1;
        }
        if start == i {
            break;
        }
        let name = s[start..i].to_ascii_lowercase();
        while i < n && b[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < n && b[i] == b'=' {
            i += 1;
            while i < n && b[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < n && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i];
                let vs = i + 1;
                i = vs;
                while i < n && b[i] != q {
                    i += 1;
                }
                value = decode_entities(&s[vs..i]).into_owned();
                i = (i + 1).min(n);
            } else {
                let vs = i;
                while i < n && !b[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = decode_entities(&s[vs..i]).into_owned();
            }
        }
        out.push((name, value));
    }
    out
}

pub fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Decode `&amp;`, `&#39;`, `&#x27;` and the handful of named entities that show up in prose.
pub fn decode_entities(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let semi = rest[..rest.len().min(12)].find(';');
        let decoded = semi.and_then(|semi| {
            let name = &rest[1..semi];
            let ch = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = name.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                named_entity(name)
            };
            ch.map(|ch| (ch, semi + 1))
        });
        match decoded {
            Some((ch, len)) => {
                out.push(ch);
                rest = &rest[len..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    Cow::Owned(out)
}

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "mdash" => '—',
        "ndash" => '–',
        "hellip" => '…',
        "middot" => '·',
        "bull" => '•',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "harr" => '↔',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "times" => '×',
        "divide" => '÷',
        "deg" => '°',
        "plusmn" => '±',
        "micro" => 'µ',
        "para" => '¶',
        "sect" => '§',
        "check" => '✓',
        "star" => '☆',
        "hearts" => '♥',
        "infin" => '∞',
        "ne" => '≠',
        "le" => '≤',
        "ge" => '≥',
        "asymp" => '≈',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "emsp" => '\u{2003}',
        "ensp" => '\u{2002}',
        "thinsp" => '\u{2009}',
        "zwj" => '\u{200d}',
        "zwnj" => '\u{200c}',
        "shy" => '\u{ad}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizes_tags_and_attrs() {
        let t = tokens("a <img src=\"x.png\" width=200 alt='hi > there'/> b</p><!-- c -->d");
        assert_eq!(t.len(), 5);
        match &t[1] {
            Tok::Open { name, attrs } => {
                assert_eq!(name, "img");
                assert_eq!(attr(attrs, "src"), Some("x.png"));
                assert_eq!(attr(attrs, "width"), Some("200"));
                assert_eq!(attr(attrs, "alt"), Some("hi > there"));
            }
            _ => panic!("expected an open tag"),
        }
        assert!(matches!(&t[3], Tok::Close(n) if n == "p"));
        assert!(matches!(&t[4], Tok::Text("d")));
    }

    #[test]
    fn stray_angle_brackets_are_text() {
        let t = tokens("1 < 2 and 3 > 2");
        assert!(matches!(t.as_slice(), [Tok::Text("1 < 2 and 3 > 2")]));
    }

    #[test]
    fn entities() {
        assert_eq!(decode_entities("a &amp; b &#x41;&#66; &nope; &copy;"), "a & b AB &nope; ©");
    }
}
