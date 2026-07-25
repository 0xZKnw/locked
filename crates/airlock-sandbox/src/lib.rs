//! Where the agent's code runs, and where the network does not exist.
//!
//! The container is started with `--network none`: no interface, no resolver, no
//! route. Code the model writes and executes fails at the syscall, not at a
//! policy check. That is the difference between a rule and an invariant.
//!
//! This crate has no HTTP dependency — see `scripts/check-egress-isolation.sh`.
//! It drives the Docker CLI as a subprocess rather than talking to the daemon
//! socket, which keeps the dependency surface to `tokio::process`.

use async_trait::async_trait;
use std::path::{Path, PathBuf};

/// What the sandbox is, for one run.
///
/// The image is declared per run rather than baked in: a run that analyses
/// parquet files and a run that builds a Rust crate want different environments,
/// and pinning the image in the journal is part of making a run reproducible.
#[derive(Clone, Debug)]
pub struct SandboxSpec {
    pub image: String,
    /// Host directory mounted as the run workspace. The only path that crosses
    /// the boundary — the journal deliberately lives outside it.
    pub workspace: PathBuf,
    pub memory_limit: String,
    pub pids_limit: u32,
}

impl SandboxSpec {
    pub fn new(image: impl Into<String>, workspace: PathBuf) -> Self {
        Self {
            image: image.into(),
            workspace,
            memory_limit: "2g".into(),
            pids_limit: 512,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox io: {0}")]
    Io(#[from] std::io::Error),
    #[error("sandbox unavailable: {0}")]
    Unavailable(String),
    #[error("path {0} escapes the run workspace")]
    PathEscape(String),
}

/// What a sandbox actually guarantees — stated, not assumed.
///
/// This is the honest core of running without a container runtime. Two of these
/// tiers deny the network absolutely; they just do it in very different ways, and
/// a reader of the journal is entitled to know which one a run had.
#[derive(Clone, Debug, PartialEq)]
pub enum Isolation {
    /// A container with no network interface at all. Code the model writes can
    /// run, and it fails at the syscall if it reaches for a socket. The kernel
    /// enforces this; nothing here has to be trusted.
    Container { image: String },
    /// A directory this process confines writes to, and no code execution at
    /// all. The network invariant holds trivially — nothing runs, so nothing can
    /// call out. Weaker than a container in what it *offers*, not in what it
    /// permits: the shell is absent rather than restrained.
    Workspace,
    /// No filesystem and no shell. TAP is the run's entire surface.
    None,
}

impl Isolation {
    /// A short, stable label for the journal and the window.
    pub fn label(&self) -> String {
        match self {
            Self::Container { image } => format!("container:{image}"),
            Self::Workspace => "workspace".into(),
            Self::None => "none".into(),
        }
    }
}

#[async_trait]
pub trait Sandbox: Send + Sync {
    async fn exec(&self, command: &str) -> Result<ExecOutput, SandboxError>;
    async fn read(&self, path: &str) -> Result<String, SandboxError>;
    async fn write(&self, path: &str, contents: &str) -> Result<(), SandboxError>;
    async fn glob(&self, pattern: &str) -> Result<Vec<String>, SandboxError>;
    /// What this run's receipts will claim. Every backend must answer.
    fn isolation(&self) -> Isolation;
    /// Release whatever the backend holds. A no-op for the ones that hold
    /// nothing, so callers can tear down without knowing which they got.
    async fn shutdown(&self) {}
}

/// Reject a model-supplied path before it reaches the filesystem.
///
/// Shared by both backends because it is the same decision either way: a path
/// with `..` in it, or an absolute one, is refused rather than resolved. The
/// container also confines writes by construction; the local workspace has only
/// this, so it is checked again after joining (see `LocalWorkspace::resolve`).
fn confine(path: &str) -> Result<&str, SandboxError> {
    let bad = path.is_empty()
        || path.contains("..")
        || path.starts_with('/')
        || path.starts_with('\\')
        // `C:\...` — an absolute Windows path slips past the checks above.
        || path.chars().nth(1) == Some(':');
    if bad {
        return Err(SandboxError::PathEscape(path.to_string()));
    }
    Ok(path)
}

/// A run with no sandbox at all.
///
/// This is *not* "the sandbox, disabled". A run wired with this one is not
/// offered `exec` or the `fs_*` tools in the first place — see
/// `airlock_tools::Capabilities`. The agent's power is reduced to calling
/// credentials it does not hold, which is a legitimate way to run and the
/// project's first milestone.
///
/// The methods below exist only to satisfy the trait; nothing should reach them.
pub struct NoSandbox;

#[async_trait]
impl Sandbox for NoSandbox {
    async fn exec(&self, _: &str) -> Result<ExecOutput, SandboxError> {
        Err(Self::refuse())
    }
    async fn read(&self, _: &str) -> Result<String, SandboxError> {
        Err(Self::refuse())
    }
    async fn write(&self, _: &str, _: &str) -> Result<(), SandboxError> {
        Err(Self::refuse())
    }
    async fn glob(&self, _: &str) -> Result<Vec<String>, SandboxError> {
        Err(Self::refuse())
    }
    fn isolation(&self) -> Isolation {
        Isolation::None
    }
}

impl NoSandbox {
    fn refuse() -> SandboxError {
        SandboxError::Unavailable(
            "this run has no sandbox, so filesystem and shell tools were never offered"
                .into(),
        )
    }
}

// ---------------------------------------------------------------------------
// The container-free backend
// ---------------------------------------------------------------------------

/// A workspace directory, confined, with no code execution.
///
/// This exists so the app runs on a machine with no container runtime installed.
/// The temptation is to keep `exec` and drop the container — that would hand the
/// model a real shell on the user's machine with a live network, which is the
/// exact opposite of what this project claims. So the trade is made in the other
/// direction: keep the guarantee, drop the capability. `exec` is not offered at
/// all at this tier (see `Capabilities::FILES`), and the method below exists only
/// to satisfy the trait.
pub struct LocalWorkspace {
    root: PathBuf,
}

impl LocalWorkspace {
    /// The root is created if absent and canonicalised once, so every later
    /// comparison is against a real path rather than a string.
    pub fn open(root: PathBuf) -> Result<Self, SandboxError> {
        std::fs::create_dir_all(&root)?;
        Ok(Self {
            root: root.canonicalize()?,
        })
    }

    /// Join a model-supplied path onto the root, then prove the result is still
    /// inside it.
    ///
    /// The textual check in `confine` is not enough on its own: a symlink placed
    /// in the workspace could point anywhere, and only resolving the real path
    /// catches that. Canonicalising the parent handles files that do not exist
    /// yet, which is most writes.
    fn resolve(&self, path: &str) -> Result<PathBuf, SandboxError> {
        let joined = self.root.join(confine(path)?);

        let anchor = match joined.canonicalize() {
            Ok(real) => real,
            Err(_) => {
                let parent = joined
                    .parent()
                    .ok_or_else(|| SandboxError::PathEscape(path.to_string()))?;
                std::fs::create_dir_all(parent)?;
                parent
                    .canonicalize()?
                    .join(joined.file_name().ok_or_else(|| {
                        SandboxError::PathEscape(path.to_string())
                    })?)
            }
        };

        if !anchor.starts_with(&self.root) {
            return Err(SandboxError::PathEscape(path.to_string()));
        }
        Ok(anchor)
    }

    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.path().is_dir() {
                Self::walk(&entry.path(), &rel, out);
            } else {
                out.push(rel);
            }
        }
    }
}

#[async_trait]
impl Sandbox for LocalWorkspace {
    async fn exec(&self, _: &str) -> Result<ExecOutput, SandboxError> {
        Err(SandboxError::Unavailable(
            "this run has a workspace but no shell: running code needs a sandbox that \
             can deny it a network, and none is available here. Files, and TAP for \
             anything outbound."
                .into(),
        ))
    }

