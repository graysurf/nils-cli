"use strict";

const REQUIRED_CHECKS = Object.freeze(["test", "test_macos", "coverage"]);
const FULL_VALIDATION_MARKER = "Full validation marker";

function canonicalReleaseBranch(ref) {
  const match = /^refs\/tags\/v(\d+)\.(\d+)\.(\d+)$/.exec(ref || "");
  return match ? `chore/release-${match[1]}-${match[2]}-${match[3]}` : null;
}

function timestamp(run) {
  return Date.parse(
    run.completed_at || run.started_at || run.check_suite?.created_at || "",
  ) || 0;
}

function classifyLatestChecks(runs, requiredChecks = REQUIRED_CHECKS) {
  const latest = new Map();
  for (const run of runs) {
    if (!requiredChecks.includes(run.name)) {
      continue;
    }
    const previous = latest.get(run.name);
    if (!previous || timestamp(run) > timestamp(previous)) {
      latest.set(run.name, run);
    }
  }

  const pending = [];
  const failing = [];
  for (const name of requiredChecks) {
    const run = latest.get(name);
    if (!run) {
      pending.push(`${name}: missing`);
    } else if (run.status !== "completed") {
      pending.push(`${name}: ${run.status}`);
    } else if (run.conclusion !== "success") {
      failing.push(`${name}: ${run.conclusion} (${run.html_url})`);
    }
  }

  return {
    state: failing.length > 0 ? "failure" : pending.length > 0 ? "pending" : "success",
    pending,
    failing,
  };
}

async function listAll(github, method, params, responseKey) {
  if (typeof github.paginate === "function") {
    return github.paginate(method, params);
  }
  const response = await method(params);
  return responseKey ? response.data[responseKey] : response.data;
}

function exactSuccessfulJob(jobs, name, { requireFullMarker = false } = {}) {
  const matches = jobs.filter((job) => job.name === name);
  if (
    matches.length !== 1 ||
    matches[0].status !== "completed" ||
    matches[0].conclusion !== "success"
  ) {
    return false;
  }
  if (!requireFullMarker) {
    return true;
  }
  const markers = (matches[0].steps || []).filter(
    (step) => step.name === FULL_VALIDATION_MARKER,
  );
  return (
    markers.length === 1 &&
    markers[0].status === "completed" &&
    markers[0].conclusion === "success"
  );
}

async function hasSuccessfulRequiredJobs({
  github,
  context,
  runId,
  requiredChecks = REQUIRED_CHECKS,
  requireFullMarker = false,
}) {
  const jobs = await listAll(
    github,
    github.rest.actions.listJobsForWorkflowRun,
    {
      owner: context.repo.owner,
      repo: context.repo.repo,
      run_id: runId,
      filter: "latest",
      per_page: 100,
    },
    "jobs",
  );
  return requiredChecks.every((name) =>
    exactSuccessfulJob(jobs, name, { requireFullMarker }),
  );
}

async function findTrustedPullRequestCi({
  github,
  context,
  sha,
  requiredChecks = REQUIRED_CHECKS,
}) {
  const branch = canonicalReleaseBranch(context.ref);
  if (!branch) {
    return null;
  }

  const fullName = `${context.repo.owner}/${context.repo.repo}`;
  const pulls = await listAll(
    github,
    github.rest.pulls.list,
    {
      owner: context.repo.owner,
      repo: context.repo.repo,
      state: "closed",
      base: "main",
      head: `${context.repo.owner}:${branch}`,
      per_page: 100,
    },
  );
  const trustedPulls = pulls.filter(
    (pull) =>
      pull.merged_at &&
      pull.merge_commit_sha === sha &&
      pull.head?.ref === branch &&
      pull.head?.sha === sha &&
      pull.head?.repo?.full_name === fullName &&
      pull.base?.ref === "main" &&
      pull.base?.repo?.full_name === fullName,
  );
  if (trustedPulls.length !== 1) {
    return null;
  }

  const runs = await listAll(
    github,
    github.rest.actions.listWorkflowRuns,
    {
      owner: context.repo.owner,
      repo: context.repo.repo,
      workflow_id: "ci.yml",
      event: "pull_request",
      head_sha: sha,
      per_page: 100,
    },
    "workflow_runs",
  );
  const trustedRuns = runs.filter(
    (run) =>
      run.event === "pull_request" &&
      run.status === "completed" &&
      run.conclusion === "success" &&
      run.head_branch === branch &&
      run.head_sha === sha &&
      run.repository?.full_name === fullName,
  );
  if (trustedRuns.length !== 1) {
    return null;
  }

  const run = trustedRuns[0];
  if (
    !(await hasSuccessfulRequiredJobs({
      github,
      context,
      runId: run.id,
      requiredChecks,
    }))
  ) {
    return null;
  }

  return {
    prNumber: trustedPulls[0].number,
    runId: run.id,
    runUrl: run.html_url,
  };
}

