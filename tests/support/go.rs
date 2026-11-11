use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

const TAG: &str = "v1.0.0";
const BINARY: &str = "diario-alerr-go";
const STALE_STAGING: Duration = Duration::from_secs(5 * 60);

pub fn go_v1_binary() -> Option<PathBuf> {
    static BUILT: OnceLock<Option<PathBuf>> = OnceLock::new();
    BUILT.get_or_init(build_once).clone()
}

fn build_once() -> Option<PathBuf> {
    if let Some(reason) = missing_requirement() {
        return unavailable(reason);
    }
    Some(build())
}

fn missing_requirement() -> Option<&'static str> {
    let tag = format!("{TAG}^{{commit}}");
    let requirements = [
        ("go is not on PATH", command("go", &["version"])),
        ("git is not on PATH", command("git", &["--version"])),
        (
            "the tag v1.0.0 is not in this repository",
            git(&["rev-parse", "--verify", "--quiet", &tag]),
        ),
    ];
    requirements
        .into_iter()
        .find_map(|(reason, mut check)| (!succeeds(&mut check)).then_some(reason))
}

fn unavailable(reason: &str) -> Option<PathBuf> {
    if env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        panic!("the Go release is required when CI is set: {reason}");
    }
    eprintln!("skipping: {reason}");
    None
}

fn build() -> PathBuf {
    let source = tempfile::tempdir().expect("a directory for the Go release's source");
    extract(source.path());
    let cache = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("go-{TAG}"));
    fs::create_dir_all(&cache).expect("create the Go release's build directory");
    remove_stale_staging(&cache);
    let staging = cache.join(format!(".{BINARY}-{}", process::id()));
    let mut go = Command::new("go");
    go.args(["build", "-o"])
        .arg(&staging)
        .arg(".")
        .current_dir(source.path());
    expect_success(&mut go, "go build of the Go release");
    let binary = cache.join(BINARY);
    fs::rename(&staging, &binary).expect("move the Go release into place");
    binary
}

fn remove_stale_staging(cache: &Path) {
    let Ok(entries) = fs::read_dir(cache) else {
        return;
    };
    let prefix = format!(".{BINARY}-");
    for entry in entries.flatten() {
        let stale = entry.file_name().to_string_lossy().starts_with(&prefix)
            && entry
                .metadata()
                .and_then(|found| found.modified())
                .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > STALE_STAGING));
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn extract(into: &Path) {
    let mut archive = git(&["archive", "--format=tar", TAG]);
    let mut archive = archive
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run git archive");
    let tar = archive.stdout.take().expect("git archive's stdout");
    let mut untar = Command::new("tar");
    untar.args(["-x", "-C"]).arg(into).stdin(tar);
    let untarred = untar.output().expect("run tar");
    let archived = archive.wait_with_output().expect("wait for git archive");
    check(&archived, "git archive of the tag v1.0.0");
    check(&untarred, "tar extraction of the tag v1.0.0");
}

fn expect_success(command: &mut Command, what: &str) {
    let output = command
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|err| panic!("{what} could not start: {err}"));
    check(&output, what);
}

fn check(output: &Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed ({}):\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn command(program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command.args(args);
    command
}

fn git(args: &[&str]) -> Command {
    let mut git = command("git", &["-C", env!("CARGO_MANIFEST_DIR")]);
    git.args(args);
    git
}

fn succeeds(command: &mut Command) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
