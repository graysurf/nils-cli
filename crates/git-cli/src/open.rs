use nils_common::git::strip_userinfo;
use nils_common::process;
use std::io::{self, Write};
use std::process::Output;

pub fn dispatch(cmd: &str, args: &[String]) -> Option<i32> {
    match cmd {
        "repo" => Some(open_repo(args)),
        "branch" => Some(open_branch(args)),
        "default" | "default-branch" => Some(open_default_branch(args)),
        "commit" => Some(open_commit(args)),
        "compare" => Some(open_compare(args)),
        "pr" | "pull-request" | "mr" | "merge-request" => Some(open_pr(args)),
        "pulls" | "prs" | "merge-requests" | "mrs" => Some(open_pulls(args)),
        "issue" | "issues" => Some(open_issues(args)),
        "action" | "actions" => Some(open_actions(args)),
        "release" | "releases" => Some(open_releases(args)),
        "tag" | "tags" => Some(open_tags(args)),
        "commits" | "history" => Some(open_commits(args)),
        "file" | "blob" => Some(open_file(args)),
        "blame" => Some(open_blame(args)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Provider {
    Github,
    Gitlab,
    Generic,
}

impl Provider {
    fn from_base_url(base_url: &str) -> Self {
        let host = host_from_url(base_url);
        match host.as_str() {
            "github.com" => Self::Github,
            "gitlab.com" => Self::Gitlab,
            _ => {
                if host.contains("gitlab") {
                    Self::Gitlab
                } else if host.contains("github") {
                    Self::Github
                } else {
                    Self::Generic
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
struct OpenContext {
    base_url: String,
    remote: String,
    remote_branch: String,
    provider: Provider,
}

#[derive(Debug, Clone)]
struct CollabContext {
    base_url: String,
    remote: String,
    provider: Provider,
}

fn open_repo(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open repo takes at most one remote name");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let base_url = if let Some(remote) = args.first() {
        match normalize_remote_url(remote) {
            Ok(url) => url,
            Err(code) => return code,
        }
    } else {
        match resolve_context() {
            Ok(ctx) => ctx.base_url,
            Err(code) => return code,
        }
    };

    open_url(&base_url, "🌐 Opened")
}

fn open_branch(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open branch takes at most one ref");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };

    let reference = args
        .first()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| ctx.remote_branch.clone());
    let url = tree_url(ctx.provider, &ctx.base_url, &reference);
    open_url(&url, "🌿 Opened")
}

fn open_default_branch(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open default-branch takes at most one remote name");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let (base_url, remote, provider) = if let Some(remote) = args.first() {
        let base_url = match normalize_remote_url(remote) {
            Ok(url) => url,
            Err(code) => return code,
        };
        let provider = Provider::from_base_url(&base_url);
        (base_url, remote.to_string(), provider)
    } else {
        let ctx = match resolve_context() {
            Ok(ctx) => ctx,
            Err(code) => return code,
        };
        (ctx.base_url, ctx.remote, ctx.provider)
    };

    let default_branch = match default_branch_name(&remote) {
        Ok(branch) => branch,
        Err(code) => return code,
    };
    let url = tree_url(provider, &base_url, &default_branch);
    open_url(&url, "🌿 Opened")
}

fn open_commit(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open commit takes at most one ref");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let reference = args.first().map(String::as_str).unwrap_or("HEAD");
    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };

    let commit_ref = format!("{reference}^{{commit}}");
    let commit = match git_stdout_trimmed(&["rev-parse", &commit_ref]) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("❌ Invalid commit/tag/branch: {reference}");
            return 1;
        }
    };

    let url = commit_url(ctx.provider, &ctx.base_url, &commit);
    open_url(&url, "🔗 Opened")
}

fn open_compare(args: &[String]) -> i32 {
    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    if args.len() > 2 {
        eprintln!("❌ git-cli open compare takes at most two refs");
        print_usage();
        return 2;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };

    let (base, head) = match args.len() {
        0 => {
            let base = match default_branch_name(&ctx.remote) {
                Ok(value) => value,
                Err(code) => return code,
            };
            (base, ctx.remote_branch)
        }
        1 => (args[0].to_string(), ctx.remote_branch),
        _ => (args[0].to_string(), args[1].to_string()),
    };

    let url = compare_url(ctx.provider, &ctx.base_url, &base, &head);
    open_url(&url, "🔀 Opened")
}

fn open_pr(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open pr takes at most one number");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };
    let collab = match resolve_collab_context(&ctx) {
        Ok(value) => value,
        Err(code) => return code,
    };

    if let Some(raw_number) = args.first() {
        let pr_number = match parse_positive_number(raw_number, "PR") {
            Ok(value) => value,
            Err(code) => return code,
        };

        let url = match collab.provider {
            Provider::Github => format!("{}/pull/{pr_number}", collab.base_url),
            Provider::Gitlab => format!("{}/-/merge_requests/{pr_number}", collab.base_url),
            Provider::Generic => {
                eprintln!("❗ pr <number> is only supported for GitHub/GitLab remotes.");
                return 1;
            }
        };
        return open_url(&url, "🧷 Opened");
    }

    if collab.provider == Provider::Github
        && process::cmd_exists("gh")
        && try_open_pr_with_gh(&ctx, &collab)
    {
        return 0;
    }

    match collab.provider {
        Provider::Github => {
            let base = match default_branch_name(&collab.remote) {
                Ok(value) => value,
                Err(code) => return code,
            };

            let mut head_ref = ctx.remote_branch.clone();
            if collab.base_url != ctx.base_url
                && let Some(slug) = github_repo_slug(&ctx.base_url)
                && let Some((owner, _)) = slug.split_once('/')
            {
                head_ref = format!("{owner}:{}", ctx.remote_branch);
            }

            let url = format!(
                "{}/compare/{}...{}?expand=1",
                collab.base_url, base, head_ref
            );
            open_url(&url, "🧷 Opened")
        }
        Provider::Gitlab => {
            let base = match default_branch_name(&collab.remote) {
                Ok(value) => value,
                Err(code) => return code,
            };
            let source_enc = percent_encode(&ctx.remote_branch, false);
            let target_enc = percent_encode(&base, false);
            let url = format!(
                "{}/-/merge_requests/new?merge_request[source_branch]={source_enc}&merge_request[target_branch]={target_enc}",
                collab.base_url
            );
            open_url(&url, "🧷 Opened")
        }
        Provider::Generic => {
            let base = match default_branch_name(&collab.remote) {
                Ok(value) => value,
                Err(code) => return code,
            };
            let url = format!(
                "{}/compare/{}...{}",
                collab.base_url, base, ctx.remote_branch
            );
            open_url(&url, "🧷 Opened")
        }
    }
}

fn open_pulls(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open pulls takes at most one number");
        print_usage();
        return 2;
    }

    if let Some(value) = args.first() {
        return open_pr(&[value.to_string()]);
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };
    let collab = match resolve_collab_context(&ctx) {
        Ok(value) => value,
        Err(code) => return code,
    };

    let url = match collab.provider {
        Provider::Gitlab => format!("{}/-/merge_requests", collab.base_url),
        _ => format!("{}/pulls", collab.base_url),
    };
    open_url(&url, "📌 Opened")
}

fn open_issues(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open issues takes at most one number");
        print_usage();
        return 2;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };
    let collab = match resolve_collab_context(&ctx) {
        Ok(value) => value,
        Err(code) => return code,
    };

    let url = if let Some(raw_number) = args.first() {
        let issue_number = match parse_positive_number(raw_number, "issue") {
            Ok(value) => value,
            Err(code) => return code,
        };
        match collab.provider {
            Provider::Gitlab => format!("{}/-/issues/{issue_number}", collab.base_url),
            _ => format!("{}/issues/{issue_number}", collab.base_url),
        }
    } else {
        match collab.provider {
            Provider::Gitlab => format!("{}/-/issues", collab.base_url),
            _ => format!("{}/issues", collab.base_url),
        }
    };
    open_url(&url, "📌 Opened")
}

