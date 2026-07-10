use crate::backend::applescript::Modifier;
use crate::backend::process::{ProcessFailure, ProcessRequest, ProcessRunner, map_failure};
use crate::cli::MouseButton;
use crate::error::CliError;

pub fn click(
    runner: &dyn ProcessRunner,
    x: i32,
    y: i32,
    button: MouseButton,
    count: u8,
    mods: &[Modifier],
    timeout_ms: u64,
) -> Result<(), CliError> {
    if count == 0 {
        return Err(CliError::usage("--count must be at least 1"));
    }

    let action = match button {
        MouseButton::Left => "c",
        MouseButton::Right => "rc",
        MouseButton::Middle => "mc",
    };

    let mut args = Vec::with_capacity(count as usize);
    for _ in 0..count {
        args.push(format!(
            "{action}:{},{}",
            absolute_coordinate(x),
            absolute_coordinate(y)
        ));
    }

    let (args, cleanup) = with_modifiers(args, mods, false);
    run_cliclick_with_cleanup(runner, "input.click", args, cleanup, timeout_ms)
}

pub fn move_pointer(
    runner: &dyn ProcessRunner,
    x: i32,
    y: i32,
    timeout_ms: u64,
) -> Result<(), CliError> {
    run_cliclick(
        runner,
        "input.move",
        vec![format!(
            "m:{},{}",
            absolute_coordinate(x),
            absolute_coordinate(y)
        )],
        timeout_ms,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn drag(
    runner: &dyn ProcessRunner,
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
    duration_ms: u64,
    steps: u16,
    mods: &[Modifier],
    timeout_ms: u64,
) -> Result<(), CliError> {
    if steps == 0 || steps > 100 {
        return Err(CliError::usage("--steps must be between 1 and 100"));
    }

    let mut args = Vec::with_capacity((steps as usize * 2) + 2);
    args.push(format!(
        "dd:{},{}",
        absolute_coordinate(from_x),
        absolute_coordinate(from_y)
    ));
    let wait_ms = duration_ms / u64::from(steps);
    for step in 1..=steps {
        let ratio = f64::from(step) / f64::from(steps);
        let x = f64::from(from_x) + (f64::from(to_x) - f64::from(from_x)) * ratio;
        let y = f64::from(from_y) + (f64::from(to_y) - f64::from(from_y)) * ratio;
        args.push(format!(
            "dm:{},{}",
            absolute_coordinate(x.round() as i32),
            absolute_coordinate(y.round() as i32)
        ));
        if wait_ms > 0 {
            args.push(format!("w:{wait_ms}"));
        }
    }
    args.push(format!(
        "du:{},{}",
        absolute_coordinate(to_x),
        absolute_coordinate(to_y)
    ));
    let (args, cleanup) = with_modifiers(args, mods, true);
    run_cliclick_with_cleanup(runner, "input.drag", args, cleanup, timeout_ms)
}

fn run_cliclick(
    runner: &dyn ProcessRunner,
    operation: &'static str,
    args: Vec<String>,
    timeout_ms: u64,
) -> Result<(), CliError> {
    run_cliclick_with_cleanup(runner, operation, args, Vec::new(), timeout_ms)
}

fn run_cliclick_with_cleanup(
    runner: &dyn ProcessRunner,
    operation: &'static str,
    args: Vec<String>,
    cleanup_args: Vec<String>,
    timeout_ms: u64,
) -> Result<(), CliError> {
    let request = ProcessRequest::new("cliclick", args, timeout_ms.max(1));
    match runner.run(&request) {
        Ok(_) => Ok(()),
        Err(failure) => {
            let cleanup_failed =
                if cleanup_args.is_empty() || matches!(failure, ProcessFailure::NotFound { .. }) {
                    false
                } else {
                    let cleanup =
                        ProcessRequest::new("cliclick", cleanup_args, timeout_ms.clamp(250, 1_000));
                    runner.run(&cleanup).is_err()
                };
            let mut error = map_failure(operation, failure);
            if cleanup_failed {
                error = error.with_hint(
                    "The best-effort mouse/modifier release cleanup could not be confirmed; release held input before continuing automation.",
                );
            }
            Err(error)
        }
    }
}

fn with_modifiers(
    mut args: Vec<String>,
    mods: &[Modifier],
    release_mouse_on_failure: bool,
) -> (Vec<String>, Vec<String>) {
    let modifier_token = mods
        .iter()
        .map(|modifier| modifier.canonical())
        .collect::<Vec<_>>()
        .join(",");
    let mut cleanup = Vec::new();
    if release_mouse_on_failure {
        cleanup.push("du:.".to_string());
    }
    if !modifier_token.is_empty() {
        args.insert(0, format!("kd:{modifier_token}"));
        args.push(format!("ku:{modifier_token}"));
        cleanup.push(format!("ku:{modifier_token}"));
    }
    (args, cleanup)
}

fn absolute_coordinate(value: i32) -> String {
    if value < 0 {
        format!("={value}")
    } else {
        value.to_string()
    }
}

pub fn button_name(button: MouseButton) -> &'static str {
    match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    }
}

#[cfg(test)]
mod tests {
    use super::{absolute_coordinate, button_name};
    use crate::cli::MouseButton;

    #[test]
    fn maps_button_name() {
        assert_eq!(button_name(MouseButton::Left), "left");
        assert_eq!(button_name(MouseButton::Right), "right");
        assert_eq!(button_name(MouseButton::Middle), "middle");
    }

    #[test]
    fn formats_negative_coordinates_as_absolute() {
        assert_eq!(absolute_coordinate(-20), "=-20");
        assert_eq!(absolute_coordinate(0), "0");
        assert_eq!(absolute_coordinate(20), "20");
    }
}
