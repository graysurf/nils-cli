use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

use crate::model::{AxClickFallbackStage, AxMatchStrategy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    Text,
    Json,
    Tsv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ErrorFormat {
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ImageFormat {
    Png,
    #[value(alias = "jpeg")]
    Jpg,
    Webp,
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = "macos-agent",
    version,
    about = "Automate macOS desktop actions for agent workflows.",
    long_about = "Automate macOS desktop observation, window management, input, AX inspection, waits, and profiles with deterministic command output.",
    after_help = "EXAMPLES:\n  macos-agent preflight\n  macos-agent windows list --format json\n  macos-agent window activate --app Finder\n  macos-agent input key --key Enter\n\nENVIRONMENT:\n  AGENT_HOME, HOME\n  AGENTS_MACOS_AGENT_TEST_MODE, AGENTS_MACOS_AGENT_TEST_TIMESTAMP\n  AGENTS_MACOS_AGENT_AX_BACKEND, AGENTS_MACOS_AGENT_AX_WATCH_POLL_JSON\n\nEXIT CODES:\n  0   success\n  1   runtime error\n  64  command-line usage error\n  65  invalid input data\n\nDECISION GUIDE (AX-first):\n\
1) Prefer `ax` commands for element-targeted interaction.\n\
2) If AX is flaky, enable fallback flags (`--allow-coordinate-fallback`, `--allow-keyboard-fallback`).\n\
3) If AX is unavailable, use `window activate` + `input` commands.\n\
4) Add `wait` commands around mutating steps for stability.",
    disable_help_subcommand = true
)]
pub struct Cli {
    /// Output format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text, global = true)]
    pub format: OutputFormat,

    /// Error output format.
    #[arg(long, value_enum, default_value_t = ErrorFormat::Text, global = true)]
    pub error_format: ErrorFormat,

    /// Print planned actions without mutating desktop state.
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Retry count for mutating actions.
    #[arg(long, default_value_t = 0, global = true)]
    pub retries: u8,

    /// Delay between retries in milliseconds.
    #[arg(long, default_value_t = 150, global = true)]
    pub retry_delay_ms: u64,

    /// Per-action timeout in milliseconds.
    #[arg(long, default_value_t = 4000, global = true)]
    pub timeout_ms: u64,

    /// Emit per-command trace artifacts under AGENT_HOME/out.
    #[arg(long, global = true)]
    pub trace: bool,

    /// Override trace output directory (requires --trace).
    #[arg(long, global = true)]
    pub trace_dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: CommandGroup,
}

#[derive(Debug, Clone, Subcommand)]
#[allow(clippy::large_enum_variant)]
pub enum CommandGroup {
    /// Check runtime dependencies and permissions.
    Preflight(PreflightArgs),

    /// List windows.
    Windows {
        #[command(subcommand)]
        command: WindowsCommand,
    },

    /// List running apps.
    Apps {
        #[command(subcommand)]
        command: AppsCommand,
    },

    /// Activate a target window/app.
    Window {
        #[command(subcommand)]
        command: WindowCommand,
    },

    /// Execute pointer and keyboard input commands.
    Input {
        #[command(subcommand)]
        command: InputCommand,
    },

    /// Query and switch macOS keyboard input sources.
    InputSource {
        #[command(subcommand)]
        command: InputSourceCommand,
    },

    /// Query and interact with Accessibility (AX) nodes.
    Ax {
        #[command(subcommand)]
        command: AxCommand,
    },

    /// Capture screenshots for observation.
    Observe {
        #[command(subcommand)]
        command: ObserveCommand,
    },

    /// Collect one-shot debug artifacts for triage.
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },

    /// Wait primitives for UI stabilization.
    Wait {
        #[command(subcommand)]
        command: WaitCommand,
    },

    /// Run declarative multi-step command chains.
    Scenario {
        #[command(subcommand)]
        command: ScenarioCommand,
    },

    /// Validate and bootstrap coordinate profiles.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },

    /// Print shell completion script.
    Completion(CompletionArgs),
}

#[derive(Debug, Clone, Args)]
pub struct CompletionArgs {
    /// Shell to generate completion for.
    #[arg(value_enum)]
    pub shell: crate::completion::CompletionShell,
}

#[derive(Debug, Clone, Args)]
pub struct PreflightArgs {
    /// Treat advisory warnings as readiness failures.
    #[arg(long)]
    pub strict: bool,

