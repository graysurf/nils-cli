use clap::{Arg, ArgAction, Command, ValueHint};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OptionKind {
    Message,
    MessageFile,
    ExpectHead,
    ReceiptOut,
    Repo,
    Format,
    Json,
    DryRun,
    Automation,
    Type,
    Scope,
    Subject,
    BodyBullet,
    Signoff,
    Trailer,
    AutoFix,
    MaxHeaderWidth,
    Help,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OptionArity {
    Flag,
    Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OptionValueHint {
    None,
    FilePath,
    DirPath,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OptionSpec {
    pub(crate) kind: OptionKind,
    pub(crate) long: &'static str,
    pub(crate) short: Option<char>,
    pub(crate) visible_aliases: &'static [&'static str],
    pub(crate) arity: OptionArity,
    pub(crate) value_name: Option<&'static str>,
    pub(crate) value_hint: OptionValueHint,
    pub(crate) repeatable: bool,
    pub(crate) required: bool,
}

const fn flag(
    kind: OptionKind,
    long: &'static str,
    visible_aliases: &'static [&'static str],
) -> OptionSpec {
    OptionSpec {
        kind,
        long,
        short: None,
        visible_aliases,
        arity: OptionArity::Flag,
        value_name: None,
        value_hint: OptionValueHint::None,
        repeatable: false,
        required: false,
    }
}

const fn value(
    kind: OptionKind,
    long: &'static str,
    short: Option<char>,
    visible_aliases: &'static [&'static str],
    value_name: &'static str,
    value_hint: OptionValueHint,
) -> OptionSpec {
    OptionSpec {
        kind,
        long,
        short,
        visible_aliases,
        arity: OptionArity::Value,
        value_name: Some(value_name),
        value_hint,
        repeatable: false,
        required: false,
    }
}

const fn repeatable(mut option: OptionSpec) -> OptionSpec {
    option.repeatable = true;
    option
}

const fn required(mut option: OptionSpec) -> OptionSpec {
    option.required = true;
    option
}

const OPTIONS: &[OptionSpec] = &[
    value(
        OptionKind::Message,
        "message",
        Some('m'),
        &[],
        "text",
        OptionValueHint::None,
    ),
    value(
        OptionKind::MessageFile,
        "message-file",
        Some('F'),
        &[],
        "path",
        OptionValueHint::FilePath,
    ),
    required(value(
        OptionKind::ExpectHead,
        "expect-head",
        None,
        &[],
        "full-sha",
        OptionValueHint::None,
    )),
    value(
        OptionKind::ReceiptOut,
        "receipt-out",
        None,
        &[],
        "path",
        OptionValueHint::FilePath,
    ),
    value(
        OptionKind::Repo,
        "repo",
        None,
        &[],
        "path",
        OptionValueHint::DirPath,
    ),
    value(
        OptionKind::Format,
        "format",
        None,
        &[],
        "text|json",
        OptionValueHint::None,
    ),
    flag(OptionKind::Json, "json", &[]),
    flag(OptionKind::DryRun, "dry-run", &[]),
    flag(OptionKind::Automation, "automation", &["non-interactive"]),
    value(
        OptionKind::Type,
        "type",
        None,
        &[],
        "type",
        OptionValueHint::None,
    ),
    value(
        OptionKind::Scope,
        "scope",
        None,
        &[],
        "scope",
        OptionValueHint::None,
    ),
    value(
        OptionKind::Subject,
        "subject",
        None,
        &[],
        "subject",
        OptionValueHint::None,
    ),
    repeatable(value(
        OptionKind::BodyBullet,
        "body-bullet",
        None,
        &["bullet"],
        "text",
        OptionValueHint::None,
    )),
    flag(OptionKind::Signoff, "signoff", &[]),
    repeatable(value(
        OptionKind::Trailer,
        "trailer",
        None,
        &[],
        "token: value",
        OptionValueHint::None,
    )),
    flag(OptionKind::AutoFix, "auto-fix", &[]),
    value(
        OptionKind::MaxHeaderWidth,
        "max-header-width",
        None,
        &[],
        "N",
        OptionValueHint::None,
    ),
    OptionSpec {
        kind: OptionKind::Help,
        long: "help",
        short: Some('h'),
        visible_aliases: &[],
        arity: OptionArity::Flag,
        value_name: None,
        value_hint: OptionValueHint::None,
        repeatable: false,
        required: false,
    },
];

