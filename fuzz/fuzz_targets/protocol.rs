//! Fuzzes the protocol decoder.
//!
//! Byte 0 selects the mode by its low bit.
//!
//! - Even: stream mode. Byte 1 seeds the chunk sizes; the rest is a raw byte stream. The stream
//!   is decoded three ways (all at once, in pseudo-random chunks of 1 to 64 bytes, and one byte
//!   at a time), and all three must yield the same sequence of requests and framing errors and
//!   leave the same number of bytes pending. A framing error must be sticky. Every decoded
//!   request except `Invalid` must survive `encode_request` and decoding unchanged (apart from
//!   the warnings of a `State` request, whose skipped fields are not re-encoded).
//! - Odd: round-trip mode. The rest is turned into a list of requests with `arbitrary`; they are
//!   encoded with `encode_request`, concatenated, decoded in chunks seeded by byte 0, and must
//!   come back equal and leave nothing pending.

#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use fast_highlight::protocol::{
    Decoder, FramingError, HighlightFields, Request, StateUpdate, encode_request,
};
use libfuzzer_sys::fuzz_target;

type Outcome = Result<Request, FramingError>;

/// A deterministic xorshift generator for chunk sizes.
struct Chunks(u32);

impl Chunks {
    fn new(seed: u8) -> Chunks {
        Chunks(0x9e37_79b9 ^ (u32::from(seed) << 8 | u32::from(seed)))
    }

    fn next_len(&mut self) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        // Mostly short chunks so splits land inside headers, sometimes long ones.
        if self.0 & 0x300 == 0 {
            1 + (self.0 as usize >> 10) % 64
        } else {
            1 + (self.0 as usize >> 10) % 8
        }
    }
}

/// Pulls every available request out of `d`. Stops after the first framing error, which is
/// checked to be sticky. Returns true when an error ended the stream.
fn drain(d: &mut Decoder, out: &mut Vec<Outcome>) -> bool {
    loop {
        match d.next_request() {
            Ok(Some(r)) => out.push(Ok(r)),
            Ok(None) => return false,
            Err(e) => {
                assert_eq!(d.next_request(), Err(e.clone()), "framing error not sticky");
                out.push(Err(e));
                return true;
            }
        }
    }
}

/// Decodes `bytes` fed in pieces of the lengths `lens` yields. Returns the outcomes and the
/// pending byte count (`None` when a framing error stopped decoding).
fn decode(bytes: &[u8], mut lens: impl FnMut() -> usize) -> (Vec<Outcome>, Option<usize>) {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (chunk, tail) = rest.split_at(lens().min(rest.len()));
        rest = tail;
        d.feed(chunk);
        if drain(&mut d, &mut out) {
            return (out, None);
        }
    }
    // An empty stream must still report a complete state.
    if drain(&mut d, &mut out) {
        return (out, None);
    }
    (out, Some(d.pending()))
}

fn decode_one(bytes: &[u8]) -> Request {
    let mut d = Decoder::new();
    d.feed(bytes);
    let r = d.next_request().expect("re-encoded frame is valid");
    let r = r.expect("re-encoded frame is complete");
    assert_eq!(d.pending(), 0, "re-encoded frame has trailing bytes");
    r
}

/// `r` with the warnings of a `State` request removed.
fn without_warnings(r: Request) -> Request {
    match r {
        Request::State { id, update, .. } => Request::State {
            id,
            update,
            warnings: Vec::new(),
        },
        other => other,
    }
}

fn stream_mode(seed: u8, bytes: &[u8]) {
    let whole = decode(bytes, || usize::MAX);
    let mut chunks = Chunks::new(seed);
    let chunked = decode(bytes, || chunks.next_len());
    assert_eq!(
        whole, chunked,
        "chunked decoding differs from whole decoding"
    );
    let bytewise = decode(bytes, || 1);
    assert_eq!(
        whole, bytewise,
        "byte-at-a-time decoding differs from whole decoding"
    );

    for r in whole.0.iter().flatten() {
        if let Some(frame) = encode_request(r) {
            assert_eq!(
                without_warnings(decode_one(&frame)),
                without_warnings(r.clone()),
                "request did not survive re-encoding"
            );
        }
    }
}