fn open_actions(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open actions takes at most one workflow");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };
    let collab = match resolve_collab_context(&ctx) {
        Ok(value) => value,
        Err(code) => return code,
    };

    if collab.provider != Provider::Github {
        eprintln!("❗ actions is only supported for GitHub remotes.");
        return 1;
    }

    let url = if let Some(workflow) = args.first() {
        if is_yaml_workflow(workflow) {
            let encoded = percent_encode(workflow, false);
            format!("{}/actions/workflows/{encoded}", collab.base_url)
        } else {
            let q = format!("workflow:{workflow}");
            let encoded = percent_encode(&q, false);
            format!("{}/actions?query={encoded}", collab.base_url)
        }
    } else {
        format!("{}/actions", collab.base_url)
    };

    open_url(&url, "📌 Opened")
}

fn open_releases(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open releases takes at most one tag");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };
    let collab = match resolve_collab_context(&ctx) {
        Ok(value) => value,
        Err(code) => return code,
    };

    let url = if let Some(tag) = args.first() {
        release_tag_url(collab.provider, &collab.base_url, tag)
    } else {
        match collab.provider {
            Provider::Gitlab => format!("{}/-/releases", collab.base_url),
            _ => format!("{}/releases", collab.base_url),
        }
    };
    open_url(&url, "📌 Opened")
}

