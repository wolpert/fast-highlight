//! The framed wire protocol. `docs/protocol.md` is the normative specification.
//!
//! [`Decoder`] turns a byte stream into frames ([`Decoder::next_frame`]) and requests
//! ([`Decoder::next_request`]). The `encode_*` functions build frames in both directions: the
//! response encoders are what the daemon sends, and the request encoders exist for tests, the
//! CLI, and fuzzing.

use std::fmt;
use std::fmt::Write as _;

/// Maximum body length in bytes.
pub const MAX_BODY: usize = 16 * 1024 * 1024;
/// Maximum header length in bytes, including the trailing LF.
pub const MAX_HEADER: usize = 64;

const MAGIC: &[u8] = b"FH1 ";
const MAX_ID_DIGITS: usize = 20;
const MAX_LENGTH_DIGITS: usize = 10;

/// A decoded `H` request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HighlightFields {
    pub buffer: Vec<u8>,
    pub prebuffer: Vec<u8>,
    /// Cursor in the request's offset unit; `None` means end of buffer.
    pub cursor: Option<usize>,
    /// `None` means "unchanged since the last request".
    pub cwd: Option<Vec<u8>>,
    /// The raw option letters.
    pub opts: String,
}

impl HighlightFields {
    /// True when offsets are in characters (`u` option).
    pub fn char_offsets(&self) -> bool {
        self.opts.contains('u')
    }
}

/// A decoded `S` request. `None` fields leave that part of the state unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StateUpdate {
    pub aliases: Option<Vec<String>>,
    pub global_aliases: Option<Vec<String>>,
    pub suffix_aliases: Option<Vec<String>>,
    pub functions: Option<Vec<String>>,
    pub builtins: Option<Vec<String>>,
    pub reserved_words: Option<Vec<String>>,
    /// `(name, directory)` pairs.
    pub named_dirs: Option<Vec<(String, String)>>,
    pub path: Option<String>,
    /// A `rehash` field was present: rescan every `$PATH` directory, whatever its mtime.
    pub rehash: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Highlight {
        id: u64,
        fields: HighlightFields,
    },
    State {
        id: u64,
        update: StateUpdate,
        /// Problems with individual fields that were skipped while the rest of the request was
        /// applied (for example, a `nameddirs` list with an odd number of entries). The daemon
        /// logs them.
        warnings: Vec<String>,
    },
    Ping {
        id: u64,
    },
    Quit {
        id: u64,
    },
    /// A frame with an unknown type letter, or a body that failed field decoding. The daemon
    /// answers it with an `E` response.
    Invalid {
        id: u64,
        reason: String,
    },
}

impl Request {
    /// The request identifier.
    pub fn id(&self) -> u64 {
        match self {
            Request::Highlight { id, .. }
            | Request::State { id, .. }
            | Request::Ping { id }
            | Request::Quit { id }
            | Request::Invalid { id, .. } => *id,
        }
    }
}

/// An unrecoverable framing error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FramingError(pub String);

impl fmt::Display for FramingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "framing error: {}", self.0)
    }
}

impl std::error::Error for FramingError {}

/// One raw frame of either direction: the type letter, the id, and the body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub id: u64,
    pub body: Vec<u8>,
}

/// Incremental frame decoder. Feed it bytes as they arrive and pull complete requests (or raw
/// frames) out.
///
/// A framing error is sticky: once one is returned, every later call returns it again, because
/// the stream cannot be resynchronised.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// Offset in `buf` of the first unconsumed byte.
    start: usize,
    error: Option<FramingError>,
}

/// A parsed header: type letter, id, body length, and the header's length including LF.
struct Header {
    kind: u8,
    id: u64,
    body_len: usize,
    header_len: usize,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Appends received bytes.
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Number of received bytes not yet consumed by a complete frame.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.start
    }

    /// Returns the next complete request, `Ok(None)` when more bytes are needed, or a framing
    /// error. Never panics on any input.
    pub fn next_request(&mut self) -> Result<Option<Request>, FramingError> {
        Ok(self.next_frame()?.map(|frame| decode_request(&frame)))
    }

    /// Returns the next complete frame of any type, `Ok(None)` when more bytes are needed, or a
    /// framing error. Never panics on any input.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, FramingError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let pending = &self.buf[self.start..];
        let header = match parse_header(pending) {
            Ok(Some(h)) => h,
            Ok(None) => return Ok(None),
            Err(e) => {
                self.error = Some(e.clone());
                return Err(e);
            }
        };
        let total = header.header_len + header.body_len;
        if pending.len() < total {
            return Ok(None);
        }
        let body = pending[header.header_len..total].to_vec();
        self.start += total;
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start >= 64 * 1024 && self.start * 2 >= self.buf.len() {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        Ok(Some(Frame {
            kind: header.kind,
            id: header.id,
            body,
        }))
    }
}

