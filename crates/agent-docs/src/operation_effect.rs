use std::ffi::OsString;

use clap::Parser;
use nils_common::cli_contract::{
    Envelope, OutputFormat as ContractOutputFormat, exit, schema_version_for,
};
use nils_common::execution_effect::{
    Effect, OperationEffectDescriptor, OperationEffectInput, ProviderEffect, canonical_target,
};

use crate::cli::{Cli, Command, ConfigCommand, IntegrationCommand, SessionCommand};
use crate::model::OutputFormat;

pub fn run(argv: Vec<OsString>, format: OutputFormat) -> i32 {
    let contract_format = match format {
        OutputFormat::Text => ContractOutputFormat::Text,
        OutputFormat::Json => ContractOutputFormat::Json,
    };
    let mut parsed_argv = Vec::with_capacity(argv.len() + 1);
    parsed_argv.push(OsString::from("agent-docs"));
    parsed_argv.extend(argv.iter().cloned());
    let parsed = match Cli::try_parse_from(parsed_argv) {
        Ok(parsed) => parsed,
        Err(error) => {
            return nils_common::cli_contract::emit_parse_error(
                "agent-docs",
                contract_format,
                "operation-effect-parse-error",
                error
                    .to_string()
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .unwrap_or("operation argv did not parse"),
            );
        }
    };
    let (operation, effect, provider_effect, reads) = classify(&parsed.command);
    let mut targets = Vec::new();
    if let Some(path) = parsed.docs_home {
        targets.push(canonical_target(path));
    }
    if let Some(path) = parsed.project_path {
        targets.push(canonical_target(path));
    }
    let descriptor = match OperationEffectDescriptor::for_current_process(OperationEffectInput {
        tool: "agent-docs",
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
                "agent-docs",
                contract_format,
                "operation-effect-binding-failed",
                &error,
            );
        }
    };
    emit(format, descriptor)
}

fn classify(command: &Command) -> (&'static str, Effect, ProviderEffect, Vec<&'static str>) {
    let read = Effect::ReadOnly;
    let mutation = Effect::Mutation;
    let local = ProviderEffect::LocalRead;
    match command {
        Command::Audit(_) => ("audit", read, local, vec!["catalog", "filesystem"]),
        Command::Preflight(_) => ("preflight", read, local, vec!["catalog", "filesystem"]),
        Command::Explain(_) => ("explain", read, local, vec!["catalog", "filesystem"]),
        Command::List(_) => ("list", read, local, vec!["catalog", "filesystem"]),
        Command::Integration(args) => match args.command {
            IntegrationCommand::Resolve(_) => (
                "integration.resolve",
                read,
                local,
                vec!["catalog", "user_config", "filesystem"],
            ),
        },
        Command::Session(args) => match &args.command {
            SessionCommand::Status(_) => ("session.status", read, local, vec!["session_state"]),
            SessionCommand::Verify(_) => (
                "session.verify",
                read,
                local,
                vec!["session_state", "catalog", "filesystem"],
            ),
            SessionCommand::Activate(_) => (
                "session.activate",
                mutation,
                ProviderEffect::None,
                Vec::new(),
            ),
            SessionCommand::Prepare(_) => (
                "session.prepare",
                mutation,
                ProviderEffect::None,
                Vec::new(),
            ),
        },
        Command::Config(args) => match args.command {
            ConfigCommand::Show(_) => ("config.show", read, local, vec!["user_config"]),
            ConfigCommand::List(_) => ("config.list", read, local, vec!["user_config"]),
            _ => (
                "config.mutation",
                mutation,
                ProviderEffect::None,
                Vec::new(),
            ),
        },
        Command::Init(args) if args.print || args.dry_run || !args.force => {
            ("init.preview", read, local, vec!["filesystem"])
        }
        Command::Completion(_) => ("completion", read, ProviderEffect::None, Vec::new()),
        Command::Init(_) => ("init.write", mutation, ProviderEffect::None, Vec::new()),
        Command::Remove(_) => ("remove", mutation, ProviderEffect::None, Vec::new()),
        Command::OperationEffect(_) => (
            "operation-effect",
            Effect::Unknown,
            ProviderEffect::None,
            Vec::new(),
        ),
    }
}

