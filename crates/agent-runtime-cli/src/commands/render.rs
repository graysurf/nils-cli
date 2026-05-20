use crate::render::golden;
use crate::render::manifest::{self, SourceRoot};
use crate::render::writer;
use clap::Args;
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_PRODUCT: &str = "codex";

#[derive(Args, Debug)]
pub struct RenderArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Product to render (`codex` or `claude`). Defaults to `codex`.
    #[arg(long, default_value = DEFAULT_PRODUCT)]
    pub product: String,
    /// Rewrite `tests/golden/<product>/.../expected/` from the just-rendered
    /// build tree. Scoped to the active `--product` subtree only.
    #[arg(long)]
    pub update_golden: bool,
}

pub fn run(args: RenderArgs) -> anyhow::Result<u8> {
    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    let manifests = Arc::new(manifest::load_all(&root)?);
    let report = writer::write_product(&root, manifests.clone(), &args.product)?;
    eprintln!(
        "agent-runtime render: product={} root={} rendered={} cached={} skipped={}",
        report.product,
        report.output_root.display(),
        report.rendered.len(),
        report.cached.len(),
        report.skipped.len(),
    );
    if args.update_golden {
        let copied = golden::update_golden(root.path(), &manifests, &report)?;
        eprintln!(
            "agent-runtime render: update-golden copied {} file(s) into tests/golden/{}/",
            copied.len(),
            report.product,
        );
    }
    Ok(0)
}
