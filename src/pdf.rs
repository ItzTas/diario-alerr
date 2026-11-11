use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use reqwest::Response;
use tempfile::NamedTempFile;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::gazette::{GazetteError, get};

pub(crate) const PDF_TIMEOUT: Duration = Duration::from_secs(300);
const STDERR_LIMIT: usize = 32 << 10;
const EXCERPT_CHARS: usize = 200;

pub(crate) async fn download_pages(
    http: &reqwest::Client,
    url: &str,
) -> Result<Vec<String>, GazetteError> {
    download_pages_in(http, url, &std::env::temp_dir()).await
}

async fn download_pages_in(
    http: &reqwest::Client,
    url: &str,
    dir: &Path,
) -> Result<Vec<String>, GazetteError> {
    let response = get(http, url).await?;
    let file = tempfile::Builder::new()
        .prefix("edition-")
        .suffix(".pdf")
        .tempfile_in(dir)?;
    save(response, &file).await?;
    extract_pages(file.path()).await
}

async fn save(mut response: Response, file: &NamedTempFile) -> Result<(), GazetteError> {
    let mut out = tokio::fs::File::from_std(file.as_file().try_clone()?);
    while let Some(chunk) = response.chunk().await? {
        out.write_all(&chunk).await?;
    }
    out.flush().await?;
    Ok(())
}

pub(crate) async fn extract_pages(path: &Path) -> Result<Vec<String>, GazetteError> {
    let mut command = Command::new("pdftotext");
    command.args(["-enc", "UTF-8"]).arg(path).arg("-");
    run(command, path, PDF_TIMEOUT).await
}

async fn run(
    mut command: Command,
    path: &Path,
    limit: Duration,
) -> Result<Vec<String>, GazetteError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        let program = command.as_std().get_program().to_string_lossy();
        GazetteError::Pdftotext(format!("exec: {program:?}: {error}"))
    })?;
    let Ok(output) = tokio::time::timeout(limit, collect(&mut child)).await else {
        stop(&mut child).await?;
        return Err(GazetteError::Pdftotext(format!(
            "timed out after {limit:?}"
        )));
    };
    let (stdout, stderr, status) = output?;
    if !status.success() {
        return Err(GazetteError::Pdftotext(failure(status, &stderr, path)));
    }
    Ok(String::from_utf8_lossy(&stdout)
        .split('\x0C')
        .map(str::to_owned)
        .collect())
}

async fn collect(child: &mut Child) -> io::Result<(Vec<u8>, Vec<u8>, ExitStatus)> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    tokio::try_join!(
        read_prefix(stdout, usize::MAX),
        read_prefix(stderr, STDERR_LIMIT),
        child.wait()
    )
}

async fn read_prefix(pipe: Option<impl AsyncRead + Unpin>, limit: usize) -> io::Result<Vec<u8>> {
    let mut kept = Vec::new();
    let Some(mut pipe) = pipe else {
        return Ok(kept);
    };
    let mut buffer = [0; 8192];
    loop {
        let read = pipe.read(&mut buffer).await?;
        if read == 0 {
            return Ok(kept);
        }
        let room = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
    }
}

async fn stop(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.kill().await
}

fn failure(status: ExitStatus, stderr: &[u8], path: &Path) -> String {
    let state = describe(status);
    let Some(excerpt) = stderr_excerpt(stderr, path) else {
        return state;
    };
    format!("{state}: {excerpt}")
}

fn describe(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit status {code}");
    }
    let Some(signal) = status.signal() else {
        return status.to_string();
    };
    let core = if status.core_dumped() {
        " (core dumped)"
    } else {
        ""
    };
    format!("signal: {}{core}", signal_name(signal))
}

fn signal_name(signal: i32) -> String {
    let names = [
        "",
        "hangup",
        "interrupt",
        "quit",
        "illegal instruction",
        "trace/breakpoint trap",
        "aborted",
        "bus error",
        "floating point exception",
        "killed",
        "user defined signal 1",
        "segmentation fault",
        "user defined signal 2",
        "broken pipe",
        "alarm clock",
        "terminated",
        "stack fault",
        "child exited",
        "continued",
        "stopped (signal)",
        "stopped",
        "stopped (tty input)",
        "stopped (tty output)",
        "urgent I/O condition",
        "CPU time limit exceeded",
        "file size limit exceeded",
        "virtual timer expired",
        "profiling timer expired",
        "window changed",
        "I/O possible",
        "power failure",
        "bad system call",
    ];
    usize::try_from(signal)
        .ok()
        .and_then(|index| names.get(index))
        .filter(|name| !name.is_empty())
        .map_or_else(|| format!("signal {signal}"), |name| (*name).to_owned())
}

