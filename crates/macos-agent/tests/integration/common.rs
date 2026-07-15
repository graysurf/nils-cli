#![allow(dead_code)]

use std::path::{Path, PathBuf};

use nils_test_support::StubBinDir;
use nils_test_support::bin::resolve;
use nils_test_support::cmd::{CmdOptions, CmdOutput, run_with};
use tempfile::TempDir;

pub struct MacosAgentHarness {
    home_dir: TempDir,
    agent_home: PathBuf,
    stub_dir: StubBinDir,
}

impl MacosAgentHarness {
    pub fn new() -> Self {
        let home_dir = TempDir::new().expect("tempdir");
        let agent_home = home_dir.path().join(".agents");
        std::fs::create_dir_all(agent_home.join("out")).expect("create AGENT_HOME/out");

        let stub_dir = StubBinDir::new();
        stub_dir.write_exe("lipo", "#!/bin/sh\necho 'arm64 x86_64'\n");

        Self {
            home_dir,
            agent_home,
            stub_dir,
        }
    }

    pub fn macos_agent_bin(&self) -> PathBuf {
        resolve("macos-agent")
    }

    pub fn home_dir(&self) -> &Path {
        self.home_dir.path()
    }

    pub fn cmd_options(&self, cwd: &Path) -> CmdOptions {
        let home = self.home_dir.path().to_string_lossy().to_string();
        let agent_home = self.agent_home.to_string_lossy().to_string();
        CmdOptions::new()
            .with_cwd(cwd)
            .with_env("HOME", &home)
            .with_env("AGENT_HOME", &agent_home)
            .with_env("AGENTS_MACOS_AGENT_TEST_MODE", "1")
            .with_env("AGENTS_MACOS_AGENT_TEST_TIMESTAMP", "20260101-000000")
            .with_path_prepend(self.stub_dir.path())
    }

    pub fn run(&self, cwd: &Path, args: &[&str]) -> CmdOutput {
        run_with(&self.macos_agent_bin(), args, &self.cmd_options(cwd))
    }

    pub fn run_with_options(&self, cwd: &Path, args: &[&str], options: CmdOptions) -> CmdOutput {
        run_with(&self.macos_agent_bin(), args, &options.with_cwd(cwd))
    }
}

impl Default for MacosAgentHarness {
    fn default() -> Self {
        Self::new()
    }
}