    /// Run actionable probes (activate/input/screenshot) in addition to static checks.
    #[arg(long)]
    pub include_probes: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub enum WindowsCommand {
    /// List windows.
    List(ListWindowsArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ListWindowsArgs {
    /// Filter by app/owner name.
    #[arg(long)]
    pub app: Option<String>,

    /// Narrow app selection by window title substring.
    #[arg(long = "window-title-contains", requires = "app")]
    pub window_name: Option<String>,

    /// Include only on-screen windows.
    #[arg(long)]
    pub on_screen_only: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AppsCommand {
    /// List running apps.
    List(ListAppsArgs),
}

#[derive(Debug, Clone, Args, Default)]
pub struct ListAppsArgs {}

#[derive(Debug, Clone, Subcommand)]
pub enum WindowCommand {
    /// Activate a target window/app.
    Activate(WindowActivateArgs),
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(false)
            .args(["window_id", "active_window", "app", "bundle_id"])
    )
)]
pub struct WindowActivateArgs {
    /// Select by window id.
    #[arg(long)]
    pub window_id: Option<u32>,

    /// Select frontmost active window.
    #[arg(long)]
    pub active_window: bool,

    /// Select by app name.
    #[arg(long)]
    pub app: Option<String>,

    /// Narrow app selection by window title substring.
    #[arg(long = "window-title-contains", requires = "app")]
    pub window_name: Option<String>,

    /// Select by bundle id.
    #[arg(long)]
    pub bundle_id: Option<String>,

    /// Wait up to this many milliseconds for active confirmation.
    #[arg(long)]
    pub wait_ms: Option<u64>,

    /// If activation/confirmation fails, quit and relaunch the target app once then retry.
    #[arg(long, default_value_t = false)]
    pub reopen_on_fail: bool,
}

#[derive(Debug, Clone, Subcommand)]
pub enum InputCommand {
    /// Click at x/y coordinates.
    Click(InputClickArgs),

    /// Type text.
    Type(InputTypeArgs),

    /// Send hotkey chord.
    Hotkey(InputHotkeyArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum InputSourceCommand {
    /// Show current keyboard input source id.
    Current(InputSourceCurrentArgs),

    /// Switch to a keyboard input source id.
    Switch(InputSourceSwitchArgs),
}

#[derive(Debug, Clone, Args, Default)]
pub struct InputSourceCurrentArgs {}

#[derive(Debug, Clone, Args)]
pub struct InputSourceSwitchArgs {
    /// Input source id or alias (`abc`, `us`, or full source id).
    #[arg(long)]
    pub id: String,
}

#[derive(Debug, Clone, Subcommand)]
pub enum AxCommand {
    /// List AX nodes.
    List(AxListArgs),

    /// Click an AX node.
    Click(AxClickArgs),

    /// Type text into an AX node.
    Type(AxTypeArgs),

    /// Read or set arbitrary AX attributes.
    Attr {
        #[command(subcommand)]
        command: AxAttrCommand,
    },

    /// Perform arbitrary AX actions.
    Action {
        #[command(subcommand)]
        command: AxActionCommand,
    },

    /// Manage long-lived AX sessions.
    Session {
        #[command(subcommand)]
        command: AxSessionCommand,
    },

    /// Start/poll/stop AX notification watchers.
    Watch {
        #[command(subcommand)]
        command: AxWatchCommand,
    },
}

#[derive(Debug, Clone, Subcommand)]
pub enum AxAttrCommand {
    /// Read an AX attribute value.
    Get(AxAttrGetArgs),

    /// Set an AX attribute value.
    Set(AxAttrSetArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum AxActionCommand {
    /// Perform an AX action.
    Perform(AxActionPerformArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum AxSessionCommand {
    /// Create or update an AX session.
    Start(AxSessionStartArgs),

    /// List active AX sessions.
    List(AxSessionListArgs),

    /// Stop an AX session.
    Stop(AxSessionStopArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum AxWatchCommand {
    /// Start a watcher bound to an AX session.
    Start(AxWatchStartArgs),

    /// Poll buffered watcher events.
    Poll(AxWatchPollArgs),

    /// Stop a watcher.
    Stop(AxWatchStopArgs),
}

#[derive(Debug, Clone, Args, Default)]
pub struct AxTargetArgs {
    /// Select target by existing session id.
    #[arg(long)]
    pub session_id: Option<String>,

    /// Select target app by name.
    #[arg(long)]
    pub app: Option<String>,

    /// Select target app by bundle id.
    #[arg(long)]
    pub bundle_id: Option<String>,

    /// Filter roots by window title substring.
    #[arg(long)]
    pub window_title_contains: Option<String>,
}

#[derive(Debug, Clone, Args, Default)]
pub struct AxMatchFiltersArgs {
    /// Select by AX role.
    #[arg(long)]
    pub role: Option<String>,

    /// Select by title substring.
    #[arg(long)]
    pub title_contains: Option<String>,

    /// Select by identifier substring.
    #[arg(long)]
    pub identifier_contains: Option<String>,

    /// Select by value substring.
    #[arg(long)]
    pub value_contains: Option<String>,

    /// Select by AX subrole.
    #[arg(long)]
    pub subrole: Option<String>,

    /// Select by focused state.
    #[arg(long)]
    pub focused: Option<bool>,

    /// Select by enabled state.
    #[arg(long)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Args, Default)]
pub struct AxSelectorArgs {
    /// Select by node id from `ax list`.
    #[arg(
        long,
        conflicts_with_all = [
            "role",
            "title_contains",
            "identifier_contains",
            "value_contains",
            "subrole",
            "focused",
            "enabled",
            "nth"
        ]
    )]
    pub node_id: Option<String>,

    #[command(flatten)]
    pub filters: AxMatchFiltersArgs,

    /// Select the nth match from compound selector results.
    #[arg(long)]
    pub nth: Option<u32>,

    /// Text match strategy for selector filters.
    #[arg(long, value_enum, default_value_t = AxMatchStrategy::Contains)]
    pub match_strategy: AxMatchStrategy,

    /// Emit selector explain diagnostics in JSON output.
    #[arg(long)]
    pub selector_explain: bool,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct AxListArgs {
    #[command(flatten)]
    pub target: AxTargetArgs,

    #[command(flatten)]
    pub filters: AxMatchFiltersArgs,

    /// Limit traversal depth.
    #[arg(long)]
    pub max_depth: Option<u32>,

    /// Limit number of returned nodes.
    #[arg(long)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Args)]
pub struct AxActionGateArgs {
    /// Require app active gate before mutation.
    #[arg(long)]
    pub gate_app_active: bool,

    /// Require target window present gate before mutation.
    #[arg(long)]
    pub gate_window_present: bool,

    /// Require AX selector to resolve to at least one node before mutation.
    #[arg(long)]
    pub gate_ax_present: bool,

    /// Require AX selector to resolve to exactly one node before mutation.
    #[arg(long)]
    pub gate_ax_unique: bool,

    /// Timeout for gate checks in milliseconds.
    #[arg(long, default_value_t = 1500)]
    pub gate_timeout_ms: u64,

    /// Poll interval for gate checks in milliseconds.
    #[arg(long, default_value_t = 50)]
    pub gate_poll_ms: u64,
}

#[derive(Debug, Clone, Args)]
pub struct AxPostconditionArgs {
    /// Require focused state after mutation.
    #[arg(long)]
    pub postcondition_focused: Option<bool>,

    /// Require an AX attribute/value pair after mutation.
    #[arg(long, requires = "postcondition_attribute_value")]
    pub postcondition_attribute: Option<String>,

    /// Expected value for --postcondition-attribute.
    #[arg(long, requires = "postcondition_attribute")]
    pub postcondition_attribute_value: Option<String>,

    /// Timeout for postcondition checks in milliseconds.
    #[arg(long, default_value_t = 1500)]
    pub postcondition_timeout_ms: u64,

    /// Poll interval for postcondition checks in milliseconds.
    #[arg(long, default_value_t = 50)]
    pub postcondition_poll_ms: u64,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(true)
            .args([
                "node_id",
                "role",
                "title_contains",
                "identifier_contains",
                "value_contains",
                "subrole",
                "focused",
                "enabled",
            ])
    ),
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct AxClickArgs {
    #[command(flatten)]
    pub selector: AxSelectorArgs,

    #[command(flatten)]
    pub target: AxTargetArgs,

    /// Allow coordinate fallback when AX press is unavailable.
    #[arg(long)]
    pub allow_coordinate_fallback: bool,

    /// Re-resolve selector to node id immediately before click execution.
    #[arg(long)]
    pub reselect_before_click: bool,

    /// Configure click fallback stage order (`ax-press,ax-confirm,frame-center,coordinate`).
    #[arg(long, value_enum, value_delimiter = ',', num_args = 1.., value_name = "STAGE")]
    pub fallback_order: Vec<AxClickFallbackStage>,

    /// Unified wait-policy timeout override for gate/postcondition checks.
    #[arg(long)]
    pub wait_timeout_ms: Option<u64>,

    /// Unified wait-policy poll interval override for gate/postcondition checks.
    #[arg(long)]
    pub wait_poll_ms: Option<u64>,

    #[command(flatten)]
    pub gate: AxActionGateArgs,

    #[command(flatten)]
    pub postcondition: AxPostconditionArgs,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(true)
            .args([
                "node_id",
                "role",
                "title_contains",
                "identifier_contains",
                "value_contains",
                "subrole",
                "focused",
                "enabled",
            ])
    ),
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct AxTypeArgs {
    #[command(flatten)]
    pub selector: AxSelectorArgs,

    #[command(flatten)]
    pub target: AxTargetArgs,

    /// Text to type.
    #[arg(long, value_parser = clap::builder::NonEmptyStringValueParser::new())]
    pub text: String,

    /// Clear field value before typing.
    #[arg(long)]
    pub clear_first: bool,

    /// Submit (Enter) after typing.
    #[arg(long)]
    pub submit: bool,

    /// Use clipboard paste strategy.
    #[arg(long)]
    pub paste: bool,

    /// Allow keyboard fallback when AX value set/focus is unavailable.
    #[arg(long)]
    pub allow_keyboard_fallback: bool,

    /// Unified wait-policy timeout override for gate/postcondition checks.
    #[arg(long)]
    pub wait_timeout_ms: Option<u64>,

    /// Unified wait-policy poll interval override for gate/postcondition checks.
    #[arg(long)]
    pub wait_poll_ms: Option<u64>,

    #[command(flatten)]
    pub gate: AxActionGateArgs,

    #[command(flatten)]
    pub postcondition: AxPostconditionArgs,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(true)
            .args([
                "node_id",
                "role",
                "title_contains",
                "identifier_contains",
                "value_contains",
                "subrole",
                "focused",
                "enabled",
            ])
    ),
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct AxAttrGetArgs {
    #[command(flatten)]
    pub selector: AxSelectorArgs,

    #[command(flatten)]
    pub target: AxTargetArgs,

    /// AX attribute name.
    #[arg(long)]
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AxValueType {
    String,
    Number,
    Bool,
    Json,
    Null,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(true)
            .args([
                "node_id",
                "role",
                "title_contains",
                "identifier_contains",
                "value_contains",
                "subrole",
                "focused",
                "enabled",
            ])
    ),
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct AxAttrSetArgs {
    #[command(flatten)]
    pub selector: AxSelectorArgs,

    #[command(flatten)]
    pub target: AxTargetArgs,

    /// AX attribute name.
    #[arg(long)]
    pub name: String,

    /// AX attribute value.
    #[arg(long)]
    pub value: String,

    /// Parse type for --value.
    #[arg(long, value_enum, default_value_t = AxValueType::String)]
    pub value_type: AxValueType,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(true)
            .args([
                "node_id",
                "role",
                "title_contains",
                "identifier_contains",
                "value_contains",
                "subrole",
                "focused",
                "enabled",
            ])
    ),
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct AxActionPerformArgs {
    #[command(flatten)]
    pub selector: AxSelectorArgs,

    #[command(flatten)]
    pub target: AxTargetArgs,

    /// AX action name.
    #[arg(long)]
    pub name: String,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["app", "bundle_id"])
    )
)]
pub struct AxSessionStartArgs {
    /// Select target app by name.
    #[arg(long)]
    pub app: Option<String>,

    /// Select target app by bundle id.
    #[arg(long)]
    pub bundle_id: Option<String>,

    /// Optionally name the session id.
    #[arg(long)]
    pub session_id: Option<String>,

    /// Persist window title root filter in this session.
    #[arg(long)]
    pub window_title_contains: Option<String>,
}

#[derive(Debug, Clone, Args, Default)]
pub struct AxSessionListArgs {}

#[derive(Debug, Clone, Args)]
pub struct AxSessionStopArgs {
    /// Session id to remove.
    #[arg(long)]
    pub session_id: String,
}

#[derive(Debug, Clone, Args)]
pub struct AxWatchStartArgs {
    /// Session id to bind watcher to.
    #[arg(long)]
    pub session_id: String,

    /// Optional watcher id.
    #[arg(long)]
    pub watch_id: Option<String>,

    /// Comma-separated AX notification names.
    #[arg(
        long,
        value_delimiter = ',',
        default_value = "AXFocusedUIElementChanged,AXTitleChanged"
    )]
    pub events: Vec<String>,

    /// Maximum in-memory event buffer size.
    #[arg(long, default_value_t = 256)]
    pub max_buffer: usize,
}

#[derive(Debug, Clone, Args)]
pub struct AxWatchPollArgs {
    /// Watch id to poll.
    #[arg(long)]
    pub watch_id: String,

    /// Max events to return.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,

    /// Drain returned events from watcher buffer.
    #[arg(long, default_value_t = true)]
    pub drain: bool,
}

#[derive(Debug, Clone, Args)]
pub struct AxWatchStopArgs {
    /// Watch id to stop.
    #[arg(long)]
    pub watch_id: String,
}

#[derive(Debug, Clone, Args)]
pub struct InputClickArgs {
    /// X coordinate in pixels.
    #[arg(long)]
    pub x: i32,

    /// Y coordinate in pixels.
    #[arg(long)]
    pub y: i32,

    /// Mouse button.
    #[arg(long, value_enum, default_value_t = MouseButton::Left)]
    pub button: MouseButton,

    /// Number of clicks.
    #[arg(long, default_value_t = 1)]
    pub count: u8,

    /// Wait before clicking.
    #[arg(long, default_value_t = 0)]
    pub pre_wait_ms: u64,

    /// Wait after clicking.
    #[arg(long, default_value_t = 0)]
    pub post_wait_ms: u64,
}

#[derive(Debug, Clone, Args)]
pub struct InputTypeArgs {
    /// Text to type.
    #[arg(long)]
    pub text: String,

    /// Delay between key events.
    #[arg(long)]
    pub delay_ms: Option<u64>,

    /// Press Enter after typing.
    #[arg(long = "submit")]
    pub enter: bool,
}

#[derive(Debug, Clone, Args)]
pub struct InputHotkeyArgs {
    /// Modifier keys, comma-separated.
    #[arg(long)]
    pub mods: String,

    /// Main key.
    #[arg(long)]
    pub key: String,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ObserveCommand {
    /// Capture a screenshot.
    Screenshot(ObserveScreenshotArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum DebugCommand {
    /// Capture a deterministic triage artifact bundle.
    Bundle(DebugBundleArgs),
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(false)
            .multiple(false)
            .args(["window_id", "active_window", "app"])
    )
)]
pub struct DebugBundleArgs {
    /// Select by window id.
    #[arg(long)]
    pub window_id: Option<u32>,

    /// Select frontmost active window.
    #[arg(long)]
    pub active_window: bool,

    /// Select by app name.
    #[arg(long)]
    pub app: Option<String>,

    /// Narrow app selection by window title substring.
    #[arg(long = "window-title-contains", requires = "app")]
    pub window_name: Option<String>,

    /// Output directory for debug artifacts.
    #[arg(long)]
    pub output_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(false)
            .args(["window_id", "active_window", "app"])
    )
)]
pub struct ObserveScreenshotArgs {
    /// Select by window id.
    #[arg(long)]
    pub window_id: Option<u32>,

    /// Select frontmost active window.
    #[arg(long)]
    pub active_window: bool,

    /// Select by app name.
    #[arg(long)]
    pub app: Option<String>,

    /// Narrow app selection by window title substring.
    #[arg(long = "window-title-contains", requires = "app")]
    pub window_name: Option<String>,

    /// Output path.
    #[arg(long)]
    pub path: Option<PathBuf>,

    /// Output image format.
    #[arg(long, value_enum)]
    pub image_format: Option<ImageFormat>,

    #[command(flatten)]
    pub ax_selector: AxSelectorArgs,

    /// Expand selector frame by N pixels in each direction.
    #[arg(long, default_value_t = 0)]
    pub selector_padding: i32,

    /// Skip writing a new screenshot when the capture hash stays within threshold.
    #[arg(long)]
    pub if_changed: bool,

    /// Optional baseline image path used for hash comparison.
    #[arg(long, value_name = "path", requires = "if_changed")]
    pub if_changed_baseline: Option<PathBuf>,

    /// Maximum allowed hash-distance bits before capture is considered changed.
    #[arg(
        long,
        value_name = "bits",
        value_parser = clap::value_parser!(u32).range(0..=64),
        requires = "if_changed"
    )]
    pub if_changed_threshold: Option<u32>,
}

#[derive(Debug, Clone, Subcommand)]
pub enum WaitCommand {
    /// Sleep for a fixed duration.
    Sleep(WaitSleepArgs),

    /// Wait for app to become active.
    AppActive(WaitAppActiveArgs),

    /// Wait for target window to appear.
    WindowPresent(WaitWindowPresentArgs),

    /// Wait until AX selector resolves to at least one node.
    AxPresent(WaitAxPresentArgs),

    /// Wait until AX selector resolves to exactly one node.
    AxUnique(WaitAxUniqueArgs),
}

#[derive(Debug, Clone, Subcommand)]
pub enum ScenarioCommand {
    /// Run steps from a scenario JSON file.
    Run(ScenarioRunArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ScenarioRunArgs {
    /// Scenario JSON file path.
    #[arg(long)]
    pub file: PathBuf,
}

#[derive(Debug, Clone, Subcommand)]
pub enum ProfileCommand {
    /// Validate profile schema and coordinate bounds.
    Validate(ProfileValidateArgs),
    /// Write a scaffold profile template.
    Init(ProfileInitArgs),
}

#[derive(Debug, Clone, Args)]
pub struct ProfileValidateArgs {
    /// Profile JSON file path.
    #[arg(long)]
    pub file: PathBuf,
}

#[derive(Debug, Clone, Args)]
pub struct ProfileInitArgs {
    /// Profile name to embed in generated scaffold.
    #[arg(long, default_value = "default-1440p")]
    pub name: String,

    /// Output path for scaffold JSON.
    #[arg(long)]
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub struct WaitSleepArgs {
    /// Sleep duration in milliseconds.
    #[arg(long)]
    pub ms: u64,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(false)
            .args(["app", "bundle_id"])
    )
)]
pub struct WaitAppActiveArgs {
    /// App name.
    #[arg(long)]
    pub app: Option<String>,

    /// Bundle id.
    #[arg(long)]
    pub bundle_id: Option<String>,

    /// Timeout in milliseconds.
    #[arg(long, default_value_t = 1500, visible_alias = "wait-timeout-ms")]
    pub timeout_ms: u64,

    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 50, visible_alias = "wait-poll-ms")]
    pub poll_ms: u64,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(false)
            .args(["window_id", "active_window", "app"])
    )
)]
pub struct WaitWindowPresentArgs {
    /// Select by window id.
    #[arg(long)]
    pub window_id: Option<u32>,

