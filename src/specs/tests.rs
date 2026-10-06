use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const SUB: Option<TokenKind> = Some(TokenKind::Subcommand);
const OPT: Option<TokenKind> = Some(TokenKind::CmdOption);
const ERR: Option<TokenKind> = Some(TokenKind::Error);
const PLAIN: Option<TokenKind> = None;

/// Test words: a word containing `$` stands for a word with an expansion (no literal), and its
/// text before the first `$` decides `name_eq`.
fn args<'a>(words: &[&'a str]) -> Vec<ArgInput<'a>> {
    words
        .iter()
        .map(|w| {
            let head = w.split('$').next().unwrap_or_default();
            ArgInput {
                literal: if w.contains('$') { None } else { Some(w) },
                name_eq: crate::syntax::is_name_eq(head),
            }
        })
        .collect()
}

fn spec(toml: &str) -> CommandSpec {
    SpecFile::parse(toml)
        .and_then(|f| f.compile())
        .expect("test spec is valid")
}

fn classify(spec: &CommandSpec, line: &str) -> Vec<Option<TokenKind>> {
    let words: Vec<&str> = line.split_whitespace().collect();
    spec.classify_args(&args(&words))
}

fn wrapped(spec: &CommandSpec, line: &str) -> Option<usize> {
    let words: Vec<&str> = line.split_whitespace().collect();
    spec.wrapped_command(&args(&words))
}

fn tail(spec: &CommandSpec, line: &str) -> Tail {
    let words: Vec<&str> = line.split_whitespace().collect();
    spec.tail(&args(&words))
}

fn builtin(name: &str) -> &'static CommandSpec {
    builtins()
        .registry
        .specs
        .get(name)
        .map(Arc::as_ref)
        .expect("built-in spec exists")
}

const GIT_LIKE: &str = r#"
name = "vcs"
common-options = ["-h", "--help"]
options = ["-C=", "--git-dir=", "-p"]

[subcommands.commit]
options = ["-m=", "--message=", "-a", "--all", "-v", "-S=?", "--color=?", "--amend"]

[subcommands.remote]
complete = true
options = ["-v"]

[subcommands.remote.subcommands.add]
options = ["-f"]

[subcommands.remote.subcommands.remove]
aliases = ["rm"]

[subcommands.strict]
options-complete = true
options = ["-a", "--all", "-n="]
"#;

// Parsing.

#[test]
fn parses_minimal_file() {
    let s = spec(r#"name = "x""#);
    assert_eq!(s.name(), "x");
    assert!(!s.is_precommand());
    assert_eq!(classify(&s, "a -b --c"), vec![PLAIN; 3]);
}

#[test]
fn rejects_unknown_keys_at_every_level() {
    let top = SpecFile::parse("name = \"x\"\nbogus = 1\n").unwrap_err();
    assert!(top.contains("bogus"), "{top}");
    let sub = SpecFile::parse("name = \"x\"\n[subcommands.a]\nprecommand = true\n").unwrap_err();
    assert!(sub.contains("precommand"), "{sub}");
    let sub =
        SpecFile::parse("name = \"x\"\n[subcommands.a]\nskip-assignments = true\n").unwrap_err();
    assert!(sub.contains("skip-assignments"), "{sub}");
}

#[test]
fn rejects_missing_name_and_bad_toml() {
    assert!(
        SpecFile::parse("options = []")
            .unwrap_err()
            .contains("name")
    );
    assert!(SpecFile::parse("name = ").is_err());
    let empty = SpecFile::parse(r#"name = """#)
        .unwrap()
        .compile()
        .unwrap_err();
    assert!(empty.contains("name"), "{empty}");
}

#[test]
fn rejects_invalid_option_declarations() {
    for bad in ["m=", "", "--", "-a=b", "*", "-a=*", "--x=?="] {
        let text = format!("name = \"x\"\n[subcommands.s]\noptions = [{bad:?}]\n");
        let err = SpecFile::parse(&text).unwrap().compile().unwrap_err();
        assert!(
            err.contains("x s"),
            "error for {bad:?} names the level: {err}"
        );
    }
    let err = SpecFile::parse("name = \"x\"\ncommon-options = [\"q\"]")
        .unwrap()
        .compile();
    assert!(err.unwrap_err().contains("common-options"));
}

#[test]
fn rejects_empty_aliases() {
    let top = SpecFile::parse("name = \"x\"\naliases = [\"\"]")
        .unwrap()
        .compile();
    assert!(top.is_err());
    let sub = SpecFile::parse("name = \"x\"\n[subcommands.a]\naliases = [\"\"]")
        .unwrap()
        .compile();
    assert!(sub.unwrap_err().contains("x a"));
}

// Classification.

#[test]
fn known_subcommand_and_options() {
    let s = spec(GIT_LIKE);
    assert_eq!(
        classify(&s, "commit -a --amend file"),
        vec![SUB, OPT, OPT, PLAIN]
    );
}

#[test]
fn required_value_forms() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "commit -m msg -a"), vec![SUB, OPT, PLAIN, OPT]);
    assert_eq!(
        classify(&s, "commit --message msg -a"),
        vec![SUB, OPT, PLAIN, OPT]
    );
    assert_eq!(classify(&s, "commit --message=msg -a"), vec![SUB, OPT, OPT]);
    assert_eq!(classify(&s, "commit -mmsg -a"), vec![SUB, OPT, OPT]);
    // The value word is consumed even when it looks like an option.
    assert_eq!(classify(&s, "commit -m --all"), vec![SUB, OPT, PLAIN]);
    // A value-taking option at the end of the line has no value yet.
    assert_eq!(classify(&s, "commit -m"), vec![SUB, OPT]);
}

#[test]
fn optional_value_is_attached_only() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "commit --color=always"), vec![SUB, OPT]);
    assert_eq!(classify(&s, "commit --color always"), vec![SUB, OPT, PLAIN]);
    assert_eq!(classify(&s, "commit -S -a"), vec![SUB, OPT, OPT]);
    assert_eq!(classify(&s, "commit -SKEYID"), vec![SUB, OPT]);
}

#[test]
fn flag_with_attached_value_is_unknown() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "commit --all=yes"), vec![SUB, PLAIN]);
    assert_eq!(classify(&s, "strict --all=yes"), vec![SUB, ERR]);
}

#[test]
fn short_option_bundling() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "commit -av"), vec![SUB, OPT]);
    assert_eq!(
        classify(&s, "commit -avm msg x"),
        vec![SUB, OPT, PLAIN, PLAIN]
    );
    assert_eq!(classify(&s, "commit -avmmsg x"), vec![SUB, OPT, PLAIN]);
    // `-x` is not declared, so the bundle is unknown.
    assert_eq!(classify(&s, "commit -avx"), vec![SUB, PLAIN]);
    assert_eq!(classify(&s, "strict -ax"), vec![SUB, ERR]);
    assert_eq!(classify(&s, "strict -an 5 -aa"), vec![SUB, OPT, PLAIN, OPT]);
}

#[test]
fn double_dash_ends_options() {
    let s = spec(GIT_LIKE);
    assert_eq!(
        classify(&s, "commit -- -a --all"),
        vec![SUB, OPT, PLAIN, PLAIN]
    );
    assert_eq!(classify(&s, "-- commit"), vec![OPT, PLAIN]);
    assert_eq!(classify(&s, "strict -- --bogus"), vec![SUB, OPT, PLAIN]);
}

#[test]
fn single_dash_is_plain() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "strict -"), vec![SUB, PLAIN]);
    // Not an error at a complete level, and it takes the positional slot.
    assert_eq!(classify(&s, "remote - add"), vec![SUB, PLAIN, PLAIN]);
}

#[test]
fn nested_subcommands_and_aliases() {
    let s = spec(GIT_LIKE);
    assert_eq!(
        classify(&s, "remote add -f origin url"),
        vec![SUB, SUB, OPT, PLAIN, PLAIN]
    );
    assert_eq!(
        classify(&s, "remote -v rm origin"),
        vec![SUB, OPT, SUB, PLAIN]
    );
    assert_eq!(classify(&s, "remote remove origin"), vec![SUB, SUB, PLAIN]);
}

#[test]
fn unknown_subcommand_is_error_only_when_complete() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "frobnicate commit"), vec![PLAIN, PLAIN]);
    assert_eq!(classify(&s, "remote bogus add"), vec![SUB, ERR, PLAIN]);
    // A word with an expansion is never an error, and it fills the subcommand slot.
    assert_eq!(classify(&s, "remote $x add"), vec![SUB, PLAIN, PLAIN]);
}

#[test]
fn subcommand_only_at_first_positional() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "commit file remote"), vec![SUB, PLAIN, PLAIN]);
    assert_eq!(classify(&s, "$cmd commit"), vec![PLAIN, PLAIN]);
    // Options before the subcommand do not take its slot, and their values are plain.
    assert_eq!(
        classify(&s, "-C commit -p commit"),
        vec![OPT, PLAIN, OPT, SUB]
    );
    assert_eq!(classify(&s, "-C $dir commit"), vec![OPT, PLAIN, SUB]);
}

#[test]
fn level_options_do_not_leak_into_subcommands() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "-p commit -p"), vec![OPT, SUB, PLAIN]);
    assert_eq!(classify(&s, "remote add -v"), vec![SUB, SUB, PLAIN]);
}

#[test]
fn common_options_apply_at_every_level() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "-h"), vec![OPT]);
    assert_eq!(classify(&s, "remote add --help"), vec![SUB, SUB, OPT]);
}

#[test]
fn options_complete_flags_unknown_options() {
    let s = spec(GIT_LIKE);
    assert_eq!(
        classify(&s, "strict --bogus -z x"),
        vec![SUB, ERR, ERR, PLAIN]
    );
    assert_eq!(
        classify(&s, "commit --bogus -z x"),
        vec![SUB, PLAIN, PLAIN, PLAIN]
    );
    // An unknown option does not consume the next word or take the subcommand slot.
    let top = spec("name = \"x\"\noptions-complete = true\n[subcommands.a]\n");
    assert_eq!(classify(&top, "--bogus a"), vec![ERR, SUB]);
}

