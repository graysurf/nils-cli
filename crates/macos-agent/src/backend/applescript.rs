use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::backend::process::{ProcessFailure, ProcessRequest, ProcessRunner, map_failure};
use crate::error::CliError;
use crate::model::{
    AxClickRequest, AxClickResult, AxListRequest, AxListResult, AxSelector, AxTypeRequest,
    AxTypeResult,
};
use crate::test_mode;

const FRONTMOST_APP_SCRIPT: &str = r#"tell application "System Events" to get name of first application process whose frontmost is true"#;
const FRONTMOST_BUNDLE_ID_SCRIPT: &str = r#"tell application "System Events" to get bundle identifier of first application process whose frontmost is true"#;

const AX_LIST_JXA_SCRIPT: &str = r#"function run(argv) {
  function safe(callable, fallbackValue) {
    try { return callable(); } catch (_err) { return fallbackValue; }
  }

  function normalize(value) {
    if (value === null || value === undefined) { return ""; }
    return String(value);
  }

  function attr(element, name, fallbackValue) {
    return safe(function () { return element.attributes.byName(name).value(); }, fallbackValue);
  }

  function boolAttr(element, name, fallbackValue) {
    var value = attr(element, name, fallbackValue);
    if (typeof value === "boolean") { return value; }
    if (value === null || value === undefined) { return !!fallbackValue; }
    return String(value).toLowerCase() === "true";
  }

  function actionNames(element) {
    var out = [];
    var actions = safe(function () { return element.actions(); }, []);
    for (var i = 0; i < actions.length; i += 1) {
      var name = safe(function () { return actions[i].name(); }, null);
      if (name !== null && name !== undefined && String(name).length > 0) {
        out.push(String(name));
      }
    }
    return out;
  }

  function frameFor(element) {
    var pos = attr(element, "AXPosition", null);
    var size = attr(element, "AXSize", null);
    if (!pos || !size || pos.length < 2 || size.length < 2) { return null; }
    return {
      x: Number(pos[0]),
      y: Number(pos[1]),
      width: Number(size[0]),
      height: Number(size[1])
    };
  }

  function valuePreview(element) {
    var value = attr(element, "AXValue", null);
    if (value === null || value === undefined) { return null; }
    var text = String(value);
    if (text.length > 160) { text = text.slice(0, 160) + "..."; }
    return text;
  }

  function resolveProcess(systemEvents, target) {
    if (target && target.app) {
      var byName = systemEvents.applicationProcesses.whose({ name: { _equals: String(target.app) } })();
      if (byName.length > 0) { return byName[0]; }
    }
    if (target && target.bundle_id) {
      var byBundle = systemEvents.applicationProcesses.whose({ bundleIdentifier: { _equals: String(target.bundle_id) } })();
      if (byBundle.length > 0) { return byBundle[0]; }
    }
    var frontmost = systemEvents.applicationProcesses.whose({ frontmost: true })();
    if (frontmost.length > 0) { return frontmost[0]; }
    return null;
  }

  var payload = {};
  if (argv.length > 0 && argv[0]) { payload = JSON.parse(argv[0]); }
  var roleFilter = payload.role ? String(payload.role).toLowerCase() : null;
  var titleFilter = payload.title_contains ? String(payload.title_contains).toLowerCase() : null;
  var maxDepth = payload.max_depth === null || payload.max_depth === undefined ? null : Number(payload.max_depth);
  var limit = payload.limit === null || payload.limit === undefined ? null : Number(payload.limit);

  var systemEvents = Application("System Events");
  var process = resolveProcess(systemEvents, payload.target || {});
  if (!process) { throw new Error("unable to resolve target app process for ax.list"); }

  var roots = safe(function () { return process.windows(); }, []);
  if (!roots || roots.length === 0) {
    roots = safe(function () { return process.uiElements(); }, []);
  }

  var nodes = [];

  function matches(node) {
    if (roleFilter && node.role.toLowerCase() !== roleFilter) { return false; }
    if (titleFilter) {
      var title = (node.title || "").toLowerCase();
      var identifier = (node.identifier || "").toLowerCase();
      if (title.indexOf(titleFilter) === -1 && identifier.indexOf(titleFilter) === -1) {
        return false;
      }
    }
    return true;
  }

  function visit(element, path, depth) {
    if (limit !== null && nodes.length >= limit) { return; }
    var role = normalize(attr(element, "AXRole", safe(function () { return element.role(); }, "")));
    var title = normalize(attr(element, "AXTitle", safe(function () { return element.title(); }, "")));
    var identifier = normalize(attr(element, "AXIdentifier", null));
    var subrole = normalize(attr(element, "AXSubrole", null));
    var node = {
      node_id: path.join("."),
      role: role,
      subrole: subrole.length > 0 ? subrole : null,
      title: title.length > 0 ? title : null,
      identifier: identifier.length > 0 ? identifier : null,
      value_preview: valuePreview(element),
      enabled: boolAttr(element, "AXEnabled", true),
      focused: boolAttr(element, "AXFocused", false),
      frame: frameFor(element),
      actions: actionNames(element),
      path: path
    };
    if (matches(node)) { nodes.push(node); }
    if (maxDepth !== null && depth >= maxDepth) { return; }

    var children = safe(function () { return element.uiElements(); }, []);
    for (var i = 0; i < children.length; i += 1) {
      visit(children[i], path.concat([String(i + 1)]), depth + 1);
      if (limit !== null && nodes.length >= limit) { return; }
    }
  }

  for (var rootIdx = 0; rootIdx < roots.length; rootIdx += 1) {
    visit(roots[rootIdx], [String(rootIdx + 1)], 0);
    if (limit !== null && nodes.length >= limit) { break; }
  }

  return JSON.stringify({ nodes: nodes, warnings: [] });
}"#;

