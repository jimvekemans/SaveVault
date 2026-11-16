//! Thin, defensive wrapper around the `git` command line.
//!
//! The save repository is driven non-interactively: no prompts, no user hooks, no commit
//! signing, no line-ending conversion. Credentials are handed to git through the process
//! environment (never the command line, which other local users can read) and the token is
//! scoped to `github.com` so it can never be sent to another host.

use crate::auth::git_ssh_command_for_key;
use crate::chunker::normalize_rel_path;
use crate::models::{AuthMethod, CommitInfo, Provider};
use anyhow::{Context, Result, bail};
use base64::Engine;
use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Folder (inside a game's repository folder) that holds non-main profiles.
pub const PROFILES_DIR: &str = "@sgm_profiles";
/// Folder (inside a profile folder) that holds roots other than the primary one.
pub const ROOTS_DIR: &str = "@sgm_roots";
/// Per-profile snapshot description (mod fingerprint, root templates).
pub const SNAPSHOT_META_FILE: &str = "@sgm_snapshot.json";

const GITATTRIBUTES_LEGACY: &str = "*.sgm_part.* binary\n*.sav binary\n*.bin binary\n";
/// Save files are opaque: never convert line endings, never diff, never merge.
const GITATTRIBUTES: &str = "* -text -diff -merge\n";
const DEFAULT_HISTORY_LIMIT: usize = 200;

fn git_base(repo_path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo_path);
    cmd.args([
        "-c",
        "commit.gpgsign=false",
        "-c",
        "tag.gpgsign=false",
        "-c",
        "core.autocrlf=false",
        "-c",
        "core.safecrlf=false",
        "-c",
        "core.quotepath=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "advice.detachedHead=false",
    ]);
    #[cfg(unix)]
    cmd.args(["-c", "core.hooksPath=/dev/null"]);
    #[cfg(windows)]
    cmd.args(["-c", "core.longpaths=true"]);
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd.env("GIT_OPTIONAL_LOCKS", "0");
    cmd.env("LC_ALL", "C");
    cmd.stdin(Stdio::null());
    cmd
}

/// Applies credentials to a git command through its environment.
pub fn apply_auth_to_git_command(cmd: &mut Command, auth: &AuthMethod) {
    match auth {
        AuthMethod::ExistingSshKey {
            private_key_path, ..
        }
        | AuthMethod::NewSshKey {
            private_key_path, ..
        } => {
            cmd.env("GIT_SSH_COMMAND", git_ssh_command_for_key(private_key_path));
        }
        AuthMethod::PersonalAccessToken { token, .. } => {
            let trimmed = token.trim();
            if !trimmed.is_empty() {
                let raw = format!("x-access-token:{trimmed}");
                let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
                cmd.env("GIT_CONFIG_COUNT", "1");
                cmd.env("GIT_CONFIG_KEY_0", "http.https://github.com/.extraheader");
                cmd.env("GIT_CONFIG_VALUE_0", format!("Authorization: Basic {encoded}"));
            }
        }
    }
}