#[test]
fn negative_numbers_are_never_errors() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "strict -5 -12"), vec![SUB, PLAIN, PLAIN]);
    let declared = spec("name = \"x\"\noptions-complete = true\noptions = [\"-1\"]\n");
    assert_eq!(classify(&declared, "-1 -2"), vec![OPT, PLAIN]);
}

#[test]
fn options_first_stops_at_first_positional() {
    let s = spec(
        r#"
name = "ctr"
[subcommands.run]
options-first = true
options = ["--rm", "-e=", "-l="]
"#,
    );
    assert_eq!(
        classify(&s, "run --rm -e A=1 img ls -l -- x"),
        vec![SUB, OPT, OPT, PLAIN, PLAIN, PLAIN, PLAIN, PLAIN, PLAIN]
    );
    assert_eq!(classify(&s, "run $img -l"), vec![SUB, PLAIN, PLAIN]);
    assert_eq!(classify(&s, "run -- img -l"), vec![SUB, OPT, PLAIN, PLAIN]);
}

#[test]
fn abbreviations_select_unique_prefixes() {
    let s = spec(
        r#"
name = "pm"
complete = true
abbreviations = true
[subcommands.install]
aliases = ["i"]
[subcommands.publish]
[subcommands.prune]
"#,
    );
    assert_eq!(classify(&s, "inst"), vec![SUB]);
    assert_eq!(classify(&s, "i"), vec![SUB]);
    assert_eq!(classify(&s, "pu"), vec![SUB]);
    // `pr` selects `prune`; `p` is a prefix of three names, so it selects nothing.
    assert_eq!(classify(&s, "pr"), vec![SUB]);
    assert_eq!(classify(&s, "p"), vec![ERR]);
    assert_eq!(classify(&s, "installx"), vec![ERR]);
    let exact_only = spec("name = \"x\"\ncomplete = true\n[subcommands.install]\n");
    assert_eq!(classify(&exact_only, "inst"), vec![ERR]);
}

const OPTION_ABBREV: &str = r#"
name = "ab"
option-abbreviations = true
options-complete = true
common-options = ["--help", "--hidden-common"]
options = [
  "-v", "--verbose", "--version", "--output=", "--level=", "--color=?", "--colour=?",
  "--exclude=", "--exclude-caches", "--no-*", "--now", "--all", "--almost-all", "--flag",
  "--flagship",
]

[subcommands.sub]
options-complete = true
options = ["--subopt"]

[subcommands.off]
option-abbreviations = false
options-complete = true
options = ["--offopt"]
"#;

#[test]
fn option_abbreviations_are_off_by_default() {
    let s =
        spec("name = \"x\"\noptions-complete = true\noptions = [\"--verbose\", \"--output=\"]\n");
    assert_eq!(classify(&s, "--verb"), vec![ERR]);
    assert_eq!(classify(&s, "--out file"), vec![ERR, PLAIN]);
    assert_eq!(classify(&s, "--verbose"), vec![OPT]);
}

#[test]
fn option_abbreviations_resolve_unique_prefixes_with_arity() {
    let s = spec(OPTION_ABBREV);
    assert_eq!(classify(&s, "--verb"), vec![OPT]);
    assert_eq!(classify(&s, "--vers"), vec![OPT]);
    // A required value: the next word is consumed, so `--all` after it is a plain word.
    assert_eq!(classify(&s, "--out --all"), vec![OPT, PLAIN]);
    assert_eq!(classify(&s, "--out=file --all"), vec![OPT, OPT]);
    assert_eq!(classify(&s, "--lev=3"), vec![OPT]);
    // The value may itself contain `=`; only the text before the first `=` is the stem.
    assert_eq!(classify(&s, "--lev=a=b"), vec![OPT]);
    // An optional value is attached only; the next word is not consumed.
    assert_eq!(classify(&s, "--colou --all"), vec![OPT, OPT]);
    assert_eq!(classify(&s, "--colou=never"), vec![OPT]);
    // A flag never takes an attached value.
    assert_eq!(classify(&s, "--verb=1"), vec![ERR]);
    assert_eq!(classify(&s, "--alm"), vec![OPT]);
}

#[test]
fn option_abbreviations_ambiguous_and_unmatched_are_unknown() {
    let s = spec(OPTION_ABBREV);
    // `--ver` matches `--verbose` and `--version`; `--al` matches `--all` and `--almost-all`.
    assert_eq!(classify(&s, "--ver"), vec![ERR]);
    assert_eq!(classify(&s, "--al"), vec![ERR]);
    assert_eq!(classify(&s, "--ver=1"), vec![ERR]);
    // Strict: two spellings of the same option are still two candidates.
    assert_eq!(classify(&s, "--colo"), vec![ERR]);
    assert_eq!(classify(&s, "--qqq"), vec![ERR]);
    assert_eq!(classify(&s, "--verbosex"), vec![ERR]);
    // An unknown option is not an error where the option list is not complete.
    let open = spec(&OPTION_ABBREV.replacen("options-complete = true\n", "", 1));
    assert_eq!(classify(&open, "--ver"), vec![PLAIN]);
    assert_eq!(classify(&open, "--qqq"), vec![PLAIN]);
    // An unknown option is assumed to be a flag: the next word is not consumed.
    assert_eq!(classify(&open, "--ver --all"), vec![PLAIN, OPT]);
}

#[test]
fn option_abbreviations_exact_spelling_wins() {
    let s = spec(OPTION_ABBREV);
    // `--exclude` is exact although it is also a prefix of `--exclude-caches`.
    assert_eq!(classify(&s, "--exclude --all"), vec![OPT, PLAIN]);
    assert_eq!(classify(&s, "--exclude=x"), vec![OPT]);
    assert_eq!(classify(&s, "--exclude-c --all"), vec![OPT, OPT]);
    // `--flag` is exact, so `--flag=x` is a flag with a value, not a prefix of `--flagship`.
    assert_eq!(classify(&s, "--flag=x"), vec![ERR]);
    assert_eq!(classify(&s, "--flag"), vec![OPT]);
    assert_eq!(classify(&s, "--flags"), vec![OPT]);
}

#[test]
fn option_abbreviations_apply_to_long_options_only() {
    let s = spec(OPTION_ABBREV);
    // Single-dash words never abbreviate: `-ver` is a bundle with unknown letters.
    assert_eq!(classify(&s, "-ver"), vec![ERR]);
    assert_eq!(classify(&s, "-v"), vec![OPT]);
    assert_eq!(classify(&s, "--"), vec![OPT]);
    for word in ["--=", "--=x", "---", "--é", "--=verbose"] {
        assert_eq!(classify(&s, word), vec![ERR], "{word:?}");
    }
    // A pattern is not a candidate: `--no` selects `--now`, and `--no-x` matches `--no-*`.
    assert_eq!(classify(&s, "--no"), vec![OPT]);
    assert_eq!(classify(&s, "--n"), vec![OPT]);
    assert_eq!(classify(&s, "--no-x"), vec![OPT]);
}

#[test]
fn option_abbreviations_cover_common_options_per_level() {
    let s = spec(OPTION_ABBREV);
    assert_eq!(classify(&s, "--hid"), vec![OPT]);
    assert_eq!(classify(&s, "--he"), vec![OPT]);
    // A level's own options do not leak to its parent or its siblings.
    assert_eq!(classify(&s, "--sub"), vec![ERR]);
    assert_eq!(classify(&s, "sub --sub --hid"), vec![SUB, OPT, OPT]);
    assert_eq!(classify(&s, "sub --verb"), vec![SUB, ERR]);
    // A child that turns the key off resolves no prefix, common options included.
    assert_eq!(
        classify(&s, "off --offo --hid --offopt --hidden-common"),
        vec![SUB, ERR, ERR, OPT, OPT]
    );
}

#[test]
fn option_abbreviations_bare_double_dash_stem_selects_nothing() {
    // With one long option, the empty stem of `--=x` would be a unique prefix of it.
    let s = spec(
        "name = \"x\"\noption-abbreviations = true\noptions-complete = true\n\
         options = [\"--output=\"]\n",
    );
    assert_eq!(classify(&s, "--o=x"), vec![OPT]);
    for word in ["--=x", "--="] {
        assert_eq!(classify(&s, word), vec![ERR], "{word:?}");
    }
}

#[test]
fn option_abbreviations_value_consumption_affects_subcommand_position() {
    let s = spec(
        r#"
name = "x"
option-abbreviations = true
complete = true
options = ["--output=", "--verbose"]
[subcommands.sub]
"#,
    );
    // `--out` takes the next word as its value, so the following `sub` is the subcommand.
    assert_eq!(classify(&s, "--out sub sub"), vec![OPT, PLAIN, SUB]);
    assert_eq!(classify(&s, "--out=f sub"), vec![OPT, SUB]);
    // `--verb` is a flag, so `sub` right after it is the subcommand.
    assert_eq!(classify(&s, "--verb sub"), vec![OPT, SUB]);
}

#[test]
fn option_abbreviations_child_options_stay_at_their_level() {
    // `common-options` is a top-level key only, so a child cannot add options for its
    // descendants; its own options apply at that level and nowhere else.
    let err = SpecFile::parse("name = \"x\"\n[subcommands.a]\ncommon-options = [\"--apple\"]\n")
        .unwrap_err();
    assert!(err.contains("common-options"), "{err}");
    let s = spec(
        r#"
name = "x"
option-abbreviations = true
options-complete = true
[subcommands.a]
options-complete = true
options = ["--apple"]
[subcommands.a.subcommands.inner]
options-complete = true
[subcommands.b]
options-complete = true
options = ["--banana"]
"#,
    );
    assert_eq!(classify(&s, "a --app"), vec![SUB, OPT]);
    assert_eq!(classify(&s, "--app"), vec![ERR]);
    assert_eq!(classify(&s, "b --app"), vec![SUB, ERR]);
    assert_eq!(classify(&s, "a inner --app"), vec![SUB, SUB, ERR]);
    assert_eq!(classify(&s, "a --ban"), vec![SUB, ERR]);
}