pub(crate) fn option_contract() -> &'static [OptionSpec] {
    OPTIONS
}

pub(crate) fn clap_command() -> Command {
    let mut command = Command::new("default-branch")
        .about("Create one governed signed commit on the primary checkout's default branch")
        .long_about(
            "Create exactly one governed signed commit on the primary checkout's default branch. Never contacts or updates a remote.",
        )
        .disable_help_flag(true)
        .after_long_help(
            "--receipt-out is forbidden with --dry-run. Mutation writes a final receipt only after the commit and all postconditions succeed.",
        );
    for option in option_contract() {
        command = command.arg(clap_argument(option));
    }
    command
}

fn clap_argument(option: &OptionSpec) -> Arg {
    let mut argument = Arg::new(option.long)
        .long(option.long)
        .help(option_help(option.kind))
        .required(option.required);
    if let Some(short) = option.short {
        argument = argument.short(short);
    }
    if !option.visible_aliases.is_empty() {
        argument = argument.visible_aliases(option.visible_aliases.iter().copied());
    }
    argument = match (option.kind, option.arity) {
        (OptionKind::Help, _) => argument.action(ArgAction::HelpLong),
        (_, OptionArity::Flag) => argument.action(ArgAction::SetTrue),
        (_, OptionArity::Value) if option.repeatable => argument.action(ArgAction::Append),
        (_, OptionArity::Value) => argument.action(ArgAction::Set),
    };
    if let Some(value_name) = option.value_name {
        argument = argument.value_name(value_name);
    }
    argument = match option.value_hint {
        OptionValueHint::None => argument,
        OptionValueHint::FilePath => argument.value_hint(ValueHint::FilePath),
        OptionValueHint::DirPath => argument.value_hint(ValueHint::DirPath),
    };
    if option.kind == OptionKind::Format {
        argument = argument.value_parser(["text", "json"]);
    }
    argument
}

const fn option_help(kind: OptionKind) -> &'static str {
    match kind {
        OptionKind::Message => "Use the complete commit message text",
        OptionKind::MessageFile => "Read the complete commit message from a file",
        OptionKind::ExpectHead => {
            "Require the exact current HEAD; --expect-head <full-sha> is required"
        }
        OptionKind::ReceiptOut => {
            "Write the final receipt; --receipt-out <path> is required for mutation"
        }
        OptionKind::Repo => "Run against this repository path",
        OptionKind::Format => "Select text or JSON output",
        OptionKind::Json => "Alias for --format json",
        OptionKind::DryRun => "Validate preconditions and message without creating a commit",
        OptionKind::Automation => "Disallow stdin message fallback",
        OptionKind::Type => "Set the structured message type",
        OptionKind::Scope => "Set the structured message scope",
        OptionKind::Subject => "Set the structured message subject",
        OptionKind::BodyBullet => {
            "Add a structured message body bullet; --body-bullet <text> may be repeated"
        }
        OptionKind::Signoff => "Pass --signoff to git commit",
        OptionKind::Trailer => "Add a Git trailer; --trailer <token: value> may be repeated",
        OptionKind::AutoFix => {
            "Normalize body wrapping and bullet, type, and scope case before validation"
        }
        OptionKind::MaxHeaderWidth => "Override the maximum commit header width",
        OptionKind::Help => "Print help",
    }
}

pub(crate) fn option_for_spelling(spelling: &str) -> Option<&'static OptionSpec> {
    OPTIONS.iter().find(|option| {
        spelling
            .strip_prefix("--")
            .is_some_and(|long| long == option.long || option.visible_aliases.contains(&long))
            || spelling
                .strip_prefix('-')
                .and_then(|short| {
                    let mut chars = short.chars();
                    let value = chars.next()?;
                    chars.next().is_none().then_some(value)
                })
                .is_some_and(|short| option.short == Some(short))
    })
}