    async fn read(&self, path: &str) -> Result<String, SandboxError> {
        Ok(std::fs::read_to_string(self.resolve(path)?)?)
    }

    async fn write(&self, path: &str, contents: &str) -> Result<(), SandboxError> {
        std::fs::write(self.resolve(path)?, contents)?;
        Ok(())
    }

    async fn glob(&self, pattern: &str) -> Result<Vec<String>, SandboxError> {
        let mut all = Vec::new();
        Self::walk(&self.root, "", &mut all);
        all.retain(|p| glob_match(pattern, p));
        all.sort();
        Ok(all)
    }

    fn isolation(&self) -> Isolation {
        Isolation::Workspace
    }
}

/// `*` within a segment, `**` across segments, `?` for one character.
///
/// Hand-rolled because this crate is deliberately dependency-free — it is the one
/// place in the workspace that must be trivially auditable, and a glob crate is a
/// lot of surface to pull in for forty lines.
fn glob_match(pattern: &str, path: &str) -> bool {
    fn seg(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => seg(&p[1..], s) || (!s.is_empty() && seg(p, &s[1..])),
            Some('?') => !s.is_empty() && seg(&p[1..], &s[1..]),
            Some(c) => s.first() == Some(c) && seg(&p[1..], &s[1..]),
        }
    }