    /// Select frontmost active window.
    #[arg(long)]
    pub active_window: bool,

    /// Select by app name.
    #[arg(long)]
    pub app: Option<String>,

    /// Narrow app selection by window title substring.
    #[arg(long = "window-title-contains", requires = "app")]
    pub window_name: Option<String>,

    /// Timeout in milliseconds.
    #[arg(long, default_value_t = 1500, visible_alias = "wait-timeout-ms")]
    pub timeout_ms: u64,

    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 50, visible_alias = "wait-poll-ms")]
    pub poll_ms: u64,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(true)
            .args([
                "node_id",
                "role",
                "title_contains",
                "identifier_contains",
                "value_contains",
                "subrole",
                "focused",
                "enabled",
            ])
    ),
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct WaitAxPresentArgs {
    #[command(flatten)]
    pub selector: AxSelectorArgs,

    #[command(flatten)]
    pub target: AxTargetArgs,

    /// Timeout in milliseconds.
    #[arg(long, default_value_t = 1500, visible_alias = "wait-timeout-ms")]
    pub timeout_ms: u64,

    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 50, visible_alias = "wait-poll-ms")]
    pub poll_ms: u64,
}

#[derive(Debug, Clone, Args)]
#[command(
    group(
        ArgGroup::new("selector")
            .required(true)
            .multiple(true)
            .args([
                "node_id",
                "role",
                "title_contains",
                "identifier_contains",
                "value_contains",
                "subrole",
                "focused",
                "enabled",
            ])
    ),
    group(
        ArgGroup::new("target")
            .required(false)
            .multiple(false)
            .args(["session_id", "app", "bundle_id"])
    )
)]
pub struct WaitAxUniqueArgs {
    #[command(flatten)]
    pub selector: AxSelectorArgs,