#[test]
fn option_abbreviations_ambiguous_prefix_completes() {
    let s = spec(OPTION_ABBREV);
    // The semantic pass asks only for words classified as errors.
    assert!(completes(&s, "--ver", 0));
    assert!(completes(&s, "--al", 0));
    assert!(!completes(&s, "--qqq", 0));
    assert!(!completes(&s, "--verbosex", 0));
}

#[test]
fn option_abbreviations_in_builtin_specs() {
    let journalctl = builtin("journalctl");
    // `--verify` is exact and a flag, although it is a prefix of `--verify-key=`.
    assert_eq!(classify(journalctl, "--verify --utc"), vec![OPT, OPT]);
    assert_eq!(classify(journalctl, "--verif --utc"), vec![PLAIN, OPT]);
    assert_eq!(classify(journalctl, "--verify=x"), vec![PLAIN]);
    assert_eq!(classify(journalctl, "--verify-k --utc"), vec![OPT, PLAIN]);
    assert_eq!(classify(journalctl, "--t"), vec![PLAIN]);
    // `--dir` selects `--directory=`, which consumes `-x`; `--fil` is `--file` or `--files-from`.
    assert_eq!(classify(builtin("tar"), "--dir -x"), vec![OPT, PLAIN]);
    assert_eq!(classify(builtin("tar"), "--fil -x"), vec![PLAIN, OPT]);
    assert_eq!(classify(builtin("tar"), "--exclude-b"), vec![OPT]);
    assert_eq!(classify(builtin("tar"), "--exclude-cac"), vec![PLAIN]);
    assert_eq!(classify(builtin("make"), "--jobserver"), vec![PLAIN]);
    assert_eq!(classify(builtin("make"), "--te -k"), vec![OPT, PLAIN]);
    assert_eq!(classify(builtin("rm"), "--rec"), vec![OPT]);
}

#[test]
fn option_abbreviations_in_wrapped_commands() {
    let timeout = builtin("timeout");
    // An ambiguous prefix is an unknown option, skipped as a flag.
    assert_eq!(wrapped(timeout, "--ver 5 ls"), Some(2));
    assert_eq!(wrapped(timeout, "--sig KILL 5 ls"), Some(3));
    assert_eq!(wrapped(timeout, "--sig=a=b 5 ls"), Some(2));
    assert_eq!(wrapped(timeout, "--kill 1 --fore 5 ls"), Some(4));
    // `--default-signal` takes an optional attached value, so `ls` is not consumed.
    assert_eq!(wrapped(builtin("env"), "--defa ls"), Some(1));
    assert_eq!(wrapped(builtin("env"), "--uns HOME ls"), Some(2));
    assert_eq!(wrapped(builtin("nice"), "--adj 5 ls"), Some(2));
    assert_eq!(wrapped(builtin("xargs"), "--max-a 1 rm"), Some(2));
    assert_eq!(wrapped(builtin("stdbuf"), "--out L ls"), Some(2));
    assert_eq!(wrapped(builtin("time"), "--out f ls"), Some(2));
    assert_eq!(wrapped(builtin("chrt"), "--sched-r 5 10 ls"), Some(3));
    assert_eq!(wrapped(builtin("ionice"), "--classd 7 ls"), Some(2));
    assert_eq!(wrapped(builtin("ionice"), "--cl 2 ls"), Some(1));
    assert_eq!(wrapped(builtin("taskset"), "--cpu 0-3 ls"), Some(2));
    assert_eq!(wrapped(builtin("nohup"), "--he ls"), Some(1));
}

#[test]
fn option_abbreviations_builtin_set() {
    let mut enabled: Vec<String> = BUILTIN_FILES
        .iter()
        .map(|(_, text)| SpecFile::parse(text).and_then(|f| f.compile()).unwrap())
        .filter(|s| s.options.abbreviate)
        .map(|s| s.name)
        .collect();
    enabled.sort();
    // Each of these declares every long option of the checked version; enable another spec
    // only after checking that, and add it here.
    let expected = [
        "chrt",
        "env",
        "ionice",
        "journalctl",
        "make",
        "nice",
        "nohup",
        "rm",
        "stdbuf",
        "tar",
        "taskset",
        "time",
        "timeout",
        "xargs",
    ];
    assert_eq!(enabled, expected);
    for name in expected {
        assert_eq!(classify(builtin(name), "--hel"), vec![OPT], "{name}");
    }
    for name in [
        "git", "ip", "npm", "docker", "ssh", "cargo", "gh", "kubectl", "uv", "sudo", "doas",
    ] {
        assert!(!builtin(name).options.abbreviate, "{name}");
    }
}

#[test]
fn option_abbreviations_inherit_through_levels() {
    let s = spec(
        r#"
name = "t"
options-complete = true
[subcommands.a]
option-abbreviations = true
options-complete = true
options = ["--alpha"]
[subcommands.a.subcommands.b]
options-complete = true
options = ["--beta"]
[subcommands.a.subcommands.b.subcommands.c]
options-complete = true
options = ["--gamma"]
[subcommands.a.subcommands.off]
option-abbreviations = false
options-complete = true
options = ["--delta"]
"#,
    );
    // A child turns it on under a parent that leaves it unset; the top level stays off.
    assert_eq!(classify(&s, "--alp"), vec![ERR]);
    assert_eq!(classify(&s, "a --alp"), vec![SUB, OPT]);
    // A grandchild inherits through a child that leaves it unset.
    assert_eq!(classify(&s, "a b --bet"), vec![SUB, SUB, OPT]);
    assert_eq!(classify(&s, "a b c --gam"), vec![SUB, SUB, SUB, OPT]);
    // An explicit `false` wins over the inherited value.
    assert_eq!(classify(&s, "a off --del"), vec![SUB, SUB, ERR]);
}

#[test]
fn prefix_patterns_match_option_words() {
    let s = spec(
        r#"
name = "cargo"
options = ["+*", "-Z="]
common-options = ["--no-*"]
[subcommands.build]
"#,
    );
    assert_eq!(classify(&s, "+nightly build"), vec![OPT, SUB]);
    assert_eq!(
        classify(&s, "-Zunstable build --no-color"),
        vec![OPT, SUB, OPT]
    );
    assert_eq!(classify(&s, "build +x"), vec![SUB, PLAIN]);
}

fn completes(spec: &CommandSpec, line: &str, index: usize) -> bool {
    let words: Vec<&str> = line.split_whitespace().collect();
    spec.completes_at(&args(&words), index)
}

#[test]
fn completes_at_subcommand_prefixes() {
    let s = spec(GIT_LIKE);
    // Top level is not complete, but a prefix is still reported; the caller only asks when
    // classification said error.
    assert!(completes(&s, "comm", 0));
    assert!(completes(&s, "remote ad", 1));
    // An alias counts.
    let r = spec("name = \"x\"\ncomplete = true\n[subcommands.remove]\naliases = [\"rmdir\"]\n");
    assert!(completes(&r, "rmd", 0));
    // Exact names, non-prefixes, and words past the subcommand slot do not complete.
    assert!(!completes(&s, "commit", 0));
    assert!(!completes(&s, "remote addx", 1));
    assert!(!completes(&s, "commit comm", 1));
    assert!(!completes(&s, "remote add ad", 2));
    // An option value is never a subcommand slot.
    assert!(!completes(&s, "-C comm", 1));
    assert!(completes(&s, "-C dir comm", 2));
    // Out of range or not literal.
    assert!(!completes(&s, "comm", 1));
    assert!(!completes(&s, "$x", 0));
}

#[test]
fn completes_at_option_prefixes() {
    let s = spec(GIT_LIKE);
    assert_eq!(classify(&s, "strict --al"), vec![SUB, ERR]);
    assert!(completes(&s, "strict --al", 1));
    // `--` ends options and is never an error, so it never needs completing.
    assert_eq!(classify(&s, "strict --")[1], OPT);
    assert!(!completes(&s, "strict --", 1));
    // Common options are part of every level.
    assert!(completes(&s, "strict --hel", 1));
    // Options of another level do not count.
    assert!(!completes(&s, "strict --amen", 1));
    assert!(completes(&s, "commit --amen", 1));
    // Exact spellings and non-prefixes.
    assert!(!completes(&s, "strict --all", 1));
    assert!(!completes(&s, "strict --allx", 1));
    // Patterns: `--no` is a prefix of `--no-*`.
    let p = spec("name = \"x\"\noptions-complete = true\noptions = [\"--no-*\"]\n");
    assert_eq!(classify(&p, "--no"), vec![ERR]);
    assert!(completes(&p, "--no", 0));
    assert!(!completes(&p, "--yes", 0));
}

#[test]
fn builtin_specs_complete_typed_prefixes() {
    let systemctl = builtin("systemctl");
    assert_eq!(classify(systemctl, "stat"), vec![ERR]);
    assert!(completes(systemctl, "stat", 0));
    assert!(!completes(systemctl, "stax", 0));
    let npm = builtin("npm");
    // `npm st` is ambiguous (star, stars, start, ...), so it is an error, but a prefix.
    assert_eq!(classify(npm, "st"), vec![ERR]);
    assert!(completes(npm, "st", 0));
}

#[test]
fn classification_is_total_on_odd_input() {
    let s = spec(GIT_LIKE);
    assert!(s.classify_args(&[]).is_empty());
    let odd = [
        "",
        "-",
        "--",
        "---",
        "-=",
        "--=",
        "=",
        "-é",
        "-mé",
        "--message=",
        "commit",
    ];
    assert_eq!(s.classify_args(&args(&odd)).len(), odd.len());
    for i in 0..odd.len() {
        assert_eq!(s.classify_args(&args(&odd[i..])).len(), odd.len() - i);
        assert!(
            s.wrapped_command(&args(&odd[i..]))
                .is_none_or(|n| n < odd.len() - i)
        );
    }
}

// Precommands.