fn framing(msg: impl Into<String>) -> FramingError {
    FramingError(msg.into())
}

/// Parses a header from the start of `bytes`. Returns `Ok(None)` when `bytes` is a valid prefix
/// of a header but incomplete, so malformed input is rejected as early as possible.
fn parse_header(bytes: &[u8]) -> Result<Option<Header>, FramingError> {
    let window = &bytes[..bytes.len().min(MAX_HEADER)];
    let complete = scan_header(window)?;
    match complete {
        Some(h) => Ok(Some(h)),
        None if window.len() >= MAX_HEADER => Err(framing("header longer than 64 bytes")),
        None => Ok(None),
    }
}

fn scan_header(bytes: &[u8]) -> Result<Option<Header>, FramingError> {
    for (i, &want) in MAGIC.iter().enumerate() {
        match bytes.get(i) {
            None => return Ok(None),
            Some(&b) if b == want => {}
            Some(_) => return Err(framing("bad magic")),
        }
    }
    let mut pos = MAGIC.len();
    let Some(&kind) = bytes.get(pos) else {
        return Ok(None);
    };
    if !kind.is_ascii_alphabetic() {
        return Err(framing("frame type is not an ASCII letter"));
    }
    pos += 1;
    match bytes.get(pos) {
        None => return Ok(None),
        Some(b' ') => pos += 1,
        Some(_) => return Err(framing("expected space after frame type")),
    }
    let Some(id) = scan_number(bytes, &mut pos, b' ', MAX_ID_DIGITS, "id")? else {
        return Ok(None);
    };
    let Some(len) = scan_number(bytes, &mut pos, b'\n', MAX_LENGTH_DIGITS, "length")? else {
        return Ok(None);
    };
    let body_len = usize::try_from(len)
        .ok()
        .filter(|&l| l <= MAX_BODY)
        .ok_or_else(|| framing(format!("body length {len} exceeds {MAX_BODY}")))?;
    Ok(Some(Header {
        kind,
        id,
        body_len,
        header_len: pos,
    }))
}

/// Scans a decimal number starting at `*pos` and terminated by `term`, advancing `*pos` past the
/// terminator. `Ok(None)` means the input ended before the terminator.
fn scan_number(
    bytes: &[u8],
    pos: &mut usize,
    term: u8,
    max_digits: usize,
    what: &str,
) -> Result<Option<u64>, FramingError> {
    let start = *pos;
    let mut value: u64 = 0;
    loop {
        let Some(&b) = bytes.get(*pos) else {
            return Ok(None);
        };
        if b == term {
            if *pos == start {
                return Err(framing(format!("empty {what}")));
            }
            *pos += 1;
            return Ok(Some(value));
        }
        if !b.is_ascii_digit() {
            return Err(framing(format!("unexpected byte 0x{b:02x} in {what}")));
        }
        if *pos > start && bytes[start] == b'0' {
            return Err(framing(format!("leading zero in {what}")));
        }
        if *pos - start >= max_digits {
            return Err(framing(format!("{what} has too many digits")));
        }
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u64::from(b - b'0')))
            .ok_or_else(|| framing(format!("{what} out of range")))?;
        *pos += 1;
    }
}

/// Parses a complete decimal number: digits only, non-empty, no leading zeros, no overflow.
fn parse_decimal(digits: &[u8]) -> Option<u64> {
    if digits.is_empty() || (digits.len() > 1 && digits[0] == b'0') {
        return None;
    }
    digits.iter().try_fold(0u64, |acc, &b| {
        if !b.is_ascii_digit() {
            return None;
        }
        acc.checked_mul(10)?.checked_add(u64::from(b - b'0'))
    })
}

/// Turns a frame into a request. Never fails: problems become [`Request::Invalid`].
pub fn decode_request(frame: &Frame) -> Request {
    let id = frame.id;
    let invalid = |reason: String| Request::Invalid { id, reason };
    match frame.kind {
        b'H' => match parse_fields(&frame.body).and_then(|f| highlight_fields(&f)) {
            Ok(fields) => Request::Highlight { id, fields },
            Err(reason) => invalid(reason),
        },
        b'S' => match parse_fields(&frame.body) {
            Ok(fields) => {
                let (update, warnings) = state_update(&fields);
                Request::State {
                    id,
                    update,
                    warnings,
                }
            }
            Err(reason) => invalid(reason),
        },
        // Ping and quit bodies are specified as empty; any body is ignored.
        b'P' => Request::Ping { id },
        b'Q' => Request::Quit { id },
        other => invalid(format!("unknown request type '{}'", char::from(other))),
    }
}

type Fields<'a> = Vec<(&'a [u8], &'a [u8])>;

