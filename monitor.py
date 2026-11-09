from __future__ import annotations

import html
import json
import logging
import os
import subprocess
import tempfile
import threading
import time
import unicodedata
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass, field
from datetime import UTC, datetime, timedelta
from email.message import Message
from email.utils import format_datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import TypedDict, cast, override
from urllib.error import URLError
from xml.sax.saxutils import escape

API_URL = "https://diarios.al.rr.leg.br/wp-json/wp/v2/media"
SITE_URL = "https://diarios.al.rr.leg.br"
USER_AGENT = "diario-alerr-monitor/1.0"
PER_PAGE = 100
HTTP_TIMEOUT = 60
CURSOR_OVERLAP = timedelta(days=1)
MAX_SEEN = 1000
MAX_ALERTS = 100
SNIPPET_RADIUS = 160
SCAN_WORKERS = 2

log = logging.getLogger("diario-alerr")


class ApiTitle(TypedDict):
    rendered: str


class ApiMedia(TypedDict):
    id: int
    date_gmt: str
    source_url: str
    title: ApiTitle


@dataclass(frozen=True)
class Config:
    terms: tuple[str, ...]
    telegram_token: str
    telegram_chat_id: str
    interval: timedelta
    start_after: datetime
    data_dir: Path
    public_url: str
    http_port: int

    @classmethod
    def from_env(cls) -> Config:
        terms = tuple(term.strip() for term in os.environ.get("WATCH_TERMS", "").split(",") if term.strip())
        if not terms:
            raise SystemExit("WATCH_TERMS is empty: set at least one name or registration number")
        return cls(
            terms=terms,
            telegram_token=os.environ.get("TELEGRAM_BOT_TOKEN", ""),
            telegram_chat_id=os.environ.get("TELEGRAM_CHAT_ID", ""),
            interval=timedelta(minutes=float(os.environ.get("CHECK_INTERVAL_MINUTES", "30"))),
            start_after=parse_start_after(os.environ.get("START_AFTER", "")),
            data_dir=Path(os.environ.get("DATA_DIR", "/data")),
            public_url=os.environ.get("PUBLIC_URL", "").rstrip("/"),
            http_port=int(os.environ.get("HTTP_PORT", "8080")),
        )


def parse_start_after(raw: str) -> datetime:
    if not raw:
        return datetime.now(UTC) - timedelta(days=3)
    parsed = datetime.fromisoformat(raw)
    return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)


@dataclass(frozen=True)
class Edition:
    id: int
    published: datetime
    url: str
    title: str

    @classmethod
    def from_api(cls, item: ApiMedia) -> Edition:
        return cls(
            id=item["id"],
            published=datetime.fromisoformat(item["date_gmt"]).replace(tzinfo=UTC),
            url=item["source_url"],
            title=html.unescape(item["title"]["rendered"]),
        )


@dataclass(frozen=True)
class Hits:
    pages: dict[str, list[int]]
    first_page: int
    snippet: str


@dataclass
class Alert:
    id: str
    title: str
    link: str
    body: str
    published: str
    telegram_sent: bool = False


@dataclass
class State:
    cursor: datetime
    seen: list[int] = field(default_factory=list)
    alerts: list[Alert] = field(default_factory=list)
    last_success: datetime | None = None


class Store:
    def __init__(self, path: Path) -> None:
        self.path: Path = path

    def exists(self) -> bool:
        return self.path.exists()

    def load(self, default_cursor: datetime) -> State:
        if not self.path.exists():
            return State(cursor=default_cursor)
        raw = cast(dict[str, object], json.loads(self.path.read_text()))
        last_success = cast(str | None, raw.get("last_success"))
        return State(
            cursor=datetime.fromisoformat(cast(str, raw["cursor"])),
            seen=cast(list[int], raw["seen"]),
            alerts=[Alert(**alert) for alert in cast(list[dict[str, str | bool]], raw["alerts"])],  # pyright: ignore[reportArgumentType]
            last_success=datetime.fromisoformat(last_success) if last_success else None,
        )

    def save(self, state: State) -> None:
        payload = {
            "cursor": state.cursor.isoformat(),
            "seen": state.seen[-MAX_SEEN:],
            "alerts": [asdict(alert) for alert in state.alerts[-MAX_ALERTS:]],
            "last_success": state.last_success.isoformat() if state.last_success else None,
        }
        tmp = self.path.with_suffix(".tmp")
        _ = tmp.write_text(json.dumps(payload, ensure_ascii=False, indent=2))
        _ = tmp.replace(self.path)