#[derive(Debug, Arbitrary)]
enum MirrorRequest {
    Highlight {
        id: u64,
        buffer: Vec<u8>,
        prebuffer: Vec<u8>,
        cursor: Option<usize>,
        cwd: Option<Vec<u8>>,
        opts: String,
    },
    State {
        id: u64,
        aliases: Option<Vec<String>>,
        global_aliases: Option<Vec<String>>,
        suffix_aliases: Option<Vec<String>>,
        functions: Option<Vec<String>>,
        builtins: Option<Vec<String>>,
        reserved_words: Option<Vec<String>>,
        named_dirs: Option<Vec<(String, String)>>,
        path: Option<String>,
        rehash: bool,
    },
    Ping {
        id: u64,
    },
    Quit {
        id: u64,
    },
}

/// List entries cannot contain NUL on the wire (it is the entry terminator).
fn no_nul(list: Option<Vec<String>>) -> Option<Vec<String>> {
    list.map(|v| v.into_iter().map(|s| s.replace('\0', "")).collect())
}

impl From<MirrorRequest> for Request {
    fn from(m: MirrorRequest) -> Request {
        match m {
            MirrorRequest::Highlight {
                id,
                buffer,
                prebuffer,
                cursor,
                cwd,
                opts,
            } => Request::Highlight {
                id,
                fields: HighlightFields {
                    buffer,
                    prebuffer,
                    cursor,
                    cwd,
                    opts,
                },
            },
            MirrorRequest::State {
                id,
                aliases,
                global_aliases,
                suffix_aliases,
                functions,
                builtins,
                reserved_words,
                named_dirs,
                path,
                rehash,
            } => Request::State {
                id,
                update: StateUpdate {
                    aliases: no_nul(aliases),
                    global_aliases: no_nul(global_aliases),
                    suffix_aliases: no_nul(suffix_aliases),
                    functions: no_nul(functions),
                    builtins: no_nul(builtins),
                    reserved_words: no_nul(reserved_words),
                    named_dirs: named_dirs.map(|v| {
                        v.into_iter()
                            .map(|(n, d)| (n.replace('\0', ""), d.replace('\0', "")))
                            .collect()
                    }),
                    path,
                    rehash,
                },
                warnings: Vec::new(),
            },
            MirrorRequest::Ping { id } => Request::Ping { id },
            MirrorRequest::Quit { id } => Request::Quit { id },
        }
    }
}

fn round_trip_mode(seed: u8, bytes: &[u8]) {
    let Ok(mirrors) = Vec::<MirrorRequest>::arbitrary_take_rest(Unstructured::new(bytes)) else {
        return;
    };
    let requests: Vec<Request> = mirrors.into_iter().map(Request::from).collect();
    let mut stream = Vec::new();
    for r in &requests {
        stream.extend(encode_request(r).expect("non-Invalid request encodes"));
    }
    let mut chunks = Chunks::new(seed);
    let (outcomes, pending) = decode(&stream, || chunks.next_len());
    let expected: Vec<Outcome> = requests.into_iter().map(Ok).collect();
    assert_eq!(outcomes, expected, "requests did not survive encoding");
    assert_eq!(
        pending,
        Some(0),
        "bytes left over after decoding encoded requests"
    );
}

fuzz_target!(|data: &[u8]| {
    let Some((&mode, rest)) = data.split_first() else {
        return;
    };
    if mode & 1 == 0 {
        let Some((&seed, stream)) = rest.split_first() else {
            return;
        };
        stream_mode(seed, stream);
    } else {
        round_trip_mode(mode, rest);
    }
});