#[test]
fn sudo_skips_options_and_values() {
    let sudo = builtin("sudo");
    assert!(sudo.is_precommand());
    assert_eq!(wrapped(sudo, "-u root -E cmd arg"), Some(3));
    assert_eq!(wrapped(sudo, "-uroot cmd"), Some(1));
    assert_eq!(wrapped(sudo, "--user=root cmd"), Some(1));
    assert_eq!(wrapped(sudo, "--user root -- cmd"), Some(3));
    assert_eq!(wrapped(sudo, "-E FOO=bar cmd"), Some(2));
    assert_eq!(wrapped(sudo, "--bogus cmd"), Some(1));
    assert_eq!(wrapped(sudo, "$cmd"), Some(0));
    assert_eq!(wrapped(sudo, ""), None);
    assert_eq!(wrapped(sudo, "-u"), None);
    assert_eq!(wrapped(sudo, "-u root"), None);
    let own = classify(sudo, "-u root -E");
    assert_eq!(own, vec![OPT, PLAIN, OPT]);
}

#[test]
fn env_skips_assignments() {
    let env = builtin("env");
    assert_eq!(wrapped(env, "-i A=1 B=2 cmd"), Some(3));
    assert_eq!(wrapped(env, "- A=1 cmd"), Some(2));
    assert_eq!(wrapped(env, "-u HOME cmd"), Some(2));
    assert_eq!(wrapped(env, "A=1"), None);
    // Option parsing stops at the first assignment.
    assert_eq!(wrapped(env, "A=1 -i"), Some(1));
    // Not an assignment: the name is not an identifier.
    assert_eq!(wrapped(env, "1A=1 cmd"), Some(0));
}

#[test]
fn assignments_with_expansions_are_skipped() {
    let env = builtin("env");
    assert_eq!(wrapped(env, "FOO=$x ls"), Some(1));
    assert_eq!(wrapped(env, "-i FOO=$x BAR=1 ls"), Some(3));
    assert_eq!(wrapped(env, "-u FOO BAR=$x ls"), Some(3));
    assert_eq!(wrapped(env, "-- FOO=$x ls"), Some(2));
    assert_eq!(wrapped(env, "FOO=$x"), None);
    assert_eq!(wrapped(env, "$cmd ls"), Some(0));
    // An option value with an expansion is still consumed by its option.
    assert_eq!(wrapped(env, "-u $v ls"), Some(2));
    // Option parsing stops at the first assignment, with or without an expansion.
    assert_eq!(wrapped(env, "FOO=$x -i ls"), Some(1));
    // The name itself contains an expansion: not `NAME=value`.
    assert_eq!(wrapped(env, "FOO$x=1 ls"), Some(0));
    assert_eq!(wrapped(builtin("sudo"), "-u root FOO=$x ls"), Some(3));
    // Without `skip-assignments` the word is the wrapped command.
    assert_eq!(wrapped(builtin("nice"), "FOO=$x ls"), Some(0));
    // With `positional`, assignments are skipped only before the positional words.
    let wrap = spec("name = \"w\"\nprecommand = true\nskip-assignments = true\npositional = 1");
    assert_eq!(wrapped(&wrap, "FOO=$x 5 ls"), Some(2));
    assert_eq!(wrapped(&wrap, "5 FOO=$x ls"), Some(1));
    // An expansion in the positional slot fills it like a literal.
    assert_eq!(wrapped(&wrap, "$x ls"), Some(1));
    assert_eq!(wrapped(&wrap, "FOO=$x $y ls"), Some(2));
    assert_eq!(wrapped(&wrap, "$x"), None);
}

#[test]
fn nice_exec_command_and_modifiers() {
    assert_eq!(wrapped(builtin("nice"), "-n 5 cmd"), Some(2));
    assert_eq!(wrapped(builtin("nice"), "-5 cmd"), Some(1));
    assert_eq!(wrapped(builtin("exec"), "-a name cmd"), Some(2));
    assert_eq!(wrapped(builtin("exec"), "-cl cmd"), Some(1));
    assert_eq!(wrapped(builtin("command"), "-p ls"), Some(1));
    assert_eq!(wrapped(builtin("noglob"), "rm *"), Some(0));
    assert_eq!(wrapped(builtin("nocorrect"), "cmd"), Some(0));
    assert_eq!(wrapped(builtin("builtin"), "echo"), Some(0));
    assert_eq!(wrapped(builtin("-"), "zsh"), Some(0));
    assert_eq!(wrapped(builtin("nohup"), "cmd"), Some(0));
    assert_eq!(wrapped(builtin("doas"), "-u root cmd"), Some(2));
    assert_eq!(wrapped(builtin("stdbuf"), "-oL cmd"), Some(1));
    assert_eq!(wrapped(builtin("ionice"), "-c 2 -n 7 cmd"), Some(4));
    assert_eq!(wrapped(builtin("time"), "-v cmd"), Some(1));
}

#[test]
fn positional_arguments_before_wrapped_command() {
    let timeout = builtin("timeout");
    assert_eq!(wrapped(timeout, "5 cmd"), Some(1));
    assert_eq!(wrapped(timeout, "-s KILL 5 cmd"), Some(3));
    assert_eq!(wrapped(timeout, "--signal=KILL -k 1 5s cmd"), Some(4));
    assert_eq!(wrapped(timeout, "$t cmd"), Some(1));
    assert_eq!(wrapped(timeout, "5"), None);
    // Option parsing stops at the duration; assignments are not skipped.
    assert_eq!(wrapped(timeout, "5 -s"), Some(1));
    assert_eq!(wrapped(timeout, "5 A=1"), Some(1));
    assert_eq!(wrapped(builtin("chrt"), "-r 10 cmd"), Some(2));
    assert_eq!(wrapped(builtin("taskset"), "-c 0-3 cmd"), Some(2));
}

// Wrapped commands and path mode.

#[test]
fn sudo_edit_selects_path_mode() {
    let sudo = builtin("sudo");
    // Every letter parsed as an option counts, up to and including the one that takes a value.
    for line in [
        "-e /etc/hosts",
        "--edit /etc/hosts",
        "-Ee f",
        "-Eu root -e f",
        "-eu root f",
        "-u root -e f",
        "-e -u root /etc/hosts",
        "--user=root -e f",
    ] {
        assert_eq!(tail(sudo, line), Tail::Paths, "{line}");
    }
    // `e` is the value of `-u`, or the word `-e` is.
    assert_eq!(tail(sudo, "-ue ls"), Tail::Command(1));
    assert_eq!(tail(sudo, "-u -e ls"), Tail::Command(2));
    // After the options: an operand of the command, or the command itself.
    assert_eq!(tail(sudo, "ls -e"), Tail::Command(0));
    assert_eq!(tail(sudo, "-- -e"), Tail::Command(1));
    // An unknown bundle never selects the mode.
    assert_eq!(tail(sudo, "-ez f"), Tail::Command(1));
    // No operand yet is still path mode, not a missing command.
    assert_eq!(tail(sudo, "-e"), Tail::Paths);
    assert_eq!(tail(sudo, "-e -u"), Tail::Paths);
    assert_eq!(sudo.wrapped_command(&args(&["-e", "f"])), None);
}

#[test]
fn path_mode_is_sticky() {
    let sudo = builtin("sudo");
    // Later options are still parsed and never cancel the mode; assignments are operands, and
    // so is everything after `--`.
    assert_eq!(tail(sudo, "-e -u root -E f"), Tail::Paths);
    assert_eq!(tail(sudo, "-e FOO=1 ls"), Tail::Paths);
    assert_eq!(tail(sudo, "-e -- ls"), Tail::Paths);
    assert_eq!(tail(sudo, "-e $f ls"), Tail::Paths);
    // The plain words are not the command's own arguments: `options-first` makes `-n` after
    // the first file an operand.
    assert_eq!(classify(sudo, "-e f -n"), vec![OPT, PLAIN, PLAIN]);
    assert_eq!(classify(sudo, "-e -u root f"), vec![OPT, OPT, PLAIN, PLAIN]);
}

#[test]
fn sudo_without_edit_is_unchanged() {
    let sudo = builtin("sudo");
    assert_eq!(tail(sudo, "ls"), Tail::Command(0));
    assert_eq!(tail(sudo, "-u root ls"), Tail::Command(2));
    assert_eq!(tail(sudo, "-E FOO=$x ls -e"), Tail::Command(2));
    assert_eq!(tail(sudo, ""), Tail::None);
    assert_eq!(tail(sudo, "-u root"), Tail::None);
}