    #[command(flatten)]
    pub target: AxTargetArgs,

    /// Timeout in milliseconds.
    #[arg(long, default_value_t = 1500, visible_alias = "wait-timeout-ms")]
    pub timeout_ms: u64,

    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 50, visible_alias = "wait-poll-ms")]
    pub poll_ms: u64,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;
    use pretty_assertions::assert_eq;

    use super::{
        AxActionCommand, AxAttrCommand, AxCommand, AxSessionCommand, AxWatchCommand, Cli,
        CommandGroup, DebugCommand, ErrorFormat, InputSourceCommand, ObserveCommand, OutputFormat,
        WaitCommand, WindowCommand,
    };

    #[test]
    fn parses_window_activate_command_tree() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "--format",
            "json",
            "--retries",
            "2",
            "window",
            "activate",
            "--app",
            "Terminal",
            "--wait-ms",
            "1500",
        ])
        .expect("window activate should parse");

        assert_eq!(cli.format, OutputFormat::Json);
        assert_eq!(cli.error_format, ErrorFormat::Text);
        assert_eq!(cli.retries, 2);
        match cli.command {
            CommandGroup::Window {
                command: WindowCommand::Activate(args),
            } => {
                assert_eq!(args.app.as_deref(), Some("Terminal"));
                assert_eq!(args.wait_ms, Some(1500));
                assert!(!args.reopen_on_fail);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_window_activate_reopen_on_fail_flag() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "window",
            "activate",
            "--app",
            "Arc",
            "--reopen-on-fail",
        ])
        .expect("window activate reopen-on-fail should parse");

        match cli.command {
            CommandGroup::Window {
                command: WindowCommand::Activate(args),
            } => {
                assert_eq!(args.app.as_deref(), Some("Arc"));
                assert!(args.reopen_on_fail);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_wait_window_present() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "wait",
            "window-present",
            "--app",
            "Terminal",
            "--window-title-contains",
            "Inbox",
            "--timeout-ms",
            "2000",
            "--poll-ms",
            "100",
        ])
        .expect("wait window-present should parse");

        match cli.command {
            CommandGroup::Wait {
                command: WaitCommand::WindowPresent(args),
            } => {
                assert_eq!(args.app.as_deref(), Some("Terminal"));
                assert_eq!(args.window_name.as_deref(), Some("Inbox"));
                assert_eq!(args.timeout_ms, 2000);
                assert_eq!(args.poll_ms, 100);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_wait_ax_present_with_match_strategy_and_explain() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "wait",
            "ax-present",
            "--app",
            "Arc",
            "--role",
            "AXButton",
            "--title-contains",
            "^Run$",
            "--match-strategy",
            "regex",
            "--selector-explain",
            "--timeout-ms",
            "1800",
            "--poll-ms",
            "25",
        ])
        .expect("wait ax-present should parse");

        match cli.command {
            CommandGroup::Wait {
                command: WaitCommand::AxPresent(args),
            } => {
                assert_eq!(args.target.app.as_deref(), Some("Arc"));
                assert_eq!(args.selector.filters.role.as_deref(), Some("AXButton"));
                assert_eq!(
                    args.selector.filters.title_contains.as_deref(),
                    Some("^Run$")
                );
                assert_eq!(
                    args.selector.match_strategy,
                    crate::model::AxMatchStrategy::Regex
                );
                assert!(args.selector.selector_explain);
                assert_eq!(args.timeout_ms, 1800);
                assert_eq!(args.poll_ms, 25);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_ax_click_gate_and_postcondition_flags() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "click",
            "--app",
            "Terminal",
            "--node-id",
            "1.1",
            "--gate-app-active",
            "--gate-window-present",
            "--gate-ax-present",
            "--gate-ax-unique",
            "--gate-timeout-ms",
            "2100",
            "--gate-poll-ms",
            "25",
            "--postcondition-focused",
            "true",
            "--postcondition-attribute",
            "AXRole",
            "--postcondition-attribute-value",
            "AXButton",
            "--postcondition-timeout-ms",
            "1800",
            "--postcondition-poll-ms",
            "20",
        ])
        .expect("ax click gate/postcondition flags should parse");

        match cli.command {
            CommandGroup::Ax {
                command: AxCommand::Click(args),
            } => {
                assert!(args.gate.gate_app_active);
                assert!(args.gate.gate_window_present);
                assert!(args.gate.gate_ax_present);
                assert!(args.gate.gate_ax_unique);
                assert_eq!(args.gate.gate_timeout_ms, 2100);
                assert_eq!(args.gate.gate_poll_ms, 25);
                assert_eq!(args.postcondition.postcondition_focused, Some(true));
                assert_eq!(
                    args.postcondition.postcondition_attribute.as_deref(),
                    Some("AXRole")
                );
                assert_eq!(
                    args.postcondition.postcondition_attribute_value.as_deref(),
                    Some("AXButton")
                );
                assert_eq!(args.postcondition.postcondition_timeout_ms, 1800);
                assert_eq!(args.postcondition.postcondition_poll_ms, 20);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_ax_wait_policy_overrides() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "type",
            "--app",
            "Terminal",
            "--node-id",
            "1.1",
            "--text",
            "hello",
            "--wait-timeout-ms",
            "2200",
            "--wait-poll-ms",
            "40",
        ])
        .expect("ax type wait policy flags should parse");

        match cli.command {
            CommandGroup::Ax {
                command: AxCommand::Type(args),
            } => {
                assert_eq!(args.wait_timeout_ms, Some(2200));
                assert_eq!(args.wait_poll_ms, Some(40));
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_debug_bundle_and_observe_selector_padding_flags() {
        let debug_cli = Cli::try_parse_from([
            "macos-agent",
            "debug",
            "bundle",
            "--active-window",
            "--output-dir",
            "/tmp/debug-bundle",
        ])
        .expect("debug bundle should parse");
        match debug_cli.command {
            CommandGroup::Debug {
                command: DebugCommand::Bundle(args),
            } => {
                assert!(args.active_window);
                assert_eq!(
                    args.output_dir.expect("output-dir should parse"),
                    PathBuf::from("/tmp/debug-bundle")
                );
            }
            other => panic!("unexpected command variant: {other:?}"),
        }

        let observe_cli = Cli::try_parse_from([
            "macos-agent",
            "observe",
            "screenshot",
            "--active-window",
            "--role",
            "AXButton",
            "--title-contains",
            "Run",
            "--selector-padding",
            "12",
            "--if-changed",
            "--if-changed-threshold",
            "4",
        ])
        .expect("observe screenshot selector flags should parse");
        match observe_cli.command {
            CommandGroup::Observe {
                command: ObserveCommand::Screenshot(args),
            } => {
                assert!(args.active_window);
                assert_eq!(args.ax_selector.filters.role.as_deref(), Some("AXButton"));
                assert_eq!(
                    args.ax_selector.filters.title_contains.as_deref(),
                    Some("Run")
                );
                assert_eq!(args.selector_padding, 12);
                assert!(args.if_changed);
                assert_eq!(args.if_changed_threshold, Some(4));
                assert_eq!(args.if_changed_baseline, None);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn rejects_multiple_window_activate_selectors() {
        let err = Cli::try_parse_from([
            "macos-agent",
            "window",
            "activate",
            "--window-id",
            "10",
            "--app",
            "Terminal",
        ])
        .expect_err("multiple selectors must be rejected");
        let rendered = err.to_string();
        assert!(
            rendered.contains("cannot be used with")
                || rendered.contains("required arguments were not provided")
        );
    }

    #[test]
    fn rejects_window_title_contains_without_app() {
        let err = Cli::try_parse_from([
            "macos-agent",
            "wait",
            "window-present",
            "--window-title-contains",
            "Inbox",
        ])
        .expect_err("window-title-contains requires app");
        let rendered = err.to_string();
        assert!(
            rendered.contains("requires")
                || rendered.contains("required arguments were not provided")
        );
    }

    #[test]
    fn parses_wait_window_present_with_window_title_contains() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "wait",
            "window-present",
            "--app",
            "Terminal",
            "--window-title-contains",
            "Inbox",
        ])
        .expect("wait window-present with window-title-contains should parse");

        match cli.command {
            CommandGroup::Wait {
                command: WaitCommand::WindowPresent(args),
            } => {
                assert_eq!(args.window_name.as_deref(), Some("Inbox"));
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_input_source_switch_command() {
        let cli = Cli::try_parse_from(["macos-agent", "input-source", "switch", "--id", "abc"])
            .expect("input-source switch should parse");

        match cli.command {
            CommandGroup::InputSource {
                command: InputSourceCommand::Switch(args),
            } => {
                assert_eq!(args.id, "abc".to_string());
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_input_type_submit() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "input",
            "type",
            "--text",
            "hello",
            "--submit",
        ])
        .expect("input type --submit should parse");
        match cli.command {
            CommandGroup::Input {
                command: super::InputCommand::Type(args),
            } => assert!(args.enter),
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_ax_list_with_filters() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "list",
            "--app",
            "Arc",
            "--role",
            "AXButton",
            "--title-contains",
            "New tab",
            "--max-depth",
            "4",
            "--limit",
            "20",
        ])
        .expect("ax list should parse");

        match cli.command {
            CommandGroup::Ax {
                command: AxCommand::List(args),
            } => {
                assert_eq!(args.target.app.as_deref(), Some("Arc"));
                assert_eq!(args.filters.role.as_deref(), Some("AXButton"));
                assert_eq!(args.filters.title_contains.as_deref(), Some("New tab"));
                assert_eq!(args.max_depth, Some(4));
                assert_eq!(args.limit, Some(20));
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_ax_click_node_id_selector() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "--dry-run",
            "ax",
            "click",
            "--node-id",
            "node-17",
            "--allow-coordinate-fallback",
        ])
        .expect("ax click should parse");

        match cli.command {
            CommandGroup::Ax {
                command: AxCommand::Click(args),
            } => {
                assert_eq!(args.selector.node_id.as_deref(), Some("node-17"));
                assert!(args.allow_coordinate_fallback);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_ax_click_reselect_and_fallback_order() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "click",
            "--role",
            "AXButton",
            "--title-contains",
            "Run",
            "--reselect-before-click",
            "--allow-coordinate-fallback",
            "--fallback-order",
            "ax-press,ax-confirm,frame-center,coordinate",
        ])
        .expect("ax click reselect/fallback-order should parse");

        match cli.command {
            CommandGroup::Ax {
                command: AxCommand::Click(args),
            } => {
                assert!(args.reselect_before_click);
                assert_eq!(
                    args.fallback_order,
                    vec![
                        crate::model::AxClickFallbackStage::AxPress,
                        crate::model::AxClickFallbackStage::AxConfirm,
                        crate::model::AxClickFallbackStage::FrameCenter,
                        crate::model::AxClickFallbackStage::Coordinate,
                    ]
                );
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_ax_type_compound_selector() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "type",
            "--role",
            "AXTextField",
            "--title-contains",
            "Search",
            "--nth",
            "2",
            "--text",
            "hello",
            "--clear-first",
            "--submit",
            "--paste",
            "--allow-keyboard-fallback",
        ])
        .expect("ax type should parse");

        match cli.command {
            CommandGroup::Ax {
                command: AxCommand::Type(args),
            } => {
                assert_eq!(args.selector.filters.role.as_deref(), Some("AXTextField"));
                assert_eq!(
                    args.selector.filters.title_contains.as_deref(),
                    Some("Search")
                );
                assert_eq!(args.selector.nth, Some(2));
                assert_eq!(args.text, "hello");
                assert!(args.clear_first);
                assert!(args.submit);
                assert!(args.paste);
                assert!(args.allow_keyboard_fallback);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn rejects_ax_click_mixed_selectors() {
        let err = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "click",
            "--node-id",
            "node-17",
            "--role",
            "AXButton",
            "--title-contains",
            "Save",
        ])
        .expect_err("selector mix should be rejected");
        let rendered = err.to_string();
        assert!(
            rendered.contains("cannot be used with")
                || rendered.contains("required arguments were not provided")
        );
    }

    #[test]
    fn parses_ax_type_role_without_title_contains() {
        let cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "type",
            "--role",
            "AXTextField",
            "--text",
            "hello",
        ])
        .expect("role-only selector should parse");
        match cli.command {
            CommandGroup::Ax {
                command: AxCommand::Type(args),
            } => {
                assert_eq!(args.selector.filters.role.as_deref(), Some("AXTextField"));
                assert!(args.selector.filters.title_contains.is_none());
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn rejects_ax_type_nth_without_selector_filter() {
        let err =
            Cli::try_parse_from(["macos-agent", "ax", "type", "--nth", "2", "--text", "hello"])
                .expect_err("nth alone should be rejected by selector group");
        let rendered = err.to_string();
        assert!(rendered.contains("required arguments were not provided"));
    }

    #[test]
    fn rejects_ax_list_multiple_target_selectors() {
        let err = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "list",
            "--app",
            "Arc",
            "--bundle-id",
            "com.apple.Safari",
        ])
        .expect_err("app and bundle-id should be mutually exclusive");
        let rendered = err.to_string();
        assert!(rendered.contains("cannot be used with"));
    }

    #[test]
    fn parses_ax_attr_get_and_set_commands() {
        let get_cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "attr",
            "get",
            "--node-id",
            "1.2",
            "--name",
            "AXRole",
        ])
        .expect("ax attr get should parse");
        match get_cli.command {
            CommandGroup::Ax {
                command:
                    AxCommand::Attr {
                        command: AxAttrCommand::Get(args),
                    },
            } => {
                assert_eq!(args.selector.node_id.as_deref(), Some("1.2"));
                assert_eq!(args.name, "AXRole");
            }
            other => panic!("unexpected command variant: {other:?}"),
        }

        let set_cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "attr",
            "set",
            "--role",
            "AXTextField",
            "--title-contains",
            "Search",
            "--name",
            "AXValue",
            "--value",
            "hello",
            "--value-type",
            "string",
        ])
        .expect("ax attr set should parse");
        match set_cli.command {
            CommandGroup::Ax {
                command:
                    AxCommand::Attr {
                        command: AxAttrCommand::Set(args),
                    },
            } => {
                assert_eq!(args.selector.filters.role.as_deref(), Some("AXTextField"));
                assert_eq!(
                    args.selector.filters.title_contains.as_deref(),
                    Some("Search")
                );
                assert_eq!(args.name, "AXValue");
                assert_eq!(args.value, "hello");
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }

    #[test]
    fn parses_ax_action_session_and_watch_commands() {
        let action_cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "action",
            "perform",
            "--node-id",
            "1.1",
            "--name",
            "AXPress",
        ])
        .expect("ax action perform should parse");
        match action_cli.command {
            CommandGroup::Ax {
                command:
                    AxCommand::Action {
                        command: AxActionCommand::Perform(args),
                    },
            } => {
                assert_eq!(args.selector.node_id.as_deref(), Some("1.1"));
                assert_eq!(args.name, "AXPress");
            }
            other => panic!("unexpected command variant: {other:?}"),
        }

        let session_cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "session",
            "start",
            "--app",
            "Arc",
            "--session-id",
            "axs-demo",
        ])
        .expect("ax session start should parse");
        match session_cli.command {
            CommandGroup::Ax {
                command:
                    AxCommand::Session {
                        command: AxSessionCommand::Start(args),
                    },
            } => {
                assert_eq!(args.app.as_deref(), Some("Arc"));
                assert_eq!(args.session_id.as_deref(), Some("axs-demo"));
            }
            other => panic!("unexpected command variant: {other:?}"),
        }

        let watch_cli = Cli::try_parse_from([
            "macos-agent",
            "ax",
            "watch",
            "start",
            "--session-id",
            "axs-demo",
            "--events",
            "AXTitleChanged,AXFocusedUIElementChanged",
        ])
        .expect("ax watch start should parse");
        match watch_cli.command {
            CommandGroup::Ax {
                command:
                    AxCommand::Watch {
                        command: AxWatchCommand::Start(args),
                    },
            } => {
                assert_eq!(args.session_id, "axs-demo");
                assert_eq!(args.events.len(), 2);
            }
            other => panic!("unexpected command variant: {other:?}"),
        }
    }
}
