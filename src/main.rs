//! The `fast-highlight` command-line interface.

use fast_highlight::config::{Config, config_dir};
use fast_highlight::daemon::{self, Engine, Log, ServeOptions, Session};
use fast_highlight::protocol::{HighlightFields, StateUpdate, WireSpan};
use fast_highlight::specs::SpecRegistry;
use fast_highlight::text;
use fast_highlight::theme::Theme;
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: fast-highlight <command> [options]

commands:
  serve [--timing] [--log PATH] [--parent PID]
      Run the daemon on standard input and output (started by the zsh plugin).
      It exits when its parent process exits, and with --parent also when
      process PID no longer exists (the plugin passes the shell's $$).
  styles
      Print the active theme as zsh code for the plugin's style table.
  highlight [--cwd DIR] [--opts LETTERS] [--timing] [--repeat N]
            [--format spans|ansi] [--] [TEXT...]
      Highlight TEXT (the arguments joined by spaces, or standard input without
      its final newline) and print one 'start end kind \"text\"' line per span.
      --opts takes the protocol's option letters and defaults to 'u'
      (character offsets). --format ansi prints the text in color instead.
      --timing prints min/median/max processing time over --repeat runs to
      standard error.
  check-config
      Load the config, theme, and command specs and report problems.
  plugin-path
      Print the path of the zsh plugin file.
  help, --help, -h
      Print this help.
  --version, -V
      Print the version.
";

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    std::process::exit(run(&args));
}

fn run(args: &[OsString]) -> i32 {
    match parse_args(args) {
        Ok(command) => execute(command),
        Err(message) => {
            eprintln!("fast-highlight: {message}");
            eprint!("{USAGE}");
            2
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Spans,
    Ansi,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HighlightArgs {
    cwd: Option<PathBuf>,
    opts: String,
    timing: bool,
    repeat: usize,
    format: Format,
    /// The words to highlight, joined by spaces; `None` reads standard input.
    text: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Serve(ServeOptions),
    Styles,
    Highlight(HighlightArgs),
    CheckConfig,
    PluginPath,
    Version,
    Help,
}

fn parse_args(args: &[OsString]) -> Result<Command, String> {
    let Some((first, rest)) = args.split_first() else {
        return Err("missing command".to_string());
    };
    let first = first.to_string_lossy();
    if rest.iter().any(|a| a == "--help" || a == "-h") && first != "highlight" {
        return Ok(Command::Help);
    }
    let no_args = |command: Command| {
        if let Some(extra) = rest.first() {
            Err(format!("unexpected argument '{}'", extra.to_string_lossy()))
        } else {
            Ok(command)
        }
    };
    match first.as_ref() {
        "serve" => parse_serve(rest).map(Command::Serve),
        "styles" => no_args(Command::Styles),
        "highlight" => parse_highlight(rest),
        "check-config" => no_args(Command::CheckConfig),
        "plugin-path" => no_args(Command::PluginPath),
        "--version" | "-V" => no_args(Command::Version),
        "help" | "--help" | "-h" => Ok(Command::Help),
        other => Err(format!("unknown command '{other}'")),
    }
}

/// Returns the value following option `name`, advancing the iterator past it.
fn option_value<'a>(
    name: &str,
    it: &mut impl Iterator<Item = &'a OsString>,
) -> Result<&'a OsString, String> {
    it.next().ok_or_else(|| format!("{name} needs a value"))
}

fn parse_serve(args: &[OsString]) -> Result<ServeOptions, String> {
    let mut opts = ServeOptions::default();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.to_string_lossy().as_ref() {
            "--timing" => opts.timing = true,
            "--log" => opts.log = Some(PathBuf::from(option_value("--log", &mut it)?)),
            "--parent" => {
                let value = option_value("--parent", &mut it)?.to_string_lossy();
                // Zero and negative ids name process groups for kill(2); reject them.
                let pid = value.parse().ok().filter(|&p: &libc::pid_t| p > 0);
                opts.parent =
                    Some(pid.ok_or_else(|| format!("--parent needs a process id, not '{value}'"))?);
            }
            other => return Err(format!("unknown serve argument '{other}'")),
        }
    }
    Ok(opts)
}