#[test]
fn option_abbreviation_selects_path_mode() {
    let dir = TempDir::new();
    dir.write(
        "sudo.toml",
        "name = \"sudo\"\nmerge = true\noption-abbreviations = true\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    let sudo = reg.get("sudo").unwrap();
    assert_eq!(tail(sudo, "--ed f"), Tail::Paths);
    assert_eq!(tail(sudo, "--edi f"), Tail::Paths);
    // Unique for another option, or ambiguous (`--pr`: `--preserve-env`, `--prompt`).
    assert_eq!(tail(sudo, "--us root ls"), Tail::Command(2));
    assert_eq!(tail(sudo, "--pr ls"), Tail::Command(1));
    // The built-in does not abbreviate.
    assert_eq!(tail(builtin("sudo"), "--ed f"), Tail::Command(1));
}

#[test]
fn path_mode_options_declare_options() {
    let s = spec(
        r#"
name = "w"
precommand = true
options = ["-f=", "-e="]
path-mode-options = ["-e", "--file=", "-f"]
"#,
    );
    // The path-mode spelling wins over a duplicate in `options`: `-e` is a flag, `-f` too.
    assert_eq!(classify(&s, "-e -x"), vec![OPT, PLAIN]);
    assert_eq!(tail(&s, "-f x"), Tail::Paths);
    // `--file` takes a value, attached or as the next word.
    assert_eq!(classify(&s, "--file -x y"), vec![OPT, PLAIN, PLAIN]);
    assert_eq!(tail(&s, "--file x"), Tail::Paths);
    assert_eq!(tail(&s, "--file=x y"), Tail::Paths);
    assert_eq!(tail(&s, "x"), Tail::Command(0));
}

#[test]
fn path_mode_options_on_a_level_that_does_not_wrap_have_no_effect() {
    let s = spec("name = \"p\"\npath-mode-options = [\"-e\"]\n[subcommands.s]\n");
    assert_eq!(classify(&s, "-e f"), vec![OPT, PLAIN]);
    assert_eq!(tail(&s, "-e f"), Tail::None);
    assert!(!s.any_wrap);
    // Only the level the walk ends at counts.
    let w = spec(
        r#"
name = "w"
path-mode-options = ["-e"]
[subcommands.run]
positional = 0
"#,
    );
    assert_eq!(tail(&w, "-e run ls"), Tail::Command(2));
    assert_eq!(tail(&w, "run -e ls"), Tail::Command(2));
}

#[test]
fn kubectl_exec_wraps_after_dashdash() {
    let kubectl = builtin("kubectl");
    assert_eq!(tail(kubectl, "exec mypod -- ls -l"), Tail::Command(3));
    // No `--`: no wrapped command.
    assert_eq!(tail(kubectl, "exec mypod ls"), Tail::None);
    assert_eq!(tail(kubectl, "exec mypod --"), Tail::None);
    assert_eq!(tail(kubectl, "exec mypod"), Tail::None);
    // No `skip-assignments`: `FOO=1` is the command.
    assert_eq!(tail(kubectl, "exec pod -- FOO=1 ls"), Tail::Command(3));
    // Other subcommands and the top level do not wrap.
    assert_eq!(tail(kubectl, "get pods -- ls"), Tail::None);
    assert_eq!(tail(kubectl, "-- exec pod -- ls"), Tail::None);
    assert_eq!(
        classify(kubectl, "exec mypod -- ls -l")[..3],
        [SUB, PLAIN, OPT]
    );
}

/// The subcommand descent of [`CommandSpec::tail`] is the one [`CommandSpec::classify_args`]
/// makes: wherever the classification marks `exec` as a subcommand, the tail finds the wrapped
/// command after the next `--`, and nowhere else.
#[test]
fn tail_descends_as_classification_does() {
    let kubectl = builtin("kubectl");
    for (line, want) in [
        ("--context x exec pod -- ls", Tail::Command(5)),
        ("exec -it -n ns mypod -- ls", Tail::Command(6)),
        ("-n ns exec -c app mypod -- ls", Tail::Command(7)),
        // `-c` consumes the first `--` as its value.
        ("exec -c -- pod -- ls", Tail::Command(5)),
        ("exec --container=-- pod -- ls", Tail::Command(4)),
        // An expansion or `-` in subcommand position fills it: no descent.
        ("$x pod -- ls", Tail::None),
        ("- exec pod -- ls", Tail::None),
        // A value word is not in subcommand position.
        ("-n exec pod -- ls", Tail::None),
        ("--context exec exec pod -- ls", Tail::Command(5)),
    ] {
        assert_eq!(tail(kubectl, line), want, "{line}");
        let words: Vec<&str> = line.split_whitespace().collect();
        let kinds = classify(kubectl, line);
        let exec = (0..words.len()).find(|&i| words[i] == "exec" && kinds[i] == SUB);
        match want {
            Tail::Command(i) => {
                assert!(exec.is_some(), "{line}: {kinds:?}");
                assert_eq!(words[i - 1], "--", "{line}");
            }
            _ => assert_eq!(exec, None, "{line}: {kinds:?}"),
        }
    }
    let abbrev = spec(
        r#"
name = "t"
abbreviations = true
complete = true
options = ["-o="]
[subcommands.execute]
wrapped-after = "--"
[subcommands.exit]
"#,
    );
    assert_eq!(tail(&abbrev, "exe pod -- ls"), Tail::Command(3));
    assert_eq!(classify(&abbrev, "exe pod -- ls")[0], SUB);
    // `ex` is ambiguous (`execute`, `exit`).
    assert_eq!(tail(&abbrev, "ex pod -- ls"), Tail::None);
    assert_eq!(tail(&abbrev, "-o execute exe -- ls"), Tail::Command(4));
}

/// As [`tail_descends_as_classification_does`], for a `positional` subcommand level: the tail
/// finds a wrapped command exactly where the classification marks `run` as a subcommand.
#[test]
fn positional_tail_descends_as_classification_does() {
    let ru = spec(
        r#"
name = "ru"
options = ["-v", "-t="]
[subcommands.run]
positional = 1
options = ["-q", "--install="]
[subcommands.show]
"#,
    );
    for (line, want) in [
        ("run stable cargo", Tail::Command(2)),
        ("-v run stable cargo", Tail::Command(3)),
        ("run -q --install y stable cargo", Tail::Command(5)),
        ("run --install=y $tc cargo", Tail::Command(3)),
        // `-t` consumes the first `run` as its value.
        ("-t run run stable cargo", Tail::Command(4)),
        ("-t run stable cargo", Tail::None),
        // An expansion, `-`, a plain word, or `--` in subcommand position: no descent.
        ("$x run stable cargo", Tail::None),
        ("- run stable cargo", Tail::None),
        ("x run stable cargo", Tail::None),
        ("-- run stable cargo", Tail::None),
        ("show run stable cargo", Tail::None),
        // Descended, but not enough words yet.
        ("run stable", Tail::None),
        ("run", Tail::None),
    ] {
        assert_eq!(tail(&ru, line), want, "{line}");
        let words: Vec<&str> = line.split_whitespace().collect();
        let kinds = classify(&ru, line);
        let run = (0..words.len()).find(|&i| words[i] == "run" && kinds[i] == SUB);
        match want {
            Tail::Command(i) => {
                let run = run.unwrap_or_else(|| panic!("{line}: {kinds:?}"));
                assert!(run < i, "{line}");
            }
            _ if line.starts_with("run") => assert_eq!(run, Some(0), "{line}"),
            _ => assert_eq!(run, None, "{line}: {kinds:?}"),
        }
    }
}

#[test]
fn wrapped_after_under_options_first() {
    let s = spec(
        r#"
name = "r"
[subcommands.run]
options-first = true
wrapped-after = "--"
options = ["-v", "-e="]
"#,
    );
    assert_eq!(tail(&s, "run -v -- ls"), Tail::Command(3));
    assert_eq!(tail(&s, "run -- ls"), Tail::Command(2));
    assert_eq!(tail(&s, "run -e -- -- ls"), Tail::Command(4));
    // After a plain word, `--` is plain: it is found only in option position.
    assert_eq!(tail(&s, "run img -- ls"), Tail::None);
    assert_eq!(tail(&s, "run $img -- ls"), Tail::None);
}

#[test]
fn positional_at_a_subcommand_level() {
    let s = spec(
        r#"
name = "ru"
precommand = true
skip-assignments = true
options = ["-v"]
[subcommands.run]
positional = 1
options = ["-q", "--install=", "-i"]
path-mode-options = ["--files"]
"#,
    );
    assert_eq!(tail(&s, "run stable cargo build"), Tail::Command(2));
    assert_eq!(tail(&s, "-v run -q stable cargo"), Tail::Command(4));
    assert_eq!(tail(&s, "run --install x stable cargo"), Tail::Command(4));
    assert_eq!(tail(&s, "run --files a b"), Tail::Paths);
    assert_eq!(tail(&s, "run"), Tail::None);
    assert_eq!(tail(&s, "run stable"), Tail::None);
    // `skip-assignments` applies at the top level only.
    assert_eq!(tail(&s, "run A=1 stable cargo"), Tail::Command(2));
    assert_eq!(tail(&s, "A=1 cmd"), Tail::Command(1));
    // The top level of a precommand wraps with `positional = 0`.
    assert_eq!(tail(&s, "-v cmd"), Tail::Command(1));
}

#[test]
fn top_level_wrappers_without_precommand() {
    let s = spec("name = \"w\"\npositional = 1\n");
    assert!(!s.is_precommand());
    assert_eq!(tail(&s, "x cmd"), Tail::Command(1));
    let s = spec(
        r#"
name = "w"
wrapped-after = "--"
skip-assignments = true
options = ["-a="]
"#,
    );
    // `skip-assignments` skips `NAME=value` words after the `--`.
    assert_eq!(tail(&s, "-a b x -- FOO=1 BAR=$x cmd"), Tail::Command(6));
    assert_eq!(tail(&s, "x -- FOO=1"), Tail::None);
    assert_eq!(tail(&s, "FOO=1 -- cmd"), Tail::Command(2));
}

/// The built-in specs, one entry per spec (aliases are not repeated), sorted by name.
fn builtin_specs() -> Vec<&'static CommandSpec> {
    let mut out: Vec<&CommandSpec> = builtins()
        .registry
        .specs
        .iter()
        .filter(|(key, s)| **key == s.name)
        .map(|(_, s)| s.as_ref())
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    assert_eq!(out.len(), BUILTIN_FILES.len());
    out
}

#[test]
fn non_wrapping_specs_have_no_tail() {
    let mut wrapping = Vec::new();
    for s in builtin_specs() {
        if s.is_precommand() {
            continue;
        }
        if s.any_wrap {
            wrapping.push(s.name.as_str());
            continue;
        }
        for line in ["run -- ls", "exec pod -- ls", "-e f", "x y z", "--", ""] {
            assert_eq!(tail(s, line), Tail::None, "{} {line}", s.name);
        }
    }
    assert_eq!(wrapping, ["kubectl"]);
    assert_eq!(builtin("kubectl").wrap, None);
}

