use crate::render::golden;
use crate::render::manifest::{self, SourceRoot};
use crate::render::support_matrix;
use crate::render::writer;
use clap::{Args, ValueEnum};
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_PRODUCT: &str = "codex";

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RenderTarget {
    /// Render product skills into `build/<product>/`.
    Product,
    /// Render the shared support matrix into `build/shared/SUPPORT_MATRIX.md`.
    SupportMatrix,
}

#[derive(Args, Debug)]
pub struct RenderArgs {
    /// Source root containing `manifests/`, `core/`, `targets/`, `build/`.
    /// Defaults to the current working directory.
    #[arg(long)]
    pub source_root: Option<PathBuf>,
    /// Render target. Defaults to the product skill tree.
    #[arg(long, value_enum, default_value_t = RenderTarget::Product)]
    pub target: RenderTarget,
    /// Product to render (`codex` or `claude`). Used when `--target product`.
    /// Defaults to `codex`.
    #[arg(long, default_value = DEFAULT_PRODUCT)]
    pub product: String,
    /// Rewrite golden outputs from the just-rendered build tree. Product
    /// renders update `tests/golden/<product>/.../expected/`; shared target
    /// renders update `tests/golden/shared/`.
    #[arg(long)]
    pub update_golden: bool,
}

pub fn run(args: RenderArgs) -> anyhow::Result<u8> {
    let root = SourceRoot::from_arg_or_cwd(args.source_root.as_deref())?;
    if args.target == RenderTarget::SupportMatrix {
        let report = support_matrix::render(&root)?;
        eprintln!(
            "agent-runtime render: target=support-matrix root={} output={} surfaces={} rows={}",
            root.path().display(),
            report.output_path.display(),
            report.surfaces,
            report.rows,
        );
        if args.update_golden {
            let copied = support_matrix::update_golden(root.path(), &report)?;
            eprintln!(
                "agent-runtime render: update-golden copied {} into tests/golden/shared/",
                copied.display(),
            );
        }
        return Ok(0);
    }

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