def normalize(text: str) -> str:
    decomposed = unicodedata.normalize("NFKD", text)
    stripped = "".join(char for char in decomposed if not unicodedata.combining(char))
    return " ".join(stripped.upper().split())


def http_get(url: str) -> tuple[bytes, Message]:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT) as response:  # pyright: ignore[reportAny]
        return cast(bytes, response.read()), cast(Message, response.headers)  # pyright: ignore[reportAny]


def list_editions(after: datetime) -> list[Edition]:
    editions: list[Edition] = []
    page = 1
    while True:
        query = urllib.parse.urlencode({
            "mime_type": "application/pdf",
            "after": after.strftime("%Y-%m-%dT%H:%M:%S"),
            "orderby": "date",
            "order": "asc",
            "per_page": PER_PAGE,
            "page": page,
            "_fields": "id,date_gmt,source_url,title",
        })
        body, headers = http_get(f"{API_URL}?{query}")
        editions.extend(Edition.from_api(item) for item in cast(list[ApiMedia], json.loads(body)))
        if page >= int(headers.get("X-WP-TotalPages", "1")):
            return editions
        page += 1


def extract_pages(pdf: bytes) -> list[str]:
    with tempfile.NamedTemporaryFile(suffix=".pdf") as file:
        _ = file.write(pdf)
        file.flush()
        result = subprocess.run(
            ["pdftotext", "-enc", "UTF-8", file.name, "-"],
            capture_output=True,
            check=True,
            timeout=300,
        )
    return result.stdout.decode("utf-8", errors="replace").split("\f")


def find_terms(edition: Edition, terms: tuple[str, ...]) -> Hits | None:
    pdf, _ = http_get(edition.url)
    pages = [normalize(page) for page in extract_pages(pdf)]
    wanted = {term: normalize(term) for term in terms}
    found = {
        term: [number for number, text in enumerate(pages, start=1) if needle in text]
        for term, needle in wanted.items()
    }
    found = {term: numbers for term, numbers in found.items() if numbers}
    if not found:
        return None
    first_term, first_pages = min(found.items(), key=lambda entry: entry[1][0])
    first_page = first_pages[0]
    text = pages[first_page - 1]
    position = text.index(wanted[first_term])
    snippet = text[max(0, position - SNIPPET_RADIUS) : position + len(wanted[first_term]) + SNIPPET_RADIUS]
    return Hits(pages=found, first_page=first_page, snippet=snippet)


def format_pages(numbers: list[int]) -> str:
    label = "pág." if len(numbers) == 1 else "págs."
    return f"{label} {', '.join(map(str, numbers))}"


def build_alert(edition: Edition, hits: Hits) -> Alert:
    lines = [f"{term}: {format_pages(numbers)}" for term, numbers in hits.pages.items()]
    return Alert(
        id=f"edition-{edition.id}",
        title=f"Seu nome apareceu no Diário da ALE/RR: {edition.title}",
        link=f"{edition.url}#page={hits.first_page}",
        body="\n".join(lines) + f"\n\n…{hits.snippet}…",
        published=edition.published.isoformat(),
    )


def scan_new_editions(config: Config, state: State) -> bool:
    try:
        editions = list_editions(state.cursor - CURSOR_OVERLAP)
    except (URLError, TimeoutError, json.JSONDecodeError) as err:
        log.warning("could not list editions: %s", err)
        return False
    fresh = [edition for edition in editions if edition.id not in state.seen]
    with ThreadPoolExecutor(max_workers=SCAN_WORKERS) as pool:
        futures = [(edition, pool.submit(find_terms, edition, config.terms)) for edition in fresh]
        for edition, future in futures:
            try:
                hits = future.result()
            except (URLError, TimeoutError, subprocess.SubprocessError) as err:
                log.warning("could not read %s, retrying next cycle: %s", edition.title, err)
                return False
            record(state, edition, hits)
    return True


def record(state: State, edition: Edition, hits: Hits | None) -> None:
    state.seen.append(edition.id)
    state.cursor = max(state.cursor, edition.published)
    if hits is None:
        log.info("%s: no match", edition.title)
        return
    log.info("%s: MATCH %s", edition.title, hits.pages)
    state.alerts.append(build_alert(edition, hits))


