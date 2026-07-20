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