fn run_git_checked(mut cmd: Command, desc: &str) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("Failed to run git ({desc}). Is git installed?"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        bail!("git {desc} failed: {} {}", stderr.trim(), stdout.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn git_ok(repo_path: &Path, args: &[&str]) -> bool {
    git_base(repo_path)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_out(repo_path: &Path, args: &[&str]) -> Option<String> {
    git_base(repo_path)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Returns true when the `git` executable can be run.
pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Initializes a local git repository at `repo_path` if needed, configures local identity,
/// writes `.gitattributes`, and optionally sets `remote origin`.
pub fn init_or_open_local_repo(repo_path: &Path, remote_url: Option<&str>) -> Result<()> {
    fs::create_dir_all(repo_path)
        .with_context(|| format!("Failed to create local repo dir {}", repo_path.display()))?;

    if !repo_path.join(".git").exists() {
        let mut init = Command::new("git");
        init.arg("init").arg("-b").arg("main").arg(repo_path);
        if run_git_checked(init, "init -b main").is_err() {
            let mut fallback = Command::new("git");
            fallback.arg("init").arg(repo_path);
            run_git_checked(fallback, "init")?;
        }
    }

    // Ensure repo-local git author identity exists so commits never fail on fresh systems.
    if git_out(repo_path, &["config", "user.name"]).is_none_or(|s| s.is_empty()) {
        let mut cmd = git_base(repo_path);
        cmd.args(["config", "--local", "user.name", "SaveVault"]);
        run_git_checked(cmd, "config user.name")?;
    }
    if git_out(repo_path, &["config", "user.email"]).is_none_or(|s| s.is_empty()) {
        let mut cmd = git_base(repo_path);
        cmd.args(["config", "--local", "user.email", "savevault@localhost"]);
        run_git_checked(cmd, "config user.email")?;
    }

    let gitattributes_path = repo_path.join(".gitattributes");
    let current = fs::read_to_string(&gitattributes_path).ok();
    if current.is_none() || current.as_deref() == Some(GITATTRIBUTES_LEGACY) {
        fs::write(&gitattributes_path, GITATTRIBUTES)?;
    }

    if let Some(url) = remote_url {
        match git_out(repo_path, &["remote", "get-url", "origin"]) {
            Some(current) => {
                if current != url {
                    let mut cmd = git_base(repo_path);
                    cmd.args(["remote", "set-url", "origin", url]);
                    run_git_checked(cmd, "remote set-url origin")?;
                }
            }
            None => {
                let mut cmd = git_base(repo_path);
                cmd.args(["remote", "add", "origin", url]);
                run_git_checked(cmd, "remote add origin")?;
            }
        }
    }

    Ok(())
}

/// Stages all changes in `repo_path` and creates a commit if anything changed.
pub fn commit_all_if_changed(repo_path: &Path, message: &str) -> Result<Option<String>> {
    let mut add = git_base(repo_path);
    add.args(["add", "-A"]);
    run_git_checked(add, "add -A")?;

    let mut status = git_base(repo_path);
    status.args(["status", "--porcelain"]);
    let status_out = run_git_checked(status, "status --porcelain")?;
    if status_out.trim().is_empty() {
        return Ok(None);
    }

    let mut commit = git_base(repo_path);
    commit.args(["commit", "-q", "-m", message]);
    run_git_checked(commit, "commit")?;

    Ok(head_commit(repo_path))
}

/// Name of the checked-out branch (`main` when the repository has no commits yet).
pub fn current_branch(repo_path: &Path) -> String {
    git_out(repo_path, &["symbolic-ref", "--short", "-q", "HEAD"])
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| "main".to_string())
}

fn has_head_commit(repo_path: &Path) -> bool {
    git_ok(repo_path, &["rev-parse", "--verify", "-q", "HEAD"])
}

/// Full hash of `HEAD`, if the repository has any commit.
pub fn head_commit(repo_path: &Path) -> Option<String> {
    git_out(repo_path, &["rev-parse", "--verify", "-q", "HEAD"]).filter(|s| !s.is_empty())
}

fn rev_parse(repo_path: &Path, rev: &str) -> Option<String> {
    git_out(repo_path, &["rev-parse", "--verify", "-q", &format!("{rev}^{{commit}}")])
        .filter(|s| !s.is_empty())
}

/// Checks whether an `origin` remote is configured on `repo_path`.
pub fn has_origin_remote(repo_path: &Path) -> bool {
    git_ok(repo_path, &["remote", "get-url", "origin"])
}

/// Number of local commits not yet on `origin/<branch>` (0 when there is no remote branch).
pub fn unpushed_commit_count(repo_path: &Path) -> usize {
    let branch = current_branch(repo_path);
    let remote_ref = format!("origin/{branch}");
    if rev_parse(repo_path, &remote_ref).is_none() {
        return if has_origin_remote(repo_path) && has_head_commit(repo_path) {
            git_out(repo_path, &["rev-list", "--count", "HEAD"])
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        } else {
            0
        };
    }
    git_out(repo_path, &["rev-list", "--count", &format!("{remote_ref}..HEAD")])
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Pushes the active branch of `repo_path` to `origin` using `auth`.
pub fn push_to_remote(repo_path: &Path, auth: &AuthMethod) -> Result<()> {
    if !has_origin_remote(repo_path) {
        bail!("No 'origin' remote is configured for {}", repo_path.display());
    }
    if !has_head_commit(repo_path) {
        return Ok(());
    }

    let branch = current_branch(repo_path);
    let mut cmd = git_base(repo_path);
    apply_auth_to_git_command(&mut cmd, auth);
    cmd.args(["push", "-q", "-u", "origin", &branch]);
    run_git_checked(cmd, "push")?;
    Ok(())
}

/// True when a push error means the remote has commits we do not have yet.
pub fn is_non_fast_forward_error(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("non-fast-forward") || m.contains("fetch first") || m.contains("[rejected]")
}

/// Fetches `origin`. Returns true when the remote has the current branch.
pub fn fetch_remote(repo_path: &Path, auth: &AuthMethod) -> Result<bool> {
    if !has_origin_remote(repo_path) {
        return Ok(false);
    }
    let mut fetch = git_base(repo_path);
    apply_auth_to_git_command(&mut fetch, auth);
    fetch.args(["fetch", "-q", "--prune", "origin"]);
    run_git_checked(fetch, "fetch origin")?;
    let branch = current_branch(repo_path);
    Ok(rev_parse(repo_path, &format!("origin/{branch}")).is_some())
}

/// What `integrate_remote` did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum IntegrationKind {
    /// No remote branch, or nothing new on it.
    #[default]
    UpToDate,
    /// Local history is strictly ahead of the remote.
    LocalAhead,
    /// Local history was behind and moved forward to the remote.
    FastForward,
    /// Both sides had new commits; they were combined game by game.
    Merged,
}

/// A game profile both sides changed; the local version was kept in the working tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitConflict {
    /// Repository path of the profile folder (see [`unit_of_path`]).
    pub unit: String,
    /// Remote commit that holds the other side.
    pub their_commit: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IntegrationOutcome {
    pub kind: IntegrationKind,
    /// Profile folders whose content now comes from the remote.
    pub updated_units: BTreeSet<String>,
    pub conflicts: Vec<UnitConflict>,
}

/// Maps a repository file path to the profile folder ("unit") it belongs to:
/// `<provider>/<game>`, `<game>`, or either followed by `/@sgm_profiles/<profile>`.
/// Returns `None` for repository-level files.
pub fn unit_of_path(path: &str) -> Option<String> {
    let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
    if comps.len() < 2 {
        return None;
    }
    let provider_layout = comps.len() >= 3 && Provider::all().iter().any(|p| p.slug() == comps[0]);
    let game_len = if provider_layout { 2 } else { 1 };
    let mut unit = comps[..game_len].join("/");
    if comps.len() > game_len + 2 && comps[game_len] == PROFILES_DIR {
        unit = format!("{unit}/{PROFILES_DIR}/{}", comps[game_len + 1]);
    }
    Some(unit)
}

fn diff_name_status(repo_path: &Path, from: &str, to: &str) -> Result<Vec<(char, String)>> {
    let mut cmd = git_base(repo_path);
    cmd.args(["diff", "--name-status", "--no-renames", "-z", from, to]);
    let raw = run_git_checked(cmd, "diff --name-status")?;
    let mut out = Vec::new();
    let mut parts = raw.split('\0');
    while let Some(status) = parts.next() {
        if status.is_empty() {
            continue;
        }
        let Some(path) = parts.next() else { break };
        out.push((status.chars().next().unwrap_or('M'), path.to_string()));
    }
    Ok(out)
}

/// Brings `origin/<branch>` into the local repository after a fetch.
///
/// The working tree only mirrors save folders, so integration is done per game profile
/// rather than per text hunk: a profile changed only by the remote is taken from the remote,
/// one changed only locally stays, and one changed by both is reported as a conflict with the
/// local version kept (the remote version stays reachable in history).
pub fn integrate_remote(repo_path: &Path, machine: &str) -> Result<IntegrationOutcome> {
    let branch = current_branch(repo_path);
    let remote_ref = format!("origin/{branch}");
    let Some(theirs) = rev_parse(repo_path, &remote_ref) else {
        return Ok(IntegrationOutcome::default());
    };

    let Some(head) = head_commit(repo_path) else {
        // Fresh local repository: adopt the remote history as-is.
        let mut co = git_base(repo_path);
        co.args(["checkout", "-q", "-f", "-B", &branch, &remote_ref]);
        run_git_checked(co, "checkout remote branch")?;
        let mut cmd = git_base(repo_path);
        cmd.args(["ls-tree", "-r", "--name-only", "-z", "HEAD"]);
        let listing = run_git_checked(cmd, "ls-tree")?;
        return Ok(IntegrationOutcome {
            kind: IntegrationKind::FastForward,
            updated_units: listing.split('\0').filter_map(unit_of_path).collect(),
            conflicts: Vec::new(),
        });
    };

    if head == theirs {
        return Ok(IntegrationOutcome::default());
    }

    // The working tree is derived data; make sure it matches HEAD before moving it.
    let mut reset = git_base(repo_path);
    reset.args(["reset", "-q", "--hard", "HEAD"]);
    run_git_checked(reset, "reset --hard")?;
    let mut clean = git_base(repo_path);
    clean.args(["clean", "-q", "-fd"]);
    run_git_checked(clean, "clean")?;

    let base = git_out(repo_path, &["merge-base", &head, &theirs]).filter(|s| !s.is_empty());

    if base.as_deref() == Some(theirs.as_str()) {
        return Ok(IntegrationOutcome {
            kind: IntegrationKind::LocalAhead,
            ..Default::default()
        });
    }

    if base.as_deref() == Some(head.as_str()) {
        let changed = diff_name_status(repo_path, &head, &theirs)?;
        let mut ff = git_base(repo_path);
        ff.args(["merge", "-q", "--ff-only", &theirs]);
        run_git_checked(ff, "merge --ff-only")?;
        return Ok(IntegrationOutcome {
            kind: IntegrationKind::FastForward,
            updated_units: changed.iter().filter_map(|(_, p)| unit_of_path(p)).collect(),
            conflicts: Vec::new(),
        });
    }

    // Diverged (or unrelated histories): combine per profile folder.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let base_rev = base.clone().unwrap_or_else(|| EMPTY_TREE.to_string());
    let their_changes = diff_name_status(repo_path, &base_rev, &theirs)?;
    let our_units: BTreeSet<String> = diff_name_status(repo_path, &base_rev, &head)?
        .iter()
        .filter_map(|(_, p)| unit_of_path(p))
        .collect();

    let mut merge = git_base(repo_path);
    merge.args(["merge", "-q", "-s", "ours", "--no-commit"]);
    if base.is_none() {
        merge.arg("--allow-unrelated-histories");
    }
    merge.arg(&theirs);
    run_git_checked(merge, "merge -s ours")?;

    let mut outcome = IntegrationOutcome {
        kind: IntegrationKind::Merged,
        ..Default::default()
    };
    let mut take: Vec<String> = Vec::new();
    let mut remove: Vec<String> = Vec::new();
    let mut conflict_units: BTreeSet<String> = BTreeSet::new();
    for (status, path) in their_changes {
        let Some(unit) = unit_of_path(&path) else {
            continue; // repository-level files are regenerated locally
        };
        if our_units.contains(&unit) {
            conflict_units.insert(unit);
            continue;
        }
        outcome.updated_units.insert(unit);
        if status == 'D' {
            remove.push(path);
        } else {
            take.push(path);
        }
    }
    // A profile both sides changed identically is not a conflict.
    for unit in conflict_units {
        let same = git_base(repo_path)
            .args(["diff", "--quiet", &head, &theirs, "--", &unit])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !same {
            outcome.conflicts.push(UnitConflict {
                unit,
                their_commit: theirs.clone(),
            });
        }
    }

    for chunk in take.chunks(100) {
        let mut co = git_base(repo_path);
        co.args(["checkout", "-q", &theirs, "--"]);
        co.args(chunk);
        run_git_checked(co, "checkout remote files")?;
    }
    for chunk in remove.chunks(100) {
        let mut rm = git_base(repo_path);
        rm.args(["rm", "-q", "-f", "--ignore-unmatch", "--"]);
        rm.args(chunk);
        run_git_checked(rm, "rm remote-deleted files")?;
    }

    let mut commit = git_base(repo_path);
    commit.args([
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        &format!("Combine saves from the cloud\n\nMachine: {machine}"),
    ]);
    run_git_checked(commit, "commit merge")?;
    Ok(outcome)
}

/// Fetches and integrates `origin` into `repo_path` (see [`integrate_remote`]).
pub fn pull_from_remote(repo_path: &Path, auth: &AuthMethod) -> Result<()> {
    if !fetch_remote(repo_path, auth)? {
        return Ok(());
    }
    integrate_remote(repo_path, "import")?;
    Ok(())
}

fn parse_machine_trailer(body: &str) -> Option<String> {
    body.lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("Machine:"))
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty())
}

/// Lists recent commits that touched any of `relative_paths`, newest first.
pub fn list_commits_for_paths(
    repo_path: &Path,
    relative_paths: &[PathBuf],
    limit: usize,
) -> Result<Vec<CommitInfo>> {
    if !repo_path.join(".git").exists() || !has_head_commit(repo_path) {
        return Ok(Vec::new());
    }

    let mut log_cmd = git_base(repo_path);
    log_cmd.args([
        "log",
        "--pretty=format:%x1e%H%x1f%h%x1f%ad%x1f%at%x1f%s%x1f%b%x1d",
        "--date=iso",
        "--name-only",
        "--no-renames",
        "-n",
        &limit.to_string(),
        "--",
    ]);
    for p in relative_paths {
        log_cmd.arg(normalize_rel_path(p));
    }
    let raw = run_git_checked(log_cmd, "log")?;

    let mut commits = Vec::new();
    for record in raw.split('\x1e') {
        let Some((header, files)) = record.split_once('\x1d') else {
            continue;
        };
        let mut parts = header.splitn(6, '\x1f');
        let (Some(commit_hash), Some(short_hash), Some(timestamp), Some(unix), Some(message)) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            continue;
        };
        let body = parts.next().unwrap_or("");
        let changed_files = files
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        commits.push(CommitInfo {
            commit_hash: commit_hash.trim().to_string(),
            short_hash: short_hash.to_string(),
            timestamp: timestamp.to_string(),
            message: message.to_string(),
            changed_files,
            label: extract_label_from_commit_message(message),
            machine: parse_machine_trailer(body),
            unix_time: unix.trim().parse().unwrap_or(0),
        });
    }

    Ok(commits)
}

