use super::*;
use crate::token::{TokenKind, check_spans};
use TokenKind as K;

/// Parses and checks every structural invariant of the output.
fn parse_with(input: &str, opts: ParseOptions) -> ParseOutput {
    let out = parse(input, &opts);
    if let Err(e) = check_spans(input, &out.spans) {
        panic!("invalid spans for {input:?}: {e}");
    }
    assert_eq!(
        parse_spans(input, &opts),
        out.spans,
        "spans-only parse differs for {input:?}"
    );
    let check_word = |w: &Word| {
        assert!(w.start < w.end, "empty word {w:?} in {input:?}");
        assert!(
            w.end <= input.len(),
            "word {w:?} out of bounds in {input:?}"
        );
        assert!(
            input.is_char_boundary(w.start) && input.is_char_boundary(w.end),
            "word {w:?} not on a char boundary in {input:?}"
        );
    };
    let mut prev = 0;
    for c in &out.commands {
        assert!(!c.words.is_empty(), "command without words in {input:?}");
        assert!(
            c.words[0].start >= prev,
            "commands out of order in {input:?}"
        );
        prev = c.words[0].start;
        c.words.iter().for_each(check_word);
    }
    out.path_words.iter().for_each(check_word);
    out
}

fn p(input: &str) -> ParseOutput {
    parse_with(input, ParseOptions::default())
}

fn opts(ic: bool, eg: bool, kg: bool) -> ParseOptions {
    ParseOptions {
        interactive_comments: ic,
        extended_glob: eg,
        ksh_glob: kg,
    }
}

/// The spans as `(kind, text)` pairs.
fn spans_of(input: &str, out: &ParseOutput) -> Vec<(TokenKind, String)> {
    out.spans
        .iter()
        .map(|s| (s.kind, input[s.start..s.end].to_string()))
        .collect()
}

fn spans(input: &str) -> Vec<(TokenKind, String)> {
    spans_of(input, &p(input))
}

fn has(input: &str, kind: TokenKind, text: &str) -> bool {
    spans(input).iter().any(|(k, t)| *k == kind && t == text)
}

#[track_caller]
fn assert_has(input: &str, kind: TokenKind, text: &str) {
    let s = spans(input);
    assert!(
        s.iter().any(|(k, t)| *k == kind && t == text),
        "no {kind} {text:?} in {input:?}: {s:?}"
    );
}

#[track_caller]
fn assert_no_kind(input: &str, kind: TokenKind) {
    let s = spans(input);
    assert!(
        s.iter().all(|(k, _)| *k != kind),
        "unexpected {kind} in {input:?}: {s:?}"
    );
}

#[track_caller]
fn assert_no_error(input: &str) {
    assert_no_kind(input, K::Error);
}

/// The commands as lists of word texts.
fn cmds(input: &str) -> Vec<Vec<String>> {
    p(input)
        .commands
        .iter()
        .map(|c| {
            c.words
                .iter()
                .map(|w| input[w.start..w.end].to_string())
                .collect()
        })
        .collect()
}

fn paths(input: &str) -> Vec<String> {
    p(input)
        .path_words
        .iter()
        .map(|w| input[w.start..w.end].to_string())
        .collect()
}

/// The word of the first command at index `i`.
fn word(input: &str, i: usize) -> Word {
    p(input).commands[0].words[i].clone()
}