const AX_CLICK_JXA_SCRIPT: &str = r#"function run(argv) {
  function safe(callable, fallbackValue) {
    try { return callable(); } catch (_err) { return fallbackValue; }
  }

  function normalize(value) {
    if (value === null || value === undefined) { return ""; }
    return String(value);
  }

  function attr(element, name, fallbackValue) {
    return safe(function () { return element.attributes.byName(name).value(); }, fallbackValue);
  }

  function frameFor(element) {
    var pos = attr(element, "AXPosition", null);
    var size = attr(element, "AXSize", null);
    if (!pos || !size || pos.length < 2 || size.length < 2) { return null; }
    return {
      x: Math.round(Number(pos[0]) + Number(size[0]) / 2),
      y: Math.round(Number(pos[1]) + Number(size[1]) / 2)
    };
  }

  function resolveProcess(systemEvents, target) {
    if (target && target.app) {
      var byName = systemEvents.applicationProcesses.whose({ name: { _equals: String(target.app) } })();
      if (byName.length > 0) { return byName[0]; }
    }
    if (target && target.bundle_id) {
      var byBundle = systemEvents.applicationProcesses.whose({ bundleIdentifier: { _equals: String(target.bundle_id) } })();
      if (byBundle.length > 0) { return byBundle[0]; }
    }
    var frontmost = systemEvents.applicationProcesses.whose({ frontmost: true })();
    if (frontmost.length > 0) { return frontmost[0]; }
    return null;
  }

  function nodeFrom(element, path) {
    var role = normalize(attr(element, "AXRole", safe(function () { return element.role(); }, "")));
    var title = normalize(attr(element, "AXTitle", safe(function () { return element.title(); }, "")));
    var identifier = normalize(attr(element, "AXIdentifier", null));
    return {
      node_id: path.join("."),
      role: role,
      title: title,
      identifier: identifier
    };
  }

  function resolveByNodeId(roots, nodeId) {
    var segments = String(nodeId).split(".");
    if (segments.length === 0) { return null; }
    var rootIndex = Number(segments[0]);
    if (!rootIndex || rootIndex < 1 || rootIndex > roots.length) { return null; }
    var element = roots[rootIndex - 1];
    var path = [String(rootIndex)];
    for (var i = 1; i < segments.length; i += 1) {
      var index = Number(segments[i]);
      if (!index || index < 1) { return null; }
      var children = safe(function () { return element.uiElements(); }, []);
      if (!children || index > children.length) { return null; }
      element = children[index - 1];
      path.push(String(index));
    }
    return { element: element, node: nodeFrom(element, path) };
  }

  var payload = {};
  if (argv.length > 0 && argv[0]) { payload = JSON.parse(argv[0]); }
  var selector = payload.selector || {};
  var roleFilter = selector.role ? String(selector.role).toLowerCase() : null;
  var titleFilter = selector.title_contains ? String(selector.title_contains).toLowerCase() : null;
  var nth = selector.nth === null || selector.nth === undefined ? null : Number(selector.nth);
  var allowCoordinateFallback = !!payload.allow_coordinate_fallback;

  var systemEvents = Application("System Events");
  var process = resolveProcess(systemEvents, payload.target || {});
  if (!process) { throw new Error("unable to resolve target app process for ax.click"); }

  var roots = safe(function () { return process.windows(); }, []);
  if (!roots || roots.length === 0) { roots = safe(function () { return process.uiElements(); }, []); }

  var matches = [];
  if (selector.node_id) {
    var byId = resolveByNodeId(roots, selector.node_id);
    if (byId !== null) { matches = [byId]; }
  } else {
    function walk(element, path) {
      var node = nodeFrom(element, path);
      var roleMatch = roleFilter === null || node.role.toLowerCase() === roleFilter;
      var titleLower = node.title.toLowerCase();
      var identifierLower = node.identifier.toLowerCase();
      var titleMatch = titleFilter === null || titleLower.indexOf(titleFilter) !== -1 || identifierLower.indexOf(titleFilter) !== -1;
      if (roleMatch && titleMatch) { matches.push({ element: element, node: node }); }
      var children = safe(function () { return element.uiElements(); }, []);
      for (var i = 0; i < children.length; i += 1) {
        walk(children[i], path.concat([String(i + 1)]));
      }
    }

    for (var rootIdx = 0; rootIdx < roots.length; rootIdx += 1) {
      walk(roots[rootIdx], [String(rootIdx + 1)]);
    }
  }

  if (matches.length === 0) { throw new Error("selector returned zero AX matches"); }
  var matchedCount = matches.length;

  var selected = null;
  if (selector.node_id) {
    selected = matches[0];
  } else if (nth !== null) {
    if (nth < 1 || nth > matches.length) {
      throw new Error("selector nth is out of range");
    }
    selected = matches[nth - 1];
  } else {
    if (matches.length !== 1) {
      throw new Error("selector is ambiguous; add --nth or narrow role/title filters");
    }
    selected = matches[0];
  }

  var result = {
    node_id: selected.node.node_id,
    matched_count: matchedCount,
    action: "ax-press",
    used_coordinate_fallback: false
  };

  var actions = safe(function () { return selected.element.actions(); }, []);
  var pressAction = null;
  for (var idx = 0; idx < actions.length; idx += 1) {
    var actionName = normalize(safe(function () { return actions[idx].name(); }, ""));
    if (actionName === "AXPress" || actionName === "AXConfirm") {
      pressAction = actions[idx];
      break;
    }
  }

  try {
    if (!pressAction) { throw new Error("AXPress action unavailable"); }
    pressAction.perform();
  } catch (err) {
    if (!allowCoordinateFallback) { throw err; }
    var center = frameFor(selected.element);
    if (!center) {
      throw new Error("coordinate fallback requested but AXPosition/AXSize unavailable");
    }
    result.action = "ax-press-fallback";
    result.used_coordinate_fallback = true;
    result.fallback_x = center.x;
    result.fallback_y = center.y;
  }

  return JSON.stringify(result);
}"#;