fn open_tags(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open tags takes at most one tag");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };
    let collab = match resolve_collab_context(&ctx) {
        Ok(value) => value,
        Err(code) => return code,
    };

    let url = if let Some(tag) = args.first() {
        release_tag_url(collab.provider, &collab.base_url, tag)
    } else {
        match collab.provider {
            Provider::Gitlab => format!("{}/-/tags", collab.base_url),
            _ => format!("{}/tags", collab.base_url),
        }
    };
    open_url(&url, "📌 Opened")
}

fn open_commits(args: &[String]) -> i32 {
    if args.len() > 1 {
        eprintln!("❌ git-cli open commits takes at most one ref");
        print_usage();
        return 2;
    }

    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };
    let reference = args
        .first()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| ctx.remote_branch.clone());
    let url = commits_url(ctx.provider, &ctx.base_url, &reference);
    open_url(&url, "📜 Opened")
}

fn open_file(args: &[String]) -> i32 {
    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    if args.is_empty() || args.len() > 2 {
        eprintln!("❌ Usage: git-cli open file <path> [ref]");
        return 2;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };

    let path = normalize_repo_path(&args[0]);
    let reference = args
        .get(1)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| ctx.remote_branch.clone());

    let url = blob_url(ctx.provider, &ctx.base_url, &reference, &path);
    open_url(&url, "📄 Opened")
}

fn open_blame(args: &[String]) -> i32 {
    if args.first().is_some_and(|arg| is_help_token(arg)) {
        print_usage();
        return 0;
    }

    if args.is_empty() || args.len() > 2 {
        eprintln!("❌ Usage: git-cli open blame <path> [ref]");
        return 2;
    }

    let ctx = match resolve_context() {
        Ok(ctx) => ctx,
        Err(code) => return code,
    };

    let path = normalize_repo_path(&args[0]);
    let reference = args
        .get(1)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| ctx.remote_branch.clone());

    let url = blame_url(ctx.provider, &ctx.base_url, &reference, &path);
    open_url(&url, "🕵️ Opened")
}

fn try_open_pr_with_gh(ctx: &OpenContext, collab: &CollabContext) -> bool {
    let mut candidates: Vec<Option<String>> = Vec::new();

    if let Some(slug) = github_repo_slug(&collab.base_url) {
        candidates.push(Some(slug));
    }
    if let Some(slug) = github_repo_slug(&ctx.base_url)
        && !candidates
            .iter()
            .any(|value| value.as_deref() == Some(&slug))
    {
        candidates.push(Some(slug));
    }
    candidates.push(None);

    for repo in candidates {
        if run_gh_pr_view(repo.as_deref(), Some(&ctx.remote_branch)) {
            return true;
        }
    }

    false
}

fn run_gh_pr_view(repo: Option<&str>, selector: Option<&str>) -> bool {
    let mut owned_args: Vec<String> = vec!["pr".into(), "view".into(), "--web".into()];
    if let Some(repo) = repo {
        owned_args.push("--repo".into());
        owned_args.push(repo.to_string());
    }
    if let Some(selector) = selector {
        owned_args.push(selector.to_string());
    }
    let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();

    let output = match run_output("gh", &args) {
        Ok(output) => output,
        Err(_) => return false,
    };

    if output.status.success() {
        println!("🧷 Opened PR via gh");
        true
    } else {
        false
    }
}

