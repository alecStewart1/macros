//! A small pull tokenizer for SpreadsheetML, plus the escaping both
//! directions need.
//!
//! It covers what Office packages actually contain: elements, attributes
//! (either quote), namespace prefixes (callers match local names, so the
//! transitional and strict namespaces read alike), text, CDATA, the five
//! predefined entities and character references, comments, processing
//! instructions and a UTF-8 BOM. DTDs are refused: nothing in a package
//! needs one, and refusing them rules out entity-expansion tricks.
//!
//! Text and attribute values are borrowed from the input unless decoding
//! changes them. Line endings are normalized (CRLF and lone CR become LF) as
//! XML requires, and attribute values also turn tab/newline into a space.
//! End tags must match their start tags; an empty element `<a/>` produces a
//! [`Event::Start`] followed by an [`Event::End`], so callers track depth one
//! way.

use std::borrow::Cow;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XmlError {
    pub msg: &'static str,
    pub offset: usize,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.msg, self.offset)
    }
}

type Result<T> = std::result::Result<T, XmlError>;

#[derive(Debug, PartialEq)]
pub enum Event<'a> {
    Start(Tag<'a>),
    /// An end tag (or the implied end of an empty element), by local name.
    End(&'a str),
    Text(Cow<'a, str>),
}

/// A start tag. Attributes are validated when the tag is read and decoded
/// on access.
#[derive(Clone, Debug, PartialEq)]
pub struct Tag<'a> {
    /// The local name (prefix removed).
    pub name: &'a str,
    attrs: &'a str,
    pub empty: bool,
}

fn local(qname: &str) -> &str {
    qname.rsplit_once(':').map_or(qname, |(_, l)| l)
}

impl<'a> Tag<'a> {
    /// The value of the first attribute with this local name.
    pub fn attr(&self, name: &str) -> Option<Cow<'a, str>> {
        self.attrs().find(|(q, _)| local(q) == name).map(|(_, v)| v)
    }

    /// The value of the attribute with this exact qualified name
    /// (`xml:space`, where the prefix matters).
    pub fn qattr(&self, qname: &str) -> Option<Cow<'a, str>> {
        self.attrs().find(|(q, _)| *q == qname).map(|(_, v)| v)
    }

    /// `(qualified name, value)` pairs in document order.
    pub fn attrs(&self) -> impl Iterator<Item = (&'a str, Cow<'a, str>)> + 'a {
        let mut rest = self.attrs;
        std::iter::from_fn(move || {
            // The tag was validated by `scan_attrs`, so this can assume the
            // `name = "value"` shape.
            rest = rest.trim_start_matches(is_ws);
            let eq = rest.find('=')?;
            let name = rest[..eq].trim_end_matches(is_ws);
            let after = rest[eq + 1..].trim_start_matches(is_ws);
            let quote = after.chars().next()?;
            let body = &after[1..];
            let close = body.find(quote)?;
            let value = decode(&body[..close], true, 0).unwrap_or(Cow::Borrowed(&body[..close]));
            rest = &body[close + 1..];
            Some((name, value))
        })
    }
}

fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

fn is_name_byte(b: u8) -> bool {
    !matches!(
        b,
        b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>' | b'<' | b'=' | b'"' | b'\'' | b'&'
    )
}