/// Splits a body into `(name, value)` fields in order of appearance.
fn parse_fields(body: &[u8]) -> Result<Fields<'_>, String> {
    let mut fields = Vec::new();
    let mut pos = 0;
    while pos < body.len() {
        let name_start = pos;
        while pos < body.len() && (body[pos].is_ascii_alphanumeric() || body[pos] == b'-') {
            pos += 1;
        }
        let name = &body[name_start..pos];
        if name.is_empty() {
            return Err(format!("bad field name at body offset {name_start}"));
        }
        if body.get(pos) != Some(&b' ') {
            return Err(format!(
                "expected space after field name '{}'",
                String::from_utf8_lossy(name)
            ));
        }
        pos += 1;
        let len_start = pos;
        while pos < body.len() && body[pos] != b'\n' {
            pos += 1;
        }
        if pos == body.len() {
            return Err("truncated field header".to_string());
        }
        let field_name = String::from_utf8_lossy(name);
        let len = parse_decimal(&body[len_start..pos])
            .and_then(|l| usize::try_from(l).ok())
            .ok_or_else(|| format!("bad length for field '{field_name}'"))?;
        pos += 1;
        let value_end = pos
            .checked_add(len)
            .filter(|&end| end < body.len())
            .ok_or_else(|| format!("field '{field_name}' overruns the body"))?;
        if body[value_end] != b'\n' {
            return Err(format!("missing LF after value of field '{field_name}'"));
        }
        fields.push((name, &body[pos..value_end]));
        pos = value_end + 1;
    }
    Ok(fields)
}

fn highlight_fields(fields: &Fields<'_>) -> Result<HighlightFields, String> {
    let mut out = HighlightFields::default();
    for &(name, value) in fields {
        match name {
            b"buf" => out.buffer = value.to_vec(),
            b"pre" => out.prebuffer = value.to_vec(),
            b"cur" => {
                let cursor = parse_decimal(value)
                    .and_then(|c| usize::try_from(c).ok())
                    .ok_or_else(|| "bad cur value".to_string())?;
                out.cursor = Some(cursor);
            }
            b"cwd" => out.cwd = Some(value.to_vec()),
            b"opt" => out.opts = String::from_utf8_lossy(value).into_owned(),
            _ => {}
        }
    }
    Ok(out)
}

/// Decodes the fields of an `S` body. A field whose value is malformed is skipped with a warning;
/// the other fields still apply.
fn state_update(fields: &Fields<'_>) -> (StateUpdate, Vec<String>) {
    let mut out = StateUpdate::default();
    let mut warnings = Vec::new();
    for &(name, value) in fields {
        match name {
            b"alias" => out.aliases = Some(parse_list(value)),
            b"galias" => out.global_aliases = Some(parse_list(value)),
            b"salias" => out.suffix_aliases = Some(parse_list(value)),
            b"func" => out.functions = Some(parse_list(value)),
            b"builtin" => out.builtins = Some(parse_list(value)),
            b"reswords" => out.reserved_words = Some(parse_list(value)),
            b"nameddirs" => {
                let list = parse_list(value);
                if !list.len().is_multiple_of(2) {
                    warnings.push(format!(
                        "nameddirs has an odd number of entries ({}); field ignored",
                        list.len()
                    ));
                    continue;
                }
                let mut pairs = Vec::with_capacity(list.len() / 2);
                let mut it = list.into_iter();
                while let (Some(name), Some(dir)) = (it.next(), it.next()) {
                    pairs.push((name, dir));
                }
                out.named_dirs = Some(pairs);
            }
            b"path" => out.path = Some(String::from_utf8_lossy(value).into_owned()),
            b"rehash" => out.rehash = true,
            _ => {}
        }
    }
    (out, warnings)
}

/// Splits a list value into entries, each terminated by a NUL byte. A final entry without its
/// terminating NUL is accepted, and an empty value is an empty list. So `a\0b\0` and `a\0b` are
/// both `[a, b]`, `\0` is one empty entry, and `a\0\0` is `[a, ""]`.
fn parse_list(value: &[u8]) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }
    // "\0" is one empty entry: stripping the terminator leaves "", which splits into [""].
    value
        .strip_suffix(b"\0")
        .unwrap_or(value)
        .split(|&b| b == 0)
        .map(|entry| String::from_utf8_lossy(entry).into_owned())
        .collect()
}

/// A span ready for the wire: offsets already converted to the request's unit and made
/// relative to the start of `buf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireSpan {
    pub start: usize,
    pub end: usize,
    pub kind: crate::token::TokenKind,
}

/// Encodes one frame. `body` must not exceed [`MAX_BODY`] bytes; the encoders in this module
/// guarantee that for the frames they build.
pub fn encode_frame(kind: u8, id: u64, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32 + body.len());
    out.extend_from_slice(MAGIC);
    out.push(kind);
    out.extend_from_slice(format!(" {id} {}\n", body.len()).as_bytes());
    out.extend_from_slice(body);
    out
}

