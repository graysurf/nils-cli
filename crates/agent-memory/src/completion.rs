use clap::{CommandFactory, ValueEnum};
use clap_complete::CompleteEnv;
use clap_complete::engine::CompletionCandidate;
use clap_complete::env::{Bash, EnvCompleter, Zsh};
use std::io;

use crate::cli::Cli;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
}

pub fn run(shell: CompletionShell) -> i32 {
    match shell {
        CompletionShell::Bash => emit_registration(&Bash),
        CompletionShell::Zsh => emit_registration(&Zsh),
    }
}

/// Emit a `clap_complete` `CompleteEnv` dynamic-completion registration stub for
/// the given shell.
///
/// agent-memory is a `completion_engine=dynamic` CLI (see the completion
/// coverage matrix): SCOPE candidates (`global`, `root`, and each `agents/<id>`
/// / `personas/<id>`) are enumerated from the memory store at TAB time by the
/// binary itself, so the exported script is a thin registration that calls back
/// into `agent-memory` rather than a static `generate()` script. This remains a
/// single completion path per the completion development standard.
fn emit_registration<C: EnvCompleter>(completer: &C) -> i32 {
    match completer.write_registration(
        "COMPLETE",
        "agent-memory",
        "agent-memory",
        "agent-memory",
        &mut io::stdout(),
    ) {
        Ok(()) => 0,
        Err(err) => {
            eprintln!("agent-memory: error: failed to emit completion registration: {err}");
            1
        }
    }
}

/// Intercept `COMPLETE=<shell> agent-memory ...` completion requests before the
/// normal parse.
///
/// On a completion request `CompleteEnv::complete()` prints the registration
/// stub (or the runtime candidates) and exits the process itself; when
/// `COMPLETE` is unset it returns and the normal application path proceeds
/// unchanged, so this is a no-op for ordinary invocations.
pub(crate) fn complete_env() {
    CompleteEnv::with_factory(Cli::command).complete();
}

/// Live completion candidates for a `SCOPE` positional: the resolvable memory
/// scopes sourced at TAB time from the real store layout (`root`, `global`, and
/// each `agents/<id>` / `personas/<id>`). Fails soft to an empty vec on any
/// error so a broken or absent store never panics the completer.
pub(crate) fn scope_candidates() -> Vec<CompletionCandidate> {
    crate::scope_completion_values()
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}