/// Lists recent commits that affected `relative_subpath` in `repo_path`.
pub fn list_commits_for_path(repo_path: &Path, relative_subpath: &Path) -> Result<Vec<CommitInfo>> {
    list_commits_for_paths(repo_path, &[relative_subpath.to_path_buf()], DEFAULT_HISTORY_LIMIT)
}

/// Extracts an embedded `[Label: ...]` tag from a git commit message if present.
pub fn extract_label_from_commit_message(message: &str) -> Option<String> {
    let marker = "[Label:";
    let start = message.find(marker)?;
    let after = &message[start + marker.len()..];
    let end = after.find(']')?;
    let lbl = after[..end].trim();
    if lbl.is_empty() {
        None
    } else {
        Some(lbl.to_string())
    }
}

/// Restores `relative_subpath` inside `repo_path` to its exact state at `commit_hash`.
pub fn checkout_path_at_commit(
    repo_path: &Path,
    commit_hash: &str,
    relative_subpath: &Path,
) -> Result<()> {
    let subpath_str = normalize_rel_path(relative_subpath);

    if rev_parse(repo_path, commit_hash).is_none() {
        bail!("Snapshot {commit_hash} does not exist in this repository.");
    }

    let mut rm_cmd = git_base(repo_path);
    rm_cmd.args(["rm", "-rqf", "--ignore-unmatch", "--", &subpath_str]);
    run_git_checked(rm_cmd, "rm before checkout")?;

    let full_subpath = repo_path.join(relative_subpath);
    if full_subpath.exists() {
        if full_subpath.is_dir() {
            fs::remove_dir_all(&full_subpath)?;
        } else {
            fs::remove_file(&full_subpath)?;
        }
    }

    let mut co_cmd = git_base(repo_path);
    co_cmd.args(["checkout", "-q", commit_hash, "--", &subpath_str]);
    run_git_checked(co_cmd, "checkout path at commit")?;

    Ok(())
}

