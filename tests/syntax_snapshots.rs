//! Snapshot tests mapping input lines to the parser's spans, commands, and path words.

use fast_highlight::syntax::{ParseOptions, Word, parse};
use fast_highlight::token::check_spans;
use std::fmt::Write;

fn render_word(input: &str, w: &Word) -> String {
    let mut out = format!("{:?}", &input[w.start..w.end]);
    let mut flags = Vec::new();
    match &w.literal {
        Some(l) if l != &input[w.start..w.end] => flags.push(format!("literal {l:?}")),
        Some(_) => {}
        None => flags.push("expanded".to_string()),
    }
    if w.tilde {
        flags.push("tilde".to_string());
    }
    if w.has_glob {
        flags.push("glob".to_string());
    }
    if !flags.is_empty() {
        let _ = write!(out, " ({})", flags.join(", "));
    }
    out
}

/// One line per span `start..end kind "text"`, then the commands and path words.
fn render(input: &str, opts: ParseOptions) -> String {
    let out = parse(input, &opts);
    check_spans(input, &out.spans).expect("spans satisfy the invariants");
    let mut s = format!("input: {input:?}\n");
    if opts != ParseOptions::default() {
        let _ = writeln!(s, "options: {opts:?}");
    }
    s.push_str("spans:\n");
    for sp in &out.spans {
        let _ = writeln!(
            s,
            "  {}..{} {} {:?}",
            sp.start,
            sp.end,
            sp.kind,
            &input[sp.start..sp.end]
        );
    }
    s.push_str("commands:\n");
    for c in &out.commands {
        let words: Vec<String> = c.words.iter().map(|w| render_word(input, w)).collect();
        let _ = writeln!(s, "  {}", words.join(" | "));
    }
    if !out.path_words.is_empty() {
        s.push_str("path words:\n");
        for w in &out.path_words {
            let _ = writeln!(s, "  {}", render_word(input, w));
        }
    }
    s
}

fn snap(name: &str, input: &str) {
    insta::assert_snapshot!(name, render(input, ParseOptions::default()));
}

fn snap_with(name: &str, input: &str, opts: ParseOptions) {
    insta::assert_snapshot!(name, render(input, opts));
}

const ALL_OPTIONS: ParseOptions = ParseOptions {
    interactive_comments: true,
    extended_glob: true,
    ksh_glob: false,
};

#[test]
fn pipeline_with_redirections() {
    snap(
        "pipeline_with_redirections",
        "echo \"hello $USER\" | grep -i foo > out.txt 2>&1",
    );
}

#[test]
fn conditional_and_expansions() {
    snap(
        "conditional_and_expansions",
        "if [[ -f ~/x && $a == b* ]]; then echo ${(j:,:)arr} $((1+$x)); fi",
    );
}

#[test]
fn for_loop_with_substitutions() {
    snap(
        "for_loop_with_substitutions",
        "for x in a b *.c; do print -l $x[1] `date` $(ls -l; pwd); done",
    );
}

#[test]
fn case_statement() {
    snap(
        "case_statement",
        "case $x in (a|b) echo hi;; *.c) ;& *) ;| esac",
    );
}

#[test]
fn function_and_assignments() {
    snap(
        "function_and_assignments",
        "foo() { local x=1 arr=(a b); y+=2 cmd $'a\\tb' }",
    );
}

#[test]
fn words_and_literals() {
    snap(
        "words_and_literals",
        "echo {a,b} {1..3} {x} ~/foo ~ 'q w' \"e\"r a\\ b !! !$:h",
    );
}

#[test]
fn globs_and_qualifiers() {
    snap(
        "globs_and_qualifiers",
        "ls *(.) **/*.rs(N) [a-z]?* <1-5> (a|b).txt",
    );
}

#[test]
fn extended_glob_and_comments() {
    snap_with(
        "extended_glob_and_comments",
        "ls ^*.o *.c~x.c a## (#i)y *(#qN) ~/d # list $files",
        ALL_OPTIONS,
    );
}

#[test]
fn ksh_glob() {
    snap_with(
        "ksh_glob",
        "ls @(a|b) !(c) *(d)",
        ParseOptions {
            ksh_glob: true,
            ..ParseOptions::default()
        },
    );
}