    let pp: Vec<&str> = pattern.split('/').collect();
    let sp: Vec<&str> = path.split('/').collect();

    fn walk(pp: &[&str], sp: &[&str]) -> bool {
        match pp.first() {
            None => sp.is_empty(),
            Some(&"**") => {
                // Zero or more path segments.
                (0..=sp.len()).any(|skip| walk(&pp[1..], &sp[skip..]))
            }
            Some(p) => {
                !sp.is_empty()
                    && seg(&p.chars().collect::<Vec<_>>(), &sp[0].chars().collect::<Vec<_>>())
                    && walk(&pp[1..], &sp[1..])
            }
        }
    }

    walk(&pp, &sp)
}

pub struct DockerSandbox {
    spec: SandboxSpec,
    container: String,
}

impl DockerSandbox {
    /// Start the container. Every hardening flag here is load-bearing; the one
    /// that carries the pitch is `--network none`.
    ///
    /// `id` names the container, so it should identify the conversation rather
    /// than the individual run: a chat that writes a file in one message and
    /// reads it back in the next needs the same box both times.
    pub async fn start(spec: SandboxSpec, id: &str) -> Result<Self, SandboxError> {
        let container = format!("airlock-{id}");
        let workspace = spec
            .workspace
            .to_str()
            .ok_or_else(|| SandboxError::Unavailable("workspace path is not UTF-8".into()))?;

        // A container left behind by a crashed run still holds this name. Take
        // it back rather than failing — the workspace is on the host, so nothing
        // of value lives inside the old one.
        let _ = tokio::process::Command::new("docker")
            .args(["rm", "--force", &container])
            .output()
            .await;

        let status = tokio::process::Command::new("docker")
            .args([
                "run",
                "--detach",
                // Exit removes it. Paired with `stop` below, so a normal run
                // cleans up immediately and a crashed one cleans up on restart.
                "--rm",
                "--name",
                &container,
                // The invariant.
                "--network",
                "none",
                // Ordinary hardening — not the pitch, but no reason to skip it.
                "--cap-drop",
                "ALL",
                "--security-opt",
                "no-new-privileges",
                "--memory",
                &spec.memory_limit,
                "--pids-limit",
                &spec.pids_limit.to_string(),
                "--volume",
                &format!("{workspace}:/workspace"),
                "--workdir",
                "/workspace",
                &spec.image,
                "sleep",
                "infinity",
            ])
            .status()
            .await?;

        if !status.success() {
            return Err(SandboxError::Unavailable(format!(
                "docker run failed for image {}",
                spec.image
            )));
        }
        Ok(Self { spec, container })
    }

    pub fn spec(&self) -> &SandboxSpec {
        &self.spec
    }

    /// Tear the container down.
    ///
    /// Explicit rather than a `Drop`, because removing a container is an await
    /// and `Drop` cannot be async. Every caller must remember — the alternative
    /// is one `sleep infinity` process left running per prompt, forever.
    pub async fn stop(&self) {
        let _ = tokio::process::Command::new("docker")
            .args(["rm", "--force", &self.container])
            .output()
            .await;
    }

