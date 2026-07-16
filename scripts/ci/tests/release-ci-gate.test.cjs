#!/usr/bin/env node

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const test = require("node:test");

const repoRoot = path.resolve(__dirname, "../../..");
const modulePath = path.join(repoRoot, ".github/scripts/release-ci-gate.cjs");

test("the checked-in release CI gate module exists", () => {
  assert.equal(
    fs.existsSync(modulePath),
    true,
    ".github/scripts/release-ci-gate.cjs must own the release provenance checks",
  );
});

if (fs.existsSync(modulePath)) {
  const {
    REQUIRED_CHECKS,
    canonicalReleaseBranch,
    classifyLatestChecks,
    findTrustedMainCi,
    findTrustedPullRequestCi,
    runReleaseGate,
  } = require(modulePath);

  const owner = "sympoies";
  const repo = "nils-cli";
  const fullName = `${owner}/${repo}`;
  const releaseSha = "a".repeat(40);
  const baseSha = "b".repeat(40);
  const branch = "chore/release-1-22-10";
  const context = {
    ref: "refs/tags/v1.22.10",
    repo: { owner, repo },
  };

  function successfulJobs({ withFullMarkers = false } = {}) {
    return REQUIRED_CHECKS.map((name, index) => ({
      id: index + 1,
      name,
      status: "completed",
      conclusion: "success",
      steps: withFullMarkers
        ? [{ name: "Full validation marker", status: "completed", conclusion: "success" }]
        : [],
    }));
  }

  function fixture(overrides = {}) {
    const pull = {
      number: 1253,
      merged_at: "2026-07-16T12:00:00Z",
      merge_commit_sha: releaseSha,
      html_url: `https://github.com/${fullName}/pull/1253`,
      head: {
        ref: branch,
        sha: releaseSha,
        repo: { full_name: fullName },
      },
      base: {
        ref: "main",
        repo: { full_name: fullName },
      },
    };
    const pullRun = {
      id: 100,
      event: "pull_request",
      status: "completed",
      conclusion: "success",
      head_branch: branch,
      head_sha: releaseSha,
      html_url: `https://github.com/${fullName}/actions/runs/100`,
      repository: { full_name: fullName },
      head_repository: { full_name: fullName },
      pull_requests: [],
    };
    const mainRun = {
      id: 200,
      event: "push",
      status: "completed",
      conclusion: "success",
      head_branch: "main",
      head_sha: baseSha,
      html_url: `https://github.com/${fullName}/actions/runs/200`,
      repository: { full_name: fullName },
    };
    const state = {
      pulls: [pull],
      workflowRuns: [pullRun, mainRun],
      jobsByRun: {
        100: successfulJobs(),
        200: successfulJobs({ withFullMarkers: true }),
      },
      checkRuns: REQUIRED_CHECKS.map((name) => ({
        name,
        status: "completed",
        conclusion: "success",
        completed_at: "2026-07-16T12:00:00Z",
      })),
      ...overrides,
    };
    return {
      state,
      github: {
        rest: {
          pulls: {
            list: async () => ({ data: state.pulls }),
          },
          actions: {
            listWorkflowRuns: async ({ event, head_sha: headSha }) => ({
              data: {
                workflow_runs: state.workflowRuns.filter(
                  (run) => run.event === event && run.head_sha === headSha,
                ),
              },
            }),
            listJobsForWorkflowRun: async ({ run_id: runId }) => ({
              data: { jobs: state.jobsByRun[runId] || [] },
            }),
          },
          checks: {
            listForRef: async () => ({ data: { check_runs: state.checkRuns } }),
          },
        },
      },
    };
  }

  test("canonicalReleaseBranch accepts only stable v-prefixed release tags", () => {
    assert.equal(canonicalReleaseBranch("refs/tags/v1.22.10"), branch);
    assert.equal(canonicalReleaseBranch("refs/tags/v1.22.10-rc.1"), null);
    assert.equal(canonicalReleaseBranch("refs/heads/main"), null);
  });

  test("a unique same-repository merged release PR with exact-SHA green jobs is trusted", async () => {
    const { github } = fixture();
    const trusted = await findTrustedPullRequestCi({ github, context, sha: releaseSha });

    assert.deepEqual(trusted, {
      prNumber: 1253,
      runId: 100,
      runUrl: `https://github.com/${fullName}/actions/runs/100`,
    });
  });

  test("ambiguous workflow runs fail closed", async () => {
    const { state, github } = fixture();
    state.workflowRuns.push({ ...state.workflowRuns[0], id: 101 });
    state.jobsByRun[101] = successfulJobs();

    assert.equal(await findTrustedPullRequestCi({ github, context, sha: releaseSha }), null);
  });

  test("forked, unmerged, wrong-SHA, and incomplete PR evidence fail closed", async (t) => {
    const mutations = {
      forked: (state) => {
        state.pulls[0].head.repo.full_name = "someone/nils-cli";
      },
      unmerged: (state) => {
        state.pulls[0].merged_at = null;
      },
      "wrong SHA": (state) => {
        state.pulls[0].merge_commit_sha = "c".repeat(40);
      },
      "missing required job": (state) => {
        state.jobsByRun[100] = state.jobsByRun[100].filter(({ name }) => name !== "coverage");
      },
      "failed required job": (state) => {
        state.jobsByRun[100][0].conclusion = "failure";
      },
      "fork workflow run": (state) => {
        state.workflowRuns[0].head_repository.full_name = "someone/nils-cli";
      },
      "mismatched associated PR": (state) => {
        state.workflowRuns[0].pull_requests = [
          {
            number: 9999,
            head: { sha: releaseSha },
            base: { ref: "main" },
          },
        ];
      },
    };

    for (const [name, mutate] of Object.entries(mutations)) {
      await t.test(name, async () => {
        const { state, github } = fixture();
        mutate(state);
        assert.equal(await findTrustedPullRequestCi({ github, context, sha: releaseSha }), null);
      });
    }
  });

  test("base main CI is trusted only when every full-validation marker succeeded", async () => {
    const { github } = fixture();
    assert.deepEqual(await findTrustedMainCi({ github, context, sha: baseSha }), {
      runId: 200,
      runUrl: `https://github.com/${fullName}/actions/runs/200`,
    });

    const missingMarker = fixture();
    missingMarker.state.jobsByRun[200][1].steps = [];
    assert.equal(
      await findTrustedMainCi({ github: missingMarker.github, context, sha: baseSha }),
      null,
    );
  });

  test("release gate reuses trusted PR CI without polling duplicate checks", async () => {
    const { github } = fixture();
    github.rest.checks.listForRef = async () => {
      assert.fail("trusted PR CI should avoid exact-SHA check polling");
    };
    const messages = [];
    const core = {
      info: (message) => messages.push(message),
      warning: (message) => messages.push(message),
      setFailed: assert.fail,
    };

    assert.equal(await runReleaseGate({ github, context: { ...context, sha: releaseSha }, core }), true);
    assert.match(messages.join("\n"), /reuses trusted CI run 100/);
  });

  test("release gate falls back to exact-SHA checks when provenance lookup errors", async () => {
    const { github } = fixture();
    github.rest.pulls.list = async () => {
      throw new Error("temporary API failure");
    };
    const messages = [];
    const core = {
      info: (message) => messages.push(message),
      warning: (message) => messages.push(message),
      setFailed: assert.fail,
    };

    assert.equal(
      await runReleaseGate({
        github,
        context: { ...context, sha: releaseSha },
        core,
        attempts: 1,
      }),
      true,
    );
    assert.match(messages.join("\n"), /falling back to exact-SHA check runs/);
  });

  test("latest exact-SHA check runs retain success, pending, and failure fallback states", () => {
    const successRuns = REQUIRED_CHECKS.map((name, index) => ({
      name,
      status: "completed",
      conclusion: "success",
      completed_at: `2026-07-16T12:0${index}:00Z`,
    }));
    assert.deepEqual(classifyLatestChecks(successRuns), {
      state: "success",
      pending: [],
      failing: [],
    });

    const pendingRuns = [
      ...successRuns,
      {
        name: "coverage",
        status: "in_progress",
        conclusion: null,
        started_at: "2026-07-16T13:00:00Z",
      },
    ];
    assert.equal(classifyLatestChecks(pendingRuns).state, "pending");

    const failedRuns = successRuns.map((run) => ({ ...run }));
    failedRuns[0].conclusion = "failure";
    failedRuns[0].html_url = "https://example.test/failure";
    assert.deepEqual(classifyLatestChecks(failedRuns), {
      state: "failure",
      pending: [],
      failing: ["test: failure (https://example.test/failure)"],
    });
  });
}
