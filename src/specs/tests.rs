use super::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const SUB: Option<TokenKind> = Some(TokenKind::Subcommand);
const OPT: Option<TokenKind> = Some(TokenKind::CmdOption);
const ERR: Option<TokenKind> = Some(TokenKind::Error);
const PLAIN: Option<TokenKind> = None;

/// Test words: a word starting with `$` stands for a word with an expansion (no literal).
fn args<'a>(words: &[&'a str]) -> Vec<ArgInput<'a>> {
    words
        .iter()
        .map(|w| ArgInput {
            literal: if w.starts_with('$') { None } else { Some(w) },
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

#[test]
fn non_precommands_report_false() {
    assert!(!builtin("git").is_precommand());
    assert!(!CommandSpec::default().is_precommand());
}

// Built-in specs.

#[test]
fn all_builtin_specs_parse_without_warnings() {
    let b = builtins();
    assert!(b.warnings.is_empty(), "{:#?}", b.warnings);
    assert_eq!(b.files.len(), BUILTIN_FILES.len());
    let mut names: Vec<_> = b.files.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(
        names.len(),
        BUILTIN_FILES.len(),
        "duplicate built-in spec names"
    );
    for name in [
        "git",
        "cargo",
        "docker",
        "kubectl",
        "systemctl",
        "npm",
        "sudo",
        "doas",
        "env",
        "nice",
        "nohup",
        "time",
        "timeout",
        "stdbuf",
        "ionice",
        "chrt",
        "taskset",
        "noglob",
        "nocorrect",
        "exec",
        "command",
        "builtin",
        "-",
    ] {
        assert!(
            SpecRegistry::builtin().get(name).is_some(),
            "missing built-in {name}"
        );
    }
    assert!(SpecRegistry::builtin().get("xargs").is_none());
    assert!(SpecRegistry::builtin().get("nope").is_none());
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