/// Encodes an `R` response. Should the span lines exceed [`MAX_BODY`], trailing spans are
/// dropped; a prefix of a sorted, well-nested list is still sorted and well nested.
pub fn encode_result(id: u64, spans: &[WireSpan]) -> Vec<u8> {
    let mut body = String::with_capacity(spans.len() * 16);
    let mut line = String::with_capacity(48);
    for span in spans {
        line.clear();
        let _ = writeln!(line, "{} {} {}", span.start, span.end, span.kind.name());
        if body.len() + line.len() > MAX_BODY {
            break;
        }
        body.push_str(&line);
    }
    encode_frame(b'R', id, body.as_bytes())
}

/// Encodes an `A` response.
pub fn encode_ack(id: u64) -> Vec<u8> {
    encode_frame(b'A', id, b"")
}

/// Encodes an `E` response. A message longer than [`MAX_BODY`] is truncated at a character
/// boundary.
pub fn encode_error(id: u64, message: &str) -> Vec<u8> {
    let mut end = message.len().min(MAX_BODY);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    encode_frame(b'E', id, &message.as_bytes()[..end])
}

fn push_field(body: &mut Vec<u8>, name: &str, value: &[u8]) {
    body.extend_from_slice(name.as_bytes());
    body.extend_from_slice(format!(" {}\n", value.len()).as_bytes());
    body.extend_from_slice(value);
    body.push(b'\n');
}

fn push_list<'a>(body: &mut Vec<u8>, name: &str, entries: impl IntoIterator<Item = &'a str>) {
    let mut value = Vec::new();
    for entry in entries {
        value.extend_from_slice(entry.as_bytes());
        value.push(0);
    }
    push_field(body, name, &value);
}

/// Encodes an `H` request. `pre`, `cur`, `cwd`, and `opt` are sent only when they differ from
/// the protocol defaults, so decoding the frame yields `fields` again.
pub fn encode_highlight(id: u64, fields: &HighlightFields) -> Vec<u8> {
    let mut body = Vec::with_capacity(fields.buffer.len() + fields.prebuffer.len() + 64);
    push_field(&mut body, "buf", &fields.buffer);
    if !fields.prebuffer.is_empty() {
        push_field(&mut body, "pre", &fields.prebuffer);
    }
    if let Some(cursor) = fields.cursor {
        push_field(&mut body, "cur", cursor.to_string().as_bytes());
    }
    if let Some(cwd) = &fields.cwd {
        push_field(&mut body, "cwd", cwd);
    }
    if !fields.opts.is_empty() {
        push_field(&mut body, "opt", fields.opts.as_bytes());
    }
    encode_frame(b'H', id, &body)
}

/// Encodes an `S` request with a field for each `Some` member of `update`. List entries must
/// not contain NUL bytes.
pub fn encode_state(id: u64, update: &StateUpdate) -> Vec<u8> {
    let mut body = Vec::new();
    let lists = [
        ("alias", &update.aliases),
        ("galias", &update.global_aliases),
        ("salias", &update.suffix_aliases),
        ("func", &update.functions),
        ("builtin", &update.builtins),
        ("reswords", &update.reserved_words),
    ];
    for (name, list) in lists {
        if let Some(list) = list {
            push_list(&mut body, name, list.iter().map(String::as_str));
        }
    }
    if let Some(dirs) = &update.named_dirs {
        push_list(
            &mut body,
            "nameddirs",
            dirs.iter().flat_map(|(n, d)| [n.as_str(), d.as_str()]),
        );
    }
    if let Some(path) = &update.path {
        push_field(&mut body, "path", path.as_bytes());
    }
    if update.rehash {
        push_field(&mut body, "rehash", b"");
    }
    encode_frame(b'S', id, &body)
}

/// Encodes a `P` request.
pub fn encode_ping(id: u64) -> Vec<u8> {
    encode_frame(b'P', id, b"")
}

/// Encodes a `Q` request.
pub fn encode_quit(id: u64) -> Vec<u8> {
    encode_frame(b'Q', id, b"")
}

