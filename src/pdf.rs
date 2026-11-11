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
const MAX_PDF_BYTES: usize = 256 << 20;
const MAX_TEXT_BYTES: usize = 64 << 20;
const STDERR_LIMIT: usize = 32 << 10;
const EXCERPT_CHARS: usize = 200;

pub(crate) async fn download_pages(
    http: &reqwest::Client,
    url: &str,
) -> Result<Vec<String>, GazetteError> {
    download_pages_in(http, url, &std::env::temp_dir(), MAX_PDF_BYTES).await
}

async fn download_pages_in(
    http: &reqwest::Client,
    url: &str,
    dir: &Path,
    max_bytes: usize,
) -> Result<Vec<String>, GazetteError> {
    let response = get(http, url).await?;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(GazetteError::PdfTooLarge {
            url: url.to_owned(),
            limit: max_bytes,
        });
    }
    let file = tempfile::Builder::new()
        .prefix("edition-")
        .suffix(".pdf")
        .tempfile_in(dir)
        .map_err(|error| GazetteError::CreateTempFile(without_path(error)))?;
    save(response, url, &file, max_bytes).await?;
    extract_pages(file.path()).await
}

fn without_path(error: io::Error) -> io::Error {
    let text = error.to_string();
    let Some((cause, _)) = text.split_once(" at path ") else {
        return error;
    };
    io::Error::new(error.kind(), cause)
}

async fn save(
    mut response: Response,
    url: &str,
    file: &NamedTempFile,
    max_bytes: usize,
) -> Result<(), GazetteError> {
    let write_error = |source| GazetteError::WriteTempFile {
        file: file_name(file.path()),
        source,
    };
    let mut out = tokio::fs::File::from_std(file.as_file().try_clone().map_err(write_error)?);
    let mut written = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|source| GazetteError::Download {
            url: url.to_owned(),
            source,
        })?
    {
        written += chunk.len();
        if written > max_bytes {
            return Err(GazetteError::PdfTooLarge {
                url: url.to_owned(),
                limit: max_bytes,
            });
        }
        out.write_all(&chunk).await.map_err(write_error)?;
    }
    out.flush().await.map_err(write_error)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub(crate) async fn extract_pages(path: &Path) -> Result<Vec<String>, GazetteError> {
    let mut command = Command::new("pdftotext");
    command.args(["-enc", "UTF-8"]).arg(path).arg("-");
    run(command, path, PDF_TIMEOUT, MAX_TEXT_BYTES).await
}

async fn run(
    mut command: Command,
    path: &Path,
    limit: Duration,
    max_text: usize,
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
    let outcome = tokio::time::timeout(limit, collect(&mut child, max_text))
        .await
        .unwrap_or_else(|_| {
            Err(GazetteError::Pdftotext(format!(
                "timed out after {limit:?}"
            )))
        });
    if outcome.is_err() {
        stop(&mut child).await?;
    }
    let (stdout, stderr, status) = outcome?;
    if !status.success() {
        return Err(GazetteError::Pdftotext(failure(status, &stderr, path)));
    }
    Ok(String::from_utf8_lossy(&stdout)
        .split('\x0C')
        .map(str::to_owned)
        .collect())
}

async fn collect(
    child: &mut Child,
    max_text: usize,
) -> Result<(Vec<u8>, Vec<u8>, ExitStatus), GazetteError> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let pipe_error = |error: io::Error| GazetteError::Pdftotext(error.to_string());
    tokio::try_join!(
        read_text(stdout, max_text),
        async { read_prefix(stderr, STDERR_LIMIT).await.map_err(pipe_error) },
        async { child.wait().await.map_err(pipe_error) },
    )
}

