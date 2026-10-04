use super::*;
use crate::token::{TokenKind, check_spans};
use TokenKind as K;

/// Parses and checks every structural invariant of the output.
fn parse_with(input: &str, opts: ParseOptions) -> ParseOutput {
    let out = parse(input, &opts);
    if let Err(e) = check_spans(input, &out.spans) {
        panic!("invalid spans for {input:?}: {e}");
    }
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
fn missing_condition_is_an_error() {
    assert_has("if then", K::Error, "then");
    assert_has("if a; then b; elif then", K::Error, "then");
    assert_has("while do", K::Error, "do");
    assert_has("until\ndo", K::Error, "do");
    assert_no_error("if\n a\nthen");
    assert_no_error("for x; do");
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
    // Keywords after an assignment or redirection are command words.
    assert_eq!(cmds("x=1 if"), vec![v(&["if"])]);
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
        "$", "(", ")", "{", "}", "[", "]", "<", ">", "|", "&", ";", "'", "\"", "`", "\\", "!", "#",
        "~", "^", "*", "?", "=", ",", "..", ":", "\n", " ", "\t", "-", "0", "2", "x", "é", "世",
        "if", "then", "fi", "do", "done", "case", "in", "esac", "for", "[[", "]]", "((", "))",
        "<<", "EOF", "$(", "${", "$((", "<(", "=(", "@(", "&&", "||", ";;", "\0",
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
    let big = line.repeat(10_240 / line.len() + 1);
    let start = std::time::Instant::now();
    for _ in 0..100 {
        std::hint::black_box(parse(std::hint::black_box(&big), &o));
    }
    println!("{} bytes: {:?} per parse", big.len(), start.elapsed() / 100);
    for (name, input) in adversarial_inputs() {
        let start = std::time::Instant::now();
        for _ in 0..10 {
            std::hint::black_box(parse(std::hint::black_box(&input), &o));
        }
        println!(
            "adversarial {name} ({} bytes): {:?} per parse",
            input.len(),
            start.elapsed() / 10
        );
    }
}

/// Roughly 10 KB inputs aimed at rescanning paths in the parser.
fn adversarial_inputs() -> Vec<(&'static str, String)> {
    vec![
        ("open parens", "(".repeat(10_000)),
        (
            "double parens closing apart",
            format!("{}{}", "((".repeat(2500), "x) ".repeat(2500)),
        ),
        (
            "dollar double parens",
            format!("{}{}", "$((".repeat(2000), "x) ".repeat(2000)),
        ),
        (
            "braces then parens",
            format!("{}{}", "{ ".repeat(2500), ") ".repeat(2500)),
        ),
        ("brackets", "[".repeat(10_000)),
        ("brace candidates", "{a".repeat(5000)),
        ("subscripts", "$a[".repeat(3000)),
        ("backquotes", "`".repeat(10_000)),
        ("heredocs", "cat <<E\n".repeat(1200)),
        ("history", "!?".repeat(5000)),
        ("dollar parens", "$(".repeat(5000)),
        ("quotes", "\"$(\"".repeat(2500)),
        ("assignment subscripts", "a[a[a[".repeat(1700)),
        ("stray closers", "fi ".repeat(3300)),
    ]
}

#[test]
fn adversarial_inputs_stay_fast() {
    let o = opts(true, true, true);
    for (name, input) in adversarial_inputs() {
        let start = std::time::Instant::now();
        parse_with(&input, o);
        // Generous bound for unoptimised builds.
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "{name}: {:?}",
            start.elapsed()
        );
    }
}
