# Wire protocol

The zsh plugin and the `fast-highlight serve` daemon exchange framed messages over a pair of byte
streams: the daemon reads requests on its standard input and writes responses on its standard
output. This document is the normative specification of that exchange. The key words `MUST`,
`MUST NOT`, `SHOULD`, `SHOULD NOT`, and `MAY` carry their RFC 2119 meaning.

## Conventions

- All numbers are unsigned ASCII decimal with no sign, no leading `+`, and no leading zeros
  (except the number `0` itself).
- `SP` is a single space (0x20) and `LF` is a single line feed (0x0A).
- Lengths count bytes, never characters.
- Text in bodies is raw bytes. The daemon decodes it as UTF-8 and treats each byte of an invalid
  sequence as one replacement character.

## Frame format

Every message in either direction is one **frame**: a header line followed by a body.

```
frame   = header LF body
header  = magic SP type SP id SP length
magic   = "FH1"
type    = one ASCII letter
id      = 1*20DIGIT
length  = 1*10DIGIT
```

- `id` is a request identifier in the range of an unsigned 64-bit integer.
- `length` is the exact byte length of `body`. A `length` above 16777216 (16 MiB) is a framing
  error.
- The header, including `LF`, is at most 64 bytes. A longer header is a framing error.

A receiver that meets a framing error cannot resynchronise the stream. The daemon exits on a
framing error; the plugin treats one as a daemon failure.

### Request types

| Type | Name      | Body                | Response        |
|------|-----------|---------------------|-----------------|
| `H`  | highlight | fields              | `R` or `E`      |
| `S`  | state     | fields              | `A` or `E`      |
| `P`  | ping      | empty               | `A`             |
| `Q`  | quit      | empty               | none            |

### Response types

| Type | Name   | Body                         |
|------|--------|------------------------------|
| `R`  | result | span lines                   |
| `A`  | ack    | empty                        |
| `E`  | error  | UTF-8 diagnostic text        |

A response carries the `id` of the request it answers. The daemon answers requests in the order it
receives them and answers every request except `Q`. An unknown request type receives an `E`
response.

## Fields

The bodies of `H` and `S` requests are a sequence of zero or more fields.

```
body    = *field
field   = name SP length LF value LF
name    = 1*( ALPHA / DIGIT / "-" )
```

`length` is the byte length of `value`, which is arbitrary bytes. The `LF` after `value` is
required and is not part of the value. A body whose fields do not exactly fill `length` bytes is
a malformed request, which receives an `E` response; it is not a framing error, because the frame
boundary is still known.

Unknown field names are ignored. When a field name repeats, the last occurrence wins.

### List values

A list value is a sequence of entries, each terminated by a NUL byte (0x00). A final entry with no
terminating NUL is accepted. An empty value is an empty list.

## Highlight request

An `H` request asks for the spans of one editor state.

| Field | Value                                                                    | Default    |
|-------|--------------------------------------------------------------------------|------------|
| `buf` | The contents of `BUFFER`.                                                | empty      |
| `pre` | The contents of `PREBUFFER` (earlier lines of a multi-line command).     | empty      |
| `cur` | The cursor position (`CURSOR`), as a number in the request's offset unit. | end of buf |
| `cwd` | The shell's current directory (`$PWD`).                                  | unchanged  |
| `opt` | A string of option letters, described below.                             | empty      |

The daemon remembers the last `cwd` it received and uses it when a request omits the field. The
plugin `SHOULD` send `cwd` only when it changes.

The `opt` field holds one letter per enabled option:

| Letter | Meaning                                                                         |
|--------|---------------------------------------------------------------------------------|
| `u`    | Offsets are in characters (zsh `MULTIBYTE` with a UTF-8 locale). Absent: bytes.  |
| `c`    | `INTERACTIVE_COMMENTS` is set.                                                  |
| `a`    | `AUTO_CD` is set.                                                               |
| `e`    | `EXTENDED_GLOB` is set.                                                         |
| `k`    | `KSH_GLOB` is set.                                                              |

Unknown letters are ignored.

The daemon parses `pre` followed immediately by `buf` as one text, so syntax that starts in an
earlier line continues into the current one. It reports only the parts of spans that fall inside
`buf`.

## Result response

An `R` body is zero or more span lines.

```
body    = *span
span    = start SP end SP kind LF
kind    = 1*( ALPHA / "-" )
```

- `start` and `end` are a half-open range relative to the start of `buf`, in the offset unit the
  request selected with the `u` option.
- `kind` is a token type wire name. The set of names is defined by `TokenKind::name` in
  `src/token.rs`. A receiver `MUST` ignore a kind it does not know.
- Spans are sorted by ascending `start` and then descending `end`. Any two spans are either
  disjoint or nested. A later span takes precedence over an earlier span that contains it.

When the text exceeds the hard size limit, the body is empty.

## State request

An `S` request updates the daemon's copy of the shell's command namespace. Every field is
optional; an absent field leaves that part of the state unchanged, and a present field replaces it
entirely.

| Field       | Value                                                              |
|-------------|--------------------------------------------------------------------|
| `alias`     | List of regular alias names.                                       |
| `galias`    | List of global alias names.                                        |
| `salias`    | List of suffix alias names (the suffix without the dot).           |
| `func`      | List of function names.                                            |
| `builtin`   | List of builtin names.                                             |
| `reswords`  | List of reserved words.                                            |
| `nameddirs` | List of alternating entries: a name, then the directory it names.  |
| `path`      | The value of `$PATH`.                                              |

When `path` changes, the daemon rescans the listed directories. The daemon also rescans when the
modification time of any listed directory changes, checking at most once per second.

## Request identifiers and stale responses

The plugin assigns each request an `id` greater than that of any request it sent before. While it
waits for the response to request `n`, it discards every frame whose `id` is less than `n`. A
response that arrives after its request timed out is therefore never applied to a newer buffer.

## Connection lifetime

The daemon exits when any of the following occurs:

- it receives a `Q` request,
- its standard input reaches end of file,
- its parent process exits, or the process named by `serve --parent PID` no longer exists (the
  daemon checks both at least once per second), or
- it meets a framing error.

The daemon writes nothing to its standard output except response frames.

## Example

A highlight request for the buffer `ls ~` with the cursor at the end, offsets in characters:

```
FH1 H 7 27
buf 4
ls ~
cur 1
4
opt 1
u
```

The body is the three fields, 27 bytes in total. A matching response:

```
FH1 R 7 31
0 2 command
3 4 path-directory
```