async fn read_text(
    pipe: Option<impl AsyncRead + Unpin>,
    limit: usize,
) -> Result<Vec<u8>, GazetteError> {
    let mut text = Vec::new();
    let Some(mut pipe) = pipe else {
        return Ok(text);
    };
    let mut buffer = [0; 8192];
    loop {
        let read = pipe
            .read(&mut buffer)
            .await
            .map_err(|error| GazetteError::Pdftotext(error.to_string()))?;
        if read == 0 {
            return Ok(text);
        }
        if text.len() + read > limit {
            return Err(GazetteError::TooMuchText { limit });
        }
        text.extend_from_slice(&buffer[..read]);
    }
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

async fn stop(child: &mut Child) -> Result<(), GazetteError> {
    let kill_error = |error: io::Error| GazetteError::Pdftotext(format!("kill: {error}"));
    if child.try_wait().map_err(kill_error)?.is_some() {
        return Ok(());
    }
    child.kill().await.map_err(kill_error)
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
    let text = text.replace(&*path.to_string_lossy(), &file_name(path));
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

    use super::{MAX_PDF_BYTES, MAX_TEXT_BYTES, download_pages_in, failure, run};
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

    fn recording_pid(pid_file: &Path, program: &str) -> Command {
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "echo $$ > '{}.tmp' && mv '{}.tmp' '{}' && exec {program}",
            pid_file.display(),
            pid_file.display(),
            pid_file.display()
        ));
        command
    }

    fn sleeper(pid_file: &Path) -> Command {
        recording_pid(pid_file, "sleep 30")
    }

    async fn promptly<T>(work: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .expect("the call returned promptly")
    }

    async fn serve_a_megabyte_then_stall() -> String {
        let mut bytes =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nConnection: close\r\n\r\n"
                .to_vec();
        bytes.extend_from_slice(&[0; 1 << 20]);
        serve_raw(bytes, true).await
    }

    #[tokio::test]
    async fn successful_scans_leave_no_file() {
        if !pdftotext_available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(FAKE_PDF.len(), FAKE_PDF), false).await;

        let pages = download_pages_in(&reqwest::Client::new(), &url, dir.path(), MAX_PDF_BYTES)
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

        let error = download_pages_in(&reqwest::Client::new(), &url, dir.path(), MAX_PDF_BYTES)
            .await
            .unwrap_err();

        assert!(
            error.to_string().starts_with(&format!("download {url}: ")),
            "{error}"
        );
        assert!(std::error::Error::source(&error).is_some(), "{error}");
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn an_unusable_temp_dir_is_reported_without_its_path() {
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(FAKE_PDF.len(), FAKE_PDF), false).await;

        let error = download_pages_in(
            &reqwest::Client::new(),
            &url,
            &dir.path().join("gone"),
            MAX_PDF_BYTES,
        )
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "create temp file: No such file or directory (os error 2)"
        );
    }

    #[test]
    fn write_errors_name_the_file_only() {
        let error = GazetteError::WriteTempFile {
            file: "edition-abc.pdf".to_owned(),
            source: std::io::Error::other("no space left on device"),
        };

        assert_eq!(
            error.to_string(),
            "write edition-abc.pdf: no space left on device"
        );
        assert!(std::error::Error::source(&error).is_some());
    }

    #[tokio::test]
    async fn failed_extractions_leave_no_file_and_no_path() {
        if !pdftotext_available() {
            return;
        }
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(9, b"not a pdf"), false).await;

        let error = download_pages_in(&reqwest::Client::new(), &url, dir.path(), MAX_PDF_BYTES)
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
        let mut download = Box::pin(download_pages_in(&http, &url, dir.path(), MAX_PDF_BYTES));

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

        let error = run(
            command,
            Path::new("edition.pdf"),
            Duration::from_secs(5),
            MAX_TEXT_BYTES,
        )
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
            MAX_TEXT_BYTES,
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
            MAX_TEXT_BYTES,
        ));

        let pid = wait_for(|| pid_from(&pid_file)).await;
        started.abort();

        wait_for(|| is_gone_or_zombie(&pid).then_some(())).await;
    }

    #[tokio::test]
    async fn pdfs_above_the_limit_are_refused_while_streaming() {
        let dir = TempDir::new().unwrap();
        let url = serve_a_megabyte_then_stall().await;

        let error = promptly(download_pages_in(
            &reqwest::Client::new(),
            &url,
            dir.path(),
            64 << 10,
        ))
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!("download {url}: larger than 64 KiB")
        );
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
    }

    #[tokio::test]
    async fn pdfs_announced_above_the_limit_are_refused_before_a_file_exists() {
        let dir = TempDir::new().unwrap();
        let url = serve_raw(response(1 << 20, b""), true).await;

        let error = promptly(download_pages_in(
            &reqwest::Client::new(),
            &url,
            &dir.path().join("gone"),
            64 << 10,
        ))
        .await
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!("download {url}: larger than 64 KiB")
        );
    }

    #[tokio::test]
    async fn extractors_printing_too_much_text_are_killed_and_reaped() {
        let dir = TempDir::new().unwrap();
        let pid_file = dir.path().join("pid");

        let error = promptly(run(
            recording_pid(&pid_file, "yes"),
            Path::new("edition.pdf"),
            Duration::from_secs(60),
            64 << 10,
        ))
        .await
        .unwrap_err();

        assert_eq!(error.to_string(), "pdftotext: more than 64 KiB of text");
        let pid = pid_from(&pid_file).expect("the extractor started");
        assert!(!Path::new(&format!("/proc/{pid}")).exists());
    }

    #[test]
    fn pdf_bounds_read_as_the_production_limits() {
        let pdf = GazetteError::PdfTooLarge {
            url: "https://x/1.pdf".to_owned(),
            limit: MAX_PDF_BYTES,
        };
        let text = GazetteError::TooMuchText {
            limit: MAX_TEXT_BYTES,
        };

        assert_eq!(
            pdf.to_string(),
            "download https://x/1.pdf: larger than 256 MiB"
        );
        assert_eq!(text.to_string(), "pdftotext: more than 64 MiB of text");
    }

    #[test]
    fn failure_text_names_the_exit_and_the_first_clean_stderr_line() {
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