fn parse_highlight(args: &[OsString]) -> Result<Command, String> {
    let mut parsed = HighlightArgs {
        cwd: None,
        opts: "u".to_string(),
        timing: false,
        repeat: 1,
        format: Format::Spans,
        text: None,
    };
    let mut words: Vec<&OsStr> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let lossy = arg.to_string_lossy();
        match lossy.as_ref() {
            "--" => {
                words.extend(it.by_ref().map(OsString::as_os_str));
                break;
            }
            "--help" | "-h" if words.is_empty() => return Ok(Command::Help),
            "--cwd" => parsed.cwd = Some(PathBuf::from(option_value("--cwd", &mut it)?)),
            "--opts" => {
                parsed.opts = option_value("--opts", &mut it)?
                    .to_string_lossy()
                    .into_owned();
            }
            "--timing" => parsed.timing = true,
            "--repeat" => {
                let value = option_value("--repeat", &mut it)?.to_string_lossy();
                parsed.repeat =
                    value.parse().ok().filter(|&n| n > 0).ok_or_else(|| {
                        format!("--repeat needs a positive number, not '{value}'")
                    })?;
            }
            "--format" => {
                parsed.format = match option_value("--format", &mut it)?
                    .to_string_lossy()
                    .as_ref()
                {
                    "spans" => Format::Spans,
                    "ansi" => Format::Ansi,
                    other => return Err(format!("unknown format '{other}'")),
                };
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown highlight option '{other}'"));
            }
            _ => words.push(arg),
        }
    }
    if !words.is_empty() {
        parsed.text = Some(words.join(OsStr::new(" ")).into_vec());
    }
    Ok(Command::Highlight(parsed))
}

fn execute(command: Command) -> i32 {
    match command {
        Command::Serve(opts) => daemon::serve(opts),
        Command::Styles => cmd_styles(),
        Command::Highlight(args) => cmd_highlight(args),
        Command::CheckConfig => cmd_check_config(),
        Command::PluginPath => cmd_plugin_path(),
        Command::Version => {
            println!("fast-highlight {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Command::Help => {
            print!("{USAGE}");
            0
        }
    }
}

/// Loads the config, reporting an error on standard error and falling back to the defaults.
/// The flag is true when loading failed.
fn load_config() -> (Config, bool) {
    match Config::load() {
        Ok(config) => (config, false),
        Err(e) => {
            eprintln!("fast-highlight: config: {e}");
            (Config::default(), true)
        }
    }
}

fn cmd_styles() -> i32 {
    let (config, mut failed) = load_config();
    let theme = match Theme::load(&config) {
        Ok(theme) => theme,
        Err(e) => {
            eprintln!("fast-highlight: theme: {e}; using the default theme");
            failed = true;
            Theme::default_theme()
        }
    };
    print!("{}", theme.to_zsh());
    i32::from(failed)
}

fn cmd_check_config() -> i32 {
    let dir = config_dir();
    println!("config directory: {}", dir.display());
    let mut errors = 0;
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("error: config: {e}");
            errors += 1;
            Config::default()
        }
    };
    if let Err(e) = Theme::load(&config) {
        eprintln!("error: theme: {e}");
        errors += 1;
    }
    let (_, warnings) = SpecRegistry::load(&dir);
    for warning in &warnings {
        eprintln!("warning: specs: {warning}");
    }
    if errors == 0 && warnings.is_empty() {
        println!("ok");
    } else {
        println!("{errors} error(s), {} warning(s)", warnings.len());
    }
    i32::from(errors > 0)
}

fn cmd_plugin_path() -> i32 {
    const FILE: &str = "fast-highlight.plugin.zsh";
    let mut candidates = Vec::new();
    if let Some(share) = std::env::var_os("FAST_HIGHLIGHT_SHARE").filter(|s| !s.is_empty()) {
        candidates.push(PathBuf::from(share).join(FILE));
    }
    if let Some(bin_dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
    {
        candidates.push(bin_dir.join("../share/fast-highlight").join(FILE));
    }
    candidates.push(PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/plugin/fast-highlight.plugin.zsh"
    )));
    match candidates.into_iter().find(|p| p.is_file()) {
        Some(path) => {
            let mut line = path.into_os_string().into_vec();
            line.push(b'\n');
            let _ = io::stdout().write_all(&line);
            0
        }
        None => {
            eprintln!("fast-highlight: plugin file {FILE} not found");
            1
        }
    }
}