const AX_TYPE_JXA_SCRIPT: &str = r#"function run(argv) {
  function safe(callable, fallbackValue) {
    try { return callable(); } catch (_err) { return fallbackValue; }
  }

  function normalize(value) {
    if (value === null || value === undefined) { return ""; }
    return String(value);
  }

  function attr(element, name, fallbackValue) {
    return safe(function () { return element.attributes.byName(name).value(); }, fallbackValue);
  }

  function setAttr(element, name, value) {
    var attribute = element.attributes.byName(name);
    attribute.value = value;
  }

  function resolveProcess(systemEvents, target) {
    if (target && target.app) {
      var byName = systemEvents.applicationProcesses.whose({ name: { _equals: String(target.app) } })();
      if (byName.length > 0) { return byName[0]; }
    }
    if (target && target.bundle_id) {
      var byBundle = systemEvents.applicationProcesses.whose({ bundleIdentifier: { _equals: String(target.bundle_id) } })();
      if (byBundle.length > 0) { return byBundle[0]; }
    }
    var frontmost = systemEvents.applicationProcesses.whose({ frontmost: true })();
    if (frontmost.length > 0) { return frontmost[0]; }
    return null;
  }

  function nodeFrom(element, path) {
    var role = normalize(attr(element, "AXRole", safe(function () { return element.role(); }, "")));
    var title = normalize(attr(element, "AXTitle", safe(function () { return element.title(); }, "")));
    var identifier = normalize(attr(element, "AXIdentifier", null));
    return {
      node_id: path.join("."),
      role: role,
      title: title,
      identifier: identifier
    };
  }

  function resolveByNodeId(roots, nodeId) {
    var segments = String(nodeId).split(".");
    if (segments.length === 0) { return null; }
    var rootIndex = Number(segments[0]);
    if (!rootIndex || rootIndex < 1 || rootIndex > roots.length) { return null; }
    var element = roots[rootIndex - 1];
    var path = [String(rootIndex)];
    for (var i = 1; i < segments.length; i += 1) {
      var index = Number(segments[i]);
      if (!index || index < 1) { return null; }
      var children = safe(function () { return element.uiElements(); }, []);
      if (!children || index > children.length) { return null; }
      element = children[index - 1];
      path.push(String(index));
    }
    return { element: element, node: nodeFrom(element, path) };
  }

  var payload = {};
  if (argv.length > 0 && argv[0]) { payload = JSON.parse(argv[0]); }
  var selector = payload.selector || {};
  var roleFilter = selector.role ? String(selector.role).toLowerCase() : null;
  var titleFilter = selector.title_contains ? String(selector.title_contains).toLowerCase() : null;
  var nth = selector.nth === null || selector.nth === undefined ? null : Number(selector.nth);
  var text = payload.text === null || payload.text === undefined ? "" : String(payload.text);
  var allowKeyboardFallback = !!payload.allow_keyboard_fallback;
  var paste = !!payload.paste;
  var clearFirst = !!payload.clear_first;
  var submit = !!payload.submit;

  if (text.length === 0) { throw new Error("text cannot be empty"); }

  var systemEvents = Application("System Events");
  var currentApp = Application.currentApplication();
  currentApp.includeStandardAdditions = true;

  var process = resolveProcess(systemEvents, payload.target || {});
  if (!process) { throw new Error("unable to resolve target app process for ax.type"); }

  var roots = safe(function () { return process.windows(); }, []);
  if (!roots || roots.length === 0) { roots = safe(function () { return process.uiElements(); }, []); }

  var matches = [];
  if (selector.node_id) {
    var byId = resolveByNodeId(roots, selector.node_id);
    if (byId !== null) { matches = [byId]; }
  } else {
    function walk(element, path) {
      var node = nodeFrom(element, path);
      var roleMatch = roleFilter === null || node.role.toLowerCase() === roleFilter;
      var titleLower = node.title.toLowerCase();
      var identifierLower = node.identifier.toLowerCase();
      var titleMatch = titleFilter === null || titleLower.indexOf(titleFilter) !== -1 || identifierLower.indexOf(titleFilter) !== -1;
      if (roleMatch && titleMatch) { matches.push({ element: element, node: node }); }
      var children = safe(function () { return element.uiElements(); }, []);
      for (var i = 0; i < children.length; i += 1) {
        walk(children[i], path.concat([String(i + 1)]));
      }
    }

    for (var rootIdx = 0; rootIdx < roots.length; rootIdx += 1) {
      walk(roots[rootIdx], [String(rootIdx + 1)]);
    }
  }

  if (matches.length === 0) { throw new Error("selector returned zero AX matches"); }
  var matchedCount = matches.length;

  var selected = null;
  if (selector.node_id) {
    selected = matches[0];
  } else if (nth !== null) {
    if (nth < 1 || nth > matches.length) {
      throw new Error("selector nth is out of range");
    }
    selected = matches[nth - 1];
  } else {
    if (matches.length !== 1) {
      throw new Error("selector is ambiguous; add --nth or narrow role/title filters");
    }
    selected = matches[0];
  }

  var appliedVia = "ax-set-value";
  var usedKeyboardFallback = false;

  try {
    safe(function () { setAttr(selected.element, "AXFocused", true); return true; }, false);
    if (clearFirst) {
      safe(function () { setAttr(selected.element, "AXValue", ""); return true; }, false);
    }
    if (paste) {
      currentApp.setTheClipboardTo(text);
      systemEvents.keystroke("v", { using: ["command down"] });
      appliedVia = "ax-paste";
    } else {
      setAttr(selected.element, "AXValue", text);
      appliedVia = "ax-set-value";
    }
  } catch (err) {
    if (!allowKeyboardFallback) { throw err; }
    usedKeyboardFallback = true;
    if (paste) {
      currentApp.setTheClipboardTo(text);
      systemEvents.keystroke("v", { using: ["command down"] });
      appliedVia = "keyboard-paste-fallback";
    } else {
      systemEvents.keystroke(text);
      appliedVia = "keyboard-keystroke-fallback";
    }
  }

  if (submit) {
    systemEvents.keyCode(36);
  }

  return JSON.stringify({
    node_id: selected.node.node_id,
    matched_count: matchedCount,
    applied_via: appliedVia,
    text_length: text.length,
    submitted: submit,
    used_keyboard_fallback: usedKeyboardFallback
  });
}"#;