/// Encodes any request except [`Request::Invalid`], which has no wire form of its own and
/// yields `None`. The warnings of a [`Request::State`] are not encoded.
pub fn encode_request(request: &Request) -> Option<Vec<u8>> {
    match request {
        Request::Highlight { id, fields } => Some(encode_highlight(*id, fields)),
        Request::State { id, update, .. } => Some(encode_state(*id, update)),
        Request::Ping { id } => Some(encode_ping(*id)),
        Request::Quit { id } => Some(encode_quit(*id)),
        Request::Invalid { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::TokenKind;

    fn decode_all(bytes: &[u8]) -> Result<Vec<Request>, FramingError> {
        let mut d = Decoder::new();
        d.feed(bytes);
        let mut out = Vec::new();
        while let Some(r) = d.next_request()? {
            out.push(r);
        }
        Ok(out)
    }

    fn decode_one(bytes: &[u8]) -> Request {
        let reqs = decode_all(bytes).expect("no framing error");
        assert_eq!(reqs.len(), 1, "{reqs:?}");
        reqs.into_iter().next().unwrap()
    }

    fn framing_error(bytes: &[u8]) -> FramingError {
        let mut d = Decoder::new();
        d.feed(bytes);
        loop {
            match d.next_request() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("expected a framing error for {bytes:?}"),
                Err(e) => return e,
            }
        }
    }

    fn incomplete(bytes: &[u8]) {
        let mut d = Decoder::new();
        d.feed(bytes);
        assert_eq!(d.next_request(), Ok(None), "{bytes:?}");
    }

    fn sample_requests() -> Vec<Request> {
        vec![
            Request::Ping { id: 0 },
            Request::Highlight {
                id: 1,
                fields: HighlightFields::default(),
            },
            Request::Highlight {
                id: 7,
                fields: HighlightFields {
                    buffer: "ls ~/日本 🎉".as_bytes().to_vec(),
                    prebuffer: b"for x in a b\n".to_vec(),
                    cursor: Some(4),
                    cwd: Some(b"/tmp/\xff dir\n".to_vec()),
                    opts: "ucae".to_string(),
                },
            },
            Request::Highlight {
                id: u64::MAX,
                fields: HighlightFields {
                    buffer: b"a\nb\0c\xe2\x82".to_vec(),
                    cursor: Some(0),
                    ..HighlightFields::default()
                },
            },
            Request::State {
                id: 2,
                update: StateUpdate::default(),
                warnings: vec![],
            },
            Request::State {
                id: 3,
                update: StateUpdate {
                    aliases: Some(vec!["ll".into(), "la".into()]),
                    global_aliases: Some(vec![]),
                    suffix_aliases: Some(vec!["txt".into()]),
                    functions: Some(vec!["".into(), "f g".into()]),
                    builtins: Some(vec!["cd".into()]),
                    reserved_words: Some(vec!["if".into(), "[[".into()]),
                    named_dirs: Some(vec![("proj".into(), "/home/u/proj".into())]),
                    path: Some("/usr/bin:/bin".into()),
                    rehash: true,
                },
                warnings: vec![],
            },
            Request::Quit { id: 99 },
        ]
    }

    #[test]
    fn round_trip_each_request() {
        for req in sample_requests() {
            let bytes = encode_request(&req).unwrap();
            assert_eq!(decode_one(&bytes), req);
        }
    }

    #[test]
    fn many_frames_in_one_feed() {
        let reqs = sample_requests();
        let bytes: Vec<u8> = reqs
            .iter()
            .flat_map(|r| encode_request(r).unwrap())
            .collect();
        assert_eq!(decode_all(&bytes).unwrap(), reqs);
    }

    #[test]
    fn one_byte_at_a_time() {
        let reqs = sample_requests();
        let bytes: Vec<u8> = reqs
            .iter()
            .flat_map(|r| encode_request(r).unwrap())
            .collect();
        let mut d = Decoder::new();
        let mut out = Vec::new();
        for b in &bytes {
            d.feed(std::slice::from_ref(b));
            while let Some(r) = d.next_request().unwrap() {
                out.push(r);
            }
        }
        assert_eq!(out, reqs);
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn invalid_has_no_wire_form() {
        let req = Request::Invalid {
            id: 1,
            reason: "x".into(),
        };
        assert_eq!(encode_request(&req), None);
    }

    #[test]
    fn spec_example_decodes() {
        let bytes = b"FH1 H 7 27\nbuf 4\nls ~\ncur 1\n4\nopt 1\nu\n";
        let Request::Highlight { id, fields } = decode_one(bytes) else {
            panic!("not a highlight request");
        };
        assert_eq!(id, 7);
        assert_eq!(fields.buffer, b"ls ~");
        assert_eq!(fields.cursor, Some(4));
        assert!(fields.char_offsets());
        assert_eq!(fields.cwd, None);
        assert!(fields.prebuffer.is_empty());
    }

    #[test]
    fn spec_example_encodes() {
        let spans = [
            WireSpan {
                start: 0,
                end: 2,
                kind: TokenKind::Command,
            },
            WireSpan {
                start: 3,
                end: 4,
                kind: TokenKind::PathDirectory,
            },
        ];
        assert_eq!(
            encode_result(7, &spans),
            b"FH1 R 7 31\n0 2 command\n3 4 path-directory\n"
        );
        let fields = HighlightFields {
            buffer: b"ls ~".to_vec(),
            cursor: Some(4),
            opts: "u".into(),
            ..HighlightFields::default()
        };
        assert_eq!(
            encode_highlight(7, &fields),
            b"FH1 H 7 27\nbuf 4\nls ~\ncur 1\n4\nopt 1\nu\n"
        );
    }

    #[test]
    fn response_encoders() {
        assert_eq!(encode_ack(5), b"FH1 A 5 0\n");
        assert_eq!(encode_result(0, &[]), b"FH1 R 0 0\n");
        assert_eq!(encode_error(3, "boom"), b"FH1 E 3 4\nboom");
        let mut d = Decoder::new();
        d.feed(&encode_error(u64::MAX, "é"));
        assert_eq!(
            d.next_frame().unwrap(),
            Some(Frame {
                kind: b'E',
                id: u64::MAX,
                body: "é".as_bytes().to_vec()
            })
        );
    }

    #[test]
    fn decoder_reads_response_frames() {
        let spans = [WireSpan {
            start: 1,
            end: 3,
            kind: TokenKind::Glob,
        }];
        let mut d = Decoder::new();
        d.feed(&encode_result(4, &spans));
        d.feed(&encode_ack(5));
        let r = d.next_frame().unwrap().unwrap();
        assert_eq!(
            (r.kind, r.id, r.body.as_slice()),
            (b'R', 4, &b"1 3 glob\n"[..])
        );
        let a = d.next_frame().unwrap().unwrap();
        assert_eq!((a.kind, a.id, a.body.len()), (b'A', 5, 0));
        assert_eq!(d.next_frame(), Ok(None));
    }

    #[test]
    fn incomplete_input_needs_more() {
        incomplete(b"");
        incomplete(b"F");
        incomplete(b"FH1");
        incomplete(b"FH1 ");
        incomplete(b"FH1 P");
        incomplete(b"FH1 P ");
        incomplete(b"FH1 P 12");
        incomplete(b"FH1 P 12 ");
        incomplete(b"FH1 P 12 0");
        incomplete(b"FH1 H 1 5\nbuf");
    }

    #[test]
    fn framing_errors() {
        let cases: &[&[u8]] = &[
            b"GET / HTTP/1.1\r\n",
            b"fh1 P 1 0\n",
            b"FH2 P 1 0\n",
            b"FH1  P 1 0\n",
            b"FH1 1 1 0\n",
            b"FH1 PP 1 0\n",
            b"FH1 P 01 0\n",
            b"FH1 P 00 0\n",
            b"FH1 P 1 00\n",
            b"FH1 P 1 05\n",
            b"FH1 P  1 0\n",
            b"FH1 P 1  0\n",
            b"FH1 P 1 0 \n",
            b"FH1 P 1 0\r\n",
            b"FH1 P +1 0\n",
            b"FH1 P -1 0\n",
            b"FH1 P 1 -0\n",
            b"FH1 P 1\n",
            b"FH1 P  0\n",
            b"FH1 P 1 \n",
            b"FH1 P 18446744073709551616 0\n",
            b"FH1 P 123456789012345678901 0\n",
            b"FH1 P 1 16777217\n",
            b"FH1 P 1 99999999999\n",
            b"FH1 P 1 4294967296\n",
        ];
        for case in cases {
            let e = framing_error(case);
            assert!(e.to_string().starts_with("framing error: "), "{e}");
        }
    }

    #[test]
    fn errors_are_detected_before_the_line_ends() {
        // A garbage stream with no LF must not leave the decoder waiting for 64 bytes.
        for case in [&b"X"[..], b"FH1 P 00", b"FH1 P 1x", b"FH1 #"] {
            let mut d = Decoder::new();
            d.feed(case);
            assert!(d.next_request().is_err(), "{case:?}");
        }
    }

    #[test]
    fn header_length_limit() {
        // The largest legal header is far below the limit, so the digit limits trip first; a
        // header that keeps going is rejected at the latest once 64 bytes have arrived.
        let mut long = b"FH1 P 1".to_vec();
        long.extend(std::iter::repeat_n(b'1', 80));
        let mut d = Decoder::new();
        for b in &long {
            d.feed(std::slice::from_ref(b));
            if d.next_request().is_err() {
                assert!(d.pending() <= MAX_HEADER);
                return;
            }
        }
        panic!("long header accepted");
    }

    #[test]
    fn boundary_values_accepted() {
        let max_id = b"FH1 P 18446744073709551615 0\n";
        assert_eq!(decode_one(max_id), Request::Ping { id: u64::MAX });
        // A maximal body length is legal in the header; the decoder just waits for the body.
        incomplete(b"FH1 X 1 16777216\n");
    }

    #[test]
    fn framing_error_is_sticky() {
        let mut d = Decoder::new();
        d.feed(b"BAD");
        assert!(d.next_request().is_err());
        d.feed(&encode_ping(1));
        assert!(d.next_request().is_err());
    }

    #[test]
    fn unknown_type_is_invalid_and_skips_body() {
        let mut bytes = encode_frame(b'X', 4, b"whatever\nbytes");
        bytes.extend(encode_ping(5));
        let reqs = decode_all(&bytes).unwrap();
        assert!(matches!(&reqs[0], Request::Invalid { id: 4, reason } if reason.contains('X')));
        assert_eq!(reqs[1], Request::Ping { id: 5 });
        // Response letters and lowercase are unknown request types too.
        for kind in [b'R', b'A', b'E', b'h'] {
            assert!(matches!(
                decode_one(&encode_frame(kind, 1, b"")),
                Request::Invalid { id: 1, .. }
            ));
        }
    }

    #[test]
    fn ping_and_quit_ignore_bodies() {
        assert_eq!(
            decode_one(&encode_frame(b'P', 1, b"x")),
            Request::Ping { id: 1 }
        );
        assert_eq!(
            decode_one(&encode_frame(b'Q', 2, b"y")),
            Request::Quit { id: 2 }
        );
    }

    fn body_is_invalid(kind: u8, body: &[u8]) {
        let mut bytes = encode_frame(kind, 9, body);
        bytes.extend(encode_ping(10));
        let reqs = decode_all(&bytes).unwrap();
        assert!(
            matches!(&reqs[0], Request::Invalid { id: 9, .. }),
            "{:?} -> {reqs:?}",
            String::from_utf8_lossy(body)
        );
        assert_eq!(
            reqs[1],
            Request::Ping { id: 10 },
            "stream must stay in sync"
        );
    }

    #[test]
    fn malformed_bodies_are_invalid_not_framing_errors() {
        let cases: &[&[u8]] = &[
            b"buf",
            b"buf ",
            b"buf 2",
            b"buf 2\n",
            b"buf 2\nab",
            b"buf 2\nabc",
            b"buf 2\nabc\n",
            b"buf 3\nab\n",
            b"buf 02\nab\n",
            b"buf -1\n\n",
            b"buf x\nab\n",
            b"buf\n2\nab\n",
            b" 2\nab\n",
            b"b_f 2\nab\n",
            b"buf 2\nab\nextra",
            b"buf 2\nab\n\n",
            b"buf 99999999999999999999999\nab\n",
            b"cur 0\n\n",
            b"cur 2\n01\n",
            b"cur 2\n-1\n",
            b"cur 1\nx\n",
            b"cur 20\n99999999999999999999\n",
        ];
        for body in cases {
            body_is_invalid(b'H', body);
        }
        body_is_invalid(b'S', b"alias 2\nab");
    }

    #[test]
    fn invalid_body_keeps_id() {
        let Request::Invalid { id, reason } = decode_one(&encode_frame(b'H', 42, b"oops")) else {
            panic!("expected invalid");
        };
        assert_eq!(id, 42);
        assert!(!reason.is_empty());
    }

    #[test]
    fn unknown_fields_ignored_and_last_duplicate_wins() {
        let body = b"buf 1\na\nfuture-field2 3\nx\0y\nbuf 2\nbc\ncur 1\n1\ncur 1\n2\n";
        let Request::Highlight { fields, .. } = decode_one(&encode_frame(b'H', 1, body)) else {
            panic!("expected highlight");
        };
        assert_eq!(fields.buffer, b"bc");
        assert_eq!(fields.cursor, Some(2));
    }

    #[test]
    fn field_values_may_contain_any_bytes() {
        let body = b"buf 5\na\nb\n\0\n";
        let Request::Highlight { fields, .. } = decode_one(&encode_frame(b'H', 1, body)) else {
            panic!("expected highlight");
        };
        assert_eq!(fields.buffer, b"a\nb\n\0");
    }

    #[test]
    fn empty_cwd_and_opts_are_present() {
        let body = b"cwd 0\n\nopt 0\n\n";
        let Request::Highlight { fields, .. } = decode_one(&encode_frame(b'H', 1, body)) else {
            panic!("expected highlight");
        };
        assert_eq!(fields.cwd, Some(Vec::new()));
        assert_eq!(fields.opts, "");
        assert_eq!(fields.cursor, None);
    }

    fn state(body: &[u8]) -> StateUpdate {
        match decode_one(&encode_frame(b'S', 1, body)) {
            Request::State {
                update, warnings, ..
            } => {
                assert_eq!(warnings, Vec::<String>::new());
                update
            }
            other => panic!("expected state, got {other:?}"),
        }
    }

    fn state_with_warnings(body: &[u8]) -> (StateUpdate, Vec<String>) {
        match decode_one(&encode_frame(b'S', 1, body)) {
            Request::State {
                update, warnings, ..
            } => (update, warnings),
            other => panic!("expected state, got {other:?}"),
        }
    }

    #[test]
    fn list_values() {
        let s = |v: &[&str]| Some(v.iter().map(|e| e.to_string()).collect::<Vec<_>>());
        assert_eq!(state(b"alias 0\n\n").aliases, s(&[]));
        assert_eq!(state(b"alias 1\n\0\n").aliases, s(&[""]));
        assert_eq!(state(b"alias 2\nab\n").aliases, s(&["ab"]));
        assert_eq!(state(b"alias 3\nab\0\n").aliases, s(&["ab"]));
        assert_eq!(state(b"alias 4\na\0\0b\n").aliases, s(&["a", "", "b"]));
        assert_eq!(state(b"func 4\na\0b\0\n").functions, s(&["a", "b"]));
        assert_eq!(state(b"func 5\na\0b\0\0\n").functions, s(&["a", "b", ""]));
    }

    /// `docs/protocol.md`: each entry is terminated by NUL, a final unterminated entry is
    /// accepted, and an empty value is an empty list.
    #[test]
    fn parse_list_matches_the_spec() {
        let cases: &[(&[u8], &[&str])] = &[
            (b"", &[]),
            (b"\0", &[""]),
            (b"\0\0", &["", ""]),
            (b"a", &["a"]),
            (b"a\0", &["a"]),
            (b"a\0b", &["a", "b"]),
            (b"a\0b\0", &["a", "b"]),
            (b"a\0\0", &["a", ""]),
            (b"\0a\0", &["", "a"]),
            (b"a\0\0b\0", &["a", "", "b"]),
            (b"\xff\0", &["\u{FFFD}"]),
        ];
        for (value, want) in cases {
            assert_eq!(parse_list(value), *want, "{value:?}");
        }
    }

    #[test]
    fn named_dir_with_empty_directory_is_a_pair() {
        // The plugin terminates every entry, so an empty directory is an empty final entry.
        assert_eq!(
            state(b"nameddirs 8\nd\0/x\0e\0\0\n").named_dirs,
            Some(vec![
                ("d".to_string(), "/x".to_string()),
                ("e".to_string(), String::new())
            ])
        );
    }

    #[test]
    fn malformed_named_dirs_are_skipped_and_the_rest_applies() {
        let body = b"alias 3\nll\0\nnameddirs 2\na\0\npath 4\n/bin\nrehash 0\n\n";
        let (update, warnings) = state_with_warnings(body);
        assert_eq!(update.named_dirs, None);
        assert_eq!(update.aliases, Some(vec!["ll".to_string()]));
        assert_eq!(update.path.as_deref(), Some("/bin"));
        assert!(update.rehash);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("nameddirs"), "{warnings:?}");

        let (update, warnings) = state_with_warnings(b"nameddirs 6\na\0b\0c\0\n");
        assert_eq!(update, StateUpdate::default());
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn rehash_presence_is_the_value() {
        assert!(state(b"rehash 0\n\n").rehash);
        assert!(state(b"rehash 3\nyes\n").rehash);
        assert!(!state(b"path 4\n/bin\n").rehash);
        let encoded = encode_state(
            1,
            &StateUpdate {
                rehash: true,
                ..StateUpdate::default()
            },
        );
        assert_eq!(encoded, b"FH1 S 1 10\nrehash 0\n\n");
    }

    #[test]
    fn named_dirs_pairs() {
        assert_eq!(
            state(b"nameddirs 9\nd\0/x\0e\0/y\n").named_dirs,
            Some(vec![
                ("d".to_string(), "/x".to_string()),
                ("e".to_string(), "/y".to_string())
            ])
        );
        assert_eq!(state(b"nameddirs 0\n\n").named_dirs, Some(vec![]));
    }

    #[test]
    fn absent_state_fields_stay_none() {
        let update = state(b"path 4\n/bin\n");
        assert_eq!(update.path.as_deref(), Some("/bin"));
        assert_eq!(update.aliases, None);
        assert_eq!(update.named_dirs, None);
        assert!(!update.rehash);
    }

    #[test]
    fn decoder_compacts_after_many_frames() {
        let mut d = Decoder::new();
        let ping = encode_ping(1);
        for _ in 0..20_000 {
            d.feed(&ping);
        }
        d.feed(&ping[..3]);
        let mut n = 0;
        while d.next_request().unwrap().is_some() {
            n += 1;
        }
        assert_eq!(n, 20_000);
        assert_eq!(d.pending(), 3);
        d.feed(&ping[3..]);
        assert_eq!(d.next_request().unwrap(), Some(Request::Ping { id: 1 }));
    }

    #[test]
    fn oversized_result_is_truncated_to_a_valid_frame() {
        let span = WireSpan {
            start: 1_000_000_000,
            end: 1_000_000_001,
            kind: TokenKind::ProcessSubstitution,
        };
        let spans = vec![span; MAX_BODY / 30 + 10];
        let frame = encode_result(1, &spans);
        let mut d = Decoder::new();
        d.feed(&frame);
        let f = d.next_frame().unwrap().unwrap();
        assert!(f.body.len() <= MAX_BODY);
        assert!(f.body.ends_with(b"\n"));
    }

    #[test]
    fn request_id_accessor() {
        for req in sample_requests() {
            let bytes = encode_request(&req).unwrap();
            let mut d = Decoder::new();
            d.feed(&bytes);
            assert_eq!(d.next_frame().unwrap().unwrap().id, req.id());
        }
    }
}
