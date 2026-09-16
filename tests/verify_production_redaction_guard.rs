use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

// Issue #145: scripts/verify-production.sh used to pipe journalctl into `rg`
// with stderr suppressed. A missing `rg` exited 127 inside `if`, and a failing
// journalctl produced an empty stream — both silently skipped the redaction
// guard and let the script report success. The guard must fail closed and each
// outcome must be mechanically distinguishable.

const EXPECTED_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const BASE_TOOLS: &[&str] = &[
    "env",
    "bash",
    "sha256sum",
    "readlink",
    "awk",
    "mktemp",
    "rm",
];

struct Fixture {
    root: TempDir,
    stub_dir: PathBuf,
    tools_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("fixture tempdir");
        let stub_dir = root.path().join("stubs");
        let tools_dir = root.path().join("tools");
        fs::create_dir_all(&stub_dir).unwrap();
        fs::create_dir_all(&tools_dir).unwrap();

        // Real binaries the script needs besides the stubbed service tools.
        for tool in BASE_TOOLS {
            symlink(real_tool(tool), tools_dir.join(tool)).unwrap();
        }

        // Active service.
        write_executable(&stub_dir.join("systemctl"), "#!/bin/sh\nexit 0\n");
        // tunnel-client doctor emits a trivial JSON document.
        write_executable(
            &stub_dir.join("tunnel-client"),
            "#!/bin/sh\nprintf '{}\\n'\n",
        );

        let release_dir = root.path().join("release");
        fs::create_dir_all(&release_dir).unwrap();
        let target = release_dir.join(format!("reading-mcp-{EXPECTED_SHA}"));
        fs::write(&target, b"candidate binary bytes").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&target, release_dir.join("reading-mcp")).unwrap();

        fs::create_dir_all(root.path().join("state")).unwrap();
        fs::create_dir_all(root.path().join("profile")).unwrap();
        fs::write(root.path().join("env-file"), "TUNNEL_TOKEN=fixture\n").unwrap();

        Self {
            root,
            stub_dir,
            tools_dir,
        }
    }

    fn journalctl(&self, body: &str) {
        write_executable(&self.stub_dir.join("journalctl"), body);
    }

    fn run(&self, include_grep: bool) -> (i32, String, String) {
        if include_grep {
            symlink(real_tool("grep"), self.tools_dir.join("grep")).unwrap();
        }
        let path = format!("{}:{}", self.stub_dir.display(), self.tools_dir.display());
        let output = Command::new("bash")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/verify-production.sh"
            ))
            .env_clear()
            .env("PATH", &path)
            .env("RELEASE_BIN_DIR", self.root.path().join("release"))
            .env("EXPECTED_VERSION", "0.4.11")
            .env("EXPECTED_SHA", EXPECTED_SHA)
            .env(
                "EXPECTED_BINARY_SHA256",
                sha256(
                    &self
                        .root
                        .path()
                        .join("release")
                        .join(format!("reading-mcp-{EXPECTED_SHA}")),
                ),
            )
            .env("SERVICE_NAME", "reading-mcp-fixture.service")
            .env("TUNNEL_CLIENT", self.stub_dir.join("tunnel-client"))
            .env("TUNNEL_PROFILE_DIR", self.root.path().join("profile"))
            .env("TUNNEL_PROFILE", "fixture")
            .env("STATE_DIR", self.root.path().join("state"))
            .env("ENV_FILE", self.root.path().join("env-file"))
            .output()
            .expect("verify-production.sh should spawn");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

#[test]
fn missing_journalctl_fails_closed_before_guard() {
    let fixture = Fixture::new();
    // No journalctl stub: the dependency check must stop the run.
    let (code, stdout, stderr) = fixture.run(true);
    assert_ne!(
        code, 0,
        "missing journalctl must fail closed: {stdout}{stderr}"
    );
    assert!(
        stderr.contains("journalctl is required"),
        "expected dependency-missing message, got: {stderr}"
    );
    assert!(!stdout.contains("redaction_guard=pass"));
}

#[test]
fn missing_grep_fails_closed_before_guard() {
    let fixture = Fixture::new();
    fixture.journalctl("#!/bin/sh\nprintf 'routine service log line\\n'\n");
    let (code, stdout, stderr) = fixture.run(false);
    assert_ne!(code, 0, "missing grep must fail closed: {stdout}{stderr}");
    assert!(
        stderr.contains("grep is required"),
        "expected dependency-missing message, got: {stderr}"
    );
    assert!(!stdout.contains("redaction_guard=pass"));
}

#[test]
fn sensitive_log_line_trips_the_guard() {
    let fixture = Fixture::new();
    fixture
        .journalctl("#!/bin/sh\nprintf 'ok\\nauthorization: bearer abcdef0123456789\\nmore\\n'\n");
    let (code, stdout, stderr) = fixture.run(true);
    assert_ne!(code, 0, "sensitive hit must exit non-zero");
    assert!(
        stderr.contains("redaction guard"),
        "expected guard-hit message, got: {stderr}"
    );
    assert!(!stdout.contains("redaction_guard=pass"));
    assert!(!stdout.contains("service=active"));
}

#[test]
fn journalctl_query_failure_is_a_distinct_error() {
    let fixture = Fixture::new();
    fixture.journalctl("#!/bin/sh\nprintf 'unit not found\\n' >&2\nexit 3\n");
    let (code, stdout, stderr) = fixture.run(true);
    assert_ne!(code, 0, "journalctl failure must exit non-zero");
    assert!(
        stderr.contains("journalctl query failed"),
        "expected distinct query-failure message, got: {stderr}"
    );
    assert!(
        !stderr.contains("matched a secret/body redaction guard")
            && !stdout.contains("redaction_guard=pass"),
        "query failure must not masquerade as guard hit or clean pass"
    );
}

#[test]
fn clean_logs_pass_only_after_guard_ran() {
    let fixture = Fixture::new();
    fixture.journalctl("#!/bin/sh\nprintf 'tunnel established\\nheartbeat ok\\n'\n");
    let (code, stdout, stderr) = fixture.run(true);
    assert_eq!(code, 0, "clean run must succeed: {stdout}{stderr}");
    assert!(
        stdout.contains("redaction_guard=pass"),
        "clean pass must carry the explicit guard line: {stdout}"
    );
    assert!(stdout.contains("service=active"));
}

fn write_executable(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn real_tool(name: &str) -> PathBuf {
    for dir in std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default()
    {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!("required tool {name} not found on PATH");
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("sha256sum should run");
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .next()
        .expect("sha256sum output")
        .to_string()
}