const AX_LIST_TEST_MODE_ENV: &str = "AGENTS_MACOS_AGENT_AX_LIST_JSON";
const AX_CLICK_TEST_MODE_ENV: &str = "AGENTS_MACOS_AGENT_AX_CLICK_JSON";
const AX_TYPE_TEST_MODE_ENV: &str = "AGENTS_MACOS_AGENT_AX_TYPE_JSON";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivationTarget {
    App(String),
    BundleId(String),
}

pub fn activate(
    runner: &dyn ProcessRunner,
    target: &ActivationTarget,
    timeout_ms: u64,
) -> Result<(), CliError> {
    let script = match target {
        ActivationTarget::App(app) => {
            format!(
                r#"tell application "{}" to activate"#,
                escape_applescript(app)
            )
        }
        ActivationTarget::BundleId(bundle_id) => {
            format!(
                r#"tell application id "{}" to activate"#,
                escape_applescript(bundle_id)
            )
        }
    };

    run_osascript(runner, "window.activate", script, timeout_ms).map(|_| ())
}

pub fn reopen(
    runner: &dyn ProcessRunner,
    target: &ActivationTarget,
    timeout_ms: u64,
) -> Result<(), CliError> {
    let script = match target {
        ActivationTarget::App(app) => {
            let escaped = escape_applescript(app);
            format!(
                r#"try
  tell application "{escaped}" to quit
on error
end try
delay 0.25
tell application "{escaped}" to activate"#
            )
        }
        ActivationTarget::BundleId(bundle_id) => {
            let escaped = escape_applescript(bundle_id);
            format!(
                r#"try
  tell application id "{escaped}" to quit
on error
end try
delay 0.25
tell application id "{escaped}" to activate"#
            )
        }
    };
    run_osascript(runner, "window.activate.reopen", script, timeout_ms).map(|_| ())
}

pub fn type_text(
    runner: &dyn ProcessRunner,
    text: &str,
    delay_ms: Option<u64>,
    enter: bool,
    timeout_ms: u64,
) -> Result<(), CliError> {
    let escaped = escape_applescript(text);
    let mut lines = vec![
        "tell application \"System Events\"".to_string(),
        format!("  keystroke \"{escaped}\""),
    ];

    if let Some(delay) = delay_ms {
        lines.push(format!("  delay {}", (delay as f64) / 1000.0));
    }
    if enter {
        lines.push("  key code 36".to_string());
    }

    lines.push("end tell".to_string());
    run_osascript(runner, "input.type", lines.join("\n"), timeout_ms).map(|_| ())
}

pub fn send_hotkey(
    runner: &dyn ProcessRunner,
    mods: &[Modifier],
    key: &str,
    timeout_ms: u64,
) -> Result<(), CliError> {
    send_key_event(runner, mods, key, 1, timeout_ms, "input.hotkey")
}

pub fn send_key(
    runner: &dyn ProcessRunner,
    key: &str,
    count: u8,
    timeout_ms: u64,
) -> Result<(), CliError> {
    send_key_event(runner, &[], key, count, timeout_ms, "input.key")
}

fn send_key_event(
    runner: &dyn ProcessRunner,
    mods: &[Modifier],
    key: &str,
    count: u8,
    timeout_ms: u64,
    operation: &'static str,
) -> Result<(), CliError> {
    if key.trim().is_empty() {
        return Err(CliError::usage("--key cannot be empty"));
    }
    if count == 0 {
        return Err(CliError::usage("--count must be at least 1"));
    }

    let modifiers = if mods.is_empty() {
        String::new()
    } else {
        let joined = mods
            .iter()
            .map(|modifier| modifier.applescript_token())
            .collect::<Vec<_>>()
            .join(", ");
        format!(" using {{{joined}}}")
    };

    let action = if let Some(code) = named_key_code(key) {
        format!("key code {code}{modifiers}")
    } else {
        format!("keystroke \"{}\"{modifiers}", escape_applescript(key))
    };
    let body = if count == 1 {
        format!("  {action}")
    } else {
        format!("  repeat {count} times\n    {action}\n  end repeat")
    };
    let script = format!("tell application \"System Events\"\n{body}\nend tell");

    run_osascript(runner, operation, script, timeout_ms).map(|_| ())
}