fn cmd_highlight(args: HighlightArgs) -> i32 {
    let input = match args.text {
        Some(text) => text,
        None => {
            let mut input = Vec::new();
            if let Err(e) = io::stdin().read_to_end(&mut input) {
                eprintln!("fast-highlight: reading standard input: {e}");
                return 1;
            }
            if input.last() == Some(&b'\n') {
                input.pop();
            }
            input
        }
    };
    let cwd = match args.cwd {
        Some(dir) => dir,
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
    };
    let mut opts = args.opts;
    if args.format == Format::Ansi && !opts.contains('u') {
        // Rendering works in characters.
        opts.push('u');
    }

    let (config, _) = load_config();
    let (mut highlighter, warnings) = daemon::new_highlighter(&config);
    for warning in warnings {
        eprintln!("fast-highlight: specs: {warning}");
    }
    highlighter.update_state(StateUpdate {
        path: Some(std::env::var("PATH").unwrap_or_default()),
        ..StateUpdate::default()
    });
    let hard_cap = config.limits.hard_cap_bytes;
    let mut session = Session::new(highlighter, hard_cap, cwd, Log::disabled());
    let fields = HighlightFields {
        buffer: input,
        opts,
        ..HighlightFields::default()
    };

    let mut times = Vec::with_capacity(args.repeat);
    let mut spans = Vec::new();
    for _ in 0..args.repeat {
        let started = Instant::now();
        let result = session.highlight(&fields);
        times.push(started.elapsed());
        match result {
            Ok(s) => spans = s,
            Err(message) => {
                eprintln!("fast-highlight: {message}");
                return 1;
            }
        }
    }

    let units = UnitText::new(&fields.buffer, fields.char_offsets());
    let output = match args.format {
        Format::Spans => format_spans(&units, &spans),
        Format::Ansi => {
            let theme = Theme::load(&config).unwrap_or_else(|e| {
                eprintln!("fast-highlight: theme: {e}; using the default theme");
                Theme::default_theme()
            });
            render_ansi(&units, &spans, &theme)
        }
    };
    let _ = io::stdout().write_all(output.as_bytes());
    if args.timing {
        eprintln!("{}", timing_summary(&mut times));
    }
    0
}

fn timing_summary(times: &mut [Duration]) -> String {
    times.sort_unstable();
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    let (min, max) = (times[0], times[times.len() - 1]);
    let median = times[times.len() / 2];
    format!(
        "runs={} min={:.1}us median={:.1}us max={:.1}us",
        times.len(),
        us(min),
        us(median),
        us(max)
    )
}

/// The buffer split into wire units, for slicing by wire offsets.
struct UnitText {
    /// One string per unit: a character, or (in byte units) the decoding of one byte, which is
    /// empty for a non-first byte of a multibyte character.
    units: Vec<String>,
}

impl UnitText {
    fn new(buffer: &[u8], chars: bool) -> UnitText {
        let units = if chars {
            text::decode(buffer).chars().map(String::from).collect()
        } else {
            let decoded = text::decode(buffer);
            let mut units = Vec::with_capacity(buffer.len());
            let mut rest = buffer;
            for c in decoded.chars() {
                // A replacement character stands for one byte unless the input held a real one.
                let width = if c == '\u{FFFD}' && !rest.starts_with("\u{FFFD}".as_bytes()) {
                    1
                } else {
                    c.len_utf8()
                };
                units.push(c.to_string());
                units.extend(std::iter::repeat_n(String::new(), width - 1));
                rest = rest.get(width..).unwrap_or_default();
            }
            units
        };
        UnitText { units }
    }

    fn slice(&self, start: usize, end: usize) -> String {
        let end = end.min(self.units.len());
        self.units[start.min(end)..end].concat()
    }
}