fn emit(format: OutputFormat, descriptor: OperationEffectDescriptor) -> i32 {
    if matches!(format, OutputFormat::Json) {
        let envelope = Envelope::success(
            schema_version_for("agent-docs", "operation-effect", 1),
            descriptor,
        );
        match serde_json::to_string(&envelope) {
            Ok(json) => println!("{json}"),
            Err(_) => return exit::SOFTWARE,
        }
    } else {
        println!(
            "agent-docs operation-effect: {} {:?}",
            descriptor.operation, descriptor.effect
        );
    }
    exit::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    use pretty_assertions::assert_eq;

    fn classify_argv(argv: &[&str]) -> (&'static str, Effect, ProviderEffect, Vec<&'static str>) {
        let mut full = vec![OsString::from("agent-docs")];
        full.extend(argv.iter().map(OsString::from));
        let parsed = Cli::try_parse_from(full)
            .unwrap_or_else(|error| panic!("argv {argv:?} must parse: {error}"));
        classify(&parsed.command)
    }

    #[test]
    fn read_only_commands_declare_their_managed_state_reads() {
        for (argv, operation, reads) in [
            (vec!["audit"], "audit", vec!["catalog", "filesystem"]),
            (
                vec!["preflight", "--intent", "project-dev"],
                "preflight",
                vec!["catalog", "filesystem"],
            ),
            (
                vec!["explain", "--intent", "project-dev"],
                "explain",
                vec!["catalog", "filesystem"],
            ),
            (
                vec!["integration", "resolve", "--product", "claude"],
                "integration.resolve",
                vec!["catalog", "user_config", "filesystem"],
            ),
            (vec!["list"], "list", vec!["catalog", "filesystem"]),
        ] {
            let (name, effect, provider, declared) = classify_argv(&argv);
            assert_eq!(name, operation, "{argv:?}");
            assert_eq!(effect, Effect::ReadOnly, "{argv:?}");
            assert_eq!(provider, ProviderEffect::LocalRead, "{argv:?}");
            assert_eq!(declared, reads, "{argv:?}");
        }
    }

    #[test]
    fn session_subcommands_split_read_from_mutation() {
        let (name, effect, provider, reads) = classify_argv(&[
            "session",
            "status",
            "--session-id",
            "s1",
            "--product",
            "claude",
            "--state-home",
            "/tmp/state",
        ]);
        assert_eq!(name, "session.status");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(provider, ProviderEffect::LocalRead);
        assert_eq!(reads, vec!["session_state"]);

        let (name, effect, _, reads) = classify_argv(&[
            "session",
            "verify",
            "--session-id",
            "s1",
            "--product",
            "claude",
            "--state-home",
            "/tmp/state",
            "--require-intent",
            "project-dev",
        ]);
        assert_eq!(name, "session.verify");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(reads, vec!["session_state", "catalog", "filesystem"]);

        // A mutation declares no managed-state reads and no provider effect.
        for (subcommand, expected) in [
            ("activate", "session.activate"),
            ("prepare", "session.prepare"),
        ] {
            let (name, effect, provider, reads) = classify_argv(&[
                "session",
                subcommand,
                "--session-id",
                "s1",
                "--product",
                "claude",
                "--state-home",
                "/tmp/state",
                "--intent",
                "project-dev",
            ]);
            assert_eq!(name, expected);
            assert_eq!(effect, Effect::Mutation);
            assert_eq!(provider, ProviderEffect::None);
            assert!(reads.is_empty());
        }
    }

    #[test]
    fn config_reads_are_distinguished_from_config_mutations() {
        let (name, effect, _, reads) = classify_argv(&["config", "show"]);
        assert_eq!(name, "config.show");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(reads, vec!["user_config"]);

        let (name, effect, _, reads) = classify_argv(&["config", "list"]);
        assert_eq!(name, "config.list");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(reads, vec!["user_config"]);

        // Anything that is not an explicit read is classified as a mutation,
        // so a newly added config subcommand fails closed.
        for argv in [
            vec!["config", "enroll", "--catalog", "/tmp/catalog.toml"],
            vec!["config", "exclude"],
            vec!["config", "remove"],
        ] {
            let (name, effect, provider, reads) = classify_argv(&argv);
            assert_eq!(name, "config.mutation", "{argv:?}");
            assert_eq!(effect, Effect::Mutation, "{argv:?}");
            assert_eq!(provider, ProviderEffect::None, "{argv:?}");
            assert!(reads.is_empty(), "{argv:?}");
        }
    }

    #[test]
    fn init_is_a_preview_until_it_is_forced() {
        for argv in [
            vec!["init", "--print"],
            vec!["init", "--dry-run"],
            vec!["init"],
        ] {
            let (name, effect, provider, reads) = classify_argv(&argv);
            assert_eq!(name, "init.preview", "{argv:?}");
            assert_eq!(effect, Effect::ReadOnly, "{argv:?}");
            assert_eq!(provider, ProviderEffect::LocalRead, "{argv:?}");
            assert_eq!(reads, vec!["filesystem"], "{argv:?}");
        }

        let (name, effect, provider, reads) = classify_argv(&["init", "--force"]);
        assert_eq!(name, "init.write");
        assert_eq!(effect, Effect::Mutation);
        assert_eq!(provider, ProviderEffect::None);
        assert!(reads.is_empty());
    }

    #[test]
    fn remove_completion_and_self_description_are_classified() {
        let (name, effect, provider, _) = classify_argv(&[
            "remove",
            "--context",
            "project-dev",
            "--scope",
            "project",
            "--path",
            "DEVELOPMENT.md",
        ]);
        assert_eq!(name, "remove");
        assert_eq!(effect, Effect::Mutation);
        assert_eq!(provider, ProviderEffect::None);

        let (name, effect, provider, reads) = classify_argv(&["completion", "zsh"]);
        assert_eq!(name, "completion");
        assert_eq!(effect, Effect::ReadOnly);
        assert_eq!(provider, ProviderEffect::None);
        assert!(reads.is_empty());

        // Describing the describer cannot claim to know its own effect.
        let (name, effect, _, _) = classify_argv(&["operation-effect", "--", "list"]);
        assert_eq!(name, "operation-effect");
        assert_eq!(effect, Effect::Unknown);
    }

    #[test]
    fn a_descriptor_is_emitted_for_both_formats() {
        let argv = vec![OsString::from("list")];
        assert_eq!(run(argv.clone(), OutputFormat::Json), exit::SUCCESS);
        assert_eq!(run(argv, OutputFormat::Text), exit::SUCCESS);
    }

    #[test]
    fn unparseable_argv_fails_with_the_parse_error_contract() {
        let argv = vec![OsString::from("definitely-not-a-subcommand")];

        assert_eq!(run(argv.clone(), OutputFormat::Json), exit::USAGE);
        assert_eq!(run(argv, OutputFormat::Text), exit::USAGE);
    }

    #[test]
    fn explicit_roots_become_canonical_descriptor_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let argv = vec![
            OsString::from("--docs-home"),
            OsString::from(tmp.path()),
            OsString::from("--project-path"),
            OsString::from(tmp.path()),
            OsString::from("list"),
        ];

        assert_eq!(run(argv, OutputFormat::Json), exit::SUCCESS);
    }
}