fn named_key_code(key: &str) -> Option<u16> {
    match key.trim().to_ascii_lowercase().as_str() {
        "tab" => Some(48),
        "return" | "enter" => Some(36),
        "escape" => Some(53),
        "space" => Some(49),
        "left" => Some(123),
        "right" => Some(124),
        "down" => Some(125),
        "up" => Some(126),
        "delete" => Some(51),
        _ => None,
    }
}

pub fn frontmost_app_name(runner: &dyn ProcessRunner, timeout_ms: u64) -> Result<String, CliError> {
    run_osascript(
        runner,
        "wait.app-active",
        FRONTMOST_APP_SCRIPT.to_string(),
        timeout_ms,
    )
    .map(|out| out.trim().to_string())
}

pub fn frontmost_bundle_id(
    runner: &dyn ProcessRunner,
    timeout_ms: u64,
) -> Result<String, CliError> {
    run_osascript(
        runner,
        "wait.app-active",
        FRONTMOST_BUNDLE_ID_SCRIPT.to_string(),
        timeout_ms,
    )
    .map(|out| out.trim().to_string())
}

pub fn ax_list(
    runner: &dyn ProcessRunner,
    request: &AxListRequest,
    timeout_ms: u64,
) -> Result<AxListResult, CliError> {
    run_jxa_json(
        runner,
        "ax.list",
        request,
        AX_LIST_JXA_SCRIPT,
        timeout_ms.max(1),
    )
}

pub fn ax_click(
    runner: &dyn ProcessRunner,
    request: &AxClickRequest,
    timeout_ms: u64,
) -> Result<AxClickResult, CliError> {
    if selector_is_empty(&request.selector) {
        return Err(
            CliError::ax_contract_failure("ax.click", "selector is empty")
                .with_hint("Provide --node-id or selector filters (--role/--title-contains)."),
        );
    }

    run_jxa_json(
        runner,
        "ax.click",
        request,
        AX_CLICK_JXA_SCRIPT,
        timeout_ms.max(1),
    )
}

pub fn ax_type(
    runner: &dyn ProcessRunner,
    request: &AxTypeRequest,
    timeout_ms: u64,
) -> Result<AxTypeResult, CliError> {
    if request.text.trim().is_empty() {
        return Err(CliError::usage("--text cannot be empty").with_operation("ax.type"));
    }
    if selector_is_empty(&request.selector) {
        return Err(
            CliError::ax_contract_failure("ax.type", "selector is empty")
                .with_hint("Provide --node-id or selector filters (--role/--title-contains)."),
        );
    }

    run_jxa_json(
        runner,
        "ax.type",
        request,
        AX_TYPE_JXA_SCRIPT,
        timeout_ms.max(1),
    )
}

pub fn parse_modifiers(raw: &str) -> Result<Vec<Modifier>, CliError> {
    let mut mods = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let modifier = Modifier::parse(token).ok_or_else(|| {
            CliError::usage(format!(
                "invalid modifier `{token}`; expected cmd,ctrl,alt,shift,fn"
            ))
        })?;
        if !mods.contains(&modifier) {
            mods.push(modifier);
        }
    }

    if mods.is_empty() {
        return Err(CliError::usage(
            "--mods cannot be empty; expected cmd,ctrl,alt,shift,fn",
        ));
    }

    Ok(mods)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Cmd,
    Ctrl,
    Alt,
    Shift,
    Fn,
}

impl Modifier {
    fn parse(token: &str) -> Option<Self> {
        match token.to_ascii_lowercase().as_str() {
            "cmd" | "command" => Some(Self::Cmd),
            "ctrl" | "control" => Some(Self::Ctrl),
            "alt" | "option" => Some(Self::Alt),
            "shift" => Some(Self::Shift),
            "fn" | "function" => Some(Self::Fn),
            _ => None,
        }
    }

    pub fn canonical(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::Ctrl => "ctrl",
            Self::Alt => "alt",
            Self::Shift => "shift",
            Self::Fn => "fn",
        }
    }

    fn applescript_token(self) -> &'static str {
        match self {
            Self::Cmd => "command down",
            Self::Ctrl => "control down",
            Self::Alt => "option down",
            Self::Shift => "shift down",
            Self::Fn => "fn down",
        }
    }
}

fn run_osascript(
    runner: &dyn ProcessRunner,
    operation: &str,
    script: String,
    timeout_ms: u64,
) -> Result<String, CliError> {
    let request = ProcessRequest::new(
        "osascript",
        vec!["-e".to_string(), script],
        timeout_ms.max(1),
    );
    runner
        .run(&request)
        .map(|output| output.stdout)
        .map_err(|failure| map_failure(operation, failure))
}

fn run_jxa_json<Request, Response>(
    runner: &dyn ProcessRunner,
    operation: &'static str,
    payload: &Request,
    script: &'static str,
    timeout_ms: u64,
) -> Result<Response, CliError>
where
    Request: Serialize,
    Response: DeserializeOwned,
{
    if let Some(override_json) = test_mode_override_json(operation) {
        return parse_jxa_output(operation, &override_json);
    }

    let payload_json = serde_json::to_string(payload)
        .map_err(|err| CliError::ax_payload_encode(operation, err.to_string()))?;
    let request = ProcessRequest::new(
        "osascript",
        vec![
            "-l".to_string(),
            "JavaScript".to_string(),
            "-e".to_string(),
            script.to_string(),
            payload_json,
        ],
        timeout_ms.max(1),
    );
    let stdout = runner
        .run(&request)
        .map(|output| output.stdout)
        .map_err(|failure| map_ax_failure(operation, failure))?;

    if stdout.trim().is_empty()
        && test_mode::enabled()
        && let Some(fallback_json) = test_mode_default_json(operation)
    {
        return parse_jxa_output(operation, fallback_json);
    }

    parse_jxa_output(operation, &stdout)
}