fn format_spans(units: &UnitText, spans: &[WireSpan]) -> String {
    spans
        .iter()
        .map(|s| {
            let text = units.slice(s.start, s.end);
            format!("{} {} {} {text:?}\n", s.start, s.end, s.kind)
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Color {
    Default,
    Basic(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    underline: bool,
    standout: bool,
}

/// Parses a zsh highlight spec such as `fg=red,bold` or `fg=#ff8800,bg=236,underline`.
/// Unrecognised parts are ignored.
fn parse_style(spec: &str) -> Style {
    let mut style = Style::default();
    for part in spec.split(',').map(str::trim) {
        if let Some(color) = part.strip_prefix("fg=") {
            style.fg = parse_color(color);
        } else if let Some(color) = part.strip_prefix("bg=") {
            style.bg = parse_color(color);
        } else {
            match part {
                "bold" => style.bold = true,
                "underline" => style.underline = true,
                "standout" => style.standout = true,
                "none" => style = Style::default(),
                _ => {}
            }
        }
    }
    style
}

fn parse_color(name: &str) -> Option<Color> {
    const NAMES: [&str; 8] = [
        "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
    ];
    if name == "default" {
        return Some(Color::Default);
    }
    if let Some(i) = NAMES.iter().position(|&n| n == name) {
        return Some(Color::Basic(i as u8));
    }
    if let Some(hex) = name.strip_prefix('#') {
        if hex.len() != 6 || !hex.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some(Color::Rgb(byte(0)?, byte(2)?, byte(4)?));
    }
    name.parse::<u8>().ok().map(Color::Indexed)
}

/// Applies `top` over `base`: colors it sets replace, attributes accumulate.
fn overlay(base: &mut Style, top: &Style) {
    base.fg = top.fg.or(base.fg);
    base.bg = top.bg.or(base.bg);
    base.bold |= top.bold;
    base.underline |= top.underline;
    base.standout |= top.standout;
}

fn sgr(style: &Style) -> String {
    let mut codes = vec!["0".to_string()];
    if style.bold {
        codes.push("1".into());
    }
    if style.underline {
        codes.push("4".into());
    }
    if style.standout {
        codes.push("7".into());
    }
    for (color, base) in [(style.fg, 30), (style.bg, 40)] {
        match color {
            None => {}
            Some(Color::Default) => codes.push((base + 9).to_string()),
            Some(Color::Basic(n)) => codes.push((base + u32::from(n)).to_string()),
            Some(Color::Indexed(n)) => codes.push(format!("{};5;{n}", base + 8)),
            Some(Color::Rgb(r, g, b)) => codes.push(format!("{};2;{r};{g};{b}", base + 8)),
        }
    }
    format!("\x1b[{}m", codes.join(";"))
}

/// Renders the buffer with ANSI colors: each span's style is applied over the spans before it,
/// as zsh applies `region_highlight` entries in order.
fn render_ansi(units: &UnitText, spans: &[WireSpan], theme: &Theme) -> String {
    let mut styles = vec![Style::default(); units.units.len()];
    for span in spans {
        let Some(spec) = theme.styles.get(&span.kind) else {
            continue;
        };
        let style = parse_style(spec);
        let end = span.end.min(styles.len());
        for slot in &mut styles[span.start.min(end)..end] {
            overlay(slot, &style);
        }
    }
    let mut out = String::new();
    let mut current = Style::default();
    for (unit, style) in units.units.iter().zip(&styles) {
        if *style != current {
            out.push_str(&sgr(style));
            current = *style;
        }
        out.push_str(unit);
    }
    out.push_str("\x1b[0m\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fast_highlight::token::TokenKind;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn parse(list: &[&str]) -> Result<Command, String> {
        parse_args(&args(list))
    }

    #[test]
    fn simple_commands() {
        assert_eq!(parse(&["styles"]), Ok(Command::Styles));
        assert_eq!(parse(&["check-config"]), Ok(Command::CheckConfig));
        assert_eq!(parse(&["plugin-path"]), Ok(Command::PluginPath));
        assert_eq!(parse(&["--version"]), Ok(Command::Version));
        assert_eq!(parse(&["-V"]), Ok(Command::Version));
        assert_eq!(parse(&["help"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["styles", "--help"]), Ok(Command::Help));
    }

    #[test]
    fn usage_errors() {
        assert!(parse(&[]).is_err());
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["styles", "extra"]).is_err());
        assert!(parse(&["serve", "--bogus"]).is_err());
        assert!(parse(&["serve", "--log"]).is_err());
        assert!(parse(&["serve", "--parent"]).is_err());
        for bad in ["0", "-1", "x", "", "99999999999"] {
            assert!(parse(&["serve", "--parent", bad]).is_err(), "{bad}");
        }
        assert!(parse(&["highlight", "--repeat", "0"]).is_err());
        assert!(parse(&["highlight", "--repeat", "x"]).is_err());
        assert!(parse(&["highlight", "--format", "html"]).is_err());
        assert!(parse(&["highlight", "--nope"]).is_err());
        assert!(parse(&["highlight", "--cwd"]).is_err());
    }

    #[test]
    fn serve_options() {
        assert_eq!(
            parse(&["serve"]),
            Ok(Command::Serve(ServeOptions::default()))
        );
        assert_eq!(
            parse(&["serve", "--log", "/tmp/x.log", "--timing"]),
            Ok(Command::Serve(ServeOptions {
                timing: true,
                log: Some(PathBuf::from("/tmp/x.log")),
                parent: None,
            }))
        );
        assert_eq!(
            parse(&["serve", "--parent", "4242"]),
            Ok(Command::Serve(ServeOptions {
                parent: Some(4242),
                ..ServeOptions::default()
            }))
        );
    }

    #[test]
    fn highlight_options() {
        let Ok(Command::Highlight(h)) = parse(&[
            "highlight",
            "--cwd",
            "/tmp",
            "--opts",
            "ce",
            "--timing",
            "--repeat",
            "50",
            "--format",
            "ansi",
            "ls",
            "-l",
            "--",
            "--x",
        ]) else {
            panic!("expected highlight");
        };
        assert_eq!(h.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(h.opts, "ce");
        assert!(h.timing);
        assert_eq!(h.repeat, 50);
        assert_eq!(h.format, Format::Ansi);
        assert_eq!(h.text.as_deref(), Some(&b"ls -l --x"[..]));

        let Ok(Command::Highlight(h)) = parse(&["highlight"]) else {
            panic!("expected highlight");
        };
        assert_eq!(h.text, None);
        assert_eq!(h.opts, "u");
        assert_eq!(h.repeat, 1);
        assert_eq!(h.format, Format::Spans);
        assert_eq!(parse(&["highlight", "--help"]), Ok(Command::Help));
    }

    #[test]
    fn style_parsing() {
        assert_eq!(
            parse_style("fg=red,bold"),
            Style {
                fg: Some(Color::Basic(1)),
                bold: true,
                ..Style::default()
            }
        );
        assert_eq!(
            parse_style("fg=#ff8800,bg=236,underline,standout"),
            Style {
                fg: Some(Color::Rgb(255, 136, 0)),
                bg: Some(Color::Indexed(236)),
                underline: true,
                standout: true,
                ..Style::default()
            }
        );
        assert_eq!(parse_style("none"), Style::default());
        assert_eq!(parse_style("fg=#zz0000,fg=300,blink").fg, None);
        assert_eq!(parse_style("bg=default").bg, Some(Color::Default));
    }

    #[test]
    fn sgr_codes() {
        let style = parse_style("fg=green,bg=#010203,bold");
        assert_eq!(sgr(&style), "\x1b[0;1;32;48;2;1;2;3m");
        assert_eq!(sgr(&parse_style("fg=208")), "\x1b[0;38;5;208m");
    }

    #[test]
    fn unit_text_slicing() {
        let chars = UnitText::new("a日\u{1F389}b".as_bytes(), true);
        assert_eq!(chars.slice(1, 3), "日\u{1F389}");
        assert_eq!(chars.slice(3, 99), "b");
        let bytes = UnitText::new("a日b".as_bytes(), false);
        assert_eq!(bytes.units.len(), 5);
        assert_eq!(bytes.slice(1, 4), "日");
        let invalid = UnitText::new(b"a\xff\xef\xbf\xbdb", false);
        assert_eq!(invalid.units.len(), 6);
        assert_eq!(invalid.slice(1, 2), "\u{FFFD}");
        assert_eq!(invalid.slice(2, 5), "\u{FFFD}");
    }

    #[test]
    fn ansi_rendering_layers_spans() {
        let theme = Theme {
            styles: [
                (TokenKind::DoubleQuoted, "fg=yellow".to_string()),
                (TokenKind::Parameter, "fg=cyan,bold".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let units = UnitText::new(b"\"$x\"", true);
        let spans = [
            WireSpan {
                start: 0,
                end: 4,
                kind: TokenKind::DoubleQuoted,
            },
            WireSpan {
                start: 1,
                end: 3,
                kind: TokenKind::Parameter,
            },
        ];
        assert_eq!(
            render_ansi(&units, &spans, &theme),
            "\x1b[0;33m\"\x1b[0;1;36m$x\x1b[0;33m\"\x1b[0m\n"
        );
    }

    #[test]
    fn timing_summary_reports_median() {
        let mut times = [30, 10, 20].map(Duration::from_micros);
        assert_eq!(
            timing_summary(&mut times),
            "runs=3 min=10.0us median=20.0us max=30.0us"
        );
    }
}