    /// The container already confines writes to its own mounts, so this is
    /// belt-and-braces — but the tool result is friendlier than a mount error,
    /// and `..` in a path is worth surfacing rather than silently resolving.
    fn confine(path: &str) -> Result<String, SandboxError> {
        Ok(format!("/workspace/{}", confine(path)?))
    }

    async fn docker_exec(&self, argv: &[&str]) -> Result<ExecOutput, SandboxError> {
        let mut cmd = tokio::process::Command::new("docker");
        cmd.arg("exec").arg(&self.container).args(argv);
        let out = cmd.output().await?;
        Ok(ExecOutput {
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            exit_code: out.status.code(),
        })
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    async fn exec(&self, command: &str) -> Result<ExecOutput, SandboxError> {
        self.docker_exec(&["sh", "-lc", command]).await
    }

    async fn read(&self, path: &str) -> Result<String, SandboxError> {
        let p = Self::confine(path)?;
        Ok(self.docker_exec(&["cat", &p]).await?.stdout)
    }

    async fn write(&self, path: &str, contents: &str) -> Result<(), SandboxError> {
        let p = Self::confine(path)?;
        // Contents go over stdin, never interpolated into a shell command — the
        // agent chooses this string, so anything with a quote or a `$` in it
        // would otherwise be a command-injection primitive against our own
        // sandbox invocation.
        let mut child = tokio::process::Command::new("docker")
            .args(["exec", "--interactive", &self.container, "sh", "-c"])
            .arg(format!("cat > {}", shell_single_quote(&p)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;

        {
            use tokio::io::AsyncWriteExt as _;
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| SandboxError::Unavailable("no stdin on docker exec".into()))?;
            stdin.write_all(contents.as_bytes()).await?;
            stdin.shutdown().await?;
        }

        let out = child.wait_with_output().await?;
        if !out.status.success() {
            return Err(SandboxError::Unavailable(
                String::from_utf8_lossy(&out.stderr).into_owned(),
            ));
        }
        Ok(())
    }

    async fn glob(&self, pattern: &str) -> Result<Vec<String>, SandboxError> {
        let out = self
            .docker_exec(&["sh", "-lc", &format!("ls -1 {pattern} 2>/dev/null || true")])
            .await?;
        Ok(out.stdout.lines().map(str::to_string).collect())
    }

    fn isolation(&self) -> Isolation {
        Isolation::Container {
            image: self.spec.image.clone(),
        }
    }

    async fn shutdown(&self) {
        self.stop().await;
    }
}

/// The default image, when a container tier is chosen and none is named.
pub const DEFAULT_IMAGE: &str = "python:3.12-slim";

/// Open the strongest sandbox this machine can actually provide.
///
/// The app must run on a machine with no container runtime installed — that is
/// the whole reason this function exists. What it must *not* do is keep `exec`
/// and quietly drop the boundary: that would hand the model a live shell on the
/// user's machine with a working network, which is the opposite of the claim
/// this project makes. So when the container is unavailable the guarantee is
/// kept and the capability is dropped.
///
/// `AIRLOCK_SANDBOX` pins the tier — `auto` (default), `container`, `workspace`,
/// `none`. Asking for `container` explicitly is an error if it is unavailable,
/// because a silent downgrade of something you asked for by name is worse than a
/// refusal. `AIRLOCK_IMAGE=NONE` is still honoured as the older spelling of
/// `none`.
///
/// Returns the sandbox and the tool surface that matches it. They are chosen
/// together on purpose: a tier and a capability set that disagree is how a run
/// ends up offering something nobody is enforcing.
pub async fn open_best(
    workspace: PathBuf,
    container_id: &str,
) -> Result<(Box<dyn Sandbox>, SurfaceHint), SandboxError> {
    let image = std::env::var("AIRLOCK_IMAGE").unwrap_or_else(|_| DEFAULT_IMAGE.into());
    let requested = std::env::var("AIRLOCK_SANDBOX").unwrap_or_else(|_| {
        if image == "NONE" { "none".into() } else { "auto".into() }
    });

    let local = |ws: PathBuf| -> Result<(Box<dyn Sandbox>, SurfaceHint), SandboxError> {
        Ok((Box::new(LocalWorkspace::open(ws)?), SurfaceHint::Files))
    };

    match requested.as_str() {
        "none" => Ok((Box::new(NoSandbox), SurfaceHint::TapOnly)),
        "workspace" => local(workspace),

        "container" => {
            std::fs::create_dir_all(&workspace)?;
            let sb = DockerSandbox::start(SandboxSpec::new(&image, workspace), container_id).await?;
            Ok((Box::new(sb), SurfaceHint::Full))
        }

        // auto
        _ => {
            if container_runtime_available().await {
                std::fs::create_dir_all(&workspace)?;
                match DockerSandbox::start(
                    SandboxSpec::new(&image, workspace.clone()),
                    container_id,
                )
                .await
                {
                    Ok(sb) => return Ok((Box::new(sb), SurfaceHint::Full)),
                    // The runtime answered but the container did not start —
                    // a missing image, most likely. Fall through rather than
                    // refuse to run at all.
                    Err(_) => return local(workspace),
                }
            }
            local(workspace)
        }
    }
}

/// Which tool surface a chosen sandbox can honour.
///
/// Deliberately not `airlock_tools::Capabilities` — this crate does not depend on
/// that one, and inverting it would put the tool list downstream of the sandbox.
/// The binaries map this onto the real capability set.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SurfaceHint {
    TapOnly,
    Files,
    Full,
}

/// Is a container runtime actually usable right now?
///
/// Installed is not the same as running — Docker Desktop being closed is the
/// common case, and `docker info` is the cheapest question that distinguishes
/// them. Callers use this to pick a backend rather than to fail.
pub async fn container_runtime_available() -> bool {
    matches!(
        tokio::process::Command::new("docker")
            .arg("info")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await,
        Ok(s) if s.success()
    )
}

/// Wrap a path in single quotes for `sh -c`, escaping any embedded quote.
/// `confine` already rejects the dangerous shapes; this closes the rest.
fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_survives_a_hostile_filename() {
        assert_eq!(shell_single_quote("a'b"), r"'a'\''b'");
        assert_eq!(shell_single_quote("x; rm -rf /"), "'x; rm -rf /'");
    }