/// The wrapped command of every built-in precommand, with hard-coded indexes: the walk that
/// finds it is the one these specs had before `positional`, `wrapped-after`, and
/// `path-mode-options`.
#[test]
fn builtin_precommand_tails_are_unchanged() {
    let golden: &[(&str, &str, usize)] = &[
        ("-", "zsh", 0),
        ("builtin", "echo", 0),
        ("chrt", "-r 10 cmd", 2),
        ("command", "-p ls", 1),
        ("doas", "-u root cmd", 2),
        ("env", "-i A=1 B=$x cmd arg", 3),
        ("exec", "-a name cmd", 2),
        ("ionice", "-c 2 -n 7 cmd", 4),
        ("nice", "-n 5 cmd", 2),
        ("nocorrect", "cmd", 0),
        ("noglob", "rm *", 0),
        ("nohup", "cmd arg", 0),
        ("stdbuf", "-oL -e 0 cmd", 3),
        ("sudo", "-u root -E FOO=1 cmd -e", 4),
        ("taskset", "-c 0-3 cmd", 2),
        ("time", "-v cmd", 1),
        ("timeout", "-s KILL 5 cmd", 3),
        ("xargs", "-0 -n1 rm -f", 2),
    ];
    let precommands: Vec<&str> = builtin_specs()
        .into_iter()
        .filter(|s| s.is_precommand())
        .map(|s| s.name.as_str())
        .collect();
    let covered: Vec<&str> = golden.iter().map(|(name, _, _)| *name).collect();
    assert_eq!(
        covered, precommands,
        "every built-in precommand has a golden line"
    );
    for &(name, line, want) in golden {
        assert_eq!(
            tail(builtin(name), line),
            Tail::Command(want),
            "{name} {line}"
        );
    }
    // `sudo` gained `options-first`: options end at the wrapped command, and nothing changes
    // before it.
    let sudo = builtin("sudo");
    for (line, want, own) in [
        ("-n -u root ls -l", 3, vec![OPT, OPT, PLAIN]),
        ("-E FOO=1 ls -e", 2, vec![OPT, PLAIN]),
        ("-u root -- ls", 3, vec![OPT, PLAIN, OPT]),
        ("ls -u", 0, vec![]),
    ] {
        assert_eq!(tail(sudo, line), Tail::Command(want), "{line}");
        let words: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(sudo.classify_args(&args(&words[..want])), own, "{line}");
    }
}

/// The wrap keys and their two built-in uses are documented in the manual page, the README,
/// and the module documentation.
#[test]
fn wrap_keys_are_documented() {
    let docs = [
        (
            "man/man5/fast-highlight.5",
            include_str!("../../man/man5/fast-highlight.5"),
        ),
        ("README.md", include_str!("../../README.md")),
        ("src/specs/mod.rs", include_str!("mod.rs")),
    ];
    for (file, text) in docs {
        // roff escapes `-` as `\-`.
        let text = text.replace("\\-", "-");
        for needle in [
            "positional",
            "wrapped-after",
            "path-mode-options",
            "Wrapped commands and path mode",
            "kubectl exec",
            "sudo -e",
        ] {
            assert!(text.contains(needle), "{file} does not mention {needle:?}");
        }
    }
}

/// Whether `s` or a level below it declares `path-mode-options`.
fn has_path_mode(s: &CommandSpec) -> bool {
    !s.options.mode.is_empty() || s.subcommands.iter().any(has_path_mode)
}

/// Every option spelling and subcommand name of `s` and the levels below it.
fn spec_words(s: &CommandSpec, out: &mut Vec<String>) {
    out.extend(s.options.exact.keys().cloned());
    out.extend(s.subcommand_names.keys().cloned());
    for sub in &s.subcommands {
        spec_words(sub, out);
    }
}

/// A built-in without `path-mode-options` never reports operands, whatever its arguments.
#[test]
fn only_path_mode_specs_report_paths() {
    let mut with_mode = Vec::new();
    for s in builtin_specs() {
        if has_path_mode(s) {
            with_mode.push(s.name.as_str());
            continue;
        }
        let mut pool = vec![
            "--".to_owned(),
            "-".to_owned(),
            "x".to_owned(),
            "FOO=1".to_owned(),
            "$x".to_owned(),
            "-e".to_owned(),
            "--edit".to_owned(),
            "-ze".to_owned(),
        ];
        spec_words(s, &mut pool);
        pool.sort_unstable();
        pool.dedup();
        // A fixed xorshift sequence, so a failure reproduces.
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for _ in 0..2000 {
            let len = next(8);
            let words: Vec<&str> = (0..len).map(|_| pool[next(pool.len())].as_str()).collect();
            assert_ne!(
                s.tail(&args(&words)),
                Tail::Paths,
                "{} {}",
                s.name,
                words.join(" ")
            );
        }
    }
    assert_eq!(with_mode, ["sudo"]);
}

#[test]
fn rejects_invalid_wrap_keys() {
    let compile = |text: &str| SpecFile::parse(text).unwrap().compile().unwrap_err();
    let both = compile("name = \"x\"\n[subcommands.s]\npositional = 1\nwrapped-after = \"--\"\n");
    assert!(
        both.contains("x s") && both.contains("positional"),
        "{both}"
    );
    let both = compile("name = \"x\"\npositional = 0\nwrapped-after = \"--\"\n");
    assert!(
        both.contains("`x`") && both.contains("wrapped-after"),
        "{both}"
    );
    for bad in ["-", "", "---", "-- "] {
        let err = compile(&format!(
            "name = \"x\"\n[subcommands.s]\nwrapped-after = {bad:?}\n"
        ));
        assert!(
            err.contains("x s") && err.contains("wrapped-after"),
            "{bad:?}: {err}"
        );
    }
    // Every form `options` accepts is accepted here, including those that take a value.
    for ok in ["-e", "--edit", "--file=", "--file=?", "-f="] {
        let text = format!(
            "name = \"x\"\n[subcommands.s]\nwrapped-after = \"--\"\npath-mode-options = [{ok:?}]\n"
        );
        assert!(SpecFile::parse(&text).unwrap().compile().is_ok(), "{ok:?}");
    }
    for bad in ["+*", "--no-*", "e", "--", "-a=b", "--file=x", ""] {
        let err = compile(&format!(
            "name = \"x\"\n[subcommands.s]\nwrapped-after = \"--\"\npath-mode-options = [{bad:?}]\n"
        ));
        assert!(
            err.contains("x s") && err.contains("path-mode-options"),
            "{bad:?}: {err}"
        );
    }
    let err = SpecFile::parse("name = \"x\"\npositional = -1\n").unwrap_err();
    assert!(err.contains("positional"), "{err}");
}