fn parse_jxa_output<Response>(operation: &str, raw: &str) -> Result<Response, CliError>
where
    Response: DeserializeOwned,
{
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ax_parse_error(operation, "stdout was empty"));
    }

    let value: Value = serde_json::from_str(trimmed).map_err(|err| {
        ax_parse_error(
            operation,
            format!("{err}; stdout preview=`{}`", output_preview(trimmed, 120)),
        )
    })?;

    validate_jxa_contract(operation, &value)?;

    serde_json::from_value(value).map_err(|err| {
        ax_parse_error(
            operation,
            format!("{err}; stdout preview=`{}`", output_preview(trimmed, 120)),
        )
    })
}

fn map_ax_failure(operation: &str, failure: ProcessFailure) -> CliError {
    map_failure(operation, failure)
        .with_hint("Run `macos-agent preflight --include-probes --strict` before AX operations.")
        .with_hint(
            "If this persists, rerun with --trace and inspect osascript stderr/stdout artifacts.",
        )
}

fn selector_is_empty(selector: &AxSelector) -> bool {
    selector.node_id.is_none() && selector.role.is_none() && selector.title_contains.is_none()
}

fn ax_parse_error(operation: &str, detail: impl Into<String>) -> CliError {
    let mut err = CliError::ax_parse_failure(operation, detail);
    if let Some(hint) = ax_contract_hint(operation) {
        err = err.with_hint(hint);
    }
    err
}

fn ax_contract_error(operation: &str, detail: impl Into<String>) -> CliError {
    let mut err = CliError::runtime(format!(
        "{operation} failed: AX backend contract violation ({})",
        detail.into().trim()
    ))
    .with_operation(operation)
    .with_hint("Run `macos-agent preflight --include-probes --strict` to verify Accessibility/Automation access.")
    .with_hint("Use --trace to capture raw backend output for diagnosis.");
    if let Some(hint) = ax_contract_hint(operation) {
        err = err.with_hint(hint);
    }
    err
}

fn ax_contract_hint(operation: &str) -> Option<&'static str> {
    match operation {
        "ax.list" => {
            Some("Expected object contract: { nodes: [...], warnings: [...] } (warnings optional).")
        }
        "ax.click" => Some(
            "Expected object contract: { matched_count, action, node_id?, used_coordinate_fallback?, fallback_x?, fallback_y? }.",
        ),
        "ax.type" => Some(
            "Expected object contract: { matched_count, applied_via, text_length, node_id?, submitted?, used_keyboard_fallback? }.",
        ),
        _ => None,
    }
}

fn validate_jxa_contract(operation: &str, value: &Value) -> Result<(), CliError> {
    match operation {
        "ax.list" => validate_ax_list_contract(operation, value),
        "ax.click" => validate_ax_click_contract(operation, value),
        "ax.type" => validate_ax_type_contract(operation, value),
        _ => Ok(()),
    }
}

fn expect_object<'a>(
    operation: &str,
    value: &'a Value,
) -> Result<&'a Map<String, Value>, CliError> {
    value
        .as_object()
        .ok_or_else(|| ax_contract_error(operation, "top-level payload must be a JSON object"))
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn ensure_required_array<'a>(
    operation: &str,
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<&'a Vec<Value>, CliError> {
    let value = object
        .get(field)
        .ok_or_else(|| ax_contract_error(operation, format!("missing required `{field}` array")))?;
    value.as_array().ok_or_else(|| {
        ax_contract_error(
            operation,
            format!(
                "`{field}` must be an array (received {})",
                json_type_name(value)
            ),
        )
    })
}

fn ensure_optional_array<'a>(
    operation: &str,
    object: &'a Map<String, Value>,
    field: &str,
) -> Result<Option<&'a Vec<Value>>, CliError> {
    match object.get(field) {
        None => Ok(None),
        Some(value) => value.as_array().map(Some).ok_or_else(|| {
            ax_contract_error(
                operation,
                format!(
                    "`{field}` must be an array when present (received {})",
                    json_type_name(value)
                ),
            )
        }),
    }
}

