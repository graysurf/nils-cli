use std::ffi::OsString;

use nils_common::cli_contract::{Envelope, OutputFormat, exit, schema_version_for};
use nils_common::execution_effect::{
    Effect, OperationEffectDescriptor, OperationEffectInput, ProviderEffect,
};

use crate::cli::{
    ActivityArgs, AuthArgs, AuthCommand, Cli, Command, InboxArgs, InboxCommand, IssueArgs,
    IssueCommand, LabelArgs, LabelCommand, PrArgs, PrCommand, PrPendingReviewCommand,
    PrReviewCommand, RepoArgs, RepoCommand, ReviewThreadsCommand, SearchArgs,
};

pub fn run(argv: Vec<OsString>, format: OutputFormat) -> i32 {
    let parsed = match super::cli::parse_or_exit(argv.clone()) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    let (operation, effect, provider_effect, reads) = classify(&parsed);
    let targets = parsed.repo.iter().cloned().collect::<Vec<_>>();
    let descriptor = match OperationEffectDescriptor::for_current_process(OperationEffectInput {
        tool: "forge-cli",
        release: env!("CARGO_PKG_VERSION"),
        operation,
        effect,
        provider_effect,
        managed_state_reads: reads.into_iter().map(str::to_string).collect(),
        argv: &argv,
        targets: &targets,
    }) {
        Ok(descriptor) => descriptor,
        Err(error) => {
            return nils_common::cli_contract::emit_parse_error(
                "forge-cli",
                format,
                "operation-effect-binding-failed",
                &error,
            );
        }
    };
    emit(format, descriptor)
}

fn classify(cli: &Cli) -> (&'static str, Effect, ProviderEffect, Vec<&'static str>) {
    let network = ProviderEffect::NetworkRead;
    let read = Effect::ReadOnly;
    let mutation = Effect::Mutation;
    match &cli.command {
        Some(Command::Auth(AuthArgs {
            command: Some(AuthCommand::Status),
        })) => ("auth.status", read, network, vec!["provider_auth"]),
        Some(Command::Repo(RepoArgs {
            command: Some(RepoCommand::View),
        })) => ("repo.view", read, network, vec!["git_remote", "provider"]),
        Some(Command::Repo(RepoArgs {
            command: Some(RepoCommand::Bootstrap(_)),
        })) => (
            "repo.bootstrap",
            mutation,
            ProviderEffect::NetworkWrite,
            vec!["provider", "user_config", "bootstrap_receipt"],
        ),
        Some(Command::Pr(PrArgs {
            command: Some(command),
        })) => match command {
            PrCommand::View { .. } => ("pr.view", read, network, vec!["provider"]),
            PrCommand::List(_) => ("pr.list", read, network, vec!["provider"]),
            PrCommand::Comments(_) => ("pr.comments", read, network, vec!["provider"]),
            PrCommand::Reviews(_) => ("pr.reviews", read, network, vec!["provider"]),
            PrCommand::Tasks(_) => ("pr.tasks", read, network, vec!["provider"]),
            PrCommand::Checks(_) => ("pr.checks", read, network, vec!["provider"]),
            PrCommand::WaitChecks(_) => ("pr.wait-checks", read, network, vec!["provider"]),
            PrCommand::ReviewThreads(args)
                if matches!(&args.command, ReviewThreadsCommand::List { .. }) =>
            {
                ("pr.review-threads.list", read, network, vec!["provider"])
            }
            PrCommand::Review(args) => match &args.command {
                Some(PrReviewCommand::Validate(validate)) => (
                    "pr.review.validate",
                    read,
                    if validate.check_diff {
                        network
                    } else {
                        ProviderEffect::LocalRead
                    },
                    if validate.check_diff {
                        vec!["local_inputs", "provider"]
                    } else {
                        vec!["local_inputs"]
                    },
                ),
                None => (
                    "pr.review.post",
                    mutation,
                    ProviderEffect::NetworkWrite,
                    Vec::new(),
                ),
            },
            PrCommand::PendingReview(args)
                if matches!(&args.command, PrPendingReviewCommand::Inspect(_)) =>
            {
                ("pr.pending-review.inspect", read, network, vec!["provider"])
            }
            _ => (
                "pr.mutation",
                mutation,
                ProviderEffect::NetworkWrite,
                Vec::new(),
            ),
        },
        Some(Command::Issue(IssueArgs {
            command: Some(command),
        })) => match command {
            IssueCommand::View(_) => ("issue.view", read, network, vec!["provider"]),
            IssueCommand::List(_) => ("issue.list", read, network, vec!["provider"]),
            _ => (
                "issue.mutation",
                mutation,
                ProviderEffect::NetworkWrite,
                Vec::new(),
            ),
        },
        Some(Command::Activity(ActivityArgs { command: Some(_) })) => {
            ("activity.query", read, network, vec!["provider"])
        }
        Some(Command::Label(LabelArgs {
            command: Some(command),
        })) => match command {
            LabelCommand::List(_) => ("label.list", read, network, vec!["provider"]),
            LabelCommand::Audit(_) => ("label.audit", read, network, vec!["provider"]),
            LabelCommand::Ensure(_) => (
                "label.ensure",
                mutation,
                ProviderEffect::NetworkWrite,
                Vec::new(),
            ),
        },
        Some(Command::Inbox(InboxArgs {
            command: Some(command),
        })) if inbox_no_cache(command) => (
            "inbox.query",
            read,
            network,
            vec!["provider", "user_config"],
        ),
        Some(Command::Inbox(InboxArgs { command: Some(_) })) => (
            "inbox.cache-write",
            mutation,
            network,
            vec!["provider", "user_config", "cache"],
        ),
        Some(Command::Search(SearchArgs { command: Some(_) })) => {
            ("search.query", read, network, vec!["provider"])
        }
        Some(Command::Completion(_)) => ("completion", read, ProviderEffect::None, Vec::new()),
        Some(Command::OperationEffect(_)) | None => (
            "operation-effect",
            Effect::Unknown,
            ProviderEffect::None,
            Vec::new(),
        ),
        _ => (
            "mutation",
            mutation,
            ProviderEffect::NetworkWrite,
            Vec::new(),
        ),
    }
}