/// True when `relative_path` exists in `rev`.
pub fn path_exists_at(repo_path: &Path, rev: &str, relative_path: &Path) -> bool {
    let spec = format!("{rev}:{}", normalize_rel_path(relative_path));
    git_ok(repo_path, &["cat-file", "-e", &spec])
}

/// Reads one file as it was at `rev`.
pub fn read_file_at(repo_path: &Path, rev: &str, relative_path: &Path) -> Option<Vec<u8>> {
    let spec = format!("{rev}:{}", normalize_rel_path(relative_path));
    git_base(repo_path)
        .args(["cat-file", "blob", &spec])
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
}

/// Extracts `relative_path` as it was at `rev` into `dst_dir` (which receives the
/// *contents* of that folder), without touching the working tree or the index.
/// Returns false when the path does not exist at `rev`.
pub fn extract_path_at(repo_path: &Path, rev: &str, relative_path: &Path, dst_dir: &Path) -> Result<bool> {
    if rev_parse(repo_path, rev).is_none() {
        bail!("Snapshot {rev} does not exist in this repository.");
    }
    if !path_exists_at(repo_path, rev, relative_path) {
        return Ok(false);
    }
    fs::create_dir_all(dst_dir)?;
    let rel = normalize_rel_path(relative_path);
    let mut child = git_base(repo_path)
        .args(["archive", "--format=tar", rev, "--", &rel])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to run git archive")?;
    let stdout = child.stdout.take().context("git archive produced no output")?;

    let prefix = PathBuf::from(&rel);
    let mut archive = tar::Archive::new(stdout);
    let mut unpack_err = None;
    match archive.entries() {
        Ok(entries) => {
            for entry in entries {
                let mut entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        unpack_err = Some(anyhow::Error::from(e));
                        break;
                    }
                };
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry.path()?.into_owned();
                let Ok(stripped) = path.strip_prefix(&prefix) else {
                    continue;
                };
                // Never follow anything that could escape the destination.
                if stripped
                    .components()
                    .any(|c| !matches!(c, std::path::Component::Normal(_)))
                {
                    continue;
                }
                let target = dst_dir.join(stripped);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut out = fs::File::create(&target)
                    .with_context(|| format!("Failed to write {}", target.display()))?;
                std::io::copy(&mut entry, &mut out)?;
            }
        }
        Err(e) => unpack_err = Some(anyhow::Error::from(e)),
    }

    let mut stderr = String::new();
    if let Some(mut s) = child.stderr.take() {
        let _ = s.read_to_string(&mut stderr);
    }
    let status = child.wait()?;
    if let Some(e) = unpack_err {
        return Err(e.context("Failed to unpack snapshot"));
    }
    if !status.success() {
        bail!("git archive failed: {}", stderr.trim());
    }
    Ok(true)
}