def telegram_text(alert: Alert) -> str:
    return (
        f"🚨 <b>{html.escape(alert.title)}</b>\n\n"
        f"<pre>{html.escape(alert.body)}</pre>\n"
        f'<a href="{html.escape(alert.link)}">Abrir o PDF na página</a>'
    )


def send_telegram(config: Config, alert: Alert) -> None:
    payload = json.dumps({
        "chat_id": config.telegram_chat_id,
        "text": telegram_text(alert),
        "parse_mode": "HTML",
        "link_preview_options": {"is_disabled": True},
    }).encode()
    request = urllib.request.Request(
        f"https://api.telegram.org/bot{config.telegram_token}/sendMessage",
        data=payload,
        headers={"Content-Type": "application/json", "User-Agent": USER_AGENT},
    )
    with urllib.request.urlopen(request, timeout=HTTP_TIMEOUT):
        pass


def deliver_pending(config: Config, state: State) -> None:
    if not (config.telegram_token and config.telegram_chat_id):
        return
    for alert in state.alerts:
        if alert.telegram_sent:
            continue
        try:
            send_telegram(config, alert)
        except (URLError, TimeoutError) as err:
            reason = getattr(err, "reason", err)
            log.warning("telegram delivery failed, retrying next cycle: %s", reason)
            return
        alert.telegram_sent = True


def render_item(alert: Alert) -> str:
    return "".join([
        f"<item><title>{escape(alert.title)}</title><link>{escape(alert.link)}</link>",
        f'<guid isPermaLink="false">{escape(alert.id)}</guid>',
        f"<pubDate>{format_datetime(datetime.fromisoformat(alert.published))}</pubDate>",
        f"<description>{escape(alert.body)}</description></item>",
    ])


def render_feed(config: Config, state: State) -> str:
    items = "".join(render_item(alert) for alert in reversed(state.alerts))
    return (
        '<?xml version="1.0" encoding="UTF-8"?><rss version="2.0"><channel>'
        f"<title>Diário da ALE/RR: {escape(', '.join(config.terms))}</title>"
        f"<link>{escape(config.public_url or SITE_URL)}</link>"
        "<description>Edições do Diário da ALE/RR em que os termos vigiados aparecem</description>"
        f"{items}</channel></rss>"
    )


def is_healthy(config: Config, state: State, started: datetime) -> bool:
    reference = state.last_success or started
    return datetime.now(UTC) - reference < config.interval * 3


def start_http_server(config: Config, store: Store, started: datetime) -> None:
    def feed(state: State) -> tuple[int, str, str]:
        return 200, "application/rss+xml; charset=utf-8", render_feed(config, state)

    def health(state: State) -> tuple[int, str, str]:
        healthy = is_healthy(config, state, started)
        return (200 if healthy else 503), "text/plain", "ok" if healthy else "stale"

    routes = {"/feed.xml": feed, "/healthz": health}

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            route = routes.get(self.path.split("?")[0])
            if route is None:
                self.send_error(404)
                return
            status, content_type, body = route(store.load(config.start_after))
            encoded = body.encode()
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            _ = self.wfile.write(encoded)

        @override
        def log_message(self, format: str, *args: object) -> None:
            pass

    server = ThreadingHTTPServer(("0.0.0.0", config.http_port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()


def welcome_alert(config: Config) -> Alert:
    now = datetime.now(UTC)
    return Alert(
        id="welcome",
        title="Monitor do Diário da ALE/RR ligado",
        link=SITE_URL,
        body=f"Vigiando: {', '.join(config.terms)}\nVerificação a cada {config.interval.total_seconds() / 60:.0f} min.",
        published=now.isoformat(),
    )


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    config = Config.from_env()
    if not (config.telegram_token and config.telegram_chat_id):
        log.warning("TELEGRAM_BOT_TOKEN/TELEGRAM_CHAT_ID not set: alerts go to the RSS feed only")
    store = Store(config.data_dir / "state.json")
    first_run = not store.exists()
    state = store.load(config.start_after)
    if first_run:
        state.alerts.append(welcome_alert(config))
        store.save(state)
    started = datetime.now(UTC)
    start_http_server(config, store, started)
    log.info("watching %s every %s", ", ".join(config.terms), config.interval)
    while True:
        if scan_new_editions(config, state):
            state.last_success = datetime.now(UTC)
        deliver_pending(config, state)
        store.save(state)
        time.sleep(config.interval.total_seconds())


if __name__ == "__main__":
    main()