pub struct Reader<'a> {
    src: &'a str,
    pos: usize,
    /// Qualified names of open elements, to check end tags.
    stack: Vec<&'a str>,
    /// An empty element's implied end, delivered on the next call.
    pending_end: Option<&'a str>,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Reader<'a>> {
        let err = |msg| XmlError { msg, offset: 0 };
        if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
            return Err(err("UTF-16 XML is not supported"));
        }
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        let src = std::str::from_utf8(bytes).map_err(|_| err("invalid UTF-8"))?;
        Ok(Reader {
            src,
            pos: 0,
            stack: Vec::new(),
            pending_end: None,
        })
    }

    /// An error at the current position — also for callers reporting
    /// content that is well-formed XML but not valid SpreadsheetML.
    pub fn err(&self, msg: &'static str) -> XmlError {
        XmlError {
            msg,
            offset: self.pos,
        }
    }

    /// Consume the rest of the element whose start tag was just read.
    pub fn skip(&mut self) -> Result<()> {
        let mut depth = 1;
        while depth > 0 {
            match self.next()? {
                Some(Event::Start(_)) => depth += 1,
                Some(Event::End(_)) => depth -= 1,
                Some(Event::Text(_)) => {}
                None => return Err(self.err("unexpected end of document")),
            }
        }
        Ok(())
    }

    /// The next child element of the element whose start tag was read last
    /// (text between children is passed over), or `None` once its end tag
    /// is reached. Each child returned must be consumed in full ([`skip`],
    /// [`text`], or its own `next_child` loop) before asking for the next.
    /// The end of the document first is an error.
    ///
    /// [`skip`]: Reader::skip
    /// [`text`]: Reader::text
    pub fn next_child(&mut self) -> Result<Option<Tag<'a>>> {
        loop {
            match self.next()? {
                Some(Event::Start(t)) => return Ok(Some(t)),
                Some(Event::End(_)) => return Ok(None),
                Some(Event::Text(_)) => {}
                None => return Err(self.err("unexpected end of document")),
            }
        }
    }

    /// All text inside the element whose start tag was just read, consuming
    /// through its end tag.
    pub fn text(&mut self) -> Result<String> {
        let mut out = String::new();
        let mut depth = 1;
        while depth > 0 {
            match self.next()? {
                Some(Event::Start(_)) => depth += 1,
                Some(Event::End(_)) => depth -= 1,
                Some(Event::Text(t)) => out.push_str(&t),
                None => return Err(self.err("unexpected end of document")),
            }
        }
        Ok(out)
    }

    /// The next event, or `None` at the end of a well-formed document.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<Event<'a>>> {
        if let Some(name) = self.pending_end.take() {
            return Ok(Some(Event::End(name)));
        }
        loop {
            let rest = &self.src[self.pos..];
            if rest.is_empty() {
                if !self.stack.is_empty() {
                    return Err(self.err("unclosed element"));
                }
                return Ok(None);
            }
            if !rest.starts_with('<') {
                let end = rest.find('<').unwrap_or(rest.len());
                let text = decode(&rest[..end], false, self.pos)?;
                self.pos += end;
                return Ok(Some(Event::Text(text)));
            }
            if let Some(body) = rest.strip_prefix("<!--") {
                let end = body
                    .find("-->")
                    .ok_or_else(|| self.err("unterminated comment"))?;
                self.pos += 4 + end + 3;
                continue;
            }
            if let Some(body) = rest.strip_prefix("<![CDATA[") {
                let end = body
                    .find("]]>")
                    .ok_or_else(|| self.err("unterminated CDATA"))?;
                self.pos += 9 + end + 3;
                return Ok(Some(Event::Text(normalize_newlines(&body[..end]))));
            }
            if rest.starts_with("<!") {
                return Err(self.err("DTDs are not supported"));
            }
            if let Some(body) = rest.strip_prefix("<?") {
                let end = body
                    .find("?>")
                    .ok_or_else(|| self.err("unterminated processing instruction"))?;
                self.pos += 2 + end + 2;
                continue;
            }
            if let Some(body) = rest.strip_prefix("</") {
                let end = body
                    .find('>')
                    .ok_or_else(|| self.err("unterminated end tag"))?;
                let qname = body[..end].trim_end_matches(is_ws);
                match self.stack.pop() {
                    Some(open) if open == qname => {}
                    _ => return Err(self.err("mismatched end tag")),
                }
                self.pos += 2 + end + 1;
                return Ok(Some(Event::End(local(qname))));
            }
            return self.start_tag().map(Some);
        }
    }

    fn start_tag(&mut self) -> Result<Event<'a>> {
        let src = self.src;
        let bytes = src.as_bytes();
        let start = self.pos + 1;
        let mut i = start;
        while i < bytes.len() && is_name_byte(bytes[i]) {
            i += 1;
        }
        if i == start {
            return Err(self.err("invalid element name"));
        }
        let qname = &src[start..i];
        let attrs_start = i;
        let (attrs_end, empty, next) = self.scan_attrs(i)?;
        self.pos = next;
        let name = local(qname);
        if empty {
            self.pending_end = Some(name);
        } else {
            self.stack.push(qname);
        }
        Ok(Event::Start(Tag {
            name,
            attrs: &src[attrs_start..attrs_end],
            empty,
        }))
    }

    /// Validate attributes from `i` to the tag's end. Returns where the
    /// attribute text ends, whether the tag is self-closing, and the offset
    /// after `>`.
    fn scan_attrs(&self, mut i: usize) -> Result<(usize, bool, usize)> {
        let bytes = self.src.as_bytes();
        let at = |msg, offset| XmlError { msg, offset };
        loop {
            while i < bytes.len() && is_ws(bytes[i] as char) {
                i += 1;
            }
            match bytes.get(i) {
                None => return Err(at("unterminated tag", i)),
                Some(b'>') => return Ok((i, false, i + 1)),
                Some(b'/') => {
                    return match bytes.get(i + 1) {
                        Some(b'>') => Ok((i, true, i + 2)),
                        _ => Err(at("expected '>' after '/'", i)),
                    };
                }
                _ => {}
            }
            let name_start = i;
            while i < bytes.len() && is_name_byte(bytes[i]) {
                i += 1;
            }
            if i == name_start {
                return Err(at("invalid attribute name", i));
            }
            while i < bytes.len() && is_ws(bytes[i] as char) {
                i += 1;
            }
            if bytes.get(i) != Some(&b'=') {
                return Err(at("expected '=' in attribute", i));
            }
            i += 1;
            while i < bytes.len() && is_ws(bytes[i] as char) {
                i += 1;
            }
            let quote = match bytes.get(i) {
                Some(&q @ (b'"' | b'\'')) => q,
                _ => return Err(at("expected quoted attribute value", i)),
            };
            let vstart = i + 1;
            let close = bytes[vstart..]
                .iter()
                .position(|&b| b == quote)
                .ok_or_else(|| at("unterminated attribute value", i))?;
            let value = &self.src[vstart..vstart + close];
            if value.contains('<') {
                return Err(at("'<' in attribute value", i));
            }
            decode(value, true, vstart)?;
            i = vstart + close + 1;
        }
    }
}

