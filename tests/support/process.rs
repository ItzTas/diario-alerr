use std::env;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use monitor_hub_plugin::{MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE};
use serde_json::Value;
use tempfile::TempDir;

pub const COOKIE: (&str, &str) = (MAGIC_COOKIE_KEY, MAGIC_COOKIE_VALUE);

const HANDSHAKE_LIMIT: Duration = Duration::from_secs(30);
const PIPE_LIMIT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(10);
const STOP_GRACE: Duration = Duration::from_secs(5);

pub struct RunningService {
    running: RunningPlugin,
    _workdir: TempDir,
}

pub struct RunningPlugin {
    pub child: Child,
    pub socket: PathBuf,
    output: Output,
}

pub struct Exited {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

struct Output {
    first_line: Receiver<String>,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
}

pub fn spawn_plugin(env: &[(&str, &str)]) -> RunningPlugin {
    let mut with_cookie = vec![COOKIE];
    with_cookie.extend_from_slice(env);
    let mut plugin = start(&["plugin"], &with_cookie);
    let Ok(line) = plugin.output.first_line.recv_timeout(HANDSHAKE_LIMIT) else {
        plugin.fail("the plugin printed no handshake line within 30 seconds");
    };
    let Some(socket) = handshake_socket(&line) else {
        plugin.fail(&format!(
            "the first stdout line is not a handshake: {line:?}"
        ));
    };
    plugin.socket = socket;
    plugin
}

pub fn run_plugin(args: &[&str], env: &[(&str, &str)], limit: Duration) -> Exited {
    start(args, env).exit_within(limit)
}

pub fn spawn_service(program: &Path, env: &[(&str, &str)]) -> RunningService {
    let workdir = tempfile::tempdir().expect("an empty working directory");
    let running = start_program(program, workdir.path(), env);
    RunningService {
        running,
        _workdir: workdir,
    }
}

pub fn terminate(child: &mut Child) {
    let status = send_term(child).expect("run kill");
    assert!(status.success(), "kill -TERM failed: {status}");
}

fn send_term(child: &Child) -> std::io::Result<ExitStatus> {
    Command::new("sh")
        .args(["-c", "kill -TERM \"$1\"", "sh", &child.id().to_string()])
        .status()
}

pub fn require_sh() -> bool {
    let installed = Command::new("sh")
        .args(["-c", "command -v kill"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if installed {
        return true;
    }
    if env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        panic!("sh with kill is required when CI is set");
    }
    eprintln!("skipping: sh with kill is not available");
    false
}

pub fn error_lines(stderr: &str) -> Vec<Value> {
    stderr
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|entry| entry["@level"] == "error")
        .collect()
}

impl RunningPlugin {
    pub fn exit_within(&mut self, limit: Duration) -> Exited {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll the plugin") {
                return Exited {
                    status,
                    stdout: collected(&self.output.stdout),
                    stderr: collected(&self.output.stderr),
                };
            }
            if Instant::now() >= deadline {
                self.fail(&format!("the plugin did not exit within {limit:?}"));
            }
            thread::sleep(POLL);
        }
    }

    fn fail(&mut self, problem: &str) -> ! {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let stderr = collected(&self.output.stderr);
        panic!("{problem}\nplugin stderr:\n{stderr}");
    }
}

impl Drop for RunningPlugin {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl RunningService {
    pub fn stop(&mut self) -> Exited {
        self.ensure_running();
        terminate(&mut self.running.child);
        let deadline = Instant::now() + STOP_GRACE;
        while self.still_running() {
            if Instant::now() >= deadline {
                self.fail(&format!("the service did not stop within {STOP_GRACE:?}"));
            }
            thread::sleep(POLL);
        }
        self.running.exit_within(Duration::ZERO)
    }

    pub fn ensure_running(&mut self) {
        let exited = self.running.child.try_wait().expect("poll the service");
        if let Some(status) = exited {
            self.fail(&format!("the service exited early: {status}"));
        }
    }

    pub fn fail(&mut self, problem: &str) -> ! {
        let _ = self.running.child.kill();
        let _ = self.running.child.wait();
        let stdout = collected(&self.running.output.stdout);
        let stderr = collected(&self.running.output.stderr);
        panic!("{problem}\nservice stdout:\n{stdout}\nservice stderr:\n{stderr}");
    }

    fn still_running(&mut self) -> bool {
        matches!(self.running.child.try_wait(), Ok(None))
    }
}

impl Drop for RunningService {
    fn drop(&mut self) {
        if !self.still_running() || send_term(&self.running.child).is_err() {
            return;
        }
        let deadline = Instant::now() + STOP_GRACE;
        while self.still_running() && Instant::now() < deadline {
            thread::sleep(POLL);
        }
    }
}

fn start(args: &[&str], env: &[(&str, &str)]) -> RunningPlugin {
    spawn(
        Command::new(env!("CARGO_BIN_EXE_diario-alerr")).args(args),
        env,
    )
}

fn start_program(program: &Path, workdir: &Path, env: &[(&str, &str)]) -> RunningPlugin {
    spawn(Command::new(program).current_dir(workdir), env)
}

fn spawn(command: &mut Command, env: &[(&str, &str)]) -> RunningPlugin {
    let mut child = command
        .env_clear()
        .env("PATH", env::var_os("PATH").unwrap_or_default())
        .envs(env.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the program");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let (first_line, stdout) = read_stdout(stdout);
    RunningPlugin {
        child,
        socket: PathBuf::new(),
        output: Output {
            first_line,
            stdout,
            stderr: read_all(stderr),
        },
    }
}

fn read_stdout(stdout: impl Read + Send + 'static) -> (Receiver<String>, Receiver<String>) {
    let (first_sender, first_line) = mpsc::channel();
    let (all_sender, all) = mpsc::channel();
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut text = String::new();
        let _ = reader.read_line(&mut text);
        let _ = first_sender.send(text.clone());
        let _ = reader.read_to_string(&mut text);
        let _ = all_sender.send(text);
    });
    (first_line, all)
}

fn read_all(pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (sender, text) = mpsc::channel();
    thread::spawn(move || {
        let mut all = String::new();
        let _ = BufReader::new(pipe).read_to_string(&mut all);
        let _ = sender.send(all);
    });
    text
}

fn collected(text: &Receiver<String>) -> String {
    text.recv_timeout(PIPE_LIMIT)
        .unwrap_or_else(|_| "<the pipe stayed open>".to_owned())
}

fn handshake_socket(line: &str) -> Option<PathBuf> {
    let fields: Vec<&str> = line.trim().split('|').collect();
    let ["1", "1", "unix", socket, "grpc"] = fields[..] else {
        return None;
    };
    Some(PathBuf::from(socket))
}
