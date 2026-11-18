use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{self, Child, Command, Stdio};
use std::time::{Duration, Instant};
use std::{env, fs, mem, thread};

use tokio::sync::OnceCell;
use tokio_postgres::{Client, NoTls};

const READY_WITHIN: Duration = Duration::from_secs(30);
const READY_POLL: Duration = Duration::from_millis(50);
const INITDB_LIMIT: Duration = Duration::from_secs(120);
const SMART_SHUTDOWN_LIMIT: Duration = Duration::from_secs(10);
const CLUSTER: &str = r#"trap '' INT PIPE
dir=$(mktemp -d "${TMPDIR:-/tmp}/monitor-hub-pg.XXXXXX") || exit 1
timeout "$1" initdb -D "$dir/data" -U postgres -A trust -E UTF8 --locale=C --no-sync --no-instructions >"$dir/initdb.log" 2>&1
status=$?
if [ "$status" -ne 0 ]; then
    cat "$dir/initdb.log"
    if [ "$status" -eq 124 ]; then
        echo "initdb did not finish within $1 seconds"
    fi
    rm -rf "$dir"
    exit 1
fi
postgres -D "$dir/data" -k "$dir" -c listen_addresses=127.0.0.1 -p "$3" -c fsync=off -c synchronous_commit=off -c full_page_writes=off </dev/null >"$dir/postgres.log" 2>&1 &
pid=$!
echo "ready $dir"
exec >/dev/null
read -r _
kill -TERM "$pid" 2>/dev/null
waited=0
while kill -0 "$pid" 2>/dev/null && [ "$waited" -lt "$2" ]; do
    sleep 1
    waited=$((waited + 1))
done
kill -INT "$pid" 2>/dev/null
wait "$pid"
rm -rf "$dir""#;

static SERVER: OnceCell<Result<Server, String>> = OnceCell::const_new();

pub struct Database {
    pub url: String,
    admin: String,
    name: String,
}

struct Server {
    admin: String,
    prefix: String,
    query: String,
    _cluster: Option<Child>,
}

pub async fn database() -> Option<Database> {
    let server = match SERVER.get_or_init(start).await {
        Ok(server) => server,
        Err(reason) => return unavailable(reason),
    };
    match server.create().await {
        Ok(database) => Some(database),
        Err(reason) => unavailable(&reason),
    }
}

fn unavailable(reason: &str) -> Option<Database> {
    if env::var_os("CI").is_some_and(|value| !value.is_empty()) {
        panic!("Postgres is required when CI is set: {reason}");
    }
    eprintln!("Postgres unavailable, skipping: {reason}");
    None
}

async fn start() -> Result<Server, String> {
    if let Some(url) = env::var("TEST_DATABASE_URL")
        .ok()
        .filter(|url| !url.is_empty())
    {
        let server = Server::new(url, None)?;
        server.wait_ready().await?;
        return Ok(server);
    }
    let (dir, port, cluster) = tokio::task::spawn_blocking(start_cluster)
        .await
        .map_err(|err| format!("start the test cluster: {err}"))??;
    let admin = format!("postgres://postgres@127.0.0.1:{port}/postgres?sslmode=disable");
    let server = Server::new(admin, Some(cluster))?;
    server
        .wait_ready()
        .await
        .map_err(|err| format!("{err}\n{}", postgres_log(&dir)))?;
    Ok(server)
}

impl Server {
    fn new(admin: String, cluster: Option<Child>) -> Result<Server, String> {
        let (address, query) = admin.split_once('?').unwrap_or((&admin, ""));
        let authority = address
            .find("://")
            .map(|at| at + 3)
            .ok_or("TEST_DATABASE_URL must be a postgres:// URL")?;
        let end = address[authority..]
            .find('/')
            .map_or(address.len(), |at| authority + at);
        let query = if query.is_empty() {
            String::new()
        } else {
            format!("?{query}")
        };
        Ok(Server {
            prefix: address[..end].to_owned(),
            query,
            admin,
            _cluster: cluster,
        })
    }

    async fn wait_ready(&self) -> Result<(), String> {
        let deadline = Instant::now() + READY_WITHIN;
        loop {
            match connect(&self.admin).await {
                Ok(_) => return Ok(()),
                Err(err) if Instant::now() >= deadline => {
                    return Err(format!("connect to the test server: {}", describe(&err)));
                }
                Err(_) => tokio::time::sleep(READY_POLL).await,
            }
        }
    }

    async fn create(&self) -> Result<Database, String> {
        let name = format!(
            "monitor_hub_rust_{}_{:016x}",
            process::id(),
            RandomState::new().hash_one(0)
        );
        let client = connect(&self.admin)
            .await
            .map_err(|err| format!("connect to the test server: {}", describe(&err)))?;
        client
            .batch_execute(&format!("CREATE DATABASE {name} TEMPLATE template0"))
            .await
            .map_err(|err| format!("create the test database {name}: {}", describe(&err)))?;
        Ok(Database {
            url: format!("{}/{name}{}", self.prefix, self.query),
            admin: self.admin.clone(),
            name,
        })
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let admin = mem::take(&mut self.admin);
        let statement = format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", self.name);
        let dropped = thread::spawn(move || drop_database(&admin, &statement)).join();
        match dropped {
            Ok(Ok(())) => {}
            Ok(Err(err)) => eprintln!("drop the test database {}: {err}", self.name),
            Err(_) => eprintln!("drop the test database {}: the thread panicked", self.name),
        }
    }
}

fn drop_database(admin: &str, statement: &str) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| err.to_string())?;
    runtime.block_on(async {
        let client = connect(admin).await.map_err(|err| describe(&err))?;
        client
            .batch_execute(statement)
            .await
            .map_err(|err| describe(&err))
    })
}

async fn connect(url: &str) -> Result<Client, tokio_postgres::Error> {
    let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
    tokio::spawn(async move {
        if let Err(err) = connection.await {
            eprintln!("test server connection: {err}");
        }
    });
    Ok(client)
}

fn describe(err: &tokio_postgres::Error) -> String {
    err.as_db_error()
        .map_or_else(|| err.to_string(), |db| db.to_string())
}

fn start_cluster() -> Result<(PathBuf, u16, Child), String> {
    if !["initdb", "postgres"].into_iter().all(on_path) {
        return Err("set TEST_DATABASE_URL or install PostgreSQL (initdb and postgres)".to_owned());
    }
    let port = free_port().map_err(|err| format!("pick a port for the test cluster: {err}"))?;
    let mut cluster = Command::new("sh")
        .args(["-c", CLUSTER, "monitor-hub-pg"])
        .arg(INITDB_LIMIT.as_secs().to_string())
        .arg(SMART_SHUTDOWN_LIMIT.as_secs().to_string())
        .arg(port.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| format!("start the test cluster: {err}"))?;
    let mut output = String::new();
    if let Some(mut stdout) = cluster.stdout.take() {
        stdout
            .read_to_string(&mut output)
            .map_err(|err| format!("read the test cluster's output: {err}"))?;
    }
    if let Some(dir) = output.strip_prefix("ready ") {
        return Ok((PathBuf::from(dir.trim_end_matches('\n')), port, cluster));
    }
    let status = cluster
        .wait()
        .map_or_else(|err| err.to_string(), |status| status.to_string());
    Err(format!(
        "the test cluster did not start ({status}):\n{output}"
    ))
}

fn free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn on_path(tool: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|dir| dir.join(tool).is_file()))
}

fn postgres_log(dir: &Path) -> String {
    fs::read_to_string(dir.join("postgres.log"))
        .unwrap_or_else(|err| format!("no postgres log: {err}"))
}