/// Lists every file in `rev` as `(path, blob hash)`.
pub fn list_tree(repo_path: &Path, rev: &str) -> Vec<(String, String)> {
    let Some(raw) = git_out(repo_path, &["ls-tree", "-r", "-z", rev]) else {
        return Vec::new();
    };
    raw.split('\0')
        .filter_map(|line| {
            // "<mode> <type> <hash>\t<path>"
            let (meta, path) = line.split_once('\t')?;
            let hash = meta.split_whitespace().nth(2)?;
            Some((path.to_string(), hash.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_mapping_handles_layouts_and_profiles() {
        assert_eq!(unit_of_path("steam/elden_ring/ER0000.sl2").as_deref(), Some("steam/elden_ring"));
        assert_eq!(
            unit_of_path("steam/elden_ring/@sgm_profiles/mage/ER0000.sl2").as_deref(),
            Some("steam/elden_ring/@sgm_profiles/mage")
        );
        assert_eq!(
            unit_of_path("steam/elden_ring/@sgm_roots/config/a.xml").as_deref(),
            Some("steam/elden_ring")
        );
        assert_eq!(unit_of_path("elden_ring/sub/ER0000.sl2").as_deref(), Some("elden_ring"));
        assert_eq!(
            unit_of_path("elden_ring/@sgm_profiles/mage/x").as_deref(),
            Some("elden_ring/@sgm_profiles/mage")
        );
        assert_eq!(unit_of_path("sgm_repo_metadata.json"), None);
        assert_eq!(unit_of_path(".gitattributes"), None);
    }

    #[test]
    fn machine_trailer_and_label_are_parsed() {
        assert_eq!(parse_machine_trailer("\nMachine: desk\n").as_deref(), Some("desk"));
        assert_eq!(parse_machine_trailer("no trailer"), None);
        assert_eq!(
            extract_label_from_commit_message("Sync x [Label: Before boss]").as_deref(),
            Some("Before boss")
        );
        assert!(is_non_fast_forward_error("! [rejected] main -> main (fetch first)"));
    }
}