fn normalize_newlines(s: &str) -> Cow<'_, str> {
    if !s.contains('\r') {
        return Cow::Borrowed(s);
    }
    Cow::Owned(s.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Decode entities and normalize line endings in text (`attr == false`) or an
/// attribute value (`attr == true`, where whitespace characters also become
/// spaces). `offset` is only for error positions.
fn decode(s: &str, attr: bool, offset: usize) -> Result<Cow<'_, str>> {
    let needs = |b: u8| b == b'&' || b == b'\r' || (attr && (b == b'\n' || b == b'\t'));
    if !s.bytes().any(needs) {
        return Ok(Cow::Borrowed(s));
    }
    let err = |msg, at| XmlError {
        msg,
        offset: offset + at,
    };
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(off) = s[i..].bytes().position(needs) {
        out.push_str(&s[i..i + off]);
        i += off;
        match s.as_bytes()[i] {
            b'&' => {
                let semi = s[i..]
                    .find(';')
                    .ok_or_else(|| err("unterminated entity", i))?;
                let ent = &s[i + 1..i + semi];
                let c = match ent {
                    "lt" => '<',
                    "gt" => '>',
                    "amp" => '&',
                    "quot" => '"',
                    "apos" => '\'',
                    _ => {
                        let code = if let Some(hex) = ent.strip_prefix("#x") {
                            u32::from_str_radix(hex, 16).ok()
                        } else if let Some(dec) = ent.strip_prefix('#') {
                            dec.parse().ok()
                        } else {
                            return Err(err("unknown entity", i));
                        };
                        code.filter(|&c| c != 0)
                            .and_then(char::from_u32)
                            .ok_or_else(|| err("invalid character reference", i))?
                    }
                };
                out.push(c);
                i += semi + 1;
            }
            b'\r' => {
                out.push(if attr { ' ' } else { '\n' });
                i += 1;
                if s.as_bytes().get(i) == Some(&b'\n') {
                    i += 1;
                }
            }
            _ => {
                out.push(' ');
                i += 1;
            }
        }
    }
    out.push_str(&s[i..]);
    Ok(Cow::Owned(out))
}

