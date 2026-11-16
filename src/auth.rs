use crate::models::AuthMethod;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const USER_AGENT: &str = concat!("savevault/", env!("CARGO_PKG_VERSION"));

/// Result of verifying or creating the GitHub remote repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoEnsureResult {
    AlreadyExists { remote_url: String },
    CreatedPrivate { remote_url: String },
    NeedsPatOrManual { remote_url: String, message: String },
}

/// Returns `<private_key_path>.pub` without stripping existing extensions on the private key filename.
pub fn public_key_path_for(private_key_path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.pub", private_key_path.display()))
}

/// Builds the GitHub remote URL corresponding to the selected authentication method.
pub fn remote_url_for_auth(owner: &str, repo_name: &str, auth: &AuthMethod) -> Option<String> {
    let owner = owner.trim();
    let repo = repo_name.trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    match auth {
        AuthMethod::ExistingSshKey { .. } | AuthMethod::NewSshKey { .. } => {
            Some(format!("git@github.com:{owner}/{repo}.git"))
        }
        AuthMethod::PersonalAccessToken { .. } => {
            Some(format!("https://github.com/{owner}/{repo}.git"))
        }
    }
}

/// Builds a non-interactive `GIT_SSH_COMMAND` string pointing to `private_key_path`.
pub fn git_ssh_command_for_key(private_key_path: &Path) -> String {
    let escaped = private_key_path.display().to_string().replace('"', "\\\"");
    format!(
        "ssh -i \"{escaped}\" -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new -o BatchMode=yes"
    )
}

/// Generates a new Ed25519 SSH keypair at `private_key_path` using `ssh-keygen` and returns the `.pub` key contents.
pub fn generate_ssh_keypair(private_key_path: &Path, passphrase: &str) -> Result<String> {
    if private_key_path.exists() {
        bail!(
            "An SSH key already exists at {}. Choose a different path or select 'Use Existing SSH Key'.",
            private_key_path.display()
        );
    }

    if let Some(parent) = private_key_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create SSH directory {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }

    let output = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-f")
        .arg(private_key_path)
        .arg("-N")
        .arg(passphrase)
        .arg("-C")
        .arg("save_game_manager")
        .arg("-q")
        .output()
        .context("Failed to execute ssh-keygen. Ensure OpenSSH is installed.")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("ssh-keygen failed: {}", stderr.trim());
    }

    let pub_path = public_key_path_for(private_key_path);
    let pub_key = fs::read_to_string(&pub_path)
        .with_context(|| format!("Failed to read generated public key {}", pub_path.display()))?;
    Ok(pub_key.trim().to_string())
}

/// Reads the `.pub` file for `private_key_path`, or derives it with `ssh-keygen -y`.
pub fn read_public_key_for_private_key(private_key_path: &Path) -> Result<String> {
    if !private_key_path.exists() {
        bail!(
            "Private key not found at {}",
            private_key_path.display()
        );
    }
    let pub_path = public_key_path_for(private_key_path);
    if pub_path.is_file() {
        let content = fs::read_to_string(&pub_path)
            .with_context(|| format!("Failed to read {}", pub_path.display()))?;
        return Ok(content.trim().to_string());
    }

    let output = Command::new("ssh-keygen")
        .arg("-y")
        .arg("-f")
        .arg(private_key_path)
        .output()
        .context("Failed to invoke ssh-keygen -y")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Could not derive public key from {}: {}", private_key_path.display(), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, Deserialize)]
struct GitHubUserResp {
    login: String,
}

#[derive(Debug, Serialize)]
struct CreateRepoReq<'a> {
    name: &'a str,
    private: bool,
    description: &'a str,
    auto_init: bool,
}

/// Verifies a GitHub Personal Access Token via `GET /user` and returns the authenticated username.
pub fn verify_github_token(token: &str) -> Result<String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        bail!("Personal Access Token cannot be empty.");
    }

    let mut resp = ureq::get("https://api.github.com/user")
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", &format!("Bearer {trimmed}"))
        .call()
        .map_err(|e| anyhow::anyhow!("GitHub token verification failed: {e}"))?;

    let user: GitHubUserResp = resp
        .body_mut()
        .read_json()
        .context("Failed to parse GitHub /user JSON response")?;
    Ok(user.login)
}

