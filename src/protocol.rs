//! The framed wire protocol. `docs/protocol.md` is the normative specification.

use std::fmt;

/// Maximum body length in bytes.
pub const MAX_BODY: usize = 16 * 1024 * 1024;
/// Maximum header length in bytes, including the trailing LF.
pub const MAX_HEADER: usize = 64;

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Highlight { id: u64, fields: HighlightFields },
    State { id: u64, update: StateUpdate },
    Ping { id: u64 },
    Quit { id: u64 },
    /// A frame with an unknown type letter, or a body that failed field decoding. The daemon
    /// answers it with an `E` response.
    Invalid { id: u64, reason: String },
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

/// Incremental request decoder. Feed it bytes as they arrive and pull complete requests out.
#[derive(Debug, Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Appends received bytes.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete request, `Ok(None)` when more bytes are needed, or a framing
    /// error. Never panics on any input.
    pub fn next_request(&mut self) -> Result<Option<Request>, FramingError> {
        todo!("protocol agent")
    }
}

/// A span ready for the wire: offsets already converted to the request's unit and made
/// relative to the start of `buf`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireSpan {
    pub start: usize,
    pub end: usize,
    pub kind: crate::token::TokenKind,
}

/// Encodes an `R` response.
pub fn encode_result(id: u64, spans: &[WireSpan]) -> Vec<u8> {
    let _ = (id, spans);
    todo!("protocol agent")
}

/// Encodes an `A` response.
pub fn encode_ack(id: u64) -> Vec<u8> {
    let _ = id;
    todo!("protocol agent")
}

/// Encodes an `E` response.
pub fn encode_error(id: u64, message: &str) -> Vec<u8> {
    let _ = (id, message);
    todo!("protocol agent")
}
