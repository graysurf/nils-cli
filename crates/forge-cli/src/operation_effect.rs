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