fn v(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// -------------------------------------------------------------------------------------------
// Basics
// -------------------------------------------------------------------------------------------

#[test]
fn empty_and_blank_input() {
    for input in ["", " ", "\t", "\n", "\n\n  \n", "\\\n"] {
        let out = p(input);
        assert!(out.commands.is_empty(), "{input:?}");
    }
}

#[test]
fn simple_command_words() {
    assert_eq!(
        cmds("git commit -m msg"),
        vec![v(&["git", "commit", "-m", "msg"])]
    );
    assert!(p("ls").spans.is_empty());
    let w = word("echo hello", 1);
    assert_eq!(w.literal.as_deref(), Some("hello"));
    assert!(!w.tilde && !w.has_glob);
}

// -------------------------------------------------------------------------------------------
// Quoting
// -------------------------------------------------------------------------------------------

#[test]
fn single_quotes() {
    assert_has("echo 'a $b'", K::SingleQuoted, "'a $b'");
    assert_no_kind("echo 'a $b'", K::Parameter);
    assert_eq!(word("echo 'a b'c", 1).literal.as_deref(), Some("a bc"));
    assert_has("echo 'open", K::SingleQuoted, "'open");
}

#[test]
fn double_quotes_with_nested_spans() {
    let input = r#"echo "a $x $(date) `id` \$ \" ${y}""#;
    assert_has(input, K::DoubleQuoted, &input[5..]);
    assert_has(input, K::Parameter, "$x");
    assert_has(input, K::Substitution, "$(");
    assert_has(input, K::Backquoted, "`id`");
    assert_has(input, K::Escape, "\\$");
    assert_has(input, K::Escape, "\\\"");
    assert_has(input, K::Parameter, "${y}");
    assert_eq!(
        cmds(input),
        vec![v(&["echo", &input[5..]]), v(&["date"]), v(&["id"])]
    );
    // Backslash before an ordinary character is not an escape inside double quotes.
    assert_no_kind(r#"echo "a\b""#, K::Escape);
    assert_eq!(word(r#"echo "a\b""#, 1).literal.as_deref(), Some("a\\b"));
    assert_eq!(word(r#"echo "x"y"#, 1).literal.as_deref(), Some("xy"));
    assert_eq!(word(r#"echo "$x""#, 1).literal, None);
}

#[test]
fn unterminated_double_quote_is_a_string() {
    let input = "echo \"abc $x";
    assert_has(input, K::DoubleQuoted, "\"abc $x");
    assert_has(input, K::Parameter, "$x");
    assert_no_error(input);
}

#[test]
fn dollar_quotes_and_escapes() {
    let input = r"echo $'a\nb\x41\u00e9\101\cA\q\''";
    assert_has(input, K::DollarQuoted, &input[5..]);
    for esc in [r"\n", r"\x41", r"\u00e9", r"\101", r"\cA", r"\q", r"\'"] {
        assert_has(input, K::Escape, esc);
    }
    assert_eq!(word(input, 1).literal.as_deref(), Some("a\nbAéA\u{1}\\q'"));
    // `$'` is not special inside double quotes.
    assert_no_kind(r#"echo "$'x'""#, K::DollarQuoted);
}

#[test]
fn backslash_escapes() {
    assert_has(r"echo \$x a\ b", K::Escape, r"\$");
    assert_has(r"echo \$x a\ b", K::Escape, r"\ ");
    assert_no_kind(r"echo \$x", K::Parameter);
    assert_eq!(word(r"echo a\ b", 1).literal.as_deref(), Some("a b"));
    // A multibyte character after a backslash is escaped whole.
    assert_has("echo \\é", K::Escape, "\\é");
    // A trailing backslash is a (partial) escape.
    assert_has("echo \\", K::Escape, "\\");
}

#[test]
fn line_continuation() {
    let input = "echo a\\\nb \\\n c";
    assert_has(input, K::Escape, "\\\n");
    assert_eq!(cmds(input), vec![v(&["echo", "a\\\nb", "c"])]);
    assert_eq!(word(input, 1).literal.as_deref(), Some("ab"));
}

// -------------------------------------------------------------------------------------------
// Parameters
// -------------------------------------------------------------------------------------------

#[test]
fn simple_parameters() {
    for param in [
        "$name", "$_x1", "$1", "$10", "$?", "$#", "$$", "$@", "$*", "$-", "$!", "$0", "$#name",
        "$+name", "$=name", "$^arr", "$~pat",
    ] {
        let input = format!("echo {param} z");
        assert_has(&input, K::Parameter, param);
    }
    assert_has("echo $arr[1] $arr[$i,-1]", K::Parameter, "$arr[1]");
    assert_has("echo $arr[1] $arr[$i,-1]", K::Parameter, "$arr[$i,-1]");
    assert_has("echo $arr[1] $arr[$i,-1]", K::Parameter, "$i");
    assert_has("echo $file:t:r.bak", K::Parameter, "$file:t:r");
    assert_has("echo $x:s/a/b/", K::Parameter, "$x:s/a/b/");
    // `:` followed by something other than a modifier is not part of the parameter.
    assert_has("echo $PATH:/usr/bin", K::Parameter, "$PATH");
    assert_has("echo $host:path", K::Parameter, "$host");
}

#[test]
fn literal_dollar() {
    for input in ["echo $", "echo $ x", "echo a$/b", "echo $+"] {
        assert_no_kind(input, K::Parameter);
    }
    assert_eq!(word("echo a$", 1).literal.as_deref(), Some("a$"));
}

#[test]
fn braced_parameters() {
    for param in [
        "${x}",
        "${(j:,:)arr}",
        "${(s: :)line}",
        "${(l:10::0:)n}",
        "${x:-default}",
        "${x/a/b}",
        "${x//a/b}",
        "${#x}",
        "${+x}",
        "${=x}",
        "${x[1,2]}",
        "${x#*/}",
        "${x:h}",
    ] {
        let input = format!("echo {param} z");
        assert_has(&input, K::Parameter, param);
    }
    assert_has("echo ${(j:}:)arr} z", K::Parameter, "${(j:}:)arr}");
}

#[test]
fn nested_parameter_expansions() {
    let input = r#"echo ${${x}[1]} ${x:-"a $y"} ${x:-$(pwd)} ${x:-{a}}"#;
    assert_has(input, K::Parameter, "${${x}[1]}");
    assert_has(input, K::Parameter, "${x}");
    assert_has(input, K::DoubleQuoted, "\"a $y\"");
    assert_has(input, K::Parameter, "$y");
    assert_has(input, K::Substitution, "$(");
    assert_has(input, K::Parameter, "${x:-{a}}");
    assert_eq!(cmds(input)[1], v(&["pwd"]));
    // Single quotes inside `${...}` within double quotes are literal.
    let input = r#"echo "${x:-it's}" next"#;
    assert_has(input, K::Parameter, "${x:-it's}");
    assert_has(input, K::DoubleQuoted, r#""${x:-it's}""#);
    assert_no_kind(input, K::SingleQuoted);
}

#[test]
fn unterminated_braced_parameter() {
    assert_has("echo ${x", K::Parameter, "${x");
    assert_no_error("echo ${x");
}

#[test]
fn multibyte_parameter_names() {
    assert_has("echo ${名前} $名前 x", K::Parameter, "${名前}");
    assert_has("echo ${名前} $名前 x", K::Parameter, "$名前");
}

// -------------------------------------------------------------------------------------------
// Substitutions
// -------------------------------------------------------------------------------------------

#[test]
fn command_substitution() {
    let input = "echo $(ls -l | wc -l) after";
    let s = spans(input);
    assert_eq!(s.iter().filter(|(k, _)| *k == K::Substitution).count(), 2);
    assert_has(input, K::Substitution, "$(");
    assert_has(input, K::Substitution, ")");
    assert_has(input, K::Separator, "|");
    assert_eq!(
        cmds(input),
        vec![
            v(&["echo", "$(ls -l | wc -l)", "after"]),
            v(&["ls", "-l"]),
            v(&["wc", "-l"])
        ]
    );
    assert_eq!(word(input, 1).literal, None);
}

#[test]
fn nested_command_substitution() {
    let input = "a $(b $(c) \"$(d)\")";
    assert_eq!(
        cmds(input),
        vec![
            v(&["a", "$(b $(c) \"$(d)\")"]),
            v(&["b", "$(c)", "\"$(d)\""]),
            v(&["c"]),
            v(&["d"])
        ]
    );
}

#[test]
fn partial_command_substitution() {
    let input = "echo $(git sta";
    assert_has(input, K::Substitution, "$(");
    assert_no_error(input);
    assert_eq!(cmds(input)[1], v(&["git", "sta"]));
}

#[test]
fn case_inside_command_substitution() {
    let input = "echo $(case x in a) echo a;; esac) z";
    assert_no_error(input);
    assert_eq!(
        cmds(input)[0],
        v(&["echo", "$(case x in a) echo a;; esac)", "z"])
    );
}

#[test]
fn backquotes() {
    let input = "echo `date +%s` x";
    assert_has(input, K::Backquoted, "`date +%s`");
    assert_eq!(
        cmds(input),
        vec![v(&["echo", "`date +%s`", "x"]), v(&["date", "+%s"])]
    );
    let input = "echo `ls \\` x`";
    assert_has(input, K::Backquoted, "`ls \\` x`");
    assert_has("echo `unterminated", K::Backquoted, "`unterminated");
}

#[test]
fn process_substitution() {
    let input = "diff <(ls a) >(cat) =(date) < <(foo)";
    for d in ["<(", ">(", "=(", ")"] {
        assert_has(input, K::ProcessSubstitution, d);
    }
    assert_has(input, K::Redirection, "<");
    assert_eq!(cmds(input)[0], v(&["diff", "<(ls a)", ">(cat)", "=(date)"]));
    assert_eq!(cmds(input).len(), 5);
}

// -------------------------------------------------------------------------------------------
// Arithmetic
// -------------------------------------------------------------------------------------------

#[test]
fn arithmetic_expansion() {
    let input = "echo $(( $x + (2 * y) )) z";
    assert_has(input, K::Arithmetic, "$(( $x + (2 * y) ))");
    assert_has(input, K::Parameter, "$x");
    assert_no_kind(input, K::Substitution);
    assert_has("echo $[1+2]", K::Arithmetic, "$[1+2]");
}

#[test]
fn dollar_paren_paren_can_be_a_subshell() {
    let input = "echo $((cd /tmp); ls)";
    assert_no_kind(input, K::Arithmetic);
    assert_has(input, K::Substitution, "$(");
    assert_has(input, K::Grouping, "(");
    assert_eq!(cmds(input)[1..], [v(&["cd", "/tmp"]), v(&["ls"])]);
}

#[test]
fn partial_arithmetic() {
    for input in ["echo $((", "echo $((1+", "echo $((1+2)", "(( i", "(( i )"] {
        assert_no_error(input);
        assert!(
            spans(input).iter().any(|(k, _)| *k == K::Arithmetic),
            "{input:?}"
        );
    }
}

#[test]
fn arithmetic_command_and_nested_subshell() {
    let input = "(( i += $n )) && echo";
    assert_has(input, K::Arithmetic, "(( i += $n ))");
    assert_has(input, K::Parameter, "$n");
    assert_eq!(cmds(input), vec![v(&["echo"])]);
    let input = "((cd /; ls) | wc)";
    assert_no_kind(input, K::Arithmetic);
    assert_no_error(input);
    assert_eq!(cmds(input), vec![v(&["cd", "/"]), v(&["ls"]), v(&["wc"])]);
}

// -------------------------------------------------------------------------------------------
// Globs
// -------------------------------------------------------------------------------------------

#[test]
fn basic_globs() {
    let input = "ls *.c file? [a-z]x **/*.rs [[:alpha:]]* [!x]";
    for g in ["*", "?", "[a-z]", "**/", "[[:alpha:]]", "[!x]"] {
        assert_has(input, K::Glob, g);
    }
    let out = p(input);
    assert!(
        out.commands[0].words[1..]
            .iter()
            .all(|w| w.has_glob && w.literal.is_none())
    );
    assert!(!out.commands[0].words[0].has_glob);
}

#[test]
fn quoted_glob_characters_are_literal() {
    let input = "ls '*' \"?\" \\[a]";
    assert_no_kind(input, K::Glob);
    let out = p(input);
    assert!(
        out.commands[0]
            .words
            .iter()
            .all(|w| !w.has_glob && w.literal.is_some())
    );
}

#[test]
fn unclosed_bracket_is_literal() {
    assert_no_kind("[ -f x ]", K::Glob);
    assert_no_kind("echo a[b", K::Glob);
    assert_eq!(cmds("[ -f x ]"), vec![v(&["[", "-f", "x", "]"])]);
    // Many unclosed brackets stay fast and literal.
    let input = format!("echo {}", "[".repeat(5000));
    assert_no_kind(&input, K::Glob);
}

#[test]
fn glob_qualifiers() {
    let input = "ls *(.) **/*.rs(N) *(om[1,3]) foo(:t)";
    for q in ["(.)", "(N)", "(om[1,3])", "(:t)"] {
        assert_has(input, K::GlobQualifier, q);
    }
    // A group with alternatives is a glob group, not a qualifier list.
    let input = "ls (a|b).txt";
    assert_has(input, K::Glob, "(");
    assert_has(input, K::Glob, "|");
    assert_has(input, K::Glob, ")");
    assert_no_kind(input, K::GlobQualifier);
    assert!(word(input, 1).has_glob);
}

#[test]
fn numeric_range_glob() {
    assert_has("ls file<1-10>.txt <->", K::Glob, "<1-10>");
    assert_has("ls file<1-10>.txt <->", K::Glob, "<->");
    assert_no_kind("ls file<1-10>.txt <->", K::Redirection);
}

#[test]
fn extended_glob_operators() {
    let o = opts(false, true, false);
    let input = "ls ^*.c *.c~foo.c a#b a## (#i)x *(#qN.) ~/x d/^y";
    let s = spans_of(input, &parse_with(input, o));
    let has = |k: TokenKind, t: &str| s.iter().any(|(kk, tt)| *kk == k && tt == t);
    for g in ["^", "~", "#", "##", "(#i)"] {
        assert!(has(K::Glob, g), "{g}: {s:?}");
    }
    assert!(has(K::GlobQualifier, "(#qN.)"), "{s:?}");
    let out = parse_with(input, o);
    let tilde_word = &out.commands[0].words[7];
    assert_eq!(&input[tilde_word.start..tilde_word.end], "~/x");
    assert!(tilde_word.tilde && !tilde_word.has_glob);
    // Without the option these are ordinary characters.
    assert_no_kind("ls ^x a#b a~b", K::Glob);
}

#[test]
fn ksh_glob_patterns() {
    let o = opts(false, false, true);
    let input = "ls @(a|b) *(x) +(y) ?(z) !(w)";
    let s = spans_of(input, &parse_with(input, o));
    for g in ["@(", "*(", "+(", "?(", "!(", ")", "|"] {
        assert!(s.iter().any(|(k, t)| *k == K::Glob && t == g), "{g}: {s:?}");
    }
    assert!(
        s.iter()
            .all(|(k, _)| *k != K::GlobQualifier && *k != K::HistoryExpansion)
    );
}

// -------------------------------------------------------------------------------------------
// Brace expansion
// -------------------------------------------------------------------------------------------

#[test]
fn brace_expansion() {
    let input = "echo {a,b} x{1..5}y {a,{b,c}} {$x,y}";
    for b in ["{a,b}", "{1..5}", "{a,{b,c}}", "{b,c}", "{$x,y}"] {
        assert_has(input, K::BraceExpansion, b);
    }
    assert_has(input, K::Parameter, "$x");
    assert_eq!(word(input, 1).literal, None);
}

#[test]
fn braces_without_separator_are_literal() {
    let input = "echo {x} {} a{b '{a,b}'";
    assert_no_kind(input, K::BraceExpansion);
    assert_eq!(word(input, 1).literal.as_deref(), Some("{x}"));
}

// -------------------------------------------------------------------------------------------
// Redirections and here-documents
// -------------------------------------------------------------------------------------------

#[test]
fn redirection_operators() {
    let input = "cmd >f 2>e 2>&1 >&- <&3 &>a &>>b >|c >!d <>g >>h {fd}>i <<<str >& j 1<k";
    for r in [
        ">", "2>", "2>&1", ">&-", "<&3", "&>", "&>>", ">|", ">!", "<>", ">>", "{fd}>", "<<<", ">&",
        "1<",
    ] {
        assert_has(input, K::Redirection, r);
    }
    assert_no_error(input);
    assert_eq!(
        paths(input),
        v(&["f", "e", "a", "b", "c", "d", "g", "h", "i", "j", "k"])
    );
    assert_eq!(cmds(input), vec![v(&["cmd"])]);
}

#[test]
fn redirection_before_command() {
    assert_eq!(cmds("> out echo hi"), vec![v(&["echo", "hi"])]);
    assert_eq!(cmds("2>/dev/null cmd"), vec![v(&["cmd"])]);
    assert_eq!(paths("2>/dev/null cmd"), v(&["/dev/null"]));
}

#[test]
fn redirection_target_fd_is_not_a_path() {
    assert!(paths("cmd >& 2").is_empty());
    assert!(paths("cmd 2>&1").is_empty());
    assert!(paths("cmd > -").is_empty());
}

#[test]
fn bad_redirections() {
    for input in [
        "echo > ;",
        "echo > | x",
        "echo >\nx",
        "echo > )",
        "echo < > f",
        "(echo >)",
    ] {
        let s = spans(input);
        assert!(
            s.iter()
                .any(|(k, t)| *k == K::Error && (t == ">" || t == "<")),
            "{input:?}: {s:?}"
        );
    }
    // Partial input: the target has not been typed yet.
    assert_no_error("echo >");
    assert_no_error("echo 2>&");
    assert_has("echo >", K::Redirection, ">");
    // A process substitution is a valid target.
    assert_no_error("cat < <(ls)");
}

#[test]
fn heredoc_bodies() {
    let input = "cat <<EOF >out\nhello $USER\nEOF\necho next";
    assert_has(input, K::Redirection, "<<");
    assert_has(input, K::Heredoc, "EOF");
    assert_has(input, K::Heredoc, "hello $USER\nEOF");
    assert_has(input, K::Parameter, "$USER");
    assert_eq!(cmds(input), vec![v(&["cat"]), v(&["echo", "next"])]);
    assert_eq!(paths(input), v(&["out"]));
}

#[test]
fn quoted_heredoc_body_is_not_expanded() {
    let input = "cat <<'EOF'\n$x\nEOF";
    assert_has(input, K::Heredoc, "'EOF'");
    assert_has(input, K::Heredoc, "$x\nEOF");
    assert_no_kind(input, K::Parameter);
    assert_no_kind("cat <<\\EOF\n$x\nEOF", K::Parameter);
}

#[test]
fn heredoc_strip_tabs_and_multiple() {
    let input = "cat <<-A <<B\n\ta\n\tA\nb\nB\nls";
    assert_has(input, K::Redirection, "<<-");
    assert_has(input, K::Heredoc, "\ta\n\tA");
    assert_has(input, K::Heredoc, "b\nB");
    assert_eq!(cmds(input), vec![v(&["cat"]), v(&["ls"])]);
}

#[test]
fn partial_heredoc() {
    let input = "cat <<EOF\nline one\nline";
    assert_has(input, K::Heredoc, "line one\nline");
    assert_no_error(input);
    assert_eq!(cmds(input), vec![v(&["cat"])]);
    // Words that look like commands in the body are not commands.
    assert_eq!(cmds("cat <<E\nfi\ndone\n"), vec![v(&["cat"])]);
    assert_no_error("cat <<E\nfi\ndone\n");
}

#[test]
fn here_string() {
    let input = "cat <<< \"$x y\"";
    assert_has(input, K::Redirection, "<<<");
    assert_has(input, K::DoubleQuoted, "\"$x y\"");
    assert!(paths(input).is_empty());
}

// -------------------------------------------------------------------------------------------
// Conditional expressions
// -------------------------------------------------------------------------------------------

#[test]
fn double_bracket_conditions() {
    let input = "[[ -f $f && ! -d ~/x || ( $a == b* ) ]] && echo ok";
    assert_has(input, K::ReservedWord, "[[");
    assert_has(input, K::ReservedWord, "]]");
    for op in ["-f", "&&", "!", "-d", "||", "(", "==", ")"] {
        assert_has(input, K::Operator, op);
    }
    assert_has(input, K::Glob, "*");
    assert_eq!(paths(input), v(&["$f", "~/x", "$a", "b*"]));
    assert_eq!(cmds(input), vec![v(&["echo", "ok"])]);
    assert_no_error(input);
}

#[test]
fn double_bracket_comparisons_and_regex() {
    let input = "[[ a < b && x -nt y && $s =~ ^(a|b)$ && 3 -eq 4 && a != b ]]";
    for op in ["<", "-nt", "=~", "-eq", "!="] {
        assert_has(input, K::Operator, op);
    }
    assert_no_error(input);
    assert_no_kind(input, K::Redirection);
    assert_no_kind(input, K::HistoryExpansion);
}

#[test]
fn double_bracket_across_lines_and_partial() {
    assert_no_error("[[ -n $x &&\n -z $y ]]");
    assert_no_error("[[ -n $x");
    assert_eq!(paths("[[ -n $x"), v(&["$x"]));
}

#[test]
fn stray_double_bracket_close() {
    assert_has("]]", K::Error, "]]");
    assert_has("[[ a ; ]]", K::Error, "]]");
    // Outside command position it is an ordinary word.
    assert_no_error("echo ]] [[");
}

#[test]
fn single_bracket_and_test_are_commands() {
    assert_eq!(
        cmds("[ -f x ] && test -d y"),
        vec![v(&["[", "-f", "x", "]"]), v(&["test", "-d", "y"])]
    );
    assert_no_kind("[ -f x ]", K::Operator);
}

// -------------------------------------------------------------------------------------------
// Reserved words and compound commands
// -------------------------------------------------------------------------------------------

#[test]
fn if_statement() {
    let input = "if true; then echo a; elif false; then echo b; else echo c; fi";
    for w in ["if", "then", "elif", "else", "fi"] {
        assert_has(input, K::ReservedWord, w);
    }
    assert_no_error(input);
    assert_eq!(cmds(input).len(), 5);
}

#[test]
fn multi_line_if() {
    let input = "if true\nthen\n  echo a\nelse\n  echo b\nfi";
    assert_no_error(input);
    assert_eq!(
        cmds(input),
        vec![v(&["true"]), v(&["echo", "a"]), v(&["echo", "b"])]
    );
}

#[test]
fn loops() {
    for input in [
        "while true; do echo; done",
        "until false; do echo; done",
        "for x in a b; do echo $x; done",
        "for x y in a b c d; do echo; done",
        "for ((i=0; i<3; i++)); do echo $i; done",
        "for ((i=0; i<3; i++)) do echo; done",
        "for x (a b) { echo $x }",
        "for x in a b; echo $x",
        "for x (a b) echo $x",
        "select x in a b; do break; done",
        "repeat 3 do echo; done",
        "repeat 3 echo hi",
        "foreach x (a b c)\n echo $x\nend",
        "while true; { echo }",
        "while [[ -n $x ]] { shift }",
    ] {
        assert_no_error(input);
    }
    assert_has("for x in a; do :; done", K::ReservedWord, "in");
    assert_has(
        "for ((i=0; i<3; i++)); do :; done",
        K::Arithmetic,
        "((i=0; i<3; i++))",
    );
    assert_eq!(paths("for x in a *.c; do :; done"), v(&["a", "*.c"]));
    assert_eq!(paths("foreach x (a b)\nend"), v(&["a", "b"]));
    // In the long form, `do` is a reserved word; in the list it is just a word.
    assert_eq!(paths("for x in a b do"), v(&["a", "b", "do"]));
}

#[test]
fn short_for_loop_has_no_done() {
    assert_has("for x (a b) echo $x; done", K::Error, "done");
}

#[test]
fn short_if_forms() {
    let input = "if [[ a ]] { echo } elif [[ b ]] { echo } else { echo }";
    assert_no_error(input);
    assert_eq!(cmds(input).len(), 3);
}

#[test]
fn case_statement() {
    let input = "case $x in (a|b) echo ab;; c*) echo c ;& d) ;| *) ;; esac";
    for w in ["case", "in", "esac"] {
        assert_has(input, K::ReservedWord, w);
    }
    for s in [";;", ";&", ";|", "|"] {
        assert_has(input, K::Separator, s);
    }
    assert_has(input, K::Grouping, "(");
    assert_has(input, K::Grouping, ")");
    assert_has(input, K::Glob, "*");
    assert_no_error(input);
    assert_eq!(cmds(input), vec![v(&["echo", "ab"]), v(&["echo", "c"])]);
    assert_eq!(paths(input), v(&["$x"]));
}

#[test]
fn multi_line_case_and_brace_form() {
    let input = "case $1 in\n  start)\n    run\n    ;;\n  <->) num ;;\nesac";
    assert_no_error(input);
    assert_has(input, K::Glob, "<->");
    assert_eq!(cmds(input), vec![v(&["run"]), v(&["num"])]);
    let input = "case x { a) echo ;; }";
    assert_no_error(input);
    assert_has(input, K::ReservedWord, "}");
    assert_no_error("case x\nin a) ;; esac");
}

#[test]
fn case_separator_outside_case_is_an_error() {
    assert_has("echo hi;;", K::Error, ";;");
}

#[test]
fn groups_and_subshells() {
    let input = "{ echo a; } && ( cd /; ls ) | wc";
    assert_has(input, K::ReservedWord, "{");
    assert_has(input, K::ReservedWord, "}");
    assert_has(input, K::Grouping, "(");
    assert_has(input, K::Grouping, ")");
    assert_no_error(input);
    assert_eq!(cmds(input).len(), 4);
    // zsh accepts `}` without a separator before it.
    assert_no_error("{ echo a }");
    assert_eq!(cmds("{ echo a }"), vec![v(&["echo", "a"])]);
}

#[test]
fn stray_closers_are_errors() {
    for (input, closer) in [
        ("fi", "fi"),
        ("done", "done"),
        ("esac", "esac"),
        ("then", "then"),
        ("}", "}"),
        (")", ")"),
        ("echo a; fi", "fi"),
        ("echo }", "}"),
        ("if a; do", "do"),
        ("end", "end"),
        ("elif", "elif"),
        ("else", "else"),
        ("echo a )", ")"),
    ] {
        assert!(
            has(input, K::Error, closer),
            "{input:?}: {:?}",
            spans(input)
        );
    }
}

#[test]
fn mismatched_closer_is_an_error() {
    assert_has("{ if a; then }", K::Error, "}");
    assert_has("( { )", K::Error, ")");
    assert_has("if a; fi", K::Error, "fi");
}

#[test]
fn empty_condition_is_accepted() {
    // zsh runs `if then echo; fi` and `while do break; done` without complaint.
    for input in [
        "if then",
        "if a; then b; elif then",
        "while do",
        "until\ndo",
        "if\n a\nthen",
        "for x; do",
    ] {
        assert_no_error(input);
    }
}

#[test]
fn heredoc_inside_command_substitution() {
    let input = "x=$(cat <<EOF\n$y )\nEOF\n) && ls";
    assert_has(input, K::Heredoc, "$y )\nEOF");
    assert_has(input, K::Parameter, "$y");
    assert_no_error(input);
    assert_eq!(cmds(input), vec![v(&["cat"]), v(&["ls"])]);
}

#[test]
fn comment_after_separator_without_space() {
    let input = "ls;# note";
    let out = parse_with(input, opts(true, false, false));
    assert!(spans_of(input, &out).contains(&(K::Comment, "# note".into())));
}

#[test]
fn unclosed_constructs_are_not_errors() {
    for input in [
        "if true; then echo",
        "if",
        "while true; do",
        "for x in",
        "for",
        "case x in a)",
        "case",
        "{ echo",
        "( echo",
        "function foo {",
        "foo() {",
        "[[ -f",
        "repeat 3",
        "select x in a",
        "foreach x (a",
        "x=(a b",
        "coproc",
    ] {
        assert_no_error(input);
    }
}

#[test]
fn word_after_compound_command_is_an_error() {
    for (input, word) in [
        ("( a ) b", "b"),
        ("{ a } x", "x"),
        ("[[ a ]] b", "b"),
        ("(( 1 )) b", "b"),
        ("if a; then b; fi c", "c"),
    ] {
        assert!(has(input, K::Error, word), "{input:?}: {:?}", spans(input));
    }
    // Redirections and separators may follow.
    assert_no_error("{ a } > f; ( b ) | c");
    assert_no_error("if [[ a ]] then b; fi");
}

#[test]
fn precommands_are_ordinary_commands() {
    for input in [
        "time ls",
        "nocorrect rm",
        "noglob echo",
        "exec zsh",
        "command ls",
        "builtin cd",
        "- ls",
    ] {
        let c = cmds(input);
        assert_eq!(c.len(), 1, "{input:?}");
        assert_eq!(c[0].len(), 2, "{input:?}");
        assert_no_kind(input, K::ReservedWord);
    }
    assert_eq!(cmds("sudo -u root git status")[0].len(), 5);
}

#[test]
fn reserved_words_only_in_command_position() {
    assert_no_kind("echo if then fi do done", K::ReservedWord);
    assert_eq!(cmds("echo if then fi")[0].len(), 4);
    assert_has("! true", K::ReservedWord, "!");
    assert_no_kind("! true", K::HistoryExpansion);
    assert_has("a && ! b", K::ReservedWord, "!");
    assert_has("coproc cat", K::ReservedWord, "coproc");
    // zsh rejects a reserved word after an assignment; it is still the command word.
    assert_eq!(cmds("x=1 if"), vec![v(&["if"])]);
    assert_has("x=1 if", K::Error, "if");
}

#[test]
fn in_is_reserved_only_after_for_case_select() {
    assert_has("for x in a", K::ReservedWord, "in");
    assert_has("case x in", K::ReservedWord, "in");
    assert_has("select x in a", K::ReservedWord, "in");
    assert_no_kind("in a", K::ReservedWord);
    assert_eq!(cmds("in a"), vec![v(&["in", "a"])]);
}

// -------------------------------------------------------------------------------------------
// Functions
// -------------------------------------------------------------------------------------------

#[test]
fn function_definitions() {
    let input = "foo() { echo; }";
    assert_has(input, K::Function, "foo");
    assert_has(input, K::Grouping, "(");
    assert_has(input, K::Grouping, ")");
    assert_eq!(cmds(input), vec![v(&["echo"])]);
    assert_no_error(input);

    let input = "function bar baz { echo }";
    assert_has(input, K::ReservedWord, "function");
    assert_has(input, K::Function, "bar");
    assert_has(input, K::Function, "baz");
    assert_no_error(input);

    let input = "function qux() echo hi";
    assert_has(input, K::Function, "qux");
    assert_eq!(cmds(input), vec![v(&["echo", "hi"])]);

    for input in [
        "foo () { echo }",
        "() { anon }",
        "function { anon }",
        "foo() echo",
        "function f\n{\n}",
    ] {
        assert_no_error(input);
    }
    assert_has("foo () { echo }", K::Function, "foo");
    assert!(cmds("foo () { echo }").iter().all(|c| c[0] != "foo"));
}

// -------------------------------------------------------------------------------------------
// Assignments
// -------------------------------------------------------------------------------------------

#[test]
fn leading_assignments() {
    let input = "FOO=1 BAR+=x arr[2]=y env";
    assert_has(input, K::Assignment, "FOO=");
    assert_has(input, K::Assignment, "BAR+=");
    assert_has(input, K::Assignment, "arr[2]=");
    assert_eq!(cmds(input), vec![v(&["env"])]);
    assert!(cmds("x=1").is_empty());
    assert_has("a[$i]=1", K::Parameter, "$i");
}

#[test]
fn array_assignments() {
    let input = "arr=(a $b \"c d\"\n e) next=1";
    assert_has(input, K::Assignment, "arr=");
    assert_has(input, K::Assignment, "(");
    assert_has(input, K::Assignment, ")");
    assert_has(input, K::Parameter, "$b");
    assert_has(input, K::DoubleQuoted, "\"c d\"");
    assert_has(input, K::Assignment, "next=");
    assert!(cmds(input).is_empty());
    let input = "typeset -A m=([k]=v [k2]+=$x)";
    assert_has(input, K::Assignment, "[k]=");
    assert_has(input, K::Assignment, "[k2]+=");
}

#[test]
fn declaration_builtin_assignments() {
    let input = "local x=1 y arr=(a b)";
    assert_has(input, K::Assignment, "x=");
    assert_has(input, K::Assignment, "arr=");
    assert_eq!(cmds(input), vec![v(&["local", "x=1", "y", "arr=(a b)"])]);
    assert_eq!(word(input, 1).literal.as_deref(), Some("x=1"));
    for b in [
        "typeset", "declare", "export", "readonly", "integer", "float",
    ] {
        assert_has(&format!("{b} a=1"), K::Assignment, "a=");
    }
    assert_has("builtin export a=1", K::Assignment, "a=");
    // Ordinary commands do not take assignments.
    assert_no_kind("echo a=1", K::Assignment);
    assert_no_kind("echo local a=1", K::Assignment);
}

// -------------------------------------------------------------------------------------------
// Lists and pipelines
// -------------------------------------------------------------------------------------------

#[test]
fn separators() {
    let input = "a | b |& c && d || e; f & g &! h &| i";
    for s in ["|", "|&", "&&", "||", ";", "&", "&!", "&|"] {
        assert_has(input, K::Separator, s);
    }
    assert_no_error(input);
    assert_eq!(cmds(input).len(), 9);
}

#[test]
fn operator_without_command_is_an_error() {
    for (input, op) in [
        ("| a", "|"),
        ("&& a", "&&"),
        ("|| a", "||"),
        ("& a", "&"),
        ("a | | b", "|"),
        ("a && || b", "||"),
        ("a\n&& b", "&&"),
        ("a | ; b", ";"),
        ("if | a", "|"),
        ("a & & b", "&"),
    ] {
        assert!(has(input, K::Error, op), "{input:?}: {:?}", spans(input));
    }
    // zsh accepts empty commands before `;`.
    assert_no_error("; ls");
    assert_no_error("a; ; b");
}

#[test]
fn trailing_operator_is_partial() {
    for input in [
        "a |", "a &&", "a ||", "a |&", "a &", "a;", "a |\n b", "a &&\n b",
    ] {
        assert_no_error(input);
    }
}

#[test]
fn newlines_separate_commands() {
    assert_eq!(
        cmds("a 1\nb 2\n\nc"),
        vec![v(&["a", "1"]), v(&["b", "2"]), v(&["c"])]
    );
}

// -------------------------------------------------------------------------------------------
// History expansion
// -------------------------------------------------------------------------------------------

#[test]
fn history_expansion_forms() {
    let input = "echo !! !$ !^ !* !-2 !12 !str !?mid? !# !!:1 !$:h !vi:s/a/b/ !{x}";
    for h in [
        "!!",
        "!$",
        "!^",
        "!*",
        "!-2",
        "!12",
        "!str",
        "!?mid?",
        "!#",
        "!!:1",
        "!$:h",
        "!vi:s/a/b/",
        "!{x}",
    ] {
        assert_has(input, K::HistoryExpansion, h);
    }
    assert_eq!(word("echo !!", 1).literal, None);
}

#[test]
fn history_expansion_exclusions() {
    for input in [
        "echo '!!'",
        "echo $'!!'",
        "echo \\!!",
        "echo !",
        "echo ! x",
        "echo a!=b",
        "echo \"hi!\"",
        "[[ a != b ]]",
    ] {
        assert_no_kind(input, K::HistoryExpansion);
    }
    assert_has("echo \"x !! y\"", K::HistoryExpansion, "!!");
    assert_has("^old^new", K::HistoryExpansion, "^old^new");
    assert!(cmds("^old^new").is_empty());
    assert_no_kind("echo ^a^b", K::HistoryExpansion);
}

// -------------------------------------------------------------------------------------------
// Comments
// -------------------------------------------------------------------------------------------

#[test]
fn comments_need_interactive_comments() {
    let o = opts(true, false, false);
    let input = "echo hi # a comment $x\n# whole\nls a#b";
    let out = parse_with(input, o);
    let s = spans_of(input, &out);
    assert!(s.contains(&(K::Comment, "# a comment $x".into())), "{s:?}");
    assert!(s.contains(&(K::Comment, "# whole".into())), "{s:?}");
    assert!(s.iter().all(|(k, _)| *k != K::Parameter));
    let words: Vec<Vec<&str>> = out
        .commands
        .iter()
        .map(|c| c.words.iter().map(|w| &input[w.start..w.end]).collect())
        .collect();
    assert_eq!(words, vec![vec!["echo", "hi"], vec!["ls", "a#b"]]);
    // Without the option, `#` is an ordinary character.
    assert_no_kind(input, K::Comment);
    assert_eq!(cmds("echo # x")[0].len(), 3);
}

// -------------------------------------------------------------------------------------------
// Tilde and literal values
// -------------------------------------------------------------------------------------------

#[test]
fn tilde_words() {
    for (input, tilde) in [
        ("ls ~", true),
        ("ls ~/x", true),
        ("ls ~user/x", true),
        ("ls ~name", true),
        ("ls a~b", false),
        ("ls '~'", false),
        ("ls \\~", false),
    ] {
        let w = word(input, 1);
        assert_eq!(w.tilde, tilde, "{input:?}");
        assert!(w.literal.is_some(), "{input:?}");
    }
    assert_eq!(word("ls ~/x", 1).literal.as_deref(), Some("~/x"));
}

#[test]
fn literal_is_none_for_any_expansion() {
    for input in [
        "e $x", "e ${x}", "e $(x)", "e `x`", "e $((1))", "e *", "e {a,b}", "e !!", "e <(x)", "e a?",
    ] {
        assert_eq!(word(input, 1).literal, None, "{input:?}");
    }
    for input in ["e 'a'", "e \"a\"", "e $'a'", "e a\\ b", "e {a}", "e a,b"] {
        assert!(word(input, 1).literal.is_some(), "{input:?}");
    }
}

#[test]
fn name_eq_is_decided_before_the_first_expansion() {
    let check = |input: &str, o: ParseOptions, want: bool| {
        let out = parse_with(input, o);
        assert_eq!(out.commands[0].words[1].name_eq, want, "{input:?} {o:?}");
    };
    for input in [
        "env FOO=$x",
        "env FOO=\"$(date)\"",
        "env \"FOO\"=$x",
        "env FOO\\=$x",
        "env $'FOO'=$x",
        "env FOO=*.c",
        "env FOO=~/x",
        "env FOO=1",
        "env _a9=",
        "env FOO=${x}",
        "env FOO=`a`",
        "env FOO=$((1))",
        "env FOO=!!",
        "env FOO={a,b}",
        "env $'FOO'=1",
        "env FOO=<(a)",
    ] {
        check(input, ParseOptions::default(), true);
    }
    check("env FOO=~/x", opts(false, true, false), true);
    for input in [
        "env FOO$x=1",
        "env FOO${x}=1",
        "env FOO`a`=1",
        "env FOO$(a)=1",
        "env FOO$((1))=1",
        "env FOO$[1]=1",
        "env FOO!!=1",
        "env FOO*=1",
        "env {A,B}=1",
        "env FOO+=$x",
        "env FOO[1]=$x",
        "env $cmd",
        "env =$x",
        "env 9A=$x",
        "env FÖO=$x",
        "env FÖO=1",
        "env FOO",
        "env 'FOO'",
    ] {
        check(input, ParseOptions::default(), false);
    }
    check("env FOO~=1", opts(false, true, false), false);
    // A process substitution or glob operator in the name; the first expansion decides even
    // when a later one follows the `=`.
    for o in [
        ParseOptions::default(),
        opts(false, true, false),
        opts(false, false, true),
        opts(false, true, true),
    ] {
        for input in [
            "env FOO<(a)=1",
            "env FOO>(a)=1",
            "env FOO?=1",
            "env FOO$x=1$y",
        ] {
            check(input, o, false);
            let words = &parse_with(input, o).commands[0].words;
            assert_eq!(
                (words.len(), words[1].end),
                (2, input.len()),
                "{input:?} {o:?}"
            );
        }
    }
    for o in [opts(false, false, true), opts(false, true, true)] {
        check("env FOO@(a)=1", o, false);
        let words = &parse_with("env FOO@(a)=1", o).commands[0].words;
        assert_eq!((words.len(), words[1].end), (2, 13), "{o:?}");
    }
    // A real assignment is not a word; the flag is about arguments.
    assert!(word("FOO=$x env BAR=$y", 1).name_eq);
    // Arguments of a declaration builtin keep the flag.
    assert!(word("export FOO=$x", 1).name_eq);
    assert!(word("typeset FOO=(a b)", 1).name_eq);
}

#[test]
fn is_name_eq_rules() {
    for t in ["A=", "a=1", "_=", "_x9=v=w", "FOO==", "A=$x"] {
        assert!(is_name_eq(t), "{t:?}");
    }
    for t in [
        "", "=", "=1", "9A=1", "A", "A+=1", "A[1]=1", "A-B=1", "é=1", "Aé=1", " A=1",
    ] {
        assert!(!is_name_eq(t), "{t:?}");
    }
}

// -------------------------------------------------------------------------------------------
// Multi-line and multibyte input
// -------------------------------------------------------------------------------------------

#[test]
fn prebuffer_continuation() {
    // PREBUFFER holds the open construct; BUFFER continues it.
    let input = "for f in *.txt; do\n  echo \"$f\"\ndone";
    assert_no_error(input);
    assert_eq!(cmds(input), vec![v(&["echo", "\"$f\""])]);
    let input = "echo \"line one\nline two\" done";
    assert_has(input, K::DoubleQuoted, "\"line one\nline two\"");
    assert_eq!(cmds(input)[0].len(), 3);
}

#[test]
fn multibyte_text_keeps_char_boundaries() {
    let input = "echo \"héllo 世界\" ${名前} 🎉 'ü' é* \\é $'\\u00e9' ü{a,b}";
    let out = p(input);
    assert!(
        out.spans
            .iter()
            .any(|s| s.kind == K::DoubleQuoted && &input[s.start..s.end] == "\"héllo 世界\"")
    );
    assert_has(input, K::Glob, "*");
    assert_has(input, K::BraceExpansion, "{a,b}");
    assert_eq!(cmds(input)[0].len(), 9);
}

// -------------------------------------------------------------------------------------------
// Robustness
// -------------------------------------------------------------------------------------------

/// Tricky lines whose every prefix must parse cleanly.
const CORPUS: &[&str] = &[
    "echo \"hello $USER\" | grep -i foo > out.txt 2>&1",
    "if [[ -f ~/x && $a == b* ]]; then echo ${(j:,:)arr} $((1+$x)); fi",
    "for x in a b *.c; do print -l $x[1] `date` $(ls -l; pwd); done",
    "case $x in (a|b) echo hi;; *) ;; esac",
    "cat <<EOF > f\nbody $x $(cmd)\nEOF\necho done",
    "foo() { local x=1 arr=(a b); x+=2 cmd $'a\\n' }",
    "echo {a,b} {1..5} ~/foo !! !$ \"a!b\" ^x",
    "fi; done; ) | && echo >; echo > | x",
    "ls *(.) **/*.rs(N) [a-z]* <1-5> (a|b)~c ^d @(e|f)",
    "echo ${x:-$(echo \"${y:-`z`}\")} $(( $(a) + ${#b} ))",
    "((cd /; ls) | wc) && (( i++ )) || $((cd x); ls)",
    "while read -r l; do [[ $l =~ ^#(.*)$ ]] && continue; done < <(cat f)",
    "x=(a\n# c\nb) y=$'\\x41' z=\"$(echo \"q\")\"",
    "function f g { repeat 3 { foreach i (1 2) echo $i; end } }",
    "echo \"héllo 世界\" ${名前} 🎉 $'\\u00e9' é* \\é",
    "{fd}>&- 3<&0 >| f >! g &>> h <> i <<< \"str\" >&2 >& file",
    "if [[ a ]] { b } elif (( c )) { d } else { e }",
    "echo a\\\nb; arr[$((i+1))]=x; typeset -A m=([k]=v)",
    "! cmd |& tee >(wc -l) =(date) 2>/dev/null &!",
    "echo `echo \\`nested\\`` \"$(echo ')')\" '${x}'",
];

#[test]
fn every_prefix_parses_cleanly() {
    let option_sets = [
        opts(false, false, false),
        opts(true, true, false),
        opts(true, false, true),
    ];
    for line in CORPUS {
        for o in option_sets {
            for (i, _) in line
                .char_indices()
                .chain(std::iter::once((line.len(), ' ')))
            {
                parse_with(&line[..i], o);
            }
        }
    }
}

/// A small deterministic PRNG so the mutation test needs no dependency.
struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn mutated_inputs_parse_cleanly() {
    const ALPHABET: &[u8] = b"$(){}[]<>|&;'\"`\\!#~^*?=,.:\n \t-0aZ\x00\xc3\xa9\xe4\xb8\x96\xf0";
    let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
    let option_sets = [opts(false, false, false), opts(true, true, true)];
    for line in CORPUS {
        for _ in 0..200 {
            let mut bytes = line.as_bytes().to_vec();
            let edits = 1 + rng.next() % 4;
            for _ in 0..edits {
                let pos = (rng.next() as usize) % (bytes.len() + 1);
                let b = ALPHABET[(rng.next() as usize) % ALPHABET.len()];
                match rng.next() % 3 {
                    0 if pos < bytes.len() => bytes[pos] = b,
                    1 if pos < bytes.len() => {
                        bytes.remove(pos);
                    }
                    _ => bytes.insert(pos, b),
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            for o in option_sets {
                parse_with(&text, o);
            }
        }
    }
}

#[test]
fn random_inputs_parse_cleanly() {
    const PIECES: &[&str] = &[
        "$",
        "(",
        ")",
        "{",
        "}",
        "[",
        "]",
        "<",
        ">",
        "|",
        "&",
        ";",
        "'",
        "\"",
        "`",
        "\\",
        "!",
        "#",
        "~",
        "^",
        "*",
        "?",
        "=",
        ",",
        "..",
        ":",
        "\n",
        " ",
        "\t",
        "-",
        "0",
        "2",
        "x",
        "é",
        "世",
        "if",
        "then",
        "fi",
        "do",
        "done",
        "case",
        "in",
        "esac",
        "for",
        "[[",
        "]]",
        "((",
        "))",
        "<<",
        "EOF",
        "$(",
        "${",
        "$((",
        "<(",
        "=(",
        "@(",
        "&&",
        "||",
        ";;",
        "\0",
        "always",
        "time",
        "() ",
        "x=",
        "local ",
        "function ",
        "$[",
        "$a[",
    ];
    let mut rng = XorShift(0x2545_f491_4f6c_dd1d);
    let option_sets = [opts(false, false, false), opts(true, true, true)];
    for _ in 0..3000 {
        let len = (rng.next() % 40) as usize;
        let text: String = (0..len)
            .map(|_| PIECES[(rng.next() as usize) % PIECES.len()])
            .collect();
        for o in option_sets {
            parse_with(&text, o);
        }
    }
}

#[test]
fn deep_nesting_is_bounded() {
    let patterns: &[(&str, &str)] = &[
        ("$(", ")"),
        ("\"${", "}\""),
        ("${", "}"),
        ("<(", ")"),
        ("`", "`"),
        ("$((", "))"),
        ("(", ")"),
        ("{ ", " }"),
        ("x(", ")"),
        ("$a[", "]"),
        ("\"$(", ")\""),
        ("[[ ( ", " ) ]]"),
        ("if a; then ", " fi"),
        ("case x in a) ", " esac"),
    ];
    for (open, close) in patterns {
        let input = format!("{}{}", open.repeat(5000), close.repeat(5000));
        parse_with(&input, ParseOptions::default());
        parse_with(&input[..input.len() / 2], ParseOptions::default());
    }
}

#[test]
fn control_characters_and_replacement_chars() {
    for input in [
        "\0",
        "echo \0 $\0 ${\0}",
        "\u{fffd}\u{fffd}",
        "a\rb",
        "echo \u{1b}[31m",
        "\x7f$'\\\0'",
    ] {
        parse_with(input, ParseOptions::default());
    }
}

#[test]
fn large_input_stays_linear() {
    let line = "echo \"$HOME/${x:-y}\" $(ls *.rs | wc -l) [[ -f a ]] && { b; } 2>&1 # c\n";
    let input = line.repeat(10_000 / line.len() + 1);
    let start = std::time::Instant::now();
    for _ in 0..10 {
        parse_with(&input, opts(true, true, false));
    }
    // Generous bound for unoptimised builds; release is far faster (see the timing test).
    assert!(
        start.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        start.elapsed()
    );
}

/// Run with `cargo test --release -- --ignored --nocapture parse_timing` to measure.
#[test]
#[ignore]
fn parse_timing() {
    let line = r#"for f in **/*.rs(N); do [[ -f $f ]] && echo "${f:t}: $(wc -l < $f) lines" | tee -a ~/log.txt 2>&1; done; git commit -m "update ${(j:,:)files}" && print -P "%F{green}ok%f" && arr=(a b c) cmd $x[1] <<< "$(date +%s)" &!"#;
    assert!(line.len() >= 200, "{}", line.len());
    let o = opts(true, true, false);
    let iters = 20_000;
    for _ in 0..1000 {
        std::hint::black_box(parse(line, &o));
    }
    let start = std::time::Instant::now();
    for _ in 0..iters {
        std::hint::black_box(parse(std::hint::black_box(line), &o));
    }
    let per = start.elapsed() / iters;
    println!("{} bytes: {per:?} per parse", line.len());
    // The daemon lexes at most 64 KiB of a buffer. Measure the steady state of a long-running
    // process; see `adversarial_inputs_scale_linearly`.
    const MAX: usize = 64 * 1024;
    drop(std::hint::black_box(Vec::<u8>::with_capacity(16 << 20)));
    let realistic = format!("{line}\n").repeat(MAX / (line.len() + 1));
    println!(
        "realistic ({} bytes): {:?} per parse",
        realistic.len(),
        min_time(&realistic, o, 10) / 10
    );
    let spans_only =
        |input: &str| min_time_of(10, || drop(std::hint::black_box(parse_spans(input, &o)))) / 10;
    let mut worst = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let dense = [
        ("pipes", "a|", ""),
        ("substitutions", "$(a)", ""),
        ("lines", "ls\n", ""),
    ];
    for (name, prefix, suffix) in SCALING_PATTERNS.iter().chain(&dense) {
        let input = scaling_input(prefix, suffix, MAX / (prefix.len() + suffix.len()));
        let (full, spans) = (min_time(&input, o, 10) / 10, spans_only(&input));
        worst = (worst.0.max(full), worst.1.max(spans));
        println!(
            "{name} ({} bytes): {full:?} per parse, {spans:?} spans only",
            input.len()
        );
    }
    println!("realistic spans only: {:?}", spans_only(&realistic));
    println!("worst: {:?} per parse, {:?} spans only", worst.0, worst.1);
}

#[test]
fn adversarial_inputs_stay_fast() {
    let o = opts(true, true, true);
    for (name, prefix, suffix) in SCALING_PATTERNS {
        let input = scaling_input(prefix, suffix, 16 * 1024 / (prefix.len() + suffix.len()));
        let start = std::time::Instant::now();
        parse_with(&input, o);
        parse_with(&input[..input.len() / 2], o);
        // Generous bound for unoptimised builds.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "{name}: {:?}",
            start.elapsed()
        );
    }
}

/// Adversarial inputs for the linearity test: `prefix` repeated `k` times, then `suffix` repeated
/// `k` times. Each targets a path that could rescan the input: a closer searching a deep stack,
/// a scan for a matching bracket from every nested opener, or a lookahead from every word.
const SCALING_PATTERNS: &[(&str, &str, &str)] = &[
    ("open parens", "(", ""),
    ("double parens closing apart", "((", "x) "),
    ("dollar double parens", "$((", "x) "),
    ("dollar double parens with quotes", "$((\"", "x) "),
    ("braces then parens", "{ ", ") "),
    ("braces then fi", "{ ", "fi "),
    ("braces then done", "{ ", "done "),
    ("braces then esac", "{ ", "esac "),
    ("braces then then", "{ ", "then "),
    ("braces then case separators", "{ ", ";; "),
    ("if bodies then then", "if a; then ", "then "),
    ("subshells then braces", "( ", "} "),
    ("loops then end", "while a; do ", "end "),
    ("brackets", "[", ""),
    ("unclosed classes", "[[:", ""),
    ("closed classes", "[[:a:]", ""),
    ("subscripts", "$a[", ""),
    ("assignment subscripts", "a[a[a[", ""),
    ("braced subscripts", "${a[", ""),
    ("old arithmetic", "$[", ""),
    ("backquotes", "`", ""),
    ("heredocs", "cat <<E\n", ""),
    ("history search", "!?", ""),
    ("history braces", "!{", ""),
    ("dollar parens", "$(", ""),
    ("quoted substitutions", "\"$(\"", ""),
    ("braced parameters", "${", ""),
    ("parameter flags", "${(j:", ""),
    ("arrays", "x=(", ""),
    ("conditions", "[[ ( ", ""),
    ("process substitutions", "<(", ""),
    ("brace candidates", "{a", ""),
    ("brace lists", "{a,", ""),
    ("glob groups", "a(", ""),
    ("assignment glob groups", "x=a(", ""),
    ("numeric ranges", "<1-", ""),
    ("function definitions", "f() ", ""),
    ("anonymous functions", "() { a } ", ""),
    ("always blocks", "{ a } always { ", "} "),
    ("for headers across lines", "for x\n", ""),
    ("stray closers", "fi ", ""),
    ("escapes", "\\", ""),
    ("dollar quotes", "$'\\", ""),
];

fn scaling_input(prefix: &str, suffix: &str, k: usize) -> String {
    format!("{}{}", prefix.repeat(k), suffix.repeat(k))
}

/// The fastest of several timed runs, each parsing `input` `reps` times.
fn min_time(input: &str, o: ParseOptions, reps: u32) -> std::time::Duration {
    min_time_of(reps, || drop(std::hint::black_box(parse(input, &o))))
}

fn min_time_of(reps: u32, mut f: impl FnMut()) -> std::time::Duration {
    (0..5)
        .map(|_| {
            let start = std::time::Instant::now();
            for _ in 0..reps {
                f();
            }
            start.elapsed()
        })
        .min()
        .unwrap_or_default()
}

/// t(4n) / t(n) for one pattern, with enough repetitions that the smaller input takes about a
/// millisecond.
fn scaling_ratio(prefix: &str, suffix: &str, bytes: usize, o: ParseOptions) -> f64 {
    let k = (bytes / (prefix.len() + suffix.len())).max(1);
    let small = scaling_input(prefix, suffix, k);
    let large = scaling_input(prefix, suffix, 4 * k);
    let once = min_time(&small, o, 1).as_secs_f64();
    let reps = (1e-3 / once.max(1e-7)).clamp(1.0, 1000.0) as u32;
    min_time(&large, o, reps).as_secs_f64() / min_time(&small, o, reps).as_secs_f64().max(1e-9)
}

#[test]
fn adversarial_inputs_scale_linearly() {
    let o = opts(true, true, false);
    // The parse output takes up to about 40 bytes per input byte. In an optimised build, a step
    // across the size of a core's L2 cache (16 to 64 KiB of input on a 1 MiB cache) measures
    // the cache rather than the parser, so it compares 64 and 256 KiB instead. An unoptimised
    // build is dominated by computation and uses smaller inputs to stay quick.
    let bytes = if cfg!(debug_assertions) { 4096 } else { 65536 };
    // glibc returns freed memory to the system above a threshold that rises when a large mapped
    // block is freed. Raise it now, or whether a parse pays for fresh page faults depends on
    // what ran before it, which can outweigh the parse itself.
    drop(std::hint::black_box(Vec::<u8>::with_capacity(16 << 20)));
    for (name, prefix, suffix) in SCALING_PATTERNS {
        // Linear growth gives a ratio near 4 and quadratic growth near 16. Timing noise from
        // tests running in parallel only ever slows a run down, so a pattern is retried before
        // it counts as a failure.
        let ratios: Vec<f64> = (0..3)
            .map(|_| scaling_ratio(prefix, suffix, bytes, o))
            .scan(false, |done, r| {
                (!std::mem::replace(done, r < 6.0)).then_some(r)
            })
            .collect();
        let last = ratios.last().copied().unwrap_or_default();
        println!("{name}: {ratios:.1?}");
        assert!(last < 6.0, "{name}: t(4n)/t(n) = {ratios:.1?}");
    }
}

// -------------------------------------------------------------------------------------------
// Agreement with zsh
// -------------------------------------------------------------------------------------------

/// One-liners that `zsh -f -n` accepts. None of them may produce an Error span.
const ZSH_VALID: &[&str] = &[
    "if true; then echo a; elif false; then echo b; else echo c; fi",
    "if [[ -n $x ]] { echo y } elif [[ -z $x ]] { echo z } else { echo w }",
    "while read -r line; do print -r -- $line; done < file",
    "until false; do break; done",
    "while [[ -z $q ]] { q=1 }",
    "while true; do\n  break\ndone",
    "for f in *.rs(N); do wc -l $f; done",
    "for x y in a b c d; do echo $x $y; done",
    "for ((i = 0; i < 3; i++)); do echo $i; done",
    "for x (a b c) echo $x",
    "for x in a b; { echo $x }",
    "for x\nin a b; do echo $x; done",
    "for x\ndo echo $x; done",
    "foreach x (a b c) echo $x; end",
    "select x in a b; do break; done",
    "repeat 3 echo hi",
    "repeat 2 { echo r }",
    "case $1 in (start|stop) run $1;; restart) run stop; run start;& *) usage;| esac",
    "case $x { a) echo a;; *) echo other;; }",
    "case $x in a) echo ;; esac",
    "case x in\n  a) echo ;;\nesac",
    "{ ls } always { echo done }",
    "{ ls; } always { echo cleanup; }",
    "a | { b } always { c } | d",
    "{ echo a } always { echo b } > log",
    "{ls}",
    "{echo hi}",
    "{echo a; echo b} | cat",
    "time { sleep 1 }",
    "time ( sleep 1 )",
    "time if true; then echo; fi",
    "! { false }",
    "true && ! false",
    "coproc { cat }",
    "coproc cat",
    "coproc ( cat )",
    "() { echo anon $@ } a b",
    "() { echo $1 } fi",
    "function { echo $1 } arg",
    "function () { echo $1 } arg",
    "() echo hi a b",
    "f() { echo in f }",
    "f() {echo in f}",
    "function g { echo in g }",
    "function h() { echo in h }",
    "function a b c { echo multi }",
    "f () ( echo subshell )",
    "function f; echo body",
    "(cd /tmp && ls) | wc -l",
    "(( i++ )) && echo $(( i * 2 ))",
    "echo $((1 + $(echo 2)))",
    "cat <<EOF\nhello $USER\nEOF",
    "cat <<-'EOF' >out\n\tliteral $x\n\tEOF",
    "x=$(cat <<EOF\n$y )\nEOF\n) && ls",
    "diff <(sort a) >(tee b) =(date)",
    "ls *(.) **/*.rs(N) [a-z]?*(om[1,3]) <1-10>",
    "echo ${(j:,:)arr} ${(s: :)x} ${(%):-%n} ${#arr} ${+x} ${=x} ${x:-default}",
    "echo ${${x#a}%b} ${x/a/b} ${x//a/b} $arr[1] $file:t:r ${(@)arr[2,-1]}",
    "x=1 y=(a b c) z=$(date) cmd",
    "typeset -A m=([k]=v [k2]=w); local -a arr=(*.rs)",
    "x=~/foo ls",
    "a && ;",
    "a || ; b",
    ">/dev/null { echo quiet }",
    ">f if true; then echo; fi",
    "if >/dev/null true; then echo; fi",
    "echo a}b }a {a} {a,b} a\\}",
    "[[ $a == (foo|bar)* && -f ~/x || $b =~ ^[0-9]+$ ]]",
    "print -P '%F{red}x%f' $'\\t' \"a $b ${c} $(d) `e`\"",
    "sudo -u root env FOO=1 nice -n 10 ls",
    "exec 3>&1 2>&- 4<>file {fd}>out",
    "echo >&2 hi &>/dev/null &>>log 2>&1 >| f >! g",
    "ls &! ls &| ls & wait",
];

/// One-liners that `zsh -f -n` rejects and that no further typing can repair. Each must
/// produce at least one Error span.
const ZSH_INVALID: &[&str] = &[
    "fi",
    "echo a; done",
    "esac",
    "then",
    "echo }",
    "echo a}",
    "{ echo } x",
    "( a ) b",
    "if a; then b; fi c",
    "x=1 if true; then echo; fi",
    "x=1 [[ a ]]",
    "a | ;",
    "a && && b",
    "a && | b",
    "| a",
    "&& a",
    "& a",
    "echo > ;",
    "cat < | wc",
    "a ;; b",
    "f() ;",
    "f() && b",
    "{ ls } always ;",
    "{ ls } always echo",
    "{ ls } > f always { echo }",
    "f() { a } always { b }",
    "{ a } always { b } always { c }",
    "for x (a) echo; done",
    "for x in\na b; do echo; done",
    "coproc NAME { cat }",
    "a | ! b",
    ">f ! true",
    "echo $(echo a})",
    "[[ a} == a ]]",
    "x=(a })",
    "( { )",
    ")",
];

#[test]
fn valid_zsh_has_no_errors() {
    for input in ZSH_VALID {
        for o in [opts(false, false, false), opts(true, true, false)] {
            let out = parse_with(input, o);
            let errors: Vec<_> = spans_of(input, &out)
                .into_iter()
                .filter(|(k, _)| *k == K::Error)
                .collect();
            assert!(errors.is_empty(), "{input:?} with {o:?}: {errors:?}");
        }
    }
}

#[test]
fn invalid_zsh_has_an_error() {
    for input in ZSH_INVALID {
        let s = spans(input);
        assert!(s.iter().any(|(k, _)| *k == K::Error), "{input:?}: {s:?}");
    }
}

#[test]
fn close_brace_ends_a_word_like_zsh() {
    // A `{` starting a word in command position opens a group; a `}` that closes no `{` of
    // its word and is followed by a terminator closes it.
    for (input, words) in [
        ("{ls}", vec![v(&["ls"])]),
        ("{echo hi}", vec![v(&["echo", "hi"])]),
        (
            "{echo a; echo b} | cat",
            vec![v(&["echo", "a"]), v(&["echo", "b"]), v(&["cat"])],
        ),
        ("{echo,hi}", vec![v(&["echo,hi"])]),
        ("{echo {a}}", vec![v(&["echo", "{a}"])]),
        ("{echo a}b}", vec![v(&["echo", "a}b"])]),
    ] {
        assert_no_error(input);
        assert_eq!(cmds(input), words, "{input:?}");
        assert_has(input, K::ReservedWord, "{");
        assert_has(input, K::ReservedWord, "}");
    }
    assert_has("echo a}", K::Error, "}");
    assert_eq!(cmds("echo a}"), vec![v(&["echo", "a"])]);
    assert_has("echo \"a\"}", K::Error, "}");
    assert_has("echo ${x}}", K::Error, "}");
    assert_has("for i in a }; do :; done", K::Error, "}");
    assert_has("echo > }", K::Error, ">");
    assert_no_error("{ echo > a}");
    assert_eq!(paths("{ echo > a}"), v(&["a"]));
    // An assignment value keeps its `}`.
    assert_no_error("x=a}");
    assert_no_error("local x=a}");
    // Not in command position, `{` is an ordinary character.
    assert_no_error("echo {a {");
    assert_eq!(cmds("x=1 {echo"), vec![v(&["{echo"])]);
}

#[test]
fn always_block() {
    let input = "{ ls } always { echo done }";
    assert_has(input, K::ReservedWord, "always");
    assert_eq!(cmds(input), vec![v(&["ls"]), v(&["echo", "done"])]);
    assert_no_error("{ ls } always\n{ echo }");
    assert_no_error("{ ls }\nalways");
    assert_no_kind("echo always", K::ReservedWord);
    assert_has("{ ls } always echo", K::Error, "echo");
    assert_has("{ ls } always ;", K::Error, ";");
    assert_has("{ ls } > f always { echo }", K::Error, "always");
    assert_has("f() { a } always { b }", K::Error, "always");
    assert_has("( a ) always { b }", K::Error, "always");
    assert_has("if a; then b; fi always { c }", K::Error, "always");
}

#[test]
fn anonymous_function_arguments() {
    for (input, args) in [
        ("() { echo $1 } a b", v(&["a", "b"])),
        ("() { echo } fi then", v(&["fi", "then"])),
        ("() ( echo ) a", v(&["a"])),
        ("function { echo } a", v(&["a"])),
        ("function () { echo } a", v(&["a"])),
        ("() { echo } a > f b | cat", v(&["a", "f", "b"])),
    ] {
        assert_no_error(input);
        assert_eq!(paths(input), args, "{input:?}");
    }
    assert!(!has("() { echo } fi", K::ReservedWord, "fi"));
    // A named function takes no arguments.
    assert_has("f() { a } b", K::Error, "b");
    assert_has("function f { a } b", K::Error, "b");
    // A `}` still closes an enclosing group.
    assert_no_error("{ () { a } b }");
}

#[test]
fn function_definition_needs_a_body() {
    assert_has("f() ;", K::Error, ";");
    assert_has("() ;", K::Error, ";");
    assert_has("f() && b", K::Error, "&&");
    assert_no_error("function f;");
    assert_no_error("f()\n{ echo }");
}

#[test]
fn separator_after_and_or_is_accepted() {
    for input in [
        "a && ;",
        "a || ; b",
        "(a && )",
        "{ a && }",
        "a &&\n;",
        "if a && then b; fi",
    ] {
        assert_no_error(input);
    }
    assert_has("a | ;", K::Error, ";");
    assert_has("a |& ;", K::Error, ";");
    assert_has("a && && b", K::Error, "&&");
    assert_has("a && &", K::Error, "&");
}

#[test]
fn bang_only_starts_a_pipeline() {
    assert_has("a | ! b", K::Error, "!");
    assert_has(">f ! true", K::Error, "!");
    assert_has("a && ! b", K::ReservedWord, "!");
    assert_no_error("! a | b");
}

#[test]
fn time_before_compound_command() {
    for input in [
        "time { sleep 1 }",
        "time {sleep 1}",
        "time ( sleep 1 )",
        "time if a; then b; fi",
        "time [[ -f x ]]",
        "time ! true",
        "! time { echo }",
    ] {
        assert_no_error(input);
    }
    assert_eq!(cmds("time { a }"), vec![v(&["time"]), v(&["a"])]);
    assert_eq!(cmds("time ls -l"), vec![v(&["time", "ls", "-l"])]);
    assert_has("time { a }", K::ReservedWord, "{");
    // Other precommands cannot precede a group.
    assert_has("noglob { a }", K::Error, "}");
}

#[test]
fn redirection_before_compound_command() {
    for input in [
        ">/dev/null { echo quiet }",
        ">f if a; then b; fi",
        "2>&1 while a; do b; done",
        "> f ( a )",
        "if >/dev/null true; then echo; fi",
        "while <f read -r l; do :; done",
    ] {
        assert_no_error(input);
    }
    assert_has(">f if a; then b; fi", K::ReservedWord, "if");
    assert_eq!(cmds(">f { a }"), vec![v(&["a"])]);
}

#[test]
fn reserved_word_after_assignment_is_an_error() {
    for (input, word) in [
        ("x=1 if", "if"),
        ("x=1 [[ a ]]", "[["),
        ("x=1 {", "{"),
        ("x=1 !", "!"),
    ] {
        assert_has(input, K::Error, word);
    }
    // `in` and `always` are only reserved after `for`/`case` and a group.
    assert_no_error("x=1 in");
    assert_no_error("x=1 always");
}

#[test]
fn for_in_on_a_later_line() {
    let input = "for x\nin a b; do echo $x; done";
    assert_no_error(input);
    assert_has(input, K::ReservedWord, "in");
    assert_eq!(paths(input), v(&["a", "b"]));
    assert_no_error("for x\n\n  in a; do :; done");
    assert_no_error("select x\nin a; do :; done");
    let input = "for x\n# c\nin a; do :; done";
    let out = parse_with(input, opts(true, false, false));
    assert!(!spans_of(input, &out).iter().any(|(k, _)| *k == K::Error));
    assert_no_error("for x\ndo echo; done");
    // `in` is not a reserved word as a command of its own.
    assert_eq!(
        cmds("for x; do :; done\nin a"),
        vec![v(&[":"]), v(&["in", "a"])]
    );
}

#[test]
fn assignment_values_are_not_globbed() {
    let eg = opts(false, true, false);
    let kinds = |input: &str| -> Vec<(TokenKind, String)> {
        spans_of(input, &parse_with(input, eg))
            .into_iter()
            .filter(|(k, _)| matches!(k, K::Glob | K::GlobQualifier | K::BraceExpansion))
            .collect()
    };
    for input in [
        "x=~/foo ls",
        "y=a~b",
        "y=*.rs",
        "y=?.rs",
        "x=a(b)",
        "x=*(.)",
        "y={a,b}",
        "z=<1-5>",
        "w=[ab]^c#",
        "p=~/a:~/b",
        "local q=*.c r=~/x",
        "m=([k]=*.rs)",
    ] {
        assert_eq!(kinds(input), vec![], "{input:?}");
    }
    // Array elements are globbed, and so is a substitution inside a value.
    assert_eq!(
        kinds("a=(*.rs a~b)"),
        vec![(K::Glob, "*".into()), (K::Glob, "~".into())]
    );
    assert_eq!(kinds("x=$(ls *.rs)"), vec![(K::Glob, "*".into())]);
    // Through a precommand, `export` is an ordinary builtin whose arguments are globbed.
    assert_eq!(kinds("builtin export x=*.rs"), vec![(K::Glob, "*".into())]);
    // A declaration argument is a word without glob or tilde flags.
    let w = &parse_with("local q=~/*.c", eg).commands[0].words[1];
    assert!(!w.has_glob && !w.tilde, "{w:?}");
    assert_eq!(w.literal.as_deref(), Some("q=~/*.c"));
}

#[test]
fn spans_only_parse_matches_full_parse() {
    // `parse_with` compares the two for every input in this module; these exercise the
    // literals the grammar depends on.
    for input in [
        "local x=*.rs",
        "builtin local x=*.rs",
        "command builtin export x=1",
        "'local' x=*",
        "time { a }",
        "time ls",
        "cat <<'E' <<F\n$x\nE\n$y\nF",
        "a|a|a|a",
        "$(a)$(a)",
        "^a^b local x=1",
    ] {
        parse_with(input, opts(true, true, false));
    }
    assert!(parse("echo a", &ParseOptions::default()).commands.len() == 1);
}

/// The scan for the end of `((...))` as it was before memoisation, for comparison.
fn reference_arith_close(b: &[u8], i: usize, end: usize) -> parser::ArithEnd {
    use parser::ArithEnd;
    let mut depth = 0usize;
    let mut j = i;
    while j < end {
        match b[j] {
            b'(' => depth += 1,
            b')' => {
                if depth == 0 {
                    return match b.get(j + 1).filter(|_| j + 1 < end) {
                        Some(b')') => ArithEnd::Closed(j),
                        None => ArithEnd::Unclosed,
                        Some(_) => ArithEnd::NotArith,
                    };
                }
                depth -= 1;
            }
            b'\\' => j += 1,
            q @ (b'\'' | b'"') => {
                j += 1;
                while j < end && b[j] != q {
                    j += 1;
                }
            }
            _ => {}
        }
        j += 1;
    }
    ArithEnd::Unclosed
}

/// The subscript scan as it was before memoisation: the closing `]`, or where it stopped.
fn reference_subscript(b: &[u8], open: usize, end: usize) -> Result<usize, usize> {
    let mut depth = 0usize;
    let mut j = open;
    loop {
        match b.get(j).filter(|_| j < end) {
            None | Some(b'\n') => return Err(j.min(end)),
            Some(b'[') => depth += 1,
            Some(b']') => {
                depth -= 1;
                if depth == 0 {
                    return Ok(j);
                }
            }
            Some(b'\\') => j += 1,
            _ => {}
        }
        j += 1;
    }
}

/// The bracket expression scan as it was before its failure cache.
fn reference_bracket_end(b: &[u8], i: usize, end: usize) -> Option<usize> {
    let at = |j: usize| b.get(j).copied().filter(|_| j < end);
    let term = |c: u8| super::word::is_word_term(c);
    let mut j = i + 1;
    if matches!(at(j), Some(b'!' | b'^')) {
        j += 1;
    }
    if at(j) == Some(b']') {
        j += 1;
    }
    while let Some(c) = at(j) {
        match c {
            b']' => return Some(j + 1),
            b'[' if at(j + 1) == Some(b':') => {
                let mut k = j + 2;
                while let Some(d) = at(k) {
                    if (d == b':' && at(k + 1) == Some(b']')) || term(d) {
                        break;
                    }
                    k += 1;
                }
                j = if at(k) == Some(b':') { k + 2 } else { j + 2 };
            }
            b'\\' => j += 2,
            _ if term(c) || c == b'(' => break,
            _ => j += 1,
        }
    }
    None
}

#[test]
fn memoised_scans_match_a_fresh_scan() {
    use parser::{Bracket, Parser};
    let mut rng = XorShift(0x9e37_79b9_7f4a_7c15);
    for round in 0..400u32 {
        let alphabet: &[u8] = if round.is_multiple_of(2) {
            b"(()))'\"\\x \n"
        } else {
            b"[[[]]]:!^\\a (\n"
        };
        let len = 1 + (rng.next() % 60) as usize;
        let text: String = (0..len)
            .map(|_| alphabet[(rng.next() as usize) % alphabet.len()] as char)
            .collect();
        let b = text.as_bytes();
        let mut p = Parser::new(&text, ParseOptions::default(), false);
        // Query in a random order and under random limits, sharing one parser's memo tables
        // the way nested constructs do.
        for _ in 0..3 * len {
            let i = (rng.next() as usize) % len;
            let end = if rng.next().is_multiple_of(3) {
                i + 1 + (rng.next() as usize) % (len - i)
            } else {
                len
            };
            p.end = end;
            match b[i] {
                b'(' => assert_eq!(
                    p.find_arith_close(i + 1),
                    reference_arith_close(b, i + 1, end),
                    "arith {text:?} at {i} end {end}"
                ),
                b'[' => {
                    assert_eq!(
                        p.matching(Bracket::Subscript, i),
                        reference_subscript(b, i, end),
                        "subscript {text:?} at {i} end {end}"
                    );
                    assert_eq!(
                        p.bracket_end(i),
                        reference_bracket_end(b, i, end),
                        "bracket {text:?} at {i} end {end}"
                    );
                }
                _ => {}
            }
        }
    }
}