fn stderr_excerpt(stderr: &[u8], path: &Path) -> Option<String> {
    let text = String::from_utf8_lossy(stderr);
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let text = text.replace(&*path.to_string_lossy(), &file_name);
    text.lines()
        .map(|line| line.chars().filter(|c| !c.is_control()).collect::<String>())
        .map(|line| line.trim().to_owned())
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(EXCERPT_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::path::Path;
    use std::process::{ExitStatus, Stdio};
    use std::time::Duration;

    use tempfile::TempDir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::process::Command;

    use super::{download_pages_in, failure, run};
    use crate::gazette::GazetteError;

    const FAKE_PDF: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/golden/fake.pdf"
    ));

    fn pdftotext_available() -> bool {
        let installed = std::process::Command::new("pdftotext")
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok();
        if installed {
            return true;
        }
        if std::env::var_os("CI").is_some_and(|value| !value.is_empty()) {
            panic!("pdftotext is required when CI is set");
        }
        eprintln!("skipping: pdftotext is not installed");
        false
    }

    async fn read_request(socket: &mut TcpStream) {
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
        }
    }

    async fn serve_raw(response: Vec<u8>, hold_open: bool) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/edition.pdf", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let response = response.clone();
                tokio::spawn(async move {
                    read_request(&mut socket).await;
                    socket.write_all(&response).await.unwrap();
                    if hold_open {
                        std::future::pending::<()>().await;
                    }
                });
            }
        });
        url
    }

    fn response(declared_length: usize, body: &[u8]) -> Vec<u8> {
        let mut bytes = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn is_gone_or_zombie(pid: &str) -> bool {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return true;
        };
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
    }

    async fn wait_for<T>(mut probe: impl FnMut() -> Option<T>) -> T {
        for _ in 0..500 {
            if let Some(found) = probe() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("condition not reached within 5 s");
    }

    fn pid_from(file: &Path) -> Option<String> {
        let pid = fs::read_to_string(file).ok()?;
        let pid = pid.trim();
        (!pid.is_empty() && pid.ends_with(|c: char| c.is_ascii_digit())).then(|| pid.to_owned())
    }

    fn sleeper(pid_file: &Path) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "echo $$ > '{}.tmp' && mv '{}.tmp' '{}' && exec sleep 30",
            pid_file.display(),
            pid_file.display(),
            pid_file.display()
        ));
        command
    }

    #[tokio::test]
    async fn successful_scans_leave_no_file() {
        if !pdftotext_available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(FAKE_PDF.len(), FAKE_PDF), false).await;

        let pages = download_pages_in(&reqwest::Client::new(), &url, dir.path())
            .await
            .unwrap();

        assert!(
            pages[1].contains("Nomeacao de (TALES) \\ SABINI"),
            "{pages:?}"
        );
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn failed_downloads_leave_no_file() {
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(1000, b"%PDF-1.4\n"), false).await;

        let error = download_pages_in(&reqwest::Client::new(), &url, dir.path())
            .await
            .unwrap_err();

        assert!(matches!(error, GazetteError::Http(_)), "{error}");
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn failed_extractions_leave_no_file_and_no_path() {
        if !pdftotext_available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(9, b"not a pdf"), false).await;

        let error = download_pages_in(&reqwest::Client::new(), &url, dir.path())
            .await
            .unwrap_err();

        let text = error.to_string();
        assert!(text.starts_with("pdftotext: exit status 1"), "{text}");
        assert!(!text.contains(&*dir.path().to_string_lossy()), "{text}");
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn dropped_downloads_leave_no_file() {
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(1000, b"%PDF-1.4\n"), true).await;
        let http = reqwest::Client::new();
        let mut download = Box::pin(download_pages_in(&http, &url, dir.path()));

        let deadline = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                result = &mut download => panic!("the download ended: {result:?}"),
                () = &mut deadline => panic!("the temporary file never appeared"),
                () = tokio::time::sleep(Duration::from_millis(10)) => {}
            }
            if !leftovers(dir.path()).is_empty() {
                break;
            }
        }
        drop(download);

        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn a_missing_extractor_is_named() {
        let command = Command::new("diario-alerr-missing-pdftotext");

        let error = run(command, Path::new("edition.pdf"), Duration::from_secs(5))
            .await
            .unwrap_err();

        assert!(matches!(error, GazetteError::Pdftotext(_)), "{error}");
        assert!(
            error
                .to_string()
                .starts_with(r#"pdftotext: exec: "diario-alerr-missing-pdftotext": "#),
            "{error}"
        );
    }

    #[tokio::test]
    async fn extraction_timeout_kills_and_reaps_the_extractor() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("pid");

        let error = run(
            sleeper(&pid_file),
            Path::new("edition.pdf"),
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "pdftotext: timed out after 2s");
        let pid = pid_from(&pid_file).expect("the extractor started");
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[tokio::test]
    async fn dropped_extractions_kill_the_extractor() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("pid");
        let started = tokio::spawn(run(
            sleeper(&pid_file),
            Path::new("edition.pdf"),
            Duration::from_secs(60),
        ));

        let pid = wait_for(|| pid_from(&pid_file)).await;
        started.abort();

        wait_for(|| is_gone_or_zombie(&pid).then_some(())).await;
    }

    #[test]
    fn failures_read_like_go() {
        let path = Path::new("/var/tmp/secret-dir/edition-abc.pdf");
        let cases: [(i32, &[u8], &str); 7] = [
            (1 << 8, b"", "exit status 1"),
            (3 << 8, b"\n  \n", "exit status 3"),
            (9, b"", "signal: killed"),
            (11 | 0x80, b"", "signal: segmentation fault (core dumped)"),
            (40, b"", "signal: signal 40"),
            (
                1 << 8,
                b"\n\x1b[1mI/O Error:\tCouldn't open file '/var/tmp/secret-dir/edition-abc.pdf'\x07\r\nmore\n",
                "exit status 1: [1mI/O Error:Couldn't open file 'edition-abc.pdf'",
            ),
            (2 << 8, b"Syntax Error \xff\xfe", "exit status 2: Syntax Error \u{FFFD}\u{FFFD}"),
        ];
        for (raw, stderr, want) in cases {
            assert_eq!(
                failure(ExitStatus::from_raw(raw), stderr, path),
                want,
                "{raw} {stderr:?}"
            );
        }
        let long = "é".repeat(300);
        let text = failure(ExitStatus::from_raw(1 << 8), long.as_bytes(), path);
        assert_eq!(text, format!("exit status 1: {}", "é".repeat(200)));
    }
}
