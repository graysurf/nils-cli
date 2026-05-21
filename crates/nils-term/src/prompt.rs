use std::fmt;
use std::io::{self, BufRead, IsTerminal, Write};

#[derive(Debug, Clone, Copy)]
pub struct PromptOptions {
    assume_yes: bool,
    require_tty: bool,
}

impl Default for PromptOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptOptions {
    pub fn new() -> Self {
        Self {
            assume_yes: false,
            require_tty: true,
        }
    }

    pub fn with_assume_yes(mut self, assume_yes: bool) -> Self {
        self.assume_yes = assume_yes;
        self
    }

    pub fn with_require_tty(mut self, require_tty: bool) -> Self {
        self.require_tty = require_tty;
        self
    }
}

#[derive(Debug)]
pub enum PromptError {
    NonInteractive,
    Io(io::Error),
}

impl fmt::Display for PromptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonInteractive => write!(f, "confirmation requires an interactive terminal"),
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for PromptError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NonInteractive => None,
            Self::Io(err) => Some(err),
        }
    }
}

impl From<io::Error> for PromptError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

pub fn confirm(
    question: &str,
    default_no: bool,
    options: PromptOptions,
) -> Result<bool, PromptError> {
    if options.assume_yes {
        return Ok(true);
    }
    if options.require_tty && (!io::stdin().is_terminal() || !io::stderr().is_terminal()) {
        return Err(PromptError::NonInteractive);
    }

    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stderr = io::stderr();
    let mut output = stderr.lock();
    confirm_with_io(question, default_no, &mut input, &mut output)
}

pub fn confirm_with_io<R: BufRead, W: Write>(
    question: &str,
    default_no: bool,
    input: &mut R,
    output: &mut W,
) -> Result<bool, PromptError> {
    write!(output, "{question}")?;
    output.flush()?;

    let mut line = String::new();
    input.read_line(&mut line)?;
    let normalized = line.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "y" | "yes") {
        return Ok(true);
    }
    if normalized.is_empty() {
        return Ok(!default_no);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{PromptOptions, confirm_with_io};
    use pretty_assertions::assert_eq;
    use std::io::Cursor;

    #[test]
    fn confirm_with_io_accepts_y_and_yes() {
        for input in ["y\n", "Y\n", " yes \n"] {
            let mut input = Cursor::new(input);
            let mut output = Vec::new();
            let confirmed =
                confirm_with_io("Proceed? [y/N] ", true, &mut input, &mut output).expect("prompt");
            assert!(confirmed);
            assert_eq!(String::from_utf8_lossy(&output), "Proceed? [y/N] ");
        }
    }

    #[test]
    fn confirm_with_io_rejects_default_no_empty_input() {
        let mut input = Cursor::new("\n");
        let mut output = Vec::new();
        let confirmed =
            confirm_with_io("Proceed? [y/N] ", true, &mut input, &mut output).expect("prompt");
        assert!(!confirmed);
    }

    #[test]
    fn confirm_with_io_accepts_empty_input_when_default_is_yes() {
        let mut input = Cursor::new("\n");
        let mut output = Vec::new();
        let confirmed =
            confirm_with_io("Proceed? [Y/n] ", false, &mut input, &mut output).expect("prompt");
        assert!(confirmed);
    }

    #[test]
    fn prompt_options_default_requires_tty() {
        let options = PromptOptions::new();
        assert!(options.require_tty);
        assert!(!options.assume_yes);
    }
}