fn inbox_no_cache(command: &InboxCommand) -> bool {
    match command {
        InboxCommand::Status(args) | InboxCommand::List(args) => args.no_cache,
        InboxCommand::Next(args) => args.no_cache,
    }
}

fn emit(format: OutputFormat, descriptor: OperationEffectDescriptor) -> i32 {
    if format.is_json() {
        let envelope = Envelope::success(
            schema_version_for("forge-cli", "operation-effect", 1),
            descriptor,
        );
        match serde_json::to_string(&envelope) {
            Ok(json) => println!("{json}"),
            Err(_) => return exit::SOFTWARE,
        }
    } else {
        println!(
            "forge-cli operation-effect: {} {:?}",
            descriptor.operation, descriptor.effect
        );
    }
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    use clap::Parser;
    use pretty_assertions::assert_eq;

    fn classify_argv(argv: &[&str]) -> (&'static str, Effect, ProviderEffect, Vec<&'static str>) {
        let mut full = vec!["forge-cli"];
        full.extend_from_slice(argv);
        let cli = Cli::try_parse_from(full)
            .unwrap_or_else(|error| panic!("argv {argv:?} must parse: {error}"));
        classify(&cli)
    }

    /// Every read must declare a read-only effect; a network read must never
    /// be classified as a write.
    fn assert_network_read(argv: &[&str], operation: &str, reads: &[&str]) {
        let (name, effect, provider, declared) = classify_argv(argv);
        assert_eq!(name, operation, "{argv:?}");
        assert_eq!(effect, Effect::ReadOnly, "{argv:?}");
        assert_eq!(provider, ProviderEffect::NetworkRead, "{argv:?}");
        assert_eq!(declared, reads.to_vec(), "{argv:?}");
    }

    #[test]
    fn provider_reads_are_classified_as_read_only_network_calls() {
        assert_network_read(&["auth", "status"], "auth.status", &["provider_auth"]);
        assert_network_read(&["repo", "view"], "repo.view", &["git_remote", "provider"]);
        assert_network_read(&["pr", "view", "7"], "pr.view", &["provider"]);
        assert_network_read(&["pr", "list"], "pr.list", &["provider"]);
        assert_network_read(&["pr", "comments", "7"], "pr.comments", &["provider"]);
        assert_network_read(&["pr", "reviews", "7"], "pr.reviews", &["provider"]);
        assert_network_read(&["pr", "tasks", "7"], "pr.tasks", &["provider"]);
        assert_network_read(&["pr", "checks", "7"], "pr.checks", &["provider"]);
        assert_network_read(&["pr", "wait-checks", "7"], "pr.wait-checks", &["provider"]);
        assert_network_read(&["issue", "view", "7"], "issue.view", &["provider"]);
        assert_network_read(&["issue", "list"], "issue.list", &["provider"]);
        assert_network_read(&["label", "list"], "label.list", &["provider"]);
        assert_network_read(
            &["label", "audit", "--catalog", "/tmp/labels.yaml"],
            "label.audit",
            &["provider"],
        );
    }

    #[test]
    fn provider_writes_are_classified_as_network_mutations() {
        for (argv, operation) in [
            (
                vec!["label", "ensure", "--catalog", "/tmp/labels.yaml"],
                "label.ensure",
            ),
            (vec!["pr", "merge", "7"], "pr.mutation"),
            (vec!["issue", "close", "7"], "issue.mutation"),
        ] {
            let (name, effect, provider, reads) = classify_argv(&argv);
            assert_eq!(name, operation, "{argv:?}");
            assert_eq!(effect, Effect::Mutation, "{argv:?}");
            assert_eq!(provider, ProviderEffect::NetworkWrite, "{argv:?}");
            assert!(reads.is_empty(), "{argv:?}");
        }
    }

    #[test]
    fn repo_bootstrap_declares_the_receipt_it_writes() {
        let (name, effect, provider, reads) = classify_argv(&[
            "repo",
            "bootstrap",
            "--owner-kind",
            "user",
            "--default-branch",
            "main",
            "--file",
            "README.md",
            "--message",
            "init",
            "--reason-file",
            "/tmp/reason.md",
        ]);

        assert_eq!(name, "repo.bootstrap");
        assert_eq!(effect, Effect::Mutation);
        assert_eq!(provider, ProviderEffect::NetworkWrite);
        assert_eq!(reads, vec!["provider", "user_config", "bootstrap_receipt"]);
    }

    #[test]
    fn review_validation_only_reaches_the_provider_when_it_checks_the_diff() {
        let (name, effect, provider, reads) = classify_argv(&["pr", "review", "validate"]);
        assert_eq!(name, "pr.review.validate");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(provider, ProviderEffect::LocalRead);
        assert_eq!(reads, vec!["local_inputs"]);

        let (name, effect, provider, reads) =
            classify_argv(&["pr", "review", "validate", "--check-diff"]);
        assert_eq!(name, "pr.review.validate");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(provider, ProviderEffect::NetworkRead);
        assert_eq!(reads, vec!["local_inputs", "provider"]);
    }

    #[test]
    fn posting_a_review_is_a_provider_write() {
        let (name, effect, provider, reads) = classify_argv(&["pr", "review"]);

        assert_eq!(name, "pr.review.post");
        assert_eq!(effect, Effect::Mutation);
        assert_eq!(provider, ProviderEffect::NetworkWrite);
        assert!(reads.is_empty());
    }

    #[test]
    fn inbox_is_a_mutation_whenever_it_may_write_its_cache() {
        // `--no-cache` keeps the command read-only.
        let (name, effect, _, reads) = classify_argv(&["inbox", "list", "--no-cache"]);
        assert_eq!(name, "inbox.query");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(reads, vec!["provider", "user_config"]);

        // Without it the cache may be rewritten, so it must not claim to be a read.
        let (name, effect, provider, reads) = classify_argv(&["inbox", "list"]);
        assert_eq!(name, "inbox.cache-write");
        assert_eq!(effect, Effect::Mutation);
        assert_eq!(provider, ProviderEffect::NetworkRead);
        assert_eq!(reads, vec!["provider", "user_config", "cache"]);

        for subcommand in ["status", "next"] {
            let (name, _, _, _) = classify_argv(&["inbox", subcommand, "--no-cache"]);
            assert_eq!(name, "inbox.query", "{subcommand}");
            let (name, _, _, _) = classify_argv(&["inbox", subcommand]);
            assert_eq!(name, "inbox.cache-write", "{subcommand}");
        }
    }

    #[test]
    fn local_only_and_self_describing_commands_declare_no_provider_effect() {
        let (name, effect, provider, reads) = classify_argv(&["completion", "zsh"]);
        assert_eq!(name, "completion");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(provider, ProviderEffect::None);
        assert!(reads.is_empty());

        // Describing the describer cannot claim to know its own effect.
        let (name, effect, provider, _) = classify_argv(&["operation-effect", "--", "pr", "list"]);
        assert_eq!(name, "operation-effect");
        assert_eq!(effect, Effect::Unknown);
        assert_eq!(provider, ProviderEffect::None);

        // A bare invocation with no subcommand is equally unknown.
        let (name, effect, _, _) = classify_argv(&[]);
        assert_eq!(name, "operation-effect");
        assert_eq!(effect, Effect::Unknown);
    }

    #[test]
    fn a_descriptor_is_emitted_for_both_formats() {
        assert_eq!(
            run(
                vec![OsString::from("pr"), OsString::from("list")],
                OutputFormat::Json
            ),
            exit::SUCCESS
        );
        assert_eq!(
            run(
                vec![OsString::from("pr"), OsString::from("list")],
                OutputFormat::Text
            ),
            exit::SUCCESS
        );
    }
}