async function findTrustedMainCi({
  github,
  context,
  sha,
  requiredChecks = REQUIRED_CHECKS,
}) {
  if (!/^[0-9a-f]{40}$/i.test(sha || "")) {
    return null;
  }
  const fullName = `${context.repo.owner}/${context.repo.repo}`;
  const runs = await listAll(
    github,
    github.rest.actions.listWorkflowRuns,
    {
      owner: context.repo.owner,
      repo: context.repo.repo,
      workflow_id: "ci.yml",
      branch: "main",
      event: "push",
      head_sha: sha,
      per_page: 100,
    },
    "workflow_runs",
  );
  const trustedRuns = runs.filter(
    (run) =>
      run.event === "push" &&
      run.status === "completed" &&
      run.conclusion === "success" &&
      run.head_branch === "main" &&
      run.head_sha === sha &&
      run.repository?.full_name === fullName,
  );
  if (trustedRuns.length !== 1) {
    return null;
  }

  const run = trustedRuns[0];
  if (
    !(await hasSuccessfulRequiredJobs({
      github,
      context,
      runId: run.id,
      requiredChecks,
      requireFullMarker: true,
    }))
  ) {
    return null;
  }

  return { runId: run.id, runUrl: run.html_url };
}

async function runReleaseGate({
  github,
  context,
  core,
  attempts = 60,
  intervalMs = 30_000,
  sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
}) {
  try {
    const trusted = await findTrustedPullRequestCi({
      github,
      context,
      sha: context.sha,
    });
    if (trusted) {
      core.info(
        `Tagged commit ${context.sha} reuses trusted CI run ${trusted.runId} from merged PR #${trusted.prNumber}: ${trusted.runUrl}`,
      );
      return true;
    }
    core.info("No unique trusted release PR CI run found; falling back to exact-SHA check runs.");
  } catch (error) {
    core.warning(
      `Could not verify release PR CI provenance; falling back to exact-SHA check runs: ${error.message}`,
    );
  }

  for (let attempt = 1; attempt <= attempts; attempt += 1) {
    const runs = await listAll(
      github,
      github.rest.checks.listForRef,
      {
        owner: context.repo.owner,
        repo: context.repo.repo,
        ref: context.sha,
        per_page: 100,
      },
      "check_runs",
    );
    const result = classifyLatestChecks(runs);

    if (result.state === "success") {
      core.info(
        `Tagged commit ${context.sha} has green CI checks: ${REQUIRED_CHECKS.join(", ")}`,
      );
      return true;
    }
    if (result.state === "failure") {
      core.setFailed(
        `Tagged commit ${context.sha} has failed CI checks: ${result.failing.join("; ")}`,
      );
      return false;
    }
    if (attempt === attempts) {
      core.setFailed(
        `Timed out waiting for CI checks on ${context.sha}: ${result.pending.join("; ")}`,
      );
      return false;
    }

    core.info(
      `Waiting for CI checks on ${context.sha} (${attempt}/${attempts}): ${result.pending.join("; ")}`,
    );
    await sleep(intervalMs);
  }
  return false;
}

module.exports = {
  FULL_VALIDATION_MARKER,
  REQUIRED_CHECKS,
  canonicalReleaseBranch,
  classifyLatestChecks,
  findTrustedMainCi,
  findTrustedPullRequestCi,
  runReleaseGate,
};