/// Append `s` escaped for a double-quoted attribute value.
pub fn escape_attr(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\t' => out.push_str("&#9;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            _ => out.push(c),
        }
    }
}

/// Characters an XML 1.0 document can't contain even as references.
fn xml_forbidden(c: char) -> bool {
    matches!(c, '\0'..='\u{8}' | '\u{B}' | '\u{C}' | '\u{E}'..='\u{1F}' | '\u{FFFE}' | '\u{FFFF}')
}

/// `_xHHHH_` at the start of `s`: the code it spells.
fn xescape_at(s: &str) -> Option<u32> {
    let b = s.as_bytes();
    if b.len() < 7 || &b[..2] != b"_x" || b[6] != b'_' {
        return None;
    }
    if !b[2..6].iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    u32::from_str_radix(&s[2..6], 16).ok()
}

/// Decode Excel's `_xHHHH_` escapes (ST_Xstring): how SpreadsheetML stores
/// characters XML can't hold, with `_x005F_` escaping a literal underscore
/// that would otherwise start one.
pub fn decode_xstring(s: &str) -> Cow<'_, str> {
    if !s.contains("_x") {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(off) = s[i..].find("_x") {
        out.push_str(&s[i..i + off]);
        i += off;
        match xescape_at(&s[i..]).and_then(char::from_u32) {
            Some(c) => {
                out.push(c);
                i += 7;
            }
            None => {
                out.push('_');
                i += 1;
            }
        }
    }
    out.push_str(&s[i..]);
    Cow::Owned(out)
}