fn ensure_required_u64(
    operation: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<u64, CliError> {
    let value = object.get(field).ok_or_else(|| {
        ax_contract_error(operation, format!("missing required `{field}` number"))
    })?;
    value.as_u64().ok_or_else(|| {
        ax_contract_error(
            operation,
            format!(
                "`{field}` must be a non-negative integer (received {})",
                json_type_name(value)
            ),
        )
    })
}

fn ensure_required_non_empty_string(
    operation: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<(), CliError> {
    let value = object.get(field).ok_or_else(|| {
        ax_contract_error(operation, format!("missing required `{field}` string"))
    })?;
    let text = value.as_str().ok_or_else(|| {
        ax_contract_error(
            operation,
            format!(
                "`{field}` must be a string (received {})",
                json_type_name(value)
            ),
        )
    })?;
    if text.trim().is_empty() {
        return Err(ax_contract_error(
            operation,
            format!("`{field}` must be a non-empty string"),
        ));
    }
    Ok(())
}

fn ensure_optional_bool(
    operation: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<(), CliError> {
    if let Some(value) = object.get(field)
        && !value.is_boolean()
    {
        return Err(ax_contract_error(
            operation,
            format!(
                "`{field}` must be a boolean when present (received {})",
                json_type_name(value)
            ),
        ));
    }
    Ok(())
}

fn ensure_optional_string_or_null(
    operation: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<(), CliError> {
    if let Some(value) = object.get(field)
        && !value.is_null()
        && !value.is_string()
    {
        return Err(ax_contract_error(
            operation,
            format!(
                "`{field}` must be a string or null when present (received {})",
                json_type_name(value)
            ),
        ));
    }
    Ok(())
}

fn ensure_optional_i64(
    operation: &str,
    object: &Map<String, Value>,
    field: &str,
) -> Result<Option<i64>, CliError> {
    match object.get(field) {
        None => Ok(None),
        Some(value) => value.as_i64().map(Some).ok_or_else(|| {
            ax_contract_error(
                operation,
                format!(
                    "`{field}` must be an integer when present (received {})",
                    json_type_name(value)
                ),
            )
        }),
    }
}

fn validate_ax_list_contract(operation: &str, value: &Value) -> Result<(), CliError> {
    let object = expect_object(operation, value)?;
    let nodes = ensure_required_array(operation, object, "nodes")?;
    for (index, node) in nodes.iter().enumerate() {
        if !node.is_object() {
            return Err(ax_contract_error(
                operation,
                format!(
                    "`nodes[{index}]` must be an object (received {})",
                    json_type_name(node)
                ),
            ));
        }
    }

    if let Some(warnings) = ensure_optional_array(operation, object, "warnings")? {
        for (index, warning) in warnings.iter().enumerate() {
            if !warning.is_string() {
                return Err(ax_contract_error(
                    operation,
                    format!(
                        "`warnings[{index}]` must be a string (received {})",
                        json_type_name(warning)
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn validate_ax_click_contract(operation: &str, value: &Value) -> Result<(), CliError> {
    let object = expect_object(operation, value)?;
    ensure_required_u64(operation, object, "matched_count")?;
    ensure_required_non_empty_string(operation, object, "action")?;
    ensure_optional_bool(operation, object, "used_coordinate_fallback")?;
    ensure_optional_string_or_null(operation, object, "node_id")?;
    let fallback_x = ensure_optional_i64(operation, object, "fallback_x")?;
    let fallback_y = ensure_optional_i64(operation, object, "fallback_y")?;

    let used_coordinate_fallback = object
        .get("used_coordinate_fallback")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if fallback_x.is_some() ^ fallback_y.is_some() {
        return Err(ax_contract_error(
            operation,
            "`fallback_x` and `fallback_y` must either both be present or both be omitted",
        ));
    }

    if used_coordinate_fallback && (fallback_x.is_none() || fallback_y.is_none()) {
        return Err(ax_contract_error(
            operation,
            "`fallback_x` and `fallback_y` are required when `used_coordinate_fallback` is true",
        ));
    }

    Ok(())
}

fn validate_ax_type_contract(operation: &str, value: &Value) -> Result<(), CliError> {
    let object = expect_object(operation, value)?;
    ensure_required_u64(operation, object, "matched_count")?;
    ensure_required_u64(operation, object, "text_length")?;
    ensure_required_non_empty_string(operation, object, "applied_via")?;
    ensure_optional_bool(operation, object, "submitted")?;
    ensure_optional_bool(operation, object, "used_keyboard_fallback")?;
    ensure_optional_string_or_null(operation, object, "node_id")?;
    Ok(())
}

fn test_mode_override_json(operation: &str) -> Option<String> {
    if !test_mode::enabled() {
        return None;
    }

    let env_name = match operation {
        "ax.list" => AX_LIST_TEST_MODE_ENV,
        "ax.click" => AX_CLICK_TEST_MODE_ENV,
        "ax.type" => AX_TYPE_TEST_MODE_ENV,
        _ => return None,
    };

    std::env::var(env_name).ok().and_then(|raw| {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn test_mode_default_json(operation: &str) -> Option<&'static str> {
    match operation {
        "ax.list" => Some(r#"{"nodes":[],"warnings":[]}"#),
        "ax.click" => Some(
            r#"{"node_id":"test-node","matched_count":1,"action":"ax-press","used_coordinate_fallback":false}"#,
        ),
        "ax.type" => Some(
            r#"{"node_id":"test-node","matched_count":1,"applied_via":"ax-set-value","text_length":0,"submitted":false,"used_keyboard_fallback":false}"#,
        ),
        _ => None,
    }
}

fn output_preview(raw: &str, max_chars: usize) -> String {
    let mut preview = raw.chars().take(max_chars).collect::<String>();
    if raw.chars().count() > max_chars {
        preview.push_str("...");
    }
    preview
}

fn escape_applescript(raw: &str) -> String {
    raw.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use nils_test_support::{EnvGuard, GlobalStateLock};
    use pretty_assertions::assert_eq;

    use crate::backend::process::{ProcessFailure, ProcessOutput, ProcessRequest, ProcessRunner};
    use crate::model::{AxClickRequest, AxListRequest, AxSelector, AxTypeRequest};

    use super::{
        AX_CLICK_JXA_SCRIPT, AX_TYPE_JXA_SCRIPT, Modifier, ax_click, ax_list, ax_type,
        escape_applescript, parse_modifiers,
    };

    struct FixedOutputRunner {
        stdout: String,
    }

    impl FixedOutputRunner {
        fn new(stdout: impl Into<String>) -> Self {
            Self {
                stdout: stdout.into(),
            }
        }
    }

    impl ProcessRunner for FixedOutputRunner {
        fn run(&self, _request: &ProcessRequest) -> Result<ProcessOutput, ProcessFailure> {
            Ok(ProcessOutput {
                stdout: self.stdout.clone(),
                stderr: String::new(),
            })
        }
    }

    struct PanicRunner;

    impl ProcessRunner for PanicRunner {
        fn run(&self, _request: &ProcessRequest) -> Result<ProcessOutput, ProcessFailure> {
            panic!("runner should not be invoked");
        }
    }

    #[test]
    fn escapes_applescript_string_literals() {
        assert_eq!(escape_applescript("a\\\"b"), "a\\\\\\\"b".to_string());
    }

    #[test]
    fn parses_modifiers_deduped_and_canonicalized() {
        let mods = parse_modifiers("cmd,shift,command").expect("modifiers should parse");
        assert_eq!(mods, vec![Modifier::Cmd, Modifier::Shift]);
        let canonical = mods
            .iter()
            .map(|m| m.canonical().to_string())
            .collect::<Vec<_>>();
        assert_eq!(canonical, vec!["cmd".to_string(), "shift".to_string()]);
    }

    #[test]
    fn rejects_unknown_modifier() {
        let err = parse_modifiers("cmd,nope").expect_err("unknown modifiers should fail");
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("invalid modifier"));
    }

    #[test]
    fn ax_list_uses_test_mode_default_when_stdout_is_empty() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_TEST_MODE", "1");
        let _override = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_AX_LIST_JSON");
        let runner = FixedOutputRunner::new("");

        let result =
            ax_list(&runner, &AxListRequest::default(), 250).expect("ax list should parse");
        assert!(result.nodes.is_empty());
    }

    #[test]
    fn ax_click_parse_failure_includes_operation_context() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_TEST_MODE");
        let _override = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_AX_CLICK_JSON");
        let runner = FixedOutputRunner::new("not-json");
        let request = AxClickRequest {
            selector: AxSelector {
                node_id: Some("node-1".to_string()),
                ..AxSelector::default()
            },
            ..AxClickRequest::default()
        };

        let err = ax_click(&runner, &request, 250).expect_err("invalid json should fail");
        assert_eq!(err.operation(), Some("ax.click"));
        assert!(err.message().contains("invalid AX backend JSON response"));
        assert!(
            err.hints()
                .iter()
                .any(|hint| hint.contains("preflight") || hint.contains("--trace"))
        );
    }

    #[test]
    fn ax_click_script_declares_node_id_helper_once() {
        assert_eq!(
            AX_CLICK_JXA_SCRIPT
                .matches("function resolveByNodeId")
                .count(),
            1
        );
    }

    #[test]
    fn ax_type_script_declares_node_id_helper_once() {
        assert_eq!(
            AX_TYPE_JXA_SCRIPT
                .matches("function resolveByNodeId")
                .count(),
            1
        );
    }

    #[test]
    fn ax_list_contract_failure_reports_missing_nodes_array() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_TEST_MODE");
        let _override = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_AX_LIST_JSON");
        let runner = FixedOutputRunner::new(r#"{"warnings":[]}"#);

        let err = ax_list(&runner, &AxListRequest::default(), 250)
            .expect_err("missing nodes contract field should fail");
        assert_eq!(err.operation(), Some("ax.list"));
        assert!(err.message().contains("AX backend contract violation"));
        assert!(err.message().contains("nodes"));
        assert!(err.hints().iter().any(|hint| hint.contains("nodes")));
    }

    #[test]
    fn ax_click_contract_failure_requires_fallback_coordinate_pair() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_TEST_MODE");
        let _override = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_AX_CLICK_JSON");
        let runner = FixedOutputRunner::new(
            r#"{"node_id":"1.1","matched_count":1,"action":"ax-press-fallback","used_coordinate_fallback":true,"fallback_x":42}"#,
        );
        let request = AxClickRequest {
            selector: AxSelector {
                node_id: Some("1.1".to_string()),
                ..AxSelector::default()
            },
            ..AxClickRequest::default()
        };

        let err = ax_click(&runner, &request, 250)
            .expect_err("fallback coordinates without pair should fail");
        assert_eq!(err.operation(), Some("ax.click"));
        assert!(err.message().contains("AX backend contract violation"));
        assert!(err.message().contains("fallback_x"));
        assert!(err.message().contains("fallback_y"));
    }

    #[test]
    fn ax_type_contract_failure_requires_text_length() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_TEST_MODE");
        let _override = EnvGuard::remove(&lock, "AGENTS_MACOS_AGENT_AX_TYPE_JSON");
        let runner = FixedOutputRunner::new(
            r#"{"node_id":"1.2","matched_count":1,"applied_via":"ax-set-value"}"#,
        );
        let request = AxTypeRequest {
            selector: AxSelector {
                node_id: Some("1.2".to_string()),
                ..AxSelector::default()
            },
            text: "hello".to_string(),
            ..AxTypeRequest::default()
        };

        let err = ax_type(&runner, &request, 250).expect_err("missing text_length should fail");
        assert_eq!(err.operation(), Some("ax.type"));
        assert!(err.message().contains("AX backend contract violation"));
        assert!(err.message().contains("text_length"));
    }

    #[test]
    fn ax_type_uses_test_mode_override_without_invoking_runner() {
        let lock = GlobalStateLock::new();
        let _mode = EnvGuard::set(&lock, "AGENTS_MACOS_AGENT_TEST_MODE", "1");
        let _override = EnvGuard::set(
            &lock,
            "AGENTS_MACOS_AGENT_AX_TYPE_JSON",
            r#"{"node_id":"node-9","matched_count":1,"applied_via":"ax-set-value","text_length":5,"submitted":true,"used_keyboard_fallback":false}"#,
        );
        let request = AxTypeRequest {
            selector: AxSelector {
                node_id: Some("node-1".to_string()),
                ..AxSelector::default()
            },
            text: "hello".to_string(),
            ..AxTypeRequest::default()
        };

        let result = ax_type(&PanicRunner, &request, 250).expect("override json should parse");
        assert_eq!(result.node_id, Some("node-9".to_string()));
        assert_eq!(result.text_length, 5);
        assert!(result.submitted);
    }
}
