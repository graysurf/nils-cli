use crate::cmd::{CmdOptions, run_resolved};

pub struct HelpCase<'a> {
    pub bin: &'a str,
    pub args: &'a [&'a str],
    pub expected: &'a [&'a str],
}

impl<'a> HelpCase<'a> {
    pub fn root(bin: &'a str, expected: &'a [&'a str]) -> Self {
        Self {
            bin,
            args: &["--help"],
            expected,
        }
    }
}

pub fn assert_help_contains(case: HelpCase<'_>) {
    let output = run_resolved(
        case.bin,
        case.args,
        &CmdOptions::new()
            .with_env("NO_COLOR", "1")
            .with_env("CLICOLOR", "0"),
    );
    assert_eq!(
        output.code,
        0,
        "{} {:?} failed\nstdout:\n{}\nstderr:\n{}",
        case.bin,
        case.args,
        output.stdout_text(),
        output.stderr_text()
    );

    let stdout = output.stdout_text();
    for needle in case.expected {
        assert!(
            stdout.contains(needle),
            "{} {:?} help missing `{}`\nstdout:\n{}",
            case.bin,
            case.args,
            needle,
            stdout
        );
    }
}