/// Ensures the private repository `repo_name` exists on GitHub, creating it if needed when a PAT or `gh` CLI is available.
pub fn ensure_remote_repo_exists(
    owner: &str,
    repo_name: &str,
    token: Option<&str>,
    auth: &AuthMethod,
) -> Result<RepoEnsureResult> {
    let owner = owner.trim();
    let repo_name = if repo_name.trim().is_empty() {
        "save_game_manager"
    } else {
        repo_name.trim()
    };

    if owner.is_empty() {
        bail!("GitHub username is required before checking or creating the repository.");
    }

    let remote_url = remote_url_for_auth(owner, repo_name, auth)
        .context("Could not construct GitHub remote URL")?;

    if let Some(tok) = token.map(str::trim).filter(|s| !s.is_empty()) {
        let check_url = format!("https://api.github.com/repos/{owner}/{repo_name}");
        match ureq::get(&check_url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", &format!("Bearer {tok}"))
            .call()
        {
            Ok(_) => {
                return Ok(RepoEnsureResult::AlreadyExists { remote_url });
            }
            Err(ureq::Error::StatusCode(404)) => {
                let payload = CreateRepoReq {
                    name: repo_name,
                    private: true,
                    description: "Private game save repository managed by Save Game Manager",
                    auto_init: false,
                };
                let mut create_resp = ureq::post("https://api.github.com/user/repos")
                    .header("User-Agent", USER_AGENT)
                    .header("Accept", "application/vnd.github+json")
                    .header("Authorization", &format!("Bearer {tok}"))
                    .send_json(&payload)
                    .map_err(|e| anyhow::anyhow!("Failed to create private GitHub repo '{repo_name}': {e}"))?;

                let _created: Value = create_resp.body_mut().read_json().unwrap_or(Value::Null);
                return Ok(RepoEnsureResult::CreatedPrivate { remote_url });
            }
            Err(err) => {
                bail!("GitHub API check for {owner}/{repo_name} failed: {err}");
            }
        }
    }

    // No PAT supplied: probe repository over SSH via `git ls-remote`.
    if let Some(key_path) = auth.ssh_private_key_path() {
        if !key_path.exists() {
            bail!("SSH private key not found at {}", key_path.display());
        }
        let ls = Command::new("git")
            .arg("ls-remote")
            .arg(&remote_url)
            .env("GIT_SSH_COMMAND", git_ssh_command_for_key(key_path))
            .output()
            .context("Failed to run git ls-remote over SSH")?;

        if ls.status.success() {
            return Ok(RepoEnsureResult::AlreadyExists { remote_url });
        }

        // Try `gh repo create` as a fallback if GitHub CLI is installed & authenticated.
        if let Ok(gh_out) = Command::new("gh")
            .args(["repo", "create", repo_name, "--private"])
            .output()
            && gh_out.status.success() {
                return Ok(RepoEnsureResult::CreatedPrivate { remote_url });
            }

        let stderr = String::from_utf8_lossy(&ls.stderr);
        return Ok(RepoEnsureResult::NeedsPatOrManual {
            remote_url,
            message: format!(
                "Repository '{owner}/{repo_name}' was not reachable via SSH ({}). \
                 Provide a Personal Access Token once to auto-create it as a private repo, \
                 or create a private repo named '{repo_name}' on GitHub.",
                stderr.trim()
            ),
        });
    }

    bail!("Provide a Personal Access Token or an SSH key to connect to GitHub.");
}

/// Uploads an SSH public key to the authenticated user's GitHub account (`POST /user/keys`).
pub fn upload_ssh_public_key_to_github(token: &str, title: &str, public_key: &str) -> Result<()> {
    let tok = token.trim();
    if tok.is_empty() {
        bail!("A Personal Access Token with write:public_key scope is required to upload an SSH key.");
    }
    let body = json!({
        "title": title,
        "key": public_key.trim(),
    });
    ureq::post("https://api.github.com/user/keys")
        .header("User-Agent", USER_AGENT)
        .header("Accept", "application/vnd.github+json")
        .header("Authorization", &format!("Bearer {tok}"))
        .send_json(&body)
        .map_err(|e| anyhow::anyhow!("Failed to upload SSH public key to GitHub: {e}"))?;
    Ok(())
}
