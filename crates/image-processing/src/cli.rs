use clap::{ArgAction, Parser, ValueEnum, ValueHint};

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum Operation {
    Convert,
    SvgValidate,
}

impl Operation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Operation::Convert => "convert",
            Operation::SvgValidate => "svg-validate",
        }
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "image-processing",
    version,
    long_version = nils_build_info::long_version(env!("CARGO_PKG_VERSION")),
    about = "Convert SVG or raster inputs to png/webp/jpg outputs and validate SVG inputs.",
    long_about = "Convert SVG, PNG, JPEG, or WebP inputs into raster outputs, or validate SVG inputs with deterministic JSON-friendly reports.",
    after_help = "EXAMPLES:\n  image-processing convert --in icon.svg --to png --out icon.png\n  image-processing convert --in photo.jpg --to webp --out photo.webp --width 1200\n  image-processing svg-validate --in icon.svg --out report.json --json\n\nENVIRONMENT:\n  HOME  Fallback base path for user-relative paths.\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data\n\nNOTES:\n  - convert requires exactly one --in + --to png|webp|jpg + --out.\n  - convert accepts svg|png|jpg|jpeg|webp inputs.\n  - convert supports optional --width/--height for raster output sizing.\n  - svg-validate requires exactly one --in + --out.\n  - Use --json for machine-readable output (stdout JSON only; logs go to stderr).\n"
)]
pub struct Cli {
    #[arg(value_enum)]
    pub subcommand: Operation,

    #[arg(
        long = "in",
        action = ArgAction::Append,
        default_value = None,
        value_hint = ValueHint::FilePath,
        help = "Input file path"
    )]
    pub inputs: Vec<String>,

    #[arg(long, help = "Output file path", value_hint = ValueHint::FilePath)]
    pub out: Option<String>,

    #[arg(long, help = "Overwrite existing output file")]
    pub overwrite: bool,
    #[arg(long = "dry-run", help = "Validate and plan without writing output")]
    pub dry_run: bool,
    #[arg(
        long,
        hide = true,
        help = "Hidden alias for `--format json` (kept for backwards compatibility)"
    )]
    pub json: bool,
    #[arg(long, help = "Print per-item processing report")]
    pub report: bool,

    #[arg(long = "to", help = "Output format: png, webp, or jpg")]
    pub to: Option<String>,

    #[arg(long, help = "Raster output width in pixels")]
    pub width: Option<i32>,
    #[arg(long, help = "Raster output height in pixels")]
    pub height: Option<i32>,
}