#[test]
fn merge_replaces_the_start_of_a_level() {
    let dir = TempDir::new();
    // The exec level wraps after `--` in the built-in; the user file gives it `positional`.
    dir.write(
        "kubectl.toml",
        "name = \"kubectl\"\nmerge = true\n[subcommands.exec]\npositional = 1\n",
    );
    dir.write(
        "timeout.toml",
        "name = \"timeout\"\nmerge = true\nwrapped-after = \"--\"\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    let kubectl = reg.get("kubectl").unwrap();
    assert_eq!(tail(kubectl, "exec pod ls"), Tail::Command(2));
    assert_eq!(tail(kubectl, "exec -it pod ls"), Tail::Command(3));
    // Other built-in levels and keys are kept.
    assert_eq!(
        classify(kubectl, "get pods -o wide"),
        vec![SUB, PLAIN, OPT, PLAIN]
    );
    let timeout = reg.get("timeout").unwrap();
    assert!(timeout.is_precommand());
    assert_eq!(tail(timeout, "5 ls"), Tail::None);
    assert_eq!(tail(timeout, "-s KILL 5 -- ls"), Tail::Command(4));
}

#[test]
fn merge_appends_path_mode_options() {
    let dir = TempDir::new();
    dir.write(
        "sudo.toml",
        "name = \"sudo\"\nmerge = true\npath-mode-options = [\"-y\"]\n",
    );
    // A partial file is compiled on its own first: `path-mode-options` on a level that does not
    // wrap there is not an error, and it takes effect once merged into a level that does.
    dir.write(
        "kubectl.toml",
        "name = \"kubectl\"\nmerge = true\n[subcommands.exec]\npath-mode-options = [\"-q\"]\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    let sudo = reg.get("sudo").unwrap();
    assert_eq!(tail(sudo, "-y ls"), Tail::Paths);
    assert_eq!(tail(sudo, "-e f"), Tail::Paths);
    assert_eq!(tail(sudo, "-u root ls"), Tail::Command(2));
    let kubectl = reg.get("kubectl").unwrap();
    assert_eq!(tail(kubectl, "exec -q pod -- ls"), Tail::Paths);
    assert_eq!(tail(kubectl, "exec pod -- ls"), Tail::Command(3));
}

#[test]
fn user_spec_with_every_new_key() {
    let dir = TempDir::new();
    dir.write(
        "tool.toml",
        r#"
name = "tool"
precommand = true
skip-assignments = true
positional = 1
options = ["-v"]
path-mode-options = ["-e", "--edit"]

[subcommands.exec]
wrapped-after = "--"
options = ["-c="]
path-mode-options = ["--copy="]

[subcommands.run]
positional = 2
path-mode-options = ["-f"]
"#,
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    let tool = reg.get("tool").unwrap();
    assert_eq!(tail(tool, "-v A=1 5 cmd"), Tail::Command(3));
    assert_eq!(tail(tool, "--edit f"), Tail::Paths);
    assert_eq!(tail(tool, "exec -c x box -- cmd"), Tail::Command(5));
    assert_eq!(tail(tool, "exec --copy x box"), Tail::Paths);
    assert_eq!(tail(tool, "run a b cmd"), Tail::Command(3));
    assert_eq!(tail(tool, "run -f a b"), Tail::Paths);
}

#[test]
fn positional_and_precommand_regressions() {
    assert_eq!(tail(builtin("timeout"), "5 cmd"), Tail::Command(1));
    assert_eq!(tail(builtin("taskset"), "-c 0-3 cmd"), Tail::Command(2));
    assert_eq!(tail(builtin("chrt"), "-r 10 cmd"), Tail::Command(2));
    // `chrt -e` is a flag, not a path-mode option.
    assert_eq!(tail(builtin("chrt"), "-e 10 cmd"), Tail::Command(2));
    assert_eq!(tail(builtin("env"), "-i A=1 cmd"), Tail::Command(2));
}

#[test]
fn non_precommands_report_false() {
    assert!(!builtin("git").is_precommand());
    assert!(!CommandSpec::default().is_precommand());
}

// Built-in specs.

/// File names in `specs/`, failing on anything that build.rs would silently leave out.
fn spec_dir_file_names() -> Vec<String> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("specs");
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("specs/ is readable") {
        let path = entry.expect("specs/ entry is readable").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(path.is_file(), "specs/{name} is not a regular file");
        assert!(
            path.extension().is_some_and(|ext| ext == "toml"),
            "specs/{name} does not have a .toml extension, so it would not be a built-in spec"
        );
        names.push(name);
    }
    names.sort_unstable();
    names
}

#[test]
fn spec_dir_files_are_all_builtin() {
    let on_disk = spec_dir_file_names();
    assert!(!on_disk.is_empty());
    let mut embedded: Vec<_> = BUILTIN_FILES.iter().map(|(name, _)| *name).collect();
    embedded.sort_unstable();
    assert_eq!(embedded, on_disk);
}

#[test]
fn all_builtin_specs_parse_without_warnings() {
    let b = builtins();
    assert!(b.warnings.is_empty(), "{:#?}", b.warnings);
    assert_eq!(b.files.len(), BUILTIN_FILES.len());
    assert!(SpecRegistry::builtin().get("nope").is_none());
}

#[test]
fn builtin_names_and_aliases_do_not_collide() {
    let mut owners: BTreeMap<String, &str> = BTreeMap::new();
    for (file_name, text) in BUILTIN_FILES {
        let f = SpecFile::parse(text).unwrap_or_else(|e| panic!("{file_name}: {e}"));
        for name in std::iter::once(&f.name).chain(f.aliases()) {
            if let Some(other) = owners.insert(name.clone(), file_name) {
                panic!("{name:?} is claimed by both {other} and {file_name}");
            }
        }
    }
}

const VERIFIED_PREFIX: &str = "# Verified against: ";

/// Checks the first line of a spec file for the fixed form `# Verified against: <tool> <version>.`
/// (the version token is the first of the first three words that has a digit, so `systemd 259`,
/// `GNU coreutils 9.7`, and `OpenSSH_10.2p1` all pass) or `# Verified against: nothing (<reason>).`
fn check_verified_line(text: &str) -> Result<(), String> {
    let first = text.lines().next().unwrap_or("");
    let Some(value) = first.strip_prefix(VERIFIED_PREFIX) else {
        return Err(format!(
            "the first line must start with {VERIFIED_PREFIX:?}, found {first:?}"
        ));
    };
    if !value.ends_with('.') {
        return Err(format!("the value {value:?} must end with a period"));
    }
    if value.starts_with("nothing (") {
        return Ok(());
    }
    let has_version = value
        .split_whitespace()
        .take(3)
        .any(|word| word.bytes().any(|b| b.is_ascii_digit()));
    if has_version {
        Ok(())
    } else {
        Err(format!(
            "the value {value:?} names no version and is not the form `nothing (<reason>).`"
        ))
    }
}

#[test]
fn every_spec_records_a_checked_version() {
    for (file_name, text) in BUILTIN_FILES {
        if let Err(why) = check_verified_line(text) {
            panic!("specs/{file_name}: {why}");
        }
    }
}

#[test]
fn verified_line_scanner_accepts_both_forms() {
    for ok in [
        "# Verified against: docker 27.2.1.\nname = \"x\"\n",
        "# Verified against: systemd 259.\n",
        "# Verified against: GNU coreutils 9.7.\n",
        "# Verified against: OpenSSH_10.2p1.\n",
        "# Verified against: npm 11.17.0 (only `trust` was compared).\n",
        "# Verified against: nothing (kubectl is not installed here).\n",
    ] {
        assert_eq!(check_verified_line(ok), Ok(()), "{ok:?}");
    }
}

#[test]
fn verified_line_scanner_rejects_bad_input() {
    for bad in [
        "",
        "name = \"x\"\n",
        "# docker 27.2.1.\n",
        "# Verified against:docker 27.2.1.\n",
        "# Verified against: \n",
        "# Verified against: docker latest.\n",
        "# Verified against: the newest stable docker release 27.\n",
        "# Verified against: docker 27.2.1\n",
        "# Verified against: nothing.\n",
        "# Verified against: nothing\n",
        "# docker(1).\n# Verified against: docker 27.2.1.\n",
    ] {
        assert!(check_verified_line(bad).is_err(), "{bad:?} was accepted");
    }
}

/// Every option list in a spec file, as `(where, spellings)`: `common-options`, and the `options`
/// with the `path-mode-options` of the top level and of each subcommand. A level may re-declare a
/// common option (the level's spelling wins, as in `apt` and `podman`), so the lists are checked
/// one at a time. A `path-mode-options` entry that repeats a common option is such a
/// re-declaration, the way to make a common option select path mode at one level, and is not
/// flagged. A spelling in both `options` and `path-mode-options` of one level is flagged: the
/// loader accepts it (the path-mode arity wins, so a user file can turn an option into a
/// path-mode option), but a built-in spec should declare each option once.
fn option_lists(table: &toml::Table, name: &str) -> Vec<(String, Vec<String>)> {
    fn strings(table: &toml::Table, key: &str) -> Vec<String> {
        table
            .get(key)
            .and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().expect("option is a string").to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn walk(table: &toml::Table, path: &str, out: &mut Vec<(String, Vec<String>)>) {
        // `path-mode-options` declares options of the level too, so a spelling in both lists
        // is declared twice.
        let mut options = strings(table, "options");
        options.extend(strings(table, "path-mode-options"));
        out.push((format!("`{path}` options and path-mode-options"), options));
        if let Some(subs) = table.get("subcommands").and_then(toml::Value::as_table) {
            for (sub, value) in subs {
                let value = value.as_table().expect("subcommand is a table");
                walk(value, &format!("{path} {sub}"), out);
            }
        }
    }
    let mut out = vec![(
        format!("`{name}` common-options"),
        strings(table, "common-options"),
    )];
    walk(table, name, &mut out);
    out
}

/// Why `spelling` is not a well-formed option declaration, if it is not. A declaration is a
/// name that starts with `-` and has no `=` apart from one trailing `=` or `=?`, or a pattern: a
/// non-empty prefix and a trailing `*`.
fn option_spelling_defect(spelling: &str) -> Option<&'static str> {
    if let Some(prefix) = spelling.strip_suffix('*') {
        return (prefix.is_empty() || prefix.contains(['=', '*', ' ']))
            .then_some("a pattern must be a plain prefix followed by one `*`");
    }
    let name = spelling
        .strip_suffix("=?")
        .or_else(|| spelling.strip_suffix('='))
        .unwrap_or(spelling);
    if !name.starts_with('-') {
        Some("an option must start with `-`")
    } else if name.contains('=') {
        Some("`=` may only end the spelling, as `=` or `=?`")
    } else if name.contains(char::is_whitespace) {
        Some("an option contains whitespace")
    } else if name == "--" {
        Some("`--` cannot be declared")
    } else {
        None
    }
}

#[test]
fn option_spelling_check_rejects_malformed_spellings() {
    for ok in ["-v", "--verbose", "-m=", "--color=?", "-", "+*", "--no-*"] {
        assert_eq!(option_spelling_defect(ok), None, "{ok:?}");
    }
    for bad in [
        "v", "", "=", "*", "--a=b", "--a==", "--a=?=", "--a =", "--", "--", "+nightly", "-a=b=",
    ] {
        assert!(
            option_spelling_defect(bad).is_some(),
            "{bad:?} was accepted"
        );
    }
}

/// The defects of one option list: malformed spellings and spellings declared twice. `-a`, `-a=`,
/// and `-a=?` declare the same option, so they collide.
fn option_list_defects(spellings: &[String]) -> Vec<String> {
    let mut defects = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for spelling in spellings {
        if let Some(defect) = option_spelling_defect(spelling) {
            defects.push(format!("{spelling:?}: {defect}"));
            continue;
        }
        let bare = spelling
            .strip_suffix("=?")
            .or_else(|| spelling.strip_suffix('='))
            .unwrap_or(spelling);
        if !seen.insert(bare) {
            defects.push(format!("{spelling:?} is declared more than once"));
        }
    }
    defects
}

#[test]
fn option_list_check_finds_duplicates() {
    let list = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    assert!(option_list_defects(&list(&["-a", "-b=", "--c=?", "+*"])).is_empty());
    for dup in [
        ["-a", "-a"],
        ["-a", "-a="],
        ["--c=?", "--c"],
        ["-b=", "-b=?"],
    ] {
        assert_eq!(option_list_defects(&list(&dup)).len(), 1, "{dup:?}");
    }
}

#[test]
fn builtin_option_spellings_are_well_formed() {
    let mut problems = Vec::new();
    let mut checked = 0;
    for (file_name, text) in BUILTIN_FILES {
        let table: toml::Table = text
            .parse()
            .unwrap_or_else(|e| panic!("specs/{file_name}: {e}"));
        let name = table["name"].as_str().expect("name is a string");
        for (path, spellings) in option_lists(&table, name) {
            checked += spellings.len();
            for defect in option_list_defects(&spellings) {
                problems.push(format!("specs/{file_name}: {path}: {defect}"));
            }
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    assert!(
        checked > 1000,
        "only {checked} option spellings were checked; the walk over the built-in specs is \
         probably missing option lists"
    );
}

#[test]
fn builtin_registry_is_shared() {
    let a = SpecRegistry::builtin();
    let b = a.clone();
    assert!(Arc::ptr_eq(&a.specs, &b.specs));
    assert!(Arc::ptr_eq(&a.specs, &SpecRegistry::builtin().specs));
}

#[test]
fn builtin_completeness() {
    // Plugins and aliases make these open-ended.
    for (cmd, word) in [
        ("git", "frobnicate"),
        ("cargo", "frobnicate"),
        ("docker", "frobnicate"),
        ("kubectl", "frobnicate"),
    ] {
        assert_eq!(classify(builtin(cmd), word), vec![PLAIN], "{cmd} {word}");
    }
    assert_eq!(classify(builtin("systemctl"), "frobnicate"), vec![ERR]);
    assert_eq!(classify(builtin("npm"), "frobnicate"), vec![ERR]);
    assert_eq!(
        classify(builtin("docker"), "container frobnicate"),
        vec![SUB, ERR]
    );
    assert_eq!(
        classify(builtin("git"), "remote frobnicate"),
        vec![SUB, ERR]
    );
}

#[test]
fn builtin_git() {
    let git = builtin("git");
    assert_eq!(
        classify(git, "-C dir commit -am msg --amend"),
        vec![OPT, PLAIN, SUB, OPT, PLAIN, OPT]
    );
    assert_eq!(
        classify(git, "checkout -b new -- file"),
        vec![SUB, OPT, PLAIN, OPT, PLAIN]
    );
    assert_eq!(
        classify(git, "remote add origin url"),
        vec![SUB, SUB, PLAIN, PLAIN]
    );
    assert_eq!(classify(git, "stash pop --index"), vec![SUB, SUB, OPT]);
    assert_eq!(
        classify(git, "log --oneline -5 --help"),
        vec![SUB, OPT, OPT, OPT]
    );
}

#[test]
fn builtin_cargo() {
    let cargo = builtin("cargo");
    assert_eq!(classify(cargo, "+nightly b --release"), vec![OPT, SUB, OPT]);
    assert_eq!(
        classify(cargo, "test -p foo -- --nocapture"),
        vec![SUB, OPT, PLAIN, OPT, PLAIN]
    );
    assert_eq!(
        classify(cargo, "run --release arg --verbose"),
        vec![SUB, OPT, PLAIN, PLAIN]
    );
    assert_eq!(
        classify(cargo, "clippy --all-targets -q"),
        vec![SUB, OPT, OPT]
    );
}

#[test]
fn builtin_docker() {
    let docker = builtin("docker");
    assert_eq!(
        classify(docker, "run --rm -it -v a:b img ls -l"),
        vec![SUB, OPT, OPT, OPT, PLAIN, PLAIN, PLAIN, PLAIN]
    );
    assert_eq!(classify(docker, "container ps -a"), vec![SUB, SUB, OPT]);
    assert_eq!(
        classify(docker, "compose -f x.yml up -d"),
        vec![SUB, OPT, PLAIN, SUB, OPT]
    );
}

#[test]
fn builtin_kubectl() {
    let kubectl = builtin("kubectl");
    assert_eq!(
        classify(kubectl, "-n ns get pods -o wide"),
        vec![OPT, PLAIN, SUB, PLAIN, OPT, PLAIN]
    );
    assert_eq!(
        classify(kubectl, "config use-context prod"),
        vec![SUB, SUB, PLAIN]
    );
    assert_eq!(classify(kubectl, "config bogus"), vec![SUB, ERR]);
    assert_eq!(
        classify(kubectl, "exec -it pod -- sh -c x"),
        vec![SUB, OPT, PLAIN, OPT, PLAIN, PLAIN, PLAIN]
    );
}

#[test]
fn builtin_systemctl_and_npm() {
    let systemctl = builtin("systemctl");
    assert_eq!(
        classify(systemctl, "--user restart foo --now"),
        vec![OPT, SUB, PLAIN, OPT]
    );
    let npm = builtin("npm");
    assert_eq!(classify(npm, "i -D typescript"), vec![SUB, OPT, PLAIN]);
    assert_eq!(classify(npm, "run-script build"), vec![SUB, PLAIN]);
    assert_eq!(classify(npm, "publ --no-git-checks"), vec![SUB, OPT]);
    assert_eq!(classify(npm, "t"), vec![SUB]);
}

// Loading.

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> TempDir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("fast-highlight-specs-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("specs")).unwrap();
        TempDir(dir)
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join("specs").join(name), text).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn load_without_specs_dir_gives_builtins() {
    let dir = TempDir::new();
    std::fs::remove_dir(dir.0.join("specs")).unwrap();
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(Arc::ptr_eq(&reg.specs, &SpecRegistry::builtin().specs));
}

#[test]
fn user_file_replaces_builtin() {
    let dir = TempDir::new();
    dir.write(
        "mygit.toml",
        "name = \"git\"\ncomplete = true\n[subcommands.frob]\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    let git = reg.get("git").unwrap();
    assert_eq!(classify(git, "frob"), vec![SUB]);
    assert_eq!(classify(git, "commit"), vec![ERR]);
    assert_eq!(classify(git, "-C"), vec![PLAIN]);
    // Other built-ins are untouched.
    assert_eq!(classify(reg.get("cargo").unwrap(), "build"), vec![SUB]);
}

#[test]
fn user_file_merges_into_builtin() {
    let dir = TempDir::new();
    dir.write(
        "git.toml",
        r#"
name = "git"
merge = true
complete = true
options = ["--my-global"]
[subcommands.commit]
options = ["--my-flag", "-m"]
[subcommands.frob]
[subcommands.remote]
complete = false
"#,
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    let git = reg.get("git").unwrap();
    assert_eq!(classify(git, "frob"), vec![SUB]);
    assert_eq!(classify(git, "bogus"), vec![ERR]);
    assert_eq!(
        classify(git, "--my-global -C dir status"),
        vec![OPT, OPT, PLAIN, SUB]
    );
    // Built-in options remain, the new one is added, and the later `-m` spelling (a flag, so it
    // no longer consumes the next word) wins.
    assert_eq!(
        classify(git, "commit --amend --my-flag -m --all"),
        vec![SUB, OPT, OPT, OPT, OPT]
    );
    assert_eq!(classify(git, "remote bogus"), vec![SUB, PLAIN]);
    assert_eq!(classify(git, "remote add x"), vec![SUB, SUB, PLAIN]);
}

#[test]
fn user_merge_sets_option_abbreviations() {
    let dir = TempDir::new();
    dir.write(
        "git.toml",
        "name = \"git\"\nmerge = true\noption-abbreviations = true\n",
    );
    dir.write(
        "tar.toml",
        "name = \"tar\"\nmerge = true\noption-abbreviations = false\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    // The top-level value reaches built-in subcommands that leave the key unset.
    let git = reg.get("git").unwrap();
    assert_eq!(
        classify(git, "--git-d x commit --amen"),
        vec![OPT, PLAIN, SUB, OPT]
    );
    // A user `false` turns an enabled built-in off.
    assert_eq!(
        classify(reg.get("tar").unwrap(), "--dir -x"),
        vec![PLAIN, OPT]
    );
    assert_eq!(classify(builtin("git"), "commit --amen"), vec![SUB, PLAIN]);
}

#[test]
fn option_abbreviations_must_be_a_bool() {
    let dir = TempDir::new();
    dir.write("a.toml", "name = \"a\"\noption-abbreviations = \"yes\"\n");
    dir.write(
        "b.toml",
        "name = \"b\"\n[subcommands.s]\noption-abbreviations = 1\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert_eq!(warnings.len(), 2, "{warnings:#?}");
    assert!(warnings[0].contains("a.toml"), "{warnings:#?}");
    assert!(warnings[1].contains("b.toml"), "{warnings:#?}");
    for w in &warnings {
        assert!(w.contains("option-abbreviations"), "{w}");
        assert!(w.contains("bool"), "{w}");
    }
    assert!(reg.get("a").is_none() && reg.get("b").is_none());
}

#[test]
fn merge_without_builtin_creates_spec_and_aliases_resolve() {
    let dir = TempDir::new();
    dir.write(
        "a.toml",
        "name = \"mytool\"\nmerge = true\naliases = [\"mt\", \"git\"]\n[subcommands.go]\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(classify(reg.get("mytool").unwrap(), "go"), vec![SUB]);
    assert_eq!(classify(reg.get("mt").unwrap(), "go"), vec![SUB]);
    // An alias never shadows a real command name.
    assert_eq!(classify(reg.get("git").unwrap(), "commit"), vec![SUB]);
}

#[test]
fn bad_files_warn_and_others_still_load() {
    let dir = TempDir::new();
    dir.write("1-syntax.toml", "name = \"broken\"\noptions = [\n");
    dir.write("2-unknown.toml", "name = \"unknown\"\ncompleat = true\n");
    dir.write("3-option.toml", "name = \"badopt\"\noptions = [\"oops\"]\n");
    dir.write("4-good.toml", "name = \"good\"\n[subcommands.ok]\n");
    dir.write("5-git.toml", "name = \"git\"\nbogus = 1\n");
    dir.write("notes.txt", "not a spec");
    std::fs::create_dir(dir.0.join("specs").join("sub.toml")).unwrap();
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert_eq!(warnings.len(), 4, "{warnings:#?}");
    assert!(warnings[0].contains("1-syntax.toml"));
    assert!(warnings[1].contains("2-unknown.toml") && warnings[1].contains("compleat"));
    assert!(warnings[2].contains("3-option.toml") && warnings[2].contains("oops"));
    assert!(warnings[3].contains("5-git.toml"));
    assert!(reg.get("broken").is_none() && reg.get("unknown").is_none());
    assert!(reg.get("badopt").is_none());
    assert_eq!(classify(reg.get("good").unwrap(), "ok"), vec![SUB]);
    // The bad git override is skipped; the built-in stays.
    assert_eq!(classify(reg.get("git").unwrap(), "commit"), vec![SUB]);
}

#[test]
fn later_files_override_earlier_ones() {
    let dir = TempDir::new();
    dir.write("a.toml", "name = \"t\"\n[subcommands.one]\n");
    dir.write("b.toml", "name = \"t\"\nmerge = true\n[subcommands.two]\n");
    dir.write("c.toml", "name = \"u\"\n[subcommands.one]\n");
    dir.write("d.toml", "name = \"u\"\n[subcommands.two]\n");
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(classify(reg.get("t").unwrap(), "one"), vec![SUB]);
    assert_eq!(classify(reg.get("t").unwrap(), "two"), vec![SUB]);
    assert_eq!(classify(reg.get("u").unwrap(), "one"), vec![PLAIN]);
    assert_eq!(classify(reg.get("u").unwrap(), "two"), vec![SUB]);
}

#[test]
fn user_precommand_spec() {
    let dir = TempDir::new();
    dir.write(
        "w.toml",
        "name = \"wrap\"\nprecommand = true\npositional = 2\noptions = [\"-x=\"]\n",
    );
    let (reg, warnings) = SpecRegistry::load(&dir.0);
    assert!(warnings.is_empty(), "{warnings:?}");
    let wrap = reg.get("wrap").unwrap();
    assert!(wrap.is_precommand());
    assert_eq!(wrapped(wrap, "-x 1 a b cmd"), Some(4));
}