    #[test]
    fn paths_are_confined() {
        assert!(DockerSandbox::confine("notes.md").is_ok());
        assert!(DockerSandbox::confine("../../etc/passwd").is_err());
        assert!(DockerSandbox::confine("/etc/passwd").is_err());
        // Absolute Windows paths slip past a leading-slash check.
        assert!(confine(r"C:\Windows\System32\drivers\etc\hosts").is_err());
        assert!(confine(r"\\server\share").is_err());
    }

    #[test]
    fn glob_matches_segments_and_depth() {
        assert!(glob_match("*.py", "check.py"));
        assert!(!glob_match("*.py", "src/check.py"));
        assert!(glob_match("**/*.py", "src/deep/check.py"));
        assert!(glob_match("**/*.py", "check.py"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/a/main.rs"));
        assert!(glob_match("data?.csv", "data1.csv"));
        assert!(!glob_match("data?.csv", "data12.csv"));
    }

    /// The local backend has no container to fall back on, so the path check is
    /// the whole defence. A symlink is the case a textual check cannot see.
    #[tokio::test]
    async fn local_workspace_refuses_to_leave_its_root() {
        let root = std::env::temp_dir().join(format!("airlock-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let ws = LocalWorkspace::open(root.clone()).unwrap();

        assert!(ws.write("notes.md", "hello").await.is_ok());
        assert_eq!(ws.read("notes.md").await.unwrap(), "hello");
        assert!(ws.write("../escaped.md", "nope").await.is_err());
        assert!(ws.read("/etc/passwd").await.is_err());

        // The shell is absent, not restrained.
        assert!(ws.exec("echo hi").await.is_err());
        assert_eq!(ws.isolation(), Isolation::Workspace);

        assert_eq!(ws.glob("*.md").await.unwrap(), vec!["notes.md".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