/// Append `s` as escaped element content in Excel's string encoding: control
/// characters (and CR, which XML would turn into LF) as `_xHHHH_`, and the
/// underscore of a literal `_xHHHH_` as `_x005F_` so it reads back verbatim.
pub fn escape_xstring(out: &mut String, s: &str) {
    for (i, c) in s.char_indices() {
        if xml_forbidden(c) || c == '\r' {
            out.push_str(&format!("_x{:04X}_", c as u32));
        } else if c == '_' && xescape_at(&s[i..]).is_some() {
            out.push_str("_x005F_");
        } else {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                _ => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Events flattened to strings for compact assertions.
    fn events(src: &str) -> Result<Vec<String>> {
        let mut r = Reader::new(src.as_bytes())?;
        let mut out = Vec::new();
        while let Some(e) = r.next()? {
            out.push(match e {
                Event::Start(t) => {
                    let attrs: Vec<String> = t.attrs().map(|(k, v)| format!(" {k}={v}")).collect();
                    format!(
                        "<{}{}{}>",
                        t.name,
                        attrs.concat(),
                        if t.empty { "/" } else { "" }
                    )
                }
                Event::End(n) => format!("</{n}>"),
                Event::Text(t) => format!("{t:?}"),
            });
        }
        Ok(out)
    }

    #[test]
    fn elements_attributes_and_text() {
        let src = "\u{FEFF}<?xml version=\"1.0\" standalone=\"yes\"?>\n<!-- hi -->\
                   <x:root a=\"1\" b = 'two' x:c=\"&lt;&amp;&gt;&quot;&apos;\"><v>3</v><e/></x:root>";
        assert_eq!(
            events(src).unwrap(),
            [
                "\"\\n\"",
                "<root a=1 b=two x:c=<&>\"'>",
                "<v>",
                "\"3\"",
                "</v>",
                "<e/>",
                "</e>",
                "</root>"
            ]
        );
    }

    #[test]
    fn entities_cdata_and_newlines() {
        assert_eq!(
            events("<t>a&#65;&#x42;&#x1F600;\r\nb\rc<![CDATA[<raw> & ]]></t>").unwrap(),
            ["<t>", "\"aAB😀\\nb\\nc\"", "\"<raw> & \"", "</t>"]
        );
        // Attribute whitespace normalization; `<` is fine as `&lt;`.
        assert_eq!(
            events("<a v=\"x\r\ny\tz&#10;\"/>").unwrap(),
            ["<a v=x y z\n/>", "</a>"]
        );
    }

    #[test]
    fn attribute_lookup_by_local_and_qualified_name() {
        let mut r = Reader::new(b"<t xml:space=\"preserve\" r:id=\"rId1\" id=\"z\"/>").unwrap();
        let Some(Event::Start(t)) = r.next().unwrap() else {
            panic!()
        };
        assert_eq!(t.qattr("xml:space").as_deref(), Some("preserve"));
        assert_eq!(t.attr("space").as_deref(), Some("preserve"));
        assert_eq!(t.attr("id").as_deref(), Some("rId1"));
        assert_eq!(t.attr("missing"), None);
    }

    #[test]
    fn malformed_documents_error() {
        for bad in [
            "<a>",
            "<a></b>",
            "</a>",
            "<a b></a>",
            "<a b=c></a>",
            "<a b=\"c></a>",
            "<a b=\"<\"/>",
            "<a>&bogus;</a>",
            "<a>&#0;</a>",
            "<a>&#xD800;</a>",
            "<a>&amp</a>",
            "<a/ >",
            "<!DOCTYPE a><a/>",
            "<a><!-- x</a>",
            "<a><![CDATA[x</a>",
            "< a/>",
        ] {
            assert!(events(bad).is_err(), "{bad}");
        }
        assert!(Reader::new(&[0xFF, 0xFE, b'<', 0]).is_err());
        assert!(Reader::new(&[b'<', 0xC3]).is_err());
    }

    #[test]
    fn xstring_codec() {
        assert_eq!(decode_xstring("a_x0009_b_x000D_"), "a\tb\r");
        assert_eq!(decode_xstring("_x005F_x0041_"), "_x0041_");
        assert_eq!(decode_xstring("_x00_ _xZZZZ_ x_x"), "_x00_ _xZZZZ_ x_x");
        assert_eq!(decode_xstring("_xD800_"), "_xD800_");
        for s in [
            "plain",
            "\u{1}\u{2}\r\n\t",
            "_x0041_",
            "__x0041__",
            "a_x",
            "_x005F_",
            "<&>",
            "日本\u{1F}",
        ] {
            let mut enc = String::new();
            escape_xstring(&mut enc, s);
            assert!(
                !enc.chars().any(|c| xml_forbidden(c) || c == '\r'),
                "{enc:?}"
            );
            let text = format!("<t>{enc}</t>");
            let mut r = Reader::new(text.as_bytes()).unwrap();
            r.next().unwrap();
            let got = match r.next().unwrap() {
                Some(Event::Text(t)) => t.into_owned(),
                Some(Event::End(_)) => String::new(),
                other => panic!("{other:?}"),
            };
            assert_eq!(decode_xstring(&got), s, "{enc}");
        }
    }

    #[test]
    fn mutations_never_panic() {
        let base = "<?xml version=\"1.0\"?><a x=\"1\" y='&amp;'><b>t&#65;<![CDATA[c]]></b><!--c--><c/></a>";
        let mut rng = 0x1234_5678u64;
        let alphabet = b"<>/=\"'&;#x!?-[]abc \r\n";
        for _ in 0..5000 {
            let mut d = base.as_bytes().to_vec();
            for _ in 0..3 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let i = (rng % d.len() as u64) as usize;
                d[i] = alphabet[(rng >> 33) as usize % alphabet.len()];
            }
            if let Ok(mut r) = Reader::new(&d) {
                for _ in 0..1000 {
                    match r.next() {
                        Ok(Some(Event::Start(t))) => {
                            let _ = t.attrs().count();
                        }
                        Ok(Some(_)) => {}
                        _ => break,
                    }
                }
            }
        }
    }
}