fn resolve_context() -> Result<OpenContext, i32> {
    let (remote, remote_branch) = resolve_upstream()?;
    let base_url = normalize_remote_url(&remote)?;
    let provider = Provider::from_base_url(&base_url);
    Ok(OpenContext {
        base_url,
        remote,
        remote_branch,
        provider,
    })
}

fn resolve_collab_context(ctx: &OpenContext) -> Result<CollabContext, i32> {
    let env_remote = std::env::var("GIT_OPEN_COLLAB_REMOTE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    if let Some(remote) = env_remote
        && let Ok(base_url) = normalize_remote_url(&remote)
    {
        let provider = Provider::from_base_url(&base_url);
        return Ok(CollabContext {
            base_url,
            remote,
            provider,
        });
    }

    Ok(CollabContext {
        base_url: ctx.base_url.clone(),
        remote: ctx.remote.clone(),
        provider: ctx.provider,
    })
}

fn resolve_upstream() -> Result<(String, String), i32> {
    if !git_status_success(&["rev-parse", "--is-inside-work-tree"]) {
        eprintln!("❌ Not in a git repository");
        return Err(1);
    }

    let branch = match git_stdout_trimmed(&["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("❌ Unable to resolve current branch");
            return Err(1);
        }
    };

    let upstream =
        git_stdout_trimmed_optional(&["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"])
            .unwrap_or_default();

    let mut remote = String::new();
    let mut remote_branch = String::new();

    if !upstream.is_empty()
        && upstream != branch
        && let Some((upstream_remote, upstream_branch)) = upstream.split_once('/')
    {
        remote = upstream_remote.to_string();
        remote_branch = upstream_branch.to_string();
    }

    if remote.is_empty() {
        remote = "origin".to_string();
    }

    if remote_branch.is_empty() || remote_branch == "HEAD" {
        remote_branch = branch;
    }

    Ok((remote, remote_branch))
}

fn normalize_remote_url(remote: &str) -> Result<String, i32> {
    if remote.trim().is_empty() {
        eprintln!("❌ Missing remote name");
        return Err(1);
    }

    let raw = match git_stdout_trimmed(&["remote", "get-url", remote]) {
        Ok(value) => value,
        Err(_) => {
            eprintln!("❌ Failed to resolve remote URL for {remote}");
            return Err(1);
        }
    };

    match normalize_remote_url_from_raw(&raw) {
        Some(value) => Ok(value),
        None => {
            eprintln!("❌ Unable to normalize remote URL for {remote}");
            Err(1)
        }
    }
}

fn normalize_remote_url_from_raw(raw: &str) -> Option<String> {
    let input = raw.trim();
    if input.is_empty() {
        return None;
    }

    let normalized = if let Some((scheme, rest)) = input.split_once("://") {
        let (host_with_auth, path) = match rest.split_once('/') {
            Some((host, path)) => (host, path),
            None => (rest, ""),
        };
        let host = strip_userinfo(host_with_auth);
        if host.is_empty() {
            return None;
        }

        match scheme {
            "ssh" | "git" => {
                if path.is_empty() {
                    format!("https://{host}")
                } else {
                    format!("https://{host}/{path}")
                }
            }
            "http" | "https" => {
                if path.is_empty() {
                    format!("{scheme}://{host}")
                } else {
                    format!("{scheme}://{host}/{path}")
                }
            }
            _ => {
                if path.is_empty() {
                    format!("{scheme}://{host}")
                } else {
                    format!("{scheme}://{host}/{path}")
                }
            }
        }
    } else if input.contains(':') {
        if let Some((host_part, path_part)) = input.rsplit_once(':') {
            let host = strip_userinfo(host_part);
            if !host.is_empty()
                && !path_part.is_empty()
                && !host.contains('/')
                && !path_part.starts_with('/')
            {
                format!("https://{host}/{path_part}")
            } else {
                input.to_string()
            }
        } else {
            input.to_string()
        }
    } else if let Some((host_part, path_part)) = input.split_once('/') {
        if host_part.contains('@') {
            let host = strip_userinfo(host_part);
            if host.is_empty() {
                return None;
            }
            format!("https://{host}/{path_part}")
        } else {
            input.to_string()
        }
    } else {
        input.to_string()
    };

    if !normalized.starts_with("http://") && !normalized.starts_with("https://") {
        return None;
    }

    let trimmed = normalized.trim_end_matches('/').trim_end_matches(".git");
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn default_branch_name(remote: &str) -> Result<String, i32> {
    let symbolic = format!("refs/remotes/{remote}/HEAD");
    if let Some(value) =
        git_stdout_trimmed_optional(&["symbolic-ref", "--quiet", "--short", &symbolic])
        && let Some((_, branch)) = value.split_once('/')
        && !branch.trim().is_empty()
    {
        return Ok(branch.to_string());
    }

    let output = match run_git_output(&["remote", "show", remote]) {
        Some(output) => output,
        None => return Err(1),
    };
    if !output.status.success() {
        emit_output(&output);
        return Err(exit_code(&output));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        if line.contains("HEAD branch") {
            let value = line
                .split_once(':')
                .map(|(_, tail)| tail.trim())
                .unwrap_or("");
            if !value.is_empty() && value != "(unknown)" {
                return Ok(value.to_string());
            }
        }
    }

    eprintln!("❌ Failed to resolve default branch for {remote}");
    Err(1)
}

fn parse_positive_number(raw: &str, kind: &str) -> Result<String, i32> {
    let cleaned = raw.trim_start_matches('#');
    if cleaned.chars().all(|ch| ch.is_ascii_digit()) && !cleaned.is_empty() {
        Ok(cleaned.to_string())
    } else {
        eprintln!("❌ Invalid {kind} number: {raw}");
        Err(2)
    }
}

fn normalize_repo_path(path: &str) -> String {
    path.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

fn is_yaml_workflow(value: &str) -> bool {
    value.ends_with(".yml") || value.ends_with(".yaml")
}

fn tree_url(provider: Provider, base_url: &str, reference: &str) -> String {
    match provider {
        Provider::Gitlab => format!("{base_url}/-/tree/{reference}"),
        _ => format!("{base_url}/tree/{reference}"),
    }
}

fn commit_url(provider: Provider, base_url: &str, commit: &str) -> String {
    match provider {
        Provider::Gitlab => format!("{base_url}/-/commit/{commit}"),
        _ => format!("{base_url}/commit/{commit}"),
    }
}

fn compare_url(provider: Provider, base_url: &str, base: &str, head: &str) -> String {
    match provider {
        Provider::Gitlab => format!("{base_url}/-/compare/{base}...{head}"),
        _ => format!("{base_url}/compare/{base}...{head}"),
    }
}

fn blob_url(provider: Provider, base_url: &str, reference: &str, path: &str) -> String {
    let encoded_path = percent_encode(path, true);
    match provider {
        Provider::Gitlab => format!("{base_url}/-/blob/{reference}/{encoded_path}"),
        _ => format!("{base_url}/blob/{reference}/{encoded_path}"),
    }
}

fn blame_url(provider: Provider, base_url: &str, reference: &str, path: &str) -> String {
    let encoded_path = percent_encode(path, true);
    match provider {
        Provider::Gitlab => format!("{base_url}/-/blame/{reference}/{encoded_path}"),
        _ => format!("{base_url}/blame/{reference}/{encoded_path}"),
    }
}

fn commits_url(provider: Provider, base_url: &str, reference: &str) -> String {
    match provider {
        Provider::Gitlab => format!("{base_url}/-/commits/{reference}"),
        _ => format!("{base_url}/commits/{reference}"),
    }
}

fn release_tag_url(provider: Provider, base_url: &str, tag: &str) -> String {
    let encoded = percent_encode(tag, false);
    match provider {
        Provider::Gitlab => format!("{base_url}/-/releases/{encoded}"),
        _ => format!("{base_url}/releases/tag/{encoded}"),
    }
}

fn github_repo_slug(base_url: &str) -> Option<String> {
    let without_scheme = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let (_, path) = without_scheme.split_once('/')?;
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(format!("{owner}/{repo}"))
}

fn host_from_url(url: &str) -> String {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    without_scheme
        .split('/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn percent_encode(value: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        let is_unreserved =
            matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~');
        if is_unreserved || (keep_slash && *byte == b'/') {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn open_url(url: &str, label: &str) -> i32 {
    if url.is_empty() {
        eprintln!("❌ Missing URL");
        return 1;
    }

    let Some(opener) = process::browser_open_command() else {
        eprintln!("❌ Cannot open URL (no open/xdg-open)");
        return 1;
    };

    let output = match run_output(opener, &[url]) {
        Ok(output) => output,
        Err(err) => {
            eprintln!("{err}");
            return 1;
        }
    };
    if !output.status.success() {
        if process::is_headless_browser_launch_failure(&output.stdout, &output.stderr) {
            println!("🔗 URL: {url}");
            eprintln!("⚠️  Could not launch a browser in this environment; open the URL manually.");
            return 0;
        }
        emit_output(&output);
        return exit_code(&output);
    }

    println!("{label}: {url}");
    0
}

fn is_help_token(raw: &str) -> bool {
    matches!(raw, "-h" | "--help" | "help")
}

fn print_usage() {
    println!("Usage:");
    println!("  git-cli open");
    println!("  git-cli open repo [remote]");
    println!("  git-cli open branch [ref]");
    println!("  git-cli open default-branch [remote]");
    println!("  git-cli open commit [ref]");
    println!("  git-cli open compare [base] [head]");
    println!("  git-cli open pr [number]");
    println!("  git-cli open pulls [number]");
    println!("  git-cli open issues [number]");
    println!("  git-cli open actions [workflow]");
    println!("  git-cli open releases [tag]");
    println!("  git-cli open tags [tag]");
    println!("  git-cli open commits [ref]");
    println!("  git-cli open file <path> [ref]");
    println!("  git-cli open blame <path> [ref]");
    println!();
    println!("Notes:");
    println!("  - Uses the upstream remote when available; falls back to origin.");
    println!("  - Collaboration pages prefer GIT_OPEN_COLLAB_REMOTE when set.");
    println!("  - `pr` prefers gh when available on GitHub remotes.");
}

fn run_git_output(args: &[&str]) -> Option<Output> {
    match run_output("git", args) {
        Ok(output) => Some(output),
        Err(err) => {
            eprintln!("{err}");
            None
        }
    }
}

fn run_output(cmd: &str, args: &[&str]) -> Result<Output, String> {
    process::run_output(cmd, args)
        .map(|output| output.into_std_output())
        .map_err(|err| format!("spawn {cmd}: {err}"))
}

fn git_stdout_trimmed(args: &[&str]) -> Result<String, i32> {
    let output = run_git_output(args).ok_or(1)?;
    if !output.status.success() {
        emit_output(&output);
        return Err(exit_code(&output));
    }
    Ok(trim_trailing_newlines(&String::from_utf8_lossy(&output.stdout)).to_string())
}

fn git_stdout_trimmed_optional(args: &[&str]) -> Option<String> {
    let output = run_git_output(args)?;
    if !output.status.success() {
        return None;
    }
    let value = trim_trailing_newlines(&String::from_utf8_lossy(&output.stdout)).to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn git_status_success(args: &[&str]) -> bool {
    run_git_output(args)
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn trim_trailing_newlines(input: &str) -> &str {
    input.trim_end_matches(['\n', '\r'])
}

fn exit_code(output: &Output) -> i32 {
    output.status.code().unwrap_or(1)
}

fn emit_output(output: &Output) {
    let _ = io::stdout().write_all(&output.stdout);
    let _ = io::stderr().write_all(&output.stderr);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn normalize_remote_url_supports_common_git_forms() {
        assert_eq!(
            normalize_remote_url_from_raw("git@github.com:acme/repo.git"),
            Some("https://github.com/acme/repo".to_string())
        );
        assert_eq!(
            normalize_remote_url_from_raw("ssh://git@gitlab.com/group/repo.git"),
            Some("https://gitlab.com/group/repo".to_string())
        );
        assert_eq!(
            normalize_remote_url_from_raw("https://github.com/acme/repo.git/"),
            Some("https://github.com/acme/repo".to_string())
        );
    }

    #[test]
    fn normalize_remote_url_rejects_non_http_like_sources() {
        assert_eq!(normalize_remote_url_from_raw("../relative/path"), None);
        assert_eq!(normalize_remote_url_from_raw("/tmp/repo.git"), None);
        assert_eq!(normalize_remote_url_from_raw(""), None);
    }

    #[test]
    fn provider_detection_matches_hosts() {
        assert_eq!(
            Provider::from_base_url("https://github.com/acme/repo"),
            Provider::Github
        );
        assert_eq!(
            Provider::from_base_url("https://gitlab.com/acme/repo"),
            Provider::Gitlab
        );
        assert_eq!(
            Provider::from_base_url("https://gitlab.internal/acme/repo"),
            Provider::Gitlab
        );
        assert_eq!(
            Provider::from_base_url("https://code.example.com/acme/repo"),
            Provider::Generic
        );
    }

    #[test]
    fn github_slug_parses_owner_repo() {
        assert_eq!(
            github_repo_slug("https://github.com/acme/repo"),
            Some("acme/repo".to_string())
        );
        assert_eq!(
            github_repo_slug("https://github.com/acme/repo/sub/path"),
            Some("acme/repo".to_string())
        );
        assert_eq!(github_repo_slug("https://github.com/acme"), None);
    }

    #[test]
    fn percent_encode_supports_paths_and_queries() {
        assert_eq!(
            percent_encode("docs/read me.md", true),
            "docs/read%20me.md".to_string()
        );
        assert_eq!(
            percent_encode("workflow:CI Build", false),
            "workflow%3ACI%20Build".to_string()
        );
    }

    #[test]
    fn url_builders_follow_provider_conventions() {
        assert_eq!(
            tree_url(Provider::Github, "https://github.com/acme/repo", "main"),
            "https://github.com/acme/repo/tree/main"
        );
        assert_eq!(
            tree_url(Provider::Gitlab, "https://gitlab.com/acme/repo", "main"),
            "https://gitlab.com/acme/repo/-/tree/main"
        );
        assert_eq!(
            release_tag_url(Provider::Github, "https://github.com/acme/repo", "v1.2.3"),
            "https://github.com/acme/repo/releases/tag/v1.2.3"
        );
        assert_eq!(
            release_tag_url(Provider::Gitlab, "https://gitlab.com/acme/repo", "v1.2.3"),
            "https://gitlab.com/acme/repo/-/releases/v1.2.3"
        );
    }

    #[test]
    fn parse_positive_number_accepts_hash_prefix() {
        assert_eq!(parse_positive_number("#123", "PR"), Ok("123".to_string()));
        assert_eq!(parse_positive_number("42", "issue"), Ok("42".to_string()));
        assert_eq!(parse_positive_number("abc", "PR"), Err(2));
    }

    #[test]
    fn open_commands_reject_too_many_args_before_git_lookups() {
        assert_eq!(open_repo(&args(&["origin", "extra"])), 2);
        assert_eq!(open_branch(&args(&["main", "extra"])), 2);
        assert_eq!(open_default_branch(&args(&["origin", "extra"])), 2);
        assert_eq!(open_commit(&args(&["HEAD", "extra"])), 2);
        assert_eq!(open_compare(&args(&["base", "head", "extra"])), 2);
        assert_eq!(open_pr(&args(&["1", "extra"])), 2);
        assert_eq!(open_pulls(&args(&["1", "extra"])), 2);
        assert_eq!(open_issues(&args(&["1", "extra"])), 2);
        assert_eq!(open_actions(&args(&["ci.yml", "extra"])), 2);
        assert_eq!(open_releases(&args(&["v1.0.0", "extra"])), 2);
        assert_eq!(open_tags(&args(&["v1.0.0", "extra"])), 2);
        assert_eq!(open_commits(&args(&["main", "extra"])), 2);
        assert_eq!(open_file(&args(&["src/lib.rs", "main", "extra"])), 2);
        assert_eq!(open_blame(&args(&["src/lib.rs", "main", "extra"])), 2);
    }

    #[test]
    fn open_commands_help_paths_return_zero_without_repo() {
        assert_eq!(open_repo(&args(&["--help"])), 0);
        assert_eq!(open_branch(&args(&["-h"])), 0);
        assert_eq!(open_default_branch(&args(&["help"])), 0);
        assert_eq!(open_commit(&args(&["--help"])), 0);
        assert_eq!(open_pr(&args(&["help"])), 0);
        assert_eq!(open_actions(&args(&["--help"])), 0);
        assert_eq!(open_releases(&args(&["--help"])), 0);
        assert_eq!(open_tags(&args(&["--help"])), 0);
        assert_eq!(open_commits(&args(&["--help"])), 0);
    }

    #[test]
    fn open_file_and_blame_require_path_argument() {
        assert_eq!(open_file(&args(&[])), 2);
        assert_eq!(open_blame(&args(&[])), 2);
    }

    #[test]
    fn normalize_remote_url_handles_additional_variants() {
        assert_eq!(
            normalize_remote_url_from_raw("https://token@github.com/acme/repo.git"),
            Some("https://github.com/acme/repo".to_string())
        );
        assert_eq!(
            normalize_remote_url_from_raw("ssh://git@gitlab.com"),
            Some("https://gitlab.com".to_string())
        );
        assert_eq!(
            normalize_remote_url_from_raw("git://gitlab.com/group/repo.git"),
            Some("https://gitlab.com/group/repo".to_string())
        );
        assert_eq!(
            normalize_remote_url_from_raw("git@github.com:acme/repo"),
            Some("https://github.com/acme/repo".to_string())
        );
        assert_eq!(
            normalize_remote_url_from_raw("user@gitlab.com/acme/repo.git"),
            Some("https://gitlab.com/acme/repo".to_string())
        );
        assert_eq!(normalize_remote_url_from_raw("ssh://git@/acme/repo"), None);
        assert_eq!(normalize_remote_url_from_raw("file:///tmp/repo.git"), None);
    }

    #[test]
    fn helper_functions_cover_url_and_path_variants() {
        assert_eq!(normalize_repo_path("./docs/read me.md"), "docs/read me.md");
        assert_eq!(normalize_repo_path("/src/lib.rs"), "src/lib.rs");
        assert!(is_yaml_workflow("ci.yml"));
        assert!(is_yaml_workflow("ci.yaml"));
        assert!(!is_yaml_workflow("ci.json"));
        assert_eq!(strip_userinfo("git@github.com"), "github.com");
        assert_eq!(
            host_from_url("HTTPS://User@Example.COM/repo"),
            "user@example.com"
        );
        assert_eq!(trim_trailing_newlines("line\r\n"), "line");
        assert!(is_help_token("help"));
        assert!(!is_help_token("--version"));
        assert_eq!(
            github_repo_slug("github.com/acme/repo"),
            Some("acme/repo".to_string())
        );
        assert_eq!(github_repo_slug("https://github.com"), None);
    }

    #[test]
    fn remaining_url_builders_cover_gitlab_and_generic_paths() {
        assert_eq!(
            commit_url(
                Provider::Generic,
                "https://code.example.com/acme/repo",
                "abc123"
            ),
            "https://code.example.com/acme/repo/commit/abc123"
        );
        assert_eq!(
            compare_url(
                Provider::Gitlab,
                "https://gitlab.com/acme/repo",
                "main",
                "feature"
            ),
            "https://gitlab.com/acme/repo/-/compare/main...feature"
        );
        assert_eq!(
            blob_url(
                Provider::Gitlab,
                "https://gitlab.com/acme/repo",
                "main",
                "dir/file name.rs"
            ),
            "https://gitlab.com/acme/repo/-/blob/main/dir/file%20name.rs"
        );
        assert_eq!(
            blame_url(
                Provider::Generic,
                "https://code.example.com/acme/repo",
                "main",
                "dir/file name.rs"
            ),
            "https://code.example.com/acme/repo/blame/main/dir/file%20name.rs"
        );
        assert_eq!(
            commits_url(Provider::Gitlab, "https://gitlab.com/acme/repo", "main"),
            "https://gitlab.com/acme/repo/-/commits/main"
        );
    }
}