#[test]
fn parameter_forms() {
    snap(
        "parameter_forms",
        "echo $1 $? $#arr ${#x} ${+x} ${=x} ${x:-\"d $y\"} ${${x}[1]} $f:t:r ${x/a/b} $ end",
    );
}

#[test]
fn arithmetic_and_subshell_disambiguation() {
    snap(
        "arithmetic_and_subshell_disambiguation",
        "(( i++ )); echo $(( (1+2) * $n )) $((cd /; ls)); ((cd /) | wc)",
    );
}

#[test]
fn process_substitution() {
    snap(
        "process_substitution",
        "diff <(sort a) >(tee b) =(date) < <(ls)",
    );
}

#[test]
fn redirection_forms() {
    snap(
        "redirection_forms",
        "cmd 2>&1 >&- <&3 &>a &>>b >|c >!d <>e {fd}>f <<<\"s\" >& 2",
    );
}

#[test]
fn heredoc_multi_line() {
    snap(
        "heredoc_multi_line",
        "cat <<EOF <<-'RAW' >out\nhello $USER\nEOF\n\t$not\n\tRAW\necho next",
    );
}

#[test]
fn multi_line_compound() {
    snap(
        "multi_line_compound",
        "while read -r line; do\n  if [[ $line == \\#* ]]; then\n    continue\n  fi\n  print -r -- $line\ndone < file",
    );
}

#[test]
fn prebuffer_continuation_string() {
    snap(
        "prebuffer_continuation_string",
        "echo \"first line\nsecond $x\" \\\n  more",
    );
}

#[test]
fn errors() {
    snap(
        "errors",
        "fi; | x && || y; echo > ; echo }; ( a ) b; esac;; )",
    );
}

#[test]
fn bad_redirection_targets() {
    snap("bad_redirection_targets", "echo > | cat; echo 2> )");
}

#[test]
fn partial_inputs() {
    for (name, input) in [
        ("partial_double_quote", "echo \"abc $x"),
        ("partial_command_substitution", "echo $(git sta"),
        ("partial_braced_parameter", "echo ${(j:,:)ar"),
        ("partial_if", "if [[ -n $x ]]; then"),
        ("partial_pipeline", "ls -l |"),
        ("partial_redirection", "sort <"),
        ("partial_heredoc", "cat <<EOF\nline one\nli"),
        ("partial_arithmetic", "echo $((1 + (2"),
        ("partial_case", "case $x in\n  a)"),
        ("partial_array", "arr=(one two"),
    ] {
        snap(name, input);
    }
}

#[test]
fn multibyte_text() {
    snap(
        "multibyte_text",
        "echo \"héllo 世界\" ${名前} $名前 🎉 '😀' é* \\é ü{a,b}",
    );
}

#[test]
fn multibyte_errors_and_history() {
    snap(
        "multibyte_errors_and_history",
        "日本 | | 語 \"!! ü\" >| 🎉 fi",
    );
}

#[test]
fn nested_quoting() {
    snap(
        "nested_quoting",
        "echo \"a $(echo \"b ${c:-`d`}\") e\" `f \\`g\\``",
    );
}

#[test]
fn declaration_and_associative_arrays() {
    snap(
        "declaration_and_associative_arrays",
        "typeset -A m=([k]=v [k2]+=$x) && builtin export P=$P:/x Q",
    );
}

#[test]
fn short_forms() {
    snap(
        "short_forms",
        "for x (a b) echo $x; if [[ a ]] { b } else { c }; repeat 2 { d }",
    );
}

#[test]
fn quick_substitution() {
    snap("quick_substitution", "^old^new");
}

#[test]
fn brace_groups_always_and_anonymous_functions() {
    snap(
        "brace_groups_always_and_anonymous_functions",
        "{ls} && { a } always { b } && () { c $1 } x fi; echo a}",
    );
}

#[test]
fn assignment_values_with_extended_glob() {
    snap_with(
        "assignment_values_with_extended_glob",
        "x=~/foo y=a~b z=*.rs a=(*.rs ~/x) local p=~/a:*.c",
        ALL_OPTIONS,
    );
}

#[test]
fn compound_after_time_and_redirection() {
    snap(
        "compound_after_time_and_redirection",
        "for x\nin a b; do time { echo $x }; done; >f if a; then b; fi",
    );
}
