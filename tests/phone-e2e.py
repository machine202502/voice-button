"""End-to-end check of the phone site against a live daemon.

What it does, without a phone:

1. starts `voice-button.exe` in a temporary profile (its own data directory, so
   the real application may keep running);
2. drives a real Chromium over the DevTools protocol, exactly like a phone
   browser: pairs with a one-time link, creates a monolog, records, stops;
3. feeds the microphone from a real speech recording instead of hardware;
4. walks through the flows that used to break: one card per recording, the
   status reaching "done", the text reaching the phone, editing, deleting,
   archiving, recording while offline and sending it later;
5. compares what the phone shows with what the computer stored;
6. writes `e2e-diag.log`: every request the page made with its status and the
   body of a failure, the page's `window.__trace`, a snapshot of the outbox
   (takes and chunks) at the interesting moments, the daemon's `db.json` and
   its log. A guess is no substitute for the file a failed run leaves behind.

Run:  python tests/phone-e2e.py [--keep]
Exit code 0 means every check passed.
"""

import base64
import json
import os
import shutil
import socket
import sqlite3
import ssl
import struct
import subprocess
import sys

try:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
except Exception:
    pass
import tempfile
import time
import traceback
import urllib.error
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EXE = os.path.join(os.environ.get("VBT_DIR", r"C:\vbt\release"), "voice-button.exe")
CHROME = os.path.join(
    os.environ["LOCALAPPDATA"], "ms-playwright", "chromium-1248", "chrome-win64", "chrome.exe"
)
FFMPEG = os.path.join(
    os.environ["LOCALAPPDATA"], "ms-playwright", "ffmpeg-1013", "ffmpeg-win64.exe"
)
PAGE = os.path.join(ROOT, "src", "web")
ASSET = os.path.join(ROOT, "tests", "assets", "dictation-8min.mp3")
CDP_PORT = 9444


def free_port():
    """A port the system is actually willing to give us.

    Windows reserves whole ranges (Hyper-V, WSL), so a hard-coded number is a
    coin toss: the daemon then cannot listen and the whole run is a false alarm.
    """
    with socket.socket() as probe:
        probe.bind(("0.0.0.0", 0))
        return probe.getsockname()[1]


PORT = free_port()

failures = []
checks = 0


def check(condition, message):
    global checks
    checks += 1
    if condition:
        print(f"  ok    {message}")
    else:
        print(f"  FAIL  {message}")
        failures.append(message)


def wait_for(predicate, timeout=30.0, interval=0.25, what="condition"):
    deadline = time.time() + timeout
    while time.time() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(interval)
    raise TimeoutError(f"timed out waiting for {what}")


def sheet_labels(browser):
    """The words on the open sheet's buttons, without the small detail spans."""
    return browser.evaluate(
        "Array.from(document.querySelectorAll('.sheet button'))"
        ".map((button) => { const first = button.querySelector('span');"
        " return (first ? first.textContent : button.textContent).trim(); })"
    )


def sheet_click(browser, label):
    """Press the sheet button that carries `label`; False when there is none."""
    wanted = json.dumps(label, ensure_ascii=False)
    return browser.evaluate(
        "(() => { const wanted = " + wanted + ";"
        " const button = Array.from(document.querySelectorAll('.sheet button'))"
        ".find((item) => { const first = item.querySelector('span');"
        " const text = (first ? first.textContent : item.textContent).trim();"
        " return text === wanted; });"
        " if (!button) return false; button.click(); return true; })()"
    )


def sheet_title(browser):
    return browser.evaluate(
        "(() => { const heading = document.querySelector('.sheet #sheet-title');"
        " return heading ? heading.textContent.trim() : ''; })()"
    )


def wait_sheet(browser, title=None, timeout=10):
    """Wait for the sheet to be there, and for its heading when one is named."""
    if title is None:
        return wait_for(
            lambda: browser.evaluate("!!document.querySelector('.sheet')"),
            timeout=timeout,
            what="the sheet",
        )
    wanted = json.dumps(title, ensure_ascii=False)
    return wait_for(
        lambda: browser.evaluate(
            "(() => { const heading = document.querySelector('.sheet #sheet-title');"
            " return !!heading && heading.textContent.trim() === " + wanted + "; })()"
        ),
        timeout=timeout,
        what=f"the sheet «{title}»",
    )


def swipe_card(browser, dx=110):
    """Pull the only monolog card sideways with the pointer events it listens to.

    The card only archives or unarchives a clear horizontal gesture: more than
    72 px of travel, and clearly more sideways than up or down. A synthetic
    touch is enough; `setPointerCapture` on it throws and the card catches that.
    """
    step = dx / 6.0
    return browser.evaluate(
        "(() => { const card = document.querySelector('.monolog-card');"
        " if (!card) return 'no card';"
        " const box = card.getBoundingClientRect();"
        " const x = box.left + box.width / 2;"
        " const y = box.top + Math.min(box.height / 2, 30);"
        " const fire = (type, at, buttons) => card.dispatchEvent(new PointerEvent(type, {"
        "   bubbles: true, cancelable: true, composed: true, pointerId: 7,"
        "   pointerType: 'touch', isPrimary: true, button: 0, buttons,"
        "   clientX: at, clientY: y }));"
        " fire('pointerdown', x, 1);"
        " for (let index = 1; index <= 5; index += 1) fire('pointermove', x + "
        + repr(step)
        + " * index, 1);"
        " fire('pointerup', x + " + repr(dx) + ", 0);"
        " return { from: Math.round(x), to: Math.round(x + " + repr(dx) + "),"
        " swipe: card.dataset.swipe }; })()"
    )


def view_geometry(browser):
    """The viewport's own numbers: a bar or a note must not change them."""
    return json.loads(
        browser.evaluate(
            "JSON.stringify((() => { const view = document.getElementById('view');"
            " return { scrollHeight: view.scrollHeight, clientHeight: view.clientHeight,"
            " scrollTop: view.scrollTop }; })())"
        )
    )


def pointer_click(browser, selector):
    """Click an element with a real mouse, so pointer-events and hit testing count."""
    target = json.dumps(selector)
    box = browser.evaluate(
        "(() => { const node = document.querySelector(" + target + ");"
        " if (!node) return null; const rect = node.getBoundingClientRect();"
        " return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }; })()"
    )
    if not box:
        return False
    browser.call(
        "Input.dispatchMouseEvent",
        {"type": "mouseMoved", "x": box["x"], "y": box["y"]},
    )
    browser.call(
        "Input.dispatchMouseEvent",
        {
            "type": "mousePressed",
            "x": box["x"],
            "y": box["y"],
            "button": "left",
            "clickCount": 1,
        },
    )
    browser.call(
        "Input.dispatchMouseEvent",
        {
            "type": "mouseReleased",
            "x": box["x"],
            "y": box["y"],
            "button": "left",
            "clickCount": 1,
        },
    )
    return True


# ---------------------------------------------------------------- diagnostics

DIAG_PATH = os.path.join(ROOT, "tests", "e2e-diag.log")


def sync_page():
    """Copy the page sources to where the daemon serves them from.

    The daemon serves `site-app` next to its executable; the sources live in
    `src/web`. Doing the copy here keeps a stale page from silently passing
    checks that no longer match the code.
    """
    target = os.path.join(os.path.dirname(EXE), "site-app")
    if os.path.isdir(target):
        shutil.rmtree(target)
    shutil.copytree(PAGE, target)


def stamp():
    return time.strftime("%H:%M:%S")


class Diag:
    """Everything one failing run needs, in a file instead of a guess.

    A phone page has no console. The only evidence is what the browser asked
    the computer for and what the page kept in its own storage, so both are
    written down at every interesting moment of the run.
    """

    def __init__(self, path):
        self.handle = open(path, "w", encoding="utf-8")

    def line(self, text=""):
        self.handle.write(str(text) + "\n")
        self.handle.flush()

    def section(self, title):
        self.line()
        self.line("=" * 72)
        self.line(f"{title} · {stamp()}")
        self.line("=" * 72)

    def close(self):
        self.handle.close()


# Everything the outbox holds: the takes and the audio of each one. `window.app`
# is the page's own object; `bytes` are reported by type and length because a
# chunk stored as the wrong thing (a view, a plain array) is a likely bug.
OUTBOX_JS = """
(async () => {
  const page = window.app;
  if (!page || !page.outbox) return JSON.stringify({ error: 'the page has no outbox' });
  const takes = await page.outbox.list();
  const out = [];
  for (const take of takes) {
    const chunks = await page.outbox.chunks(take.localId);
    out.push({
      take: take,
      chunks: chunks.map((chunk) => ({
        seq: chunk.seq,
        type: Object.prototype.toString.call(chunk.bytes),
        bytes: chunk.bytes && chunk.bytes.byteLength ? chunk.bytes.byteLength : null,
      })),
    });
  }
  return JSON.stringify(out);
})()
"""


def outbox_state(browser):
    raw = browser.evaluate(OUTBOX_JS, await_promise=True)
    try:
        return json.loads(raw)
    except (TypeError, ValueError):
        return {"raw": raw}


def sqlite_path(root):
    return os.path.join(root, "voice-button.sqlite3")


def read_kv(root, key):
    try:
        conn = sqlite3.connect(sqlite_path(root), timeout=5)
        row = conn.execute("SELECT value FROM kv WHERE key = ?", (key,)).fetchone()
        conn.close()
    except (OSError, sqlite3.Error):
        return None
    return row[0] if row else None


def write_kv(root, key, value):
    conn = sqlite3.connect(sqlite_path(root), timeout=5)
    conn.execute(
        "INSERT INTO kv(key, value) VALUES (?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (key, value),
    )
    conn.commit()
    conn.close()


def site_state(daemon):
    """What the daemon published about the phone server (SQLite kv)."""
    text = read_kv(daemon.root, "runtime.site")
    if not text:
        return None
    snapshot = {}
    for line in text.splitlines():
        if "\t" not in line:
            continue
        key, value = line.split("\t", 1)
        snapshot[key] = value
    return {
        "running": snapshot.get("running") == "1",
        "port": int(snapshot.get("port") or 0),
        "url": snapshot.get("url", ""),
        "paired": snapshot.get("paired") == "1",
    }


def summarise_outbox(state):
    """One line per take: what the page really kept."""
    if isinstance(state, dict):
        return state
    summary = []
    for item in state:
        take = item.get("take") or {}
        chunks = item.get("chunks") or []
        seconds = sum((chunk.get("bytes") or 0) for chunk in chunks) / 2.0 / 16000.0
        summary.append(
            {
                "local": (take.get("localId") or "?")[:8],
                "remote": take.get("remote"),
                "closed": take.get("closed"),
                "seq": take.get("seq"),
                "chunks": [chunk.get("seq") for chunk in chunks],
                "bytes": sum((chunk.get("bytes") or 0) for chunk in chunks),
                "seconds": round(seconds, 2),
            }
        )
    return summary


def network_rows(browser):
    return list(browser.requests.items())


def audio_posts(browser):
    """Every POST of a chunk, with the seq from the URL and the byte count."""
    found = []
    for _, row in network_rows(browser):
        url = row.get("url") or ""
        if row.get("method") != "POST" or "/audio" not in url:
            continue
        query = url.split("?", 1)[1] if "?" in url else ""
        seq = None
        for part in query.split("&"):
            if part.startswith("seq="):
                try:
                    seq = int(part[4:])
                except ValueError:
                    seq = part[4:]
        found.append(
            {
                "seq": seq,
                "status": row.get("status"),
                "bytes": row.get("sent"),
                "seconds": round((row.get("sent") or 0) / 2.0 / 16000.0, 3),
                "error": row.get("error"),
            }
        )
    return found


def log_tail(daemon, count=40):
    try:
        with open(os.path.join(daemon.root, "log.txt"), encoding="utf-8", errors="replace") as handle:
            lines = [line.rstrip() for line in handle if line.strip()]
        return lines[-count:]
    except OSError as error:
        return [f"(no daemon log: {error})"]


def daemon_db(daemon):
    """The typed phone tables, shaped like the old JSON for the assertions."""
    try:
        conn = sqlite3.connect(sqlite_path(daemon.root), timeout=5)
        conn.row_factory = sqlite3.Row
        client = conn.execute("SELECT * FROM site_client WHERE id = 1").fetchone()
        monologues = [
            dict(row)
            for row in conn.execute(
                "SELECT id, name, archived, created FROM monologs ORDER BY id"
            )
        ]
        messages = []
        for row in conn.execute(
            "SELECT id, monolog_id, recorded, received, stage, text, original, edited, "
            "deleted, duration, segments_done, history_at, closed FROM site_messages ORDER BY id"
        ):
            item = dict(row)
            item["monolog"] = item.pop("monolog_id")
            messages.append(item)
        conn.close()
        return {
            "client": dict(client) if client else {},
            "monologues": monologues,
            "messages": messages,
        }
    except (OSError, sqlite3.Error) as error:
        return {"error": str(error)}


def dump_network(browser, diag, label, bodies=False):
    diag.section(f"network · {label}")
    for request_id, row in network_rows(browser):
        diag.line(
            f'{row.get("method")} {row.get("url")} -> {row.get("status")}'
            f' sent={row.get("sent")} received={row.get("received")}'
            f'{" error=" + row["error"] if row.get("error") else ""}'
        )
        interesting = (
            row.get("status") is None
            or not 200 <= (row.get("status") or 0) < 300
            or (bodies and "/audio" in row.get("url", ""))
        )
        if not interesting:
            continue
        try:
            answer = browser.call("Network.getResponseBody", {"requestId": request_id})
            text = answer.get("body", "")
            if answer.get("base64Encoded"):
                text = base64.b64decode(text).decode("utf-8", "replace")
            diag.line("    body: " + text[:500])
        except Exception as error:  # the body may be gone already
            diag.line(f"    body: (unavailable: {error})")
    posts = audio_posts(browser)
    diag.line(f"audio POSTs: {len(posts)}")
    for post in posts:
        diag.line("  " + json.dumps(post, ensure_ascii=False))


def snapshot(browser, daemon, diag, label):
    """Write the page's trace, its outbox and the requests made so far."""
    diag.section(f"snapshot · {label}")
    trace = browser.evaluate(
        "JSON.stringify((window.app && window.app.trace) || window.__trace || [])"
    )
    diag.line("trace: " + str(trace))
    state = outbox_state(browser)
    diag.line("outbox: " + json.dumps(state, ensure_ascii=False))
    diag.line("daemon log tail:")
    for entry in log_tail(daemon, 8):
        diag.line("  " + entry)
    summary = summarise_outbox(state)
    print(f"  [{label}] outbox {json.dumps(summary, ensure_ascii=False)}")
    posts = audio_posts(browser)
    print(f"  [{label}] audio POSTs so far: {json.dumps(posts, ensure_ascii=False)}")
    return summary


# ---------------------------------------------------------------- daemon

class Daemon:
    def __init__(self, port=None, models=True, site=True):
        self.root = tempfile.mkdtemp(prefix="vb-e2e-")
        self.process = None
        self.port = port or PORT
        # The local API is how the test takes the engine the way the person at
        # the computer does: start a stream, keep it open, stop it later.
        self.api_port = free_port()
        # False keeps `root/models` empty, which is how the phone sees the
        # "no recognition model" stage; `link_models` swaps the empty folder
        # for the junction afterwards.
        self.models = models
        # False starts the app with the phone switch off; the scenario below
        # turns it on at run time through config.json, as the window does.
        self.site = site

    def link_models(self):
        """Replace the empty models folder with the real one, as a junction."""
        models = os.path.join(self.root, "models")
        real = os.path.join(os.environ["APPDATA"], "voice-button", "models")
        if os.path.isdir(models):
            # The daemon only sees an empty folder here; rmdir fails loudly if
            # that assumption ever stops being true.
            os.rmdir(models)
        subprocess.run(
            ["cmd", "/c", "mklink", "/J", models, real],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.models = True

    def start(self, pair=True):
        config = {
            "backend": "Onnx",
            "language": "auto",
            "onnx_model": "gigaam-v3-rnnt-punct",
            "ui_language": "ru",
            "site_enabled": self.site,
            "site_port": self.port,
            "site_lan_only": False,
            "api_enabled": True,
            "api_port": self.api_port,
        }
        with open(os.path.join(self.root, "config.json"), "w", encoding="utf-8") as handle:
            json.dump(config, handle, ensure_ascii=False)
        # The profile gets its own data directory, but the models are the real
        # ones: a junction costs nothing and a copy would take minutes.
        models = os.path.join(self.root, "models")
        real = os.path.join(os.environ["APPDATA"], "voice-button", "models")
        if self.models and not os.path.exists(models) and os.path.isdir(real):
            subprocess.run(
                ["cmd", "/c", "mklink", "/J", models, real],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        # A terminated daemon leaves the last "running" snapshot in SQLite;
        # delete it so the wait below cannot pass before the new process
        # listens. The database itself stays: it holds the paired token.
        if os.path.exists(sqlite_path(self.root)):
            try:
                conn = sqlite3.connect(sqlite_path(self.root), timeout=5)
                conn.execute("DELETE FROM kv WHERE key = 'runtime.site'")
                conn.commit()
                conn.close()
            except sqlite3.Error:
                pass
        environment = dict(os.environ, VB_DATA_DIR=self.root)
        self.process = subprocess.Popen([EXE], env=environment)

        def running():
            snapshot = site_state(self)
            return snapshot if snapshot and snapshot.get("running") else None

        if self.site:
            wait_for(running, timeout=60, what="the phone server")
        else:
            wait_for(
                lambda: (
                    lambda snapshot: snapshot if snapshot.get("running") is False else None
                )(site_state(self) or {}),
                timeout=60,
                what="the phone server to report that it is stopped",
            )
        self.base = f"https://127.0.0.1:{self.port}"
        if not self.site:
            self.token = None
            return

        def token():
            try:
                conn = sqlite3.connect(sqlite_path(self.root), timeout=3)
                row = conn.execute("SELECT token FROM site_client WHERE id = 1").fetchone()
                conn.close()
                return row[0] if row and row[0] else None
            except (OSError, sqlite3.Error):
                return None

        if pair:
            # Ask the app for a pairing link, then read the token it stored.
            write_kv(self.root, "request.site_pair", "1")
            self.token = wait_for(token, timeout=20, what="a pairing token")
        else:
            # The token lives in the data directory: a restart must not pair
            # again, or the page's stored token would be revoked.
            self.token = token()
            if not self.token:
                raise RuntimeError("the restarted daemon lost its pairing token")
        print(f"daemon: {self.base}, token {self.token[:8]}…")

    def api(self, path, method="GET", body=None, auth=True):
        data = None
        headers = {}
        if body is not None:
            data = json.dumps(body, ensure_ascii=False).encode()
            headers["Content-Type"] = "application/json"
        if auth:
            headers["Authorization"] = "Bearer " + self.token
        request = urllib.request.Request(self.base + path, data=data, method=method, headers=headers)
        context = ssl._create_unverified_context()
        try:
            with urllib.request.urlopen(request, context=context, timeout=10) as response:
                return json.loads(response.read().decode())
        except urllib.error.HTTPError as error:
            return {"ok": False, "status": error.code, "body": error.read().decode()}

    def messages(self, monolog=1):
        return self.api(f"/site/api/monologues/{monolog}/messages").get("messages", [])

    def site_state(self):
        return site_state(self)

    def set_site(self, enabled, port=None):
        """Rewrite the config the way the window's switch does."""
        path = os.path.join(self.root, "config.json")
        with open(path, encoding="utf-8") as handle:
            config = json.load(handle)
        config["site_enabled"] = enabled
        if port is not None:
            config["site_port"] = port
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(config, handle, ensure_ascii=False)

    def page_status(self, path="/"):
        """HTTP status of the phone page, or 0 when nothing listens."""
        context = ssl._create_unverified_context()
        try:
            with urllib.request.urlopen(
                self.base + path, context=context, timeout=10
            ) as answer:
                return answer.status
        except urllib.error.HTTPError as error:
            return error.code
        except (OSError, urllib.error.URLError):
            return 0

    def local(self, path, method="GET", body=None):
        data = None
        headers = {}
        if body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            "http://127.0.0.1:%d%s" % (self.api_port, path),
            data=data,
            method=method,
            headers=headers,
        )
        for attempt in range(3):
            try:
                with urllib.request.urlopen(request, timeout=10) as answer:
                    return json.loads(answer.read().decode() or "{}")
            except urllib.error.HTTPError as error:
                return {"ok": False, "status": error.code, "body": error.read().decode()}
            except (ConnectionError, urllib.error.URLError) as error:
                if attempt == 2:
                    return {"ok": False, "error": str(error)}
                time.sleep(0.5)
        return {"ok": False, "error": "unreachable"}

    def take_engine(self):
        """Start a local dictation: the engine belongs to the computer."""
        answer = self.local("/api/record/start", "POST", {})
        return answer.get("sessionId")

    def free_engine(self, session):
        return self.local("/api/record/stop?session=%d" % session, "POST")

    def engine_state(self):
        return self.local("/api/status").get("state")

    def stop(self):
        if self.process:
            self.process.terminate()
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.kill()


# ---------------------------------------------------------------- browser

class Browser:
    """Minimal DevTools client: enough to drive the page like a user."""

    def __init__(self):
        self.profile = tempfile.mkdtemp(prefix="vb-chrome-")
        self.process = subprocess.Popen(
            [
                CHROME,
                f"--remote-debugging-port={CDP_PORT}",
                f"--user-data-dir={self.profile}",
                "--no-first-run",
                "--no-default-browser-check",
                "--ignore-certificate-errors",
                "--autoplay-policy=no-user-gesture-required",
                "--use-fake-ui-for-media-stream",
                # A headless-agent machine often leaves the phone window behind
                # the terminal. Chrome then pauses rAF in the occluded window,
                # and the halo canvas would stay on its last (cleared) frame.
                "--disable-backgrounding-occluded-windows",
                "--disable-renderer-backgrounding",
                "--disable-background-timer-throttling",
                "--disable-features=CalculateNativeWinOcclusion",
                "--window-size=430,900",
                "about:blank",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        self.socket = None
        self.counter = 0
        self.log = []
        # Everything the page asked for and everything it logged: the
        # DevTools messages come back while we wait for our own answer, so the
        # client below keeps them instead of dropping them.
        self.events = []
        self.requests = {}

    def connect(self):
        def page():
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{CDP_PORT}/json", timeout=2) as response:
                    targets = json.load(response)
                return next((t for t in targets if t["type"] == "page"), None)
            except OSError:
                return None

        target = wait_for(page, timeout=30, what="the browser")
        path = target["webSocketDebuggerUrl"].split(str(CDP_PORT), 1)[1]
        self.socket = socket.create_connection(("127.0.0.1", CDP_PORT), timeout=30)
        key = base64.b64encode(os.urandom(16)).decode()
        self.socket.sendall(
            (
                f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{CDP_PORT}\r\nUpgrade: websocket\r\n"
                f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        answer = b""
        while b"\r\n\r\n" not in answer:
            answer += self.socket.recv(1024)
        # Network: every request, its body and its answer. Runtime/Log: what
        # the page printed or threw. Both are needed to tell "the page never
        # made the chunk" apart from "the page made it and the computer refused
        # it".
        for method in ("Network.enable", "Runtime.enable", "Log.enable"):
            self.call(method)

    def _send(self, message):
        data = json.dumps(message).encode()
        mask = os.urandom(4)
        length = len(data)
        header = bytes([0x81])
        if length < 126:
            header += bytes([0x80 | length])
        elif length < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", length)
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", length)
        self.socket.sendall(header + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def events(self):
        """Everything the page said or threw since the last call."""
        seen = []
        self.socket.settimeout(0.2)
        try:
            while True:
                seen.append(self._receive_quiet())
        except (socket.timeout, TimeoutError):
            pass
        finally:
            self.socket.settimeout(30)
        return [event for event in seen if event]

    def _receive_quiet(self):
        return self._receive()

    def _receive(self):
        header = b""
        while len(header) < 2:
            header += self.socket.recv(2 - len(header))
        length = header[1] & 0x7F
        if length == 126:
            length = struct.unpack(">H", self.socket.recv(2))[0]
        elif length == 127:
            length = struct.unpack(">Q", self.socket.recv(8))[0]
        payload = b""
        while len(payload) < length:
            payload += self.socket.recv(length - len(payload))
        return json.loads(payload)

    def call(self, method, params=None):
        self.counter += 1
        identifier = self.counter
        self._send({"id": identifier, "method": method, "params": params or {}})
        while True:
            message = self._receive()
            if "method" in message:
                self.events.append(message)
                self._track(message)
                if message["method"] in (
                    "Runtime.exceptionThrown",
                    "Log.entryAdded",
                    "Runtime.consoleAPICalled",
                ):
                    self.log.append(message)
                continue
            if message.get("id") == identifier:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error']}")
                return message.get("result", {})

    def _track(self, event):
        """Keep a request table from the Network events as they pass by."""
        method = event.get("method", "")
        params = event.get("params", {})
        if method == "Network.requestWillBeSent":
            request = params.get("request") or {}
            sent = 0
            for entry in request.get("postDataEntries") or []:
                try:
                    sent += len(base64.b64decode(entry.get("bytes") or ""))
                except (ValueError, TypeError):
                    pass
            if not sent and request.get("postData"):
                sent = len(request["postData"].encode("utf-8", "replace"))
            self.requests[params.get("requestId")] = {
                "method": request.get("method", "?"),
                "url": request.get("url", ""),
                "sent": sent,
                "status": None,
                "received": 0,
                "error": "",
            }
        elif method == "Network.responseReceived":
            row = self.requests.get(params.get("requestId"))
            if row is not None:
                row["status"] = (params.get("response") or {}).get("status")
        elif method == "Network.loadingFinished":
            row = self.requests.get(params.get("requestId"))
            if row is not None:
                row["received"] = params.get("encodedDataLength", 0)
        elif method == "Network.loadingFailed":
            row = self.requests.get(params.get("requestId"))
            if row is not None:
                row["error"] = params.get("errorText", "") + (
                    " " + params["blockedReason"] if params.get("blockedReason") else ""
                )

    def evaluate(self, expression, await_promise=False):
        result = self.call(
            "Runtime.evaluate",
            {"expression": expression, "returnByValue": True, "awaitPromise": await_promise},
        )
        if "exceptionDetails" in result:
            raise RuntimeError(f"page error: {result['exceptionDetails']}")
        return result.get("result", {}).get("value")

    def navigate(self, url):
        result = self.call("Page.navigate", {"url": url})
        print(f"  navigate {url} -> {result}")
        return result

    def offline(self, enabled):
        self.call(
            "Network.emulateNetworkConditions",
            {
                "offline": enabled,
                "latency": 0,
                "downloadThroughput": -1,
                "uploadThroughput": -1,
            },
        )

    def stop(self):
        self.process.terminate()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()


# ---------------------------------------------------------------- audio

def speech_mp3(seconds=15):
    """A slice of the real recording, still encoded.

    The browser decodes MP3 itself; a decoder on this side would add a
    dependency for nothing. 128 kbit/s is about 16 KB per second.
    """
    with open(ASSET, "rb") as handle:
        return handle.read(int(16_000 * seconds))


MIC_SCRIPT = """
(async () => {
  const bytes = Uint8Array.from(atob("BASE64"), (c) => c.charCodeAt(0));
  const context = new AudioContext();
  const decoded = await new Promise((resolve, reject) => {
    context.decodeAudioData(bytes.buffer.slice(0), resolve, reject);
  });
  const destination = context.createMediaStreamDestination();
  window.__mic = { context, decoded, destination, source: null };
  navigator.mediaDevices.getUserMedia = async () => {
    // Every take starts its own copy of the recording: the page asks for the
    // microphone each time it records, and the test needs speech again.
    if (window.__mic.source) window.__mic.source.stop();
    const source = context.createBufferSource();
    source.buffer = decoded;
    source.connect(destination);
    // SKIP lets a take start in the middle of the recording: the first
    // seconds of the asset are quiet, and a short take needs speech.
    source.start(0, SKIP);
    window.__mic.source = source;
    return destination.stream;
  };
  return 'mic stubbed with ' + decoded.duration.toFixed(1) + ' s of speech';
})()
"""


def main():
    keep = "--keep" in sys.argv
    if not os.path.exists(EXE):
        sys.exit(f"missing {EXE}: build the release binaries first")
    if not os.path.isdir(PAGE):
        sys.exit(f"missing {PAGE}: the page sources are not where they belong")
    sync_page()
    diag = Diag(DIAG_PATH)
    print(f"diagnostics: {DIAG_PATH}")
    daemon = Daemon()
    browser = Browser()
    try:
        daemon.start()
        browser.connect()

        print("pairing")
        # The page and the token are checked separately: the redirect of the
        # pairing page is a browser detail, the app is what matters here. The
        # token is put into the page's storage the way the page itself would.
        # The page has to be really there before its storage can be touched:
        # on a cold browser the first navigation sometimes stays on about:blank,
        # and then localStorage is denied for an opaque origin.
        def loaded():
            try:
                return browser.evaluate(
                    "location.origin === 'https://127.0.0.1:" + str(PORT) + "'"
                    " && document.readyState === 'complete'"
                )
            except Exception:
                return False

        for attempt in range(4):
            browser.navigate(f"https://127.0.0.1:{PORT}/")
            try:
                wait_for(loaded, timeout=10, what="the page to load")
                break
            except TimeoutError:
                print(f"  page load failed (attempt {attempt + 1})")
        else:
            raise TimeoutError("the phone page never loaded")
        browser.evaluate(f"localStorage.setItem('vb_token', {daemon.token!r})")
        # The page read its storage when it booted and found no token, so it is
        # on the pairing screen even though the token is there now. A paired
        # phone reloads; the shell then carries #composer even on the list, where
        # the composer itself is hidden.
        browser.call("Page.reload", {"ignoreCache": True})
        wait_for(
            lambda: browser.evaluate("!!document.getElementById('composer')"),
            timeout=20,
            what="the app screen after pairing",
        )
        version = browser.evaluate(
            "(() => { const node = document.getElementById('version');"
            " if (!node) return null; const style = getComputedStyle(node);"
            " return { text: node.textContent.trim(),"
            "          size: parseFloat(style.fontSize),"
            "          color: style.color }; })()"
        )

        # A second load proves the token really lives in the page's storage, not
        # only in this document.
        browser.call("Page.reload", {"ignoreCache": True})
        wait_for(
            lambda: browser.evaluate(
                "!!document.getElementById('app') && !document.getElementById('app').hidden"
            ),
            timeout=20,
            what="the phone page with a token",
        )
        # The shell is the app: its boot set the document language and the
        # manifest href, and #composer exists as soon as it is mounted.
        wait_for(
            lambda: browser.evaluate("!!document.getElementById('composer')"),
            timeout=20,
            what="the shell with the pairing token",
        )
        check(True, "the phone page opened with the pairing token")
        check(
            version is not None
            and version["text"].startswith("v")
            and version["text"][1:2].isdigit(),
            f"the header shows the site version ({version})",
        )
        check(
            version is not None and version["size"] <= 11.5,
            f"the version is small and quiet ({version})",
        )
        served = browser.evaluate(
            "fetch('version.json', { cache: 'no-store' })"
            ".then((response) => response.json()).then((data) => data.site)",
            await_promise=True,
        )
        check(
            version is not None and served == version["text"][1:],
            f"the computer serves the same version the page runs ({served})",
        )

        manifest = browser.evaluate(
            "(() => ({ href: document.getElementById('manifest').href,"
            " lang: document.documentElement.lang }))()"
        )
        check(
            manifest["href"].endswith("/manifest." + manifest["lang"] + ".webmanifest"),
            f"the manifest follows the page language ({manifest})",
        )
        described = browser.evaluate(
            "fetch(document.getElementById('manifest').href).then((r) => r.json())"
            ".then((m) => ({ ok: true, lang: m.lang, text: m.description }))"
            ".catch((e) => ({ ok: false, error: String(e) }))",
            await_promise=True,
        )
        check(
            described.get("ok")
            and described.get("lang") == manifest["lang"]
            and len(described.get("text", "")) > 10,
            f"the manifest describes the page in that language ({described})",
        )
        install = browser.evaluate(
            "fetch('/install').then((r) => r.text().then((t) => ({ ok: r.ok, text: t })))"
            ".catch((e) => ({ ok: false, text: String(e) }))",
            await_promise=True,
        )
        guide = install.get("text", "")
        check(
            install.get("ok") and "lang/install." in guide and "<h1" in guide,
            "the install guide is served, styled inline and localized",
        )
        # The new page carries its own styles and loads the guide text from
        # lang/install.<lang>.json, so the old separate stylesheet is gone.
        sheet = browser.evaluate(
            "fetch('/lang/install.en.json').then((r) => r.status).catch(() => 0)",
            await_promise=True,
        )
        check(sheet == 200, f"the page stylesheet is served ({sheet})")

        print("a note is an overlay that never moves the view")
        # The note is a dismissible overlay above the dock. It speaks once per
        # condition, leaves the scrolling viewport exactly as it was, and a tap
        # on the banner removes it until the condition ends and comes back.
        wait_for(
            lambda: browser.evaluate("!!document.getElementById('install')"),
            timeout=20,
            what="the install bar",
        )
        browser.evaluate("document.getElementById('install').hidden = false")
        before_note = view_geometry(browser)
        browser.evaluate("window.dispatchEvent(new Event('offline'))")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('#notes .banner').length > 0"),
            timeout=10,
            what="the note about the lost connection",
        )
        # The link is still down: the same condition must not add a second note.
        browser.evaluate("window.dispatchEvent(new Event('offline'))")
        time.sleep(0.6)
        notes = json.loads(
            browser.evaluate(
                "JSON.stringify((() => { const notes = document.getElementById('notes');"
                " const style = getComputedStyle(notes);"
                " const footer = document.querySelector('footer.about');"
                " const banner = notes.querySelector('.banner');"
                " const inside = (node) => { if (!node) return null;"
                "   const box = node.getBoundingClientRect();"
                "   return { top: Math.round(box.top), bottom: Math.round(box.bottom),"
                "     left: Math.round(box.left), right: Math.round(box.right) }; };"
                " return { count: notes.querySelectorAll('.banner').length,"
                "   position: style.position, pointer: style.pointerEvents,"
                "   tag: banner ? banner.tagName : '',"
                "   classes: banner ? banner.className : '',"
                "   text: banner && banner.querySelector('.banner-text')"
                "     ? banner.querySelector('.banner-text').textContent : '',"
                "   close: !!(banner && banner.querySelector('.banner-close')),"
                "   banner: inside(banner), footer: inside(footer),"
                "   width: document.documentElement.scrollWidth, page: innerWidth }; })())"
            )
        )
        after_note = view_geometry(browser)
        check(
            notes["count"] == 1,
            f"the lost link has one note, not a pile ({notes['count']} banners)",
        )
        check(
            before_note == after_note,
            "the note does not change the view's scrollHeight, clientHeight or scrollTop"
            f" ({before_note} then {after_note})",
        )
        check(
            notes["position"] == "absolute" and notes["pointer"] == "none",
            f"the note is an overlay, not a row ({notes['position']}, pointer {notes['pointer']})",
        )
        check(
            notes["tag"] == "BUTTON"
            and "banner" in notes["classes"].split()
            and notes["text"]
            and notes["close"],
            f"the note is a dismissible banner button ({notes})",
        )
        check(
            notes["banner"] is not None
            and notes["footer"] is not None
            and notes["banner"]["bottom"] <= notes["footer"]["top"],
            f"the note floats above the install bar, not on it ({notes})",
        )
        check(
            notes["width"] <= notes["page"],
            f"the note does not push the page sideways ({notes['width']} of {notes['page']})",
        )
        # A real pointer tap, not element.click(): the overlay's hit testing is
        # part of what makes the note dismissible.
        check(pointer_click(browser, "#notes .banner"), "the note banner is tappable")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('#notes .banner').length === 0"),
            timeout=5,
            what="the tapped note to go away",
        )
        check(True, "tapping the note removes it")
        # Dismissed, but the link is still down: the same condition stays quiet.
        browser.evaluate("window.dispatchEvent(new Event('offline'))")
        time.sleep(0.6)
        check(
            browser.evaluate("document.querySelectorAll('#notes .banner').length === 0"),
            "the note does not come back while the link stays down",
        )
        # A new loss after a recovery is a new condition and speaks again.
        browser.evaluate("window.dispatchEvent(new Event('online'))")
        time.sleep(0.2)
        browser.evaluate("window.dispatchEvent(new Event('offline'))")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('#notes .banner').length > 0"),
            timeout=5,
            what="the note after a new loss",
        )
        check(True, "a new loss after a recovery shows the note again")
        # The note also leaves when it is swiped away, the way a phone dismisses a
        # banner: the finger drags it sideways, past the threshold it goes.
        swiped = browser.evaluate(
            "(() => { const banner = document.querySelector('#notes .banner');"
            " if (!banner) return 'no banner';"
            " const box = banner.getBoundingClientRect();"
            " const start = { clientX: box.left + box.width / 2, clientY: box.top + box.height / 2 };"
            " const fire = (type, x) => banner.dispatchEvent(new PointerEvent(type, {"
            "   bubbles: true, cancelable: true, pointerId: 7, pointerType: 'touch',"
            "   clientX: start.clientX + x, clientY: start.clientY }));"
            " fire('pointerdown', 0);"
            " for (const step of [12, 30, 55, 80, 110]) fire('pointermove', step);"
            " fire('pointerup', 130);"
            " return 'swiped'; })()"
        )
        check(swiped == "swiped", f"the note accepts a sideways gesture ({swiped})")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('#notes .banner').length === 0"),
            timeout=5,
            what="the swiped note to go away",
        )
        check(True, "swiping the note away removes it")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('#notes .banner').length === 0"),
            timeout=6,
            what="the note to fade on its own",
        )
        check(True, "the note disappears on its own")
        browser.evaluate("document.getElementById('install').hidden = true")
        wait_for(lambda: daemon.api("/site/api/me").get("paired"), timeout=10, what="pairing")

        print("monolog")
        monolog = daemon.api("/site/api/monologues", "POST", {"name": "Проверка"})
        check(monolog.get("ok"), "the monolog is created")
        # The page asks for monologs when it boots, so a monolog made behind its
        # back needs a reload before the list can show it. A reload can land
        # while the old document is still being replaced; retrying is cheaper
        # than a flaky run.
        for attempt in range(3):
            browser.call("Page.reload", {"ignoreCache": True})
            try:
                wait_for(
                    lambda: browser.evaluate(
                        "document.querySelectorAll('.monolog-card').length > 0"
                    ),
                    timeout=12,
                    what="the monolog list",
                )
                break
            except TimeoutError:
                state = browser.evaluate(
                    "JSON.stringify({ cards: document.querySelectorAll('.monolog-card').length,"
                    " title: document.getElementById('title').textContent,"
                    " href: location.href,"
                    " app: window.app ? { monolog: window.app.monolog,"
                    "   archived: window.app.archived, loop: !!window.app.loop } : null,"
                    " trace: (window.__trace || []).length,"
                    " errors: (window.__errors || []).slice(-2) })"
                )
                print(f"  monolog list did not appear (attempt {attempt + 1}): {state}")
        else:
            raise TimeoutError("the monolog list never appeared")

        speech = browser.evaluate(MIC_SCRIPT.replace("BASE64", base64.b64encode(speech_mp3()).decode()).replace("SKIP", "0"),
            await_promise=True,
        )
        print("mic:", speech)
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.monolog-card').length > 0"),
            timeout=10,
            what="the monolog list",
        )
        browser.evaluate("document.querySelectorAll('.monolog-card')[0].click()")
        wait_for(
            lambda: browser.evaluate("!document.getElementById('composer').hidden"),
            timeout=10,
            what="the composer",
        )

        print("recording")
        browser.evaluate("document.getElementById('record').click()")
        # Starting is asynchronous (storage, microphone, audio context); waiting
        # for the state is steadier than a fixed sleep under load.
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('record').classList.contains('recording')"
            ),
            timeout=10,
            what="the recording state",
        )
        check(True, "the record button shows the recording state")
        snapshot(browser, daemon, diag, "just after the record click")

        print("the composer bar on the list")
        # Leaving the monolog must not stop the take: the list marks the monolog
        # and the same composer bar, with the monolog's name, keeps the running
        # take in sight and stoppable outside it.
        feed_geometry = view_geometry(browser)
        browser.evaluate("document.getElementById('back').click()")
        wait_for(
            lambda: browser.evaluate("document.getElementById('title').textContent === 'Монологи'"),
            timeout=10,
            what="the list while recording",
        )
        marked = browser.evaluate(
            "(() => { const card = document.querySelector('.monolog-card.active');"
            " if (!card) return '';"
            " return (card.textContent || '').trim(); })()"
        )
        check(
            "идёт расшифровка" in marked,
            f"the list marks the monolog that is being recorded ({marked!r})",
        )
        bar = json.loads(
            browser.evaluate(
                "JSON.stringify((() => { const composer = document.getElementById('composer');"
                " const record = document.getElementById('record');"
                " const view = document.getElementById('view');"
                " const name = document.getElementById('rec-name');"
                " const viewBox = view.getBoundingClientRect();"
                " const barBox = composer.getBoundingClientRect();"
                " return { hidden: composer.hidden,"
                "   record: !!record,"
                "   recordClass: record ? record.className : '',"
                "   title: (document.getElementById('rec-title') || {}).textContent || '',"
                "   timer: (document.getElementById('timer') || {}).textContent || '',"
                "   name: name ? name.textContent : null,"
                "   viewBottom: Math.round(viewBox.bottom),"
                "   barTop: Math.round(barBox.top),"
                "   position: getComputedStyle(composer).position,"
                "   geom: { clientHeight: view.clientHeight, scrollTop: view.scrollTop } }; })())"
            )
        )
        check(
            bar["hidden"] is False and bar["record"],
            f"the composer bar is on the list while recording ({bar})",
        )
        check(
            "recording" in bar["recordClass"].split(),
            f"the list's record button shows the take is live ({bar['recordClass']!r})",
        )
        # The name sits on the state line after a dot: "Идёт запись · Проверка".
        check(
            bar["title"].startswith("Идёт запись"),
            f"the list bar names the state ({bar['title']!r})",
        )
        check(
            "Проверка" in (bar["name"] or ""),
            f"the list bar names the monolog being recorded ({bar['name']!r})",
        )
        check(
            "·" in bar["title"] and bar["title"].strip().endswith("Проверка"),
            f"the monolog name follows the state on the same line ({bar['title']!r})",
        )
        first_timer = bar["timer"]
        check(
            len(first_timer) == 5
            and first_timer[2] == ":"
            and first_timer.replace(":", "").isdigit(),
            f"the list bar has a running timer ({first_timer!r})",
        )
        # Outside a monolog the bar floats above the dock on purpose: leaving a
        # monolog in the middle of a take must not move the list by the height of
        # the bar. So the bar overlaps the view instead of taking a row from it,
        # and the list keeps the height it had before the take started.
        check(
            bar.get("position") == "absolute" and bar["viewBottom"] > bar["barTop"],
            f"outside a monolog the bar floats over the list instead of taking its height ({bar})",
        )
        check(
            bar["geom"]["clientHeight"] > feed_geometry["clientHeight"]
            and bar["geom"]["scrollTop"] == 0,
            "the list keeps its full height while the floating bar is up"
            f" ({feed_geometry} then {bar['geom']})",
        )
        wait_for(
            lambda: (
                lambda value: value != first_timer and len(value) == 5
            )(
                browser.evaluate(
                    "(document.getElementById('timer') || {}).textContent || ''"
                )
            ),
            timeout=4,
            what="the composer timer to run",
        )
        check(True, "the composer timer runs on the list")
        stayed = view_geometry(browser)
        check(
            stayed["clientHeight"] == bar["geom"]["clientHeight"]
            and stayed["scrollTop"] == bar["geom"]["scrollTop"],
            f"the live bar does not move the view ({bar['geom']} then {stayed})",
        )
        # Tapping the marked card returns to the recording monolog; there is no
        # separate plate to tap any more.
        browser.evaluate("document.querySelector('.monolog-card.active').click()")
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('title').textContent === 'Проверка'"
                " && !document.getElementById('composer').hidden"
                " && document.getElementById('record').classList.contains('recording')"
            ),
            timeout=10,
            what="the recording monolog again",
        )
        check(True, "tapping the marked card opens the recording monolog")
        snapshot(browser, daemon, diag, "3 s into the recording")

        print("text while the recording is still running")

        def live_partial():
            messages = daemon.api("/site/api/monologues/1/messages").get("messages", [])
            if not messages:
                return None
            newest = max(messages, key=lambda item: item["id"])
            if newest["stage"] not in ("uploading", "transcribing"):
                return None
            spoken = newest["text"].strip()
            return spoken if len(spoken) > 3 else None

        partial = None
        try:
            # The machine may be busy with the rest of the suite: give the
            # first partial a little longer, the recording stays open either way.
            partial = wait_for(live_partial, timeout=25, what="partial text during the recording")
        except TimeoutError:
            pass
        check(
            partial is not None,
            f"the computer writes text while the take is still recording ({partial!r})",
        )

        def drawn_partial():
            return browser.evaluate(
                "(() => { const text = Array.from(document.querySelectorAll('.message .text'))"
                ".find((node) => !node.classList.contains('empty'));"
                " return text ? text.textContent.trim() : ''; })()"
            )

        if partial:
            shown = wait_for(
                lambda: drawn_partial() if len(drawn_partial() or "") > 3 else None,
                timeout=3,
                what="the phone to draw the partial text",
            )
            check(
                partial.startswith(shown),
                f"the phone shows the text while the recording is still open ({shown[:40]!r})",
            )
        time.sleep(1)
        # The same composer bar on the list stops the take: there is no separate
        # plate any more, and the bar is the only control outside the monolog.
        browser.evaluate("document.getElementById('back').click()")
        wait_for(
            lambda: browser.evaluate("document.getElementById('title').textContent === 'Монологи'"),
            timeout=10,
            what="the list to stop the take",
        )
        timer_now = browser.evaluate(
            "(document.getElementById('timer') || {}).textContent || ''"
        )
        check(
            timer_now != first_timer,
            f"the composer timer kept running on the list ({first_timer} then {timer_now})",
        )
        browser.evaluate("document.getElementById('record').click()")
        wait_for(
            lambda: browser.evaluate("document.getElementById('composer').hidden"),
            timeout=10,
            what="the composer bar to leave after Stop",
        )
        check(True, "the record button on the list stops the take")
        # Back into the monolog to watch the text that arrives after the stop.
        browser.evaluate("document.querySelectorAll('.monolog-card')[0].click()")
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('title').textContent === 'Проверка'"
                " && !document.getElementById('composer').hidden"
            ),
            timeout=10,
            what="the monolog after the stop",
        )
        check(
            not browser.evaluate(
                "document.getElementById('record').classList.contains('recording')"
            ),
            "the record button leaves the recording state after the list Stop",
        )
        time.sleep(0.2)
        snapshot(browser, daemon, diag, "just after the stop click")
        time.sleep(2)
        snapshot(browser, daemon, diag, "2 s after the stop")

        trace = json.loads(
            browser.evaluate(
                "JSON.stringify((window.app && window.app.trace) || window.__trace || [])"
            )
        )
        diag.section("full page trace")
        for entry in trace:
            diag.line("  " + str(entry))
        print("  trace:", trace[:6], "…", trace[-3:])
        print(f"  trace: {len(trace)} entries")
        for event in browser.events:
            if event.get("method") in ("Runtime.exceptionThrown", "Log.entryAdded", "Runtime.consoleAPICalled"):
                diag.line("page: " + json.dumps(event.get("params", {}), ensure_ascii=False)[:800])

        state = daemon_db(daemon)
        diag.section("daemon db.json after the stop")
        diag.line(json.dumps(state, ensure_ascii=False, indent=1))
        dump_network(browser, diag, "requests after the stop", bodies=True)
        print(f"  audio POSTs: {json.dumps(audio_posts(browser), ensure_ascii=False)}")
        print("  daemon log tail:")
        for entry in log_tail(daemon, 15):
            print("    " + entry)

        print("waiting for the text")
        def finished():
            state = daemon.api("/site/api/monologues/1/messages").get("messages", [])
            if state:
                # The page may have created an empty take before the audio one;
                # the newest message is the one this run is about.
                first = max(state, key=lambda item: item["id"])
                print(
                    f"  … stage {first['stage']}, {first['duration']:.1f} s of audio,"
                    f" {first['segments_done']} segments, {len(first['text'])} chars"
                )
                return first if first["stage"] not in ("draft", "uploading", "queued", "transcribing") else None
            return None

        # Five seconds of speech costs a few seconds of decoding. Waiting longer
        # than half a minute means something is broken, and the state below says
        # what, so the run reports instead of sitting there.
        try:
            message = wait_for(finished, timeout=40, what="the transcription")
        except TimeoutError as error:
            check(False, str(error))
            print("  messages:", json.dumps(daemon.messages(), ensure_ascii=False)[:2000])
            raise
        check(message["stage"] == "done", f"the message is done (stage {message['stage']})")
        check(len(message["text"]) > 20, f"the text arrived ({len(message['text'])} chars)")
        if partial:
            check(
                message["text"].strip().startswith(partial),
                "the final text continues what the phone showed live",
            )
        check(
            message["recorded"].startswith("20"),
            "the message keeps the phone's own timestamp",
        )

        print("one card, not two")
        cards = browser.evaluate(
            "Array.from(document.querySelectorAll('.message')).map((c) => c.textContent.trim().slice(0, 30))"
        )
        check(len(cards) == 1, f"exactly one card for one recording (got {len(cards)}: {cards})")

        print("the chat keeps the reader's place and back closes one screen")
        browser.evaluate("document.getElementById('view').scrollTop = 0")
        time.sleep(2.2)
        held = browser.evaluate("document.getElementById('view').scrollTop")
        check(held < 5, f"reading older messages keeps the scroll position ({held})")

        browser.evaluate("document.getElementById('menu').click()")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.sheet-backdrop').length > 0"),
            timeout=10,
            what="the monolog menu",
        )
        browser.evaluate("history.back()")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.sheet-backdrop').length === 0"),
            timeout=10,
            what="back to close the sheet",
        )
        check(
            browser.evaluate("document.getElementById('title').textContent === 'Проверка'"),
            "back closes the sheet and stays in the monolog",
        )
        browser.evaluate("history.back()")
        wait_for(
            lambda: browser.evaluate("document.getElementById('title').textContent === 'Монологи'"),
            timeout=10,
            what="the next back to return to the list",
        )
        check(True, "the next back returns to the list, not out of the app")
        # The editing flow below needs the monolog again.
        browser.evaluate("document.querySelectorAll('.monolog-card')[0].click()")
        wait_for(
            lambda: browser.evaluate("!document.getElementById('composer').hidden"),
            timeout=10,
            what="the monolog again",
        )

        print("editing")
        # The message menu lives in a sheet now; editing itself is a full screen.
        wait_for(
            lambda: browser.evaluate("!!document.querySelector('.message')"),
            timeout=10,
            what="the message card",
        )
        long_text = "Правка текста без потери смысла. " * 14
        browser.evaluate("document.querySelector('.message').click()")
        wait_sheet(browser)
        done_items = sheet_labels(browser)
        check(
            "Изменить текст" in done_items
            and "Повторить распознавание" not in done_items,
            f"a done message offers edit but no recognition retry ({done_items})",
        )
        check(sheet_click(browser, "Изменить текст"), "the message menu offers an edit")
        wait_for(
            lambda: browser.evaluate("!!document.querySelector('.vb-edit-field')"),
            timeout=10,
            what="the full-screen editor",
        )
        browser.evaluate(
            "(() => { const box = document.querySelector('.vb-edit-field');"
            "box.value = " + json.dumps(long_text, ensure_ascii=False) + ";"
            "box.dispatchEvent(new Event('input', { bubbles: true })); })()"
        )
        check(
            browser.evaluate("document.getElementById('edit-save').disabled === false"),
            "the editor enables Сохранить once the text changes",
        )
        browser.evaluate("document.getElementById('edit-save').click()")
        wait_for(
            lambda: browser.evaluate("!document.querySelector('.vb-edit')"),
            timeout=10,
            what="the editor to close",
        )
        wait_for(
            lambda: daemon.messages()[0]["text"] == long_text,
            timeout=15,
            what="the edited text on the computer",
        )
        check(True, "the edit reached the computer")

        print("a long message folds and expands")
        wait_for(
            lambda: browser.evaluate("!!document.querySelector('.message .more')"),
            timeout=10,
            what="the show-more button",
        )
        more = browser.evaluate("document.querySelector('.message .more').textContent.trim()")
        check(
            more == "Показать полностью",
            f"the long message offers «Показать полностью» ({more!r})",
        )
        folded = browser.evaluate("document.querySelector('.message .text').textContent")
        expected = long_text[:240].rstrip() + "…"
        check(
            folded.strip() == expected,
            "the long message is folded to its first lines"
            f" ({len(folded)} of {len(long_text)} chars)",
        )
        browser.evaluate("document.querySelector('.message .more').click()")
        wait_for(
            lambda: browser.evaluate(
                "document.querySelector('.message .more').textContent.trim() === 'Свернуть'"
            ),
            timeout=5,
            what="the expanded message",
        )
        full = browser.evaluate("document.querySelector('.message .text').textContent")
        check(
            full.strip() == long_text.strip(),
            f"tapping «Показать полностью» shows the whole text ({len(full)} chars)",
        )
        browser.evaluate("document.querySelector('.message .more').click()")
        wait_for(
            lambda: browser.evaluate(
                "document.querySelector('.message .more').textContent.trim() === 'Показать полностью'"
            ),
            timeout=5,
            what="the folded message again",
        )
        check(True, "«Свернуть» folds the message again")

        print("only a failed take can be retried")
        # A finished message has no retry (checked in its menu above); a take the
        # computer could not transcribe still does. A draft finished with no audio
        # becomes exactly that failed take.
        failed_draft = daemon.api("/site/api/messages", "POST", {"monolog": 1})
        failed_id = int((failed_draft.get("message") or {}).get("id") or 0)
        failed_finish = daemon.api(
            f"/site/api/messages/{failed_id}/finish", "POST"
        )
        check(
            failed_finish.get("stage") == "failed",
            f"the computer made a failed take to inspect ({failed_finish})",
        )
        failed_card = json.dumps(f'.message[data-id="{failed_id}"]')
        wait_for(
            lambda: browser.evaluate(
                "(document.querySelector(" + failed_card + ") || {}).textContent || ''"
            ).find("ошибка распознавания") >= 0,
            timeout=20,
            what="the failed message on the phone",
        )
        browser.evaluate(f"document.querySelector({failed_card}).click()")
        wait_sheet(browser)
        failed_items = sheet_labels(browser)
        # This failed message was created without any audio (a draft that was
        # finished at once), so the computer has nothing to re-read: a retry could
        # only fail, and the button is deliberately absent. A failed message that
        # still has its audio keeps the button — that case is covered by the Rust
        # test `only_a_failed_take_can_be_retried`.
        check(
            "Повторить распознавание" not in failed_items,
            f"a failed message with no audio offers no retry ({failed_items})",
        )
        check(
            "Удалить сообщение" in failed_items,
            f"a failed message can still be deleted ({failed_items})",
        )
        browser.evaluate("history.back()")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.sheet-backdrop').length === 0"),
            timeout=10,
            what="the failed message sheet to close",
        )

        print("the network drops in the middle of a take")
        before_drop = len(daemon.messages())
        # Start in the middle of the recording: a short take has to contain
        # speech, otherwise there is nothing for the computer to transcribe.
        drop_mic = browser.evaluate(
            MIC_SCRIPT.replace("BASE64", base64.b64encode(speech_mp3()).decode()).replace(
                "SKIP", "25"
            ),
            await_promise=True,
        )
        print("  drop take mic:", drop_mic)
        browser.evaluate("document.getElementById('record').click()")
        time.sleep(12)
        browser.offline(True)
        # The microphone must not claim to be working while nothing can reach
        # the computer: violet only when the take is actually moving.
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('record').classList.contains('offline')"
            ),
            timeout=15,
            what="the microphone to admit the lost connection",
        )
        check(
            browser.evaluate(
                "document.getElementById('record').classList.contains('recording')"
            ),
            "the microphone keeps showing a live take while the computer is unreachable",
        )
        background = browser.evaluate(
            "getComputedStyle(document.getElementById('record')).backgroundColor"
        )
        check(
            background == "rgb(199, 243, 107)",
            f"the record button stays lime while the take runs ({background})",
        )
        painted = 0
        deadline = time.time() + 4
        while time.time() < deadline and painted == 0:
            painted = browser.evaluate(
                "(() => { const canvas = document.querySelector('.vb-record-rings');"
                " if (!canvas || !canvas.classList.contains('live')) return 0;"
                " const context = canvas.getContext('2d');"
                " if (!context) return 0;"
                " const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;"
                " let drawn = 0;"
                " for (let index = 3; index < pixels.length; index += 4) {"
                "   if (pixels[index] > 0) drawn += 1; }"
                " return drawn; })()"
            )
            if painted:
                break
            time.sleep(0.2)
        check(
            painted > 0,
            f"the halo rings keep drawing while the computer is unreachable"
            f" ({painted} painted pixels)",
        )
        check(
            browser.evaluate(
                "document.getElementById('rec-title').textContent === 'Запись без связи'"
            ),
            "only the wording says the computer is away",
        )
        time.sleep(8)
        browser.evaluate("document.getElementById('record').click()")
        wait_for(
            lambda: browser.evaluate(
                "!document.getElementById('record').classList.contains('recording')"
                " && !document.getElementById('record').classList.contains('offline')"
            ),
            timeout=10,
            what="the microphone to stop after the take is stopped",
        )
        check(True, "the microphone leaves the recording state when the take stops")
        time.sleep(3)
        check(
            len(daemon.messages()) == before_drop + 1,
            "the take started while the phone was online, so the computer knows it",
        )
        partial = max(daemon.messages(), key=lambda item: item["id"])
        check(
            partial["duration"] > 1.0,
            f"the computer already has the first seconds ({partial['duration']:.1f} s)",
        )
        kept = [
            take
            for take in summarise_outbox(outbox_state(browser))
            if (take.get("bytes") or 0) > 0
        ]
        check(
            len(kept) == 1,
            f"the rest of the take waits in the phone's storage ({kept})",
        )
        print(f"  kept on the phone: {json.dumps(kept, ensure_ascii=False)}")
        browser.offline(False)

        def dropped_final():
            newest = max(daemon.messages(), key=lambda item: item["id"])
            return newest if newest["stage"] not in (
                "draft",
                "uploading",
                "queued",
                "transcribing",
            ) else None

        dropped = wait_for(
            dropped_final,
            timeout=90,
            what="the pieces kept on the phone to arrive and be transcribed",
        )
        check(
            dropped["duration"] > partial["duration"] + 1.0,
            "the pieces that waited on the phone arrived after the connection returned"
            f" ({partial['duration']:.1f} s then {dropped['duration']:.1f} s)",
        )
        check(
            dropped["stage"] in ("done", "silent"),
            f"the take finished after the drop (stage {dropped['stage']})",
        )
        if dropped["stage"] == "done":
            check(
                len(dropped["text"]) > 3,
                f"the computer transcribed the pieces once it was free ({len(dropped['text'])} chars)",
            )

        print("deleting a message")
        # The per-message actions are a sheet now: tapping the message opens it,
        # and the delete asks for a confirmation before it reaches the computer.
        before = len(daemon.messages())
        browser.evaluate("document.querySelectorAll('.message')[0].click()")
        wait_sheet(browser)
        check(
            sheet_click(browser, "Удалить сообщение"),
            "the message menu offers a delete",
        )
        wait_sheet(browser, "Удалить сообщение?")
        check(
            len(daemon.messages()) == before,
            "the message is still on the computer until the confirmation",
        )
        check(sheet_click(browser, "Удалить"), "the confirmation deletes the message")
        wait_for(
            lambda: len(daemon.messages()) < before,
            timeout=15,
            what="the deletion on the computer",
        )
        check(True, "the deletion reached the computer")

        print("archiving")
        # The monolog menu in the header holds the archive action: use it exactly
        # as a phone would instead of calling the route behind the page's back.
        # The label comes from lang/ru.json, where the "archive" key is "Архив".
        open_before = daemon.api("/site/api/monologues").get("monologs", [])
        check(
            any(item.get("id") == 1 and item.get("archived") is False for item in open_before),
            "the monolog is open before archiving",
        )
        wait_for(
            lambda: browser.evaluate("!document.getElementById('menu').hidden"),
            timeout=10,
            what="the monolog menu button",
        )
        browser.evaluate("document.getElementById('menu').click()")
        wait_sheet(browser)
        menu_labels = sheet_labels(browser)
        check(
            sheet_click(browser, "Архив"),
            f"the phone offers an archive action for the monolog (buttons {menu_labels})",
        )
        wait_for(
            lambda: any(
                item.get("id") == 1 and item.get("archived")
                for item in daemon_db(daemon).get("monologues", [])
            ),
            timeout=15,
            what="the archive on the computer",
        )
        check(True, "the archive reached the computer")
        open_after = daemon.api("/site/api/monologues").get("monologs", [])
        check(
            all(item.get("id") != 1 for item in open_after),
            "the archived monolog is absent from the computer's open list"
            f" (got {[item.get('id') for item in open_after]})",
        )
        archived = daemon.api("/site/api/monologues?archived=1").get("monologs", [])
        check(
            any(item.get("id") == 1 and item.get("archived") for item in archived),
            "the archived monolog is present in the computer's archive list",
        )
        # The page returned to the open list on its own, and the next fetch there
        # must not contain the archived monolog.
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('title').textContent === 'Монологи'"
                " && document.querySelectorAll('.monolog-card').length === 0"
            ),
            timeout=15,
            what="the open list without the archived monolog",
        )
        check(True, "the phone hides the archived monolog from the open list")
        # The archive is an item of the list menu now: there is no header button
        # for it any more.
        browser.evaluate("document.getElementById('menu').click()")
        wait_sheet(browser)
        check(
            any(label.startswith("Архив") for label in sheet_labels(browser)),
            "the list menu offers the archive",
        )
        check(sheet_click(browser, "Архив"), "the list menu opens the archive")
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('title').textContent === 'Архив'"
                " && document.querySelectorAll('.monolog-card').length === 1"
            ),
            timeout=10,
            what="the archive view",
        )
        archived_cards = browser.evaluate(
            "Array.from(document.querySelectorAll('.monolog-card')).map((c) => c.textContent)"
        )
        check(
            len(archived_cards) == 1 and "Проверка" in archived_cards[0],
            "the phone shows the archived monolog in the archive"
            f" ({archived_cards})",
        )
        check(
            browser.evaluate(
                "document.querySelector('.monolog-card').dataset.swipe === 'unarchive'"
            ),
            "a card in the archive swipes out of it, not into it",
        )
        # The archive is read-only: no microphone, and the message sheet only
        # offers a copy and a way back out of the archive.
        browser.evaluate("document.querySelector('.monolog-card').click()")
        wait_for(
            lambda: browser.evaluate("document.getElementById('title').textContent === 'Проверка'"),
            timeout=10,
            what="the archived monolog",
        )
        check(
            browser.evaluate("document.getElementById('composer').hidden"),
            "an archived monolog has no microphone",
        )
        wait_for(
            lambda: browser.evaluate("!!document.querySelector('.message')"),
            timeout=10,
            what="the archived message card",
        )
        check(
            not browser.evaluate(
                "Array.from(document.querySelectorAll('.message .actions button'))"
                ".some((b) => ['Изменить', 'Удалить', 'Повторить'].includes(b.textContent.trim()))"
            ),
            "an archived monolog has no inline edit or delete actions",
        )
        browser.evaluate("document.querySelector('.message').click()")
        wait_sheet(browser)
        archived_items = sheet_labels(browser)
        check(
            "Копировать" in archived_items and "Вернуть из архива" in archived_items,
            f"the archived message sheet offers a copy and a way back ({archived_items})",
        )
        check(
            not any(
                label in archived_items
                for label in ("Изменить текст", "Удалить сообщение", "Повторить распознавание")
            ),
            "the archived message sheet has no edit or delete",
        )
        browser.evaluate("history.back()")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.sheet-backdrop').length === 0"),
            timeout=10,
            what="the archived message sheet to close",
        )
        browser.evaluate("document.getElementById('menu').click()")
        wait_sheet(browser)
        archived_menu = sheet_labels(browser)
        check(
            "Вернуть из архива" in archived_menu,
            f"the archive menu offers to restore the monolog ({archived_menu})",
        )
        check(
            not browser.evaluate("!!document.querySelector('.sheet textarea, .sheet input')"),
            "the archive menu has no name field",
        )
        browser.evaluate("history.back()")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.sheet-backdrop').length === 0"),
            timeout=10,
            what="the archive menu to close",
        )
        browser.evaluate("document.getElementById('back').click()")
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('title').textContent === 'Архив'"
                " && document.querySelectorAll('.monolog-card').length === 1"
            ),
            timeout=10,
            what="the archive list again",
        )

        print("swiping a monolog into and out of the archive")
        # The card listens to pointer events: a clear sideways pull archives it
        # in the open list, and takes it back out of the archive.
        gesture = swipe_card(browser, 110)
        check(
            isinstance(gesture, dict) and gesture.get("swipe") == "unarchive",
            f"the archive card was pulled sideways ({gesture})",
        )
        wait_for(
            lambda: not any(
                item.get("id") == 1 and item.get("archived")
                for item in daemon_db(daemon).get("monologues", [])
            ),
            timeout=15,
            what="the unarchive on the computer",
        )
        check(True, "swiping a card in the archive unarchives it")
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('title').textContent === 'Архив'"
                " && document.querySelectorAll('.monolog-card').length === 0"
            ),
            timeout=15,
            what="the archive without the restored monolog",
        )
        check(True, "the restored monolog leaves the archive")

        # A fresh load puts the restored monolog back in the open list.
        browser.navigate(daemon.base + "/")
        wait_for(
            lambda: browser.evaluate(
                "(() => { const title = document.getElementById('title');"
                " return !!title && title.textContent === 'Монологи'"
                " && document.querySelectorAll('.monolog-card').length === 1; })()"
            ),
            timeout=20,
            what="the open list with the restored monolog",
        )
        check(
            browser.evaluate(
                "document.querySelector('.monolog-card').dataset.swipe === 'archive'"
            ),
            "a card in the open list swipes into the archive, not out of it",
        )
        gesture = swipe_card(browser, -110)
        check(
            isinstance(gesture, dict) and gesture.get("swipe") == "archive",
            f"the list card was pulled sideways ({gesture})",
        )
        wait_for(
            lambda: any(
                item.get("id") == 1 and item.get("archived")
                for item in daemon_db(daemon).get("monologues", [])
            ),
            timeout=15,
            what="the archive on the computer after the swipe",
        )
        check(True, "swiping a card in the open list archives it")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.monolog-card').length === 0"),
            timeout=15,
            what="the open list without the swiped monolog",
        )
        check(True, "the swiped monolog leaves the open list")

        print("deleting a monolog")
        # Deleting is in the monolog's own menu, behind a confirmation; the card
        # itself has no delete button at all.
        browser.evaluate("document.getElementById('menu').click()")
        wait_sheet(browser)
        check(sheet_click(browser, "Архив"), "the list menu opens the archive again")
        wait_for(
            lambda: browser.evaluate(
                "document.getElementById('title').textContent === 'Архив'"
                " && document.querySelectorAll('.monolog-card').length === 1"
            ),
            timeout=10,
            what="the archive with the swiped monolog",
        )
        browser.evaluate("document.querySelector('.monolog-card').click()")
        wait_for(
            lambda: browser.evaluate("document.getElementById('title').textContent === 'Проверка'"),
            timeout=10,
            what="the archived monolog for deleting",
        )
        check(
            not browser.evaluate(
                "Array.from(document.querySelectorAll('button'))"
                ".some((button) => button.textContent.trim() === 'Удалить монолог')"
            ),
            "there is no standalone delete button: the menu owns it",
        )
        browser.evaluate("document.getElementById('menu').click()")
        wait_sheet(browser)
        menu_labels = sheet_labels(browser)
        check(
            "Удалить монолог" in menu_labels and "Архив" not in menu_labels,
            f"the archived monolog's menu offers delete, not archive ({menu_labels})",
        )
        check(sheet_click(browser, "Удалить монолог"), "the menu opens the delete confirmation")
        wait_sheet(browser, "Удалить монолог?")
        check(
            any(item.get("id") == 1 for item in daemon_db(daemon).get("monologues", [])),
            "the monolog still exists until the confirmation",
        )
        check(sheet_click(browser, "Удалить"), "the confirmation deletes the monolog")
        wait_for(
            lambda: all(
                item.get("id") != 1 for item in daemon_db(daemon).get("monologues", [])
            ),
            timeout=20,
            what="the monolog to leave the computer",
        )
        check(True, "the monolog delete reached the computer")
        wait_for(
            lambda: browser.evaluate("document.querySelectorAll('.monolog-card').length === 0"),
            timeout=15,
            what="the deleted monolog to leave the archive",
        )
        # Gone for good: a reload does not bring it back from the cached page.
        browser.navigate(daemon.base + "/")
        wait_for(
            lambda: browser.evaluate(
                "(() => { const title = document.getElementById('title');"
                " return !!title && title.textContent === 'Монологи'"
                " && document.querySelectorAll('.monolog-card').length === 0; })()"
            ),
            timeout=20,
            what="the open list after the delete",
        )
        check(
            not daemon.api("/site/api/monologues").get("monologs")
            and not daemon.api("/site/api/monologues?archived=1").get("monologs"),
            "the deleted monolog is gone from both of the computer's lists",
        )
        check(True, "the deleted monolog stays gone after a reload")

        print("history is append-only")
        conn = sqlite3.connect(sqlite_path(daemon.root), timeout=5)
        conn.row_factory = sqlite3.Row
        entries = [
            {key: row[key] for key in row.keys()}
            for row in conn.execute("SELECT at, text, model, source FROM history ORDER BY id")
        ]
        conn.close()
        check(len(entries) > 0, "the history table has entries")
        check(
            any(entry.get("source") == "site" for entry in entries),
            "the transcription is in the history with the phone source",
        )

        print("the engine is busy before the phone starts")
        busy_session = daemon.take_engine()
        check(
            busy_session is not None,
            f"the computer took the engine before the take (session {busy_session})",
        )
        busy_created = daemon.api("/site/api/monologues", "POST", {"name": "Занято"})
        check(busy_created.get("ok"), "the second monolog is created")
        # The delete above removed the first monolog, so the computer may reuse
        # its number: take the id it really gave instead of guessing one.
        busy_id = int((busy_created.get("monolog") or {}).get("id") or 0)
        check(busy_id > 0, f"the second monolog has an id ({busy_id})")
        busy_messages = "/site/api/monologues/" + str(busy_id) + "/messages"
        browser.navigate(daemon.base + "/")
        wait_for(
            lambda: browser.evaluate(
                "Array.from(document.querySelectorAll('.monolog-card'))"
                ".some((card) => card.textContent.includes('Занято'))"
            ),
            timeout=20,
            what="the list with the second monolog",
        )
        browser.evaluate(
            "Array.from(document.querySelectorAll('.monolog-card'))"
            ".find((card) => card.textContent.includes('Занято')).click()"
        )
        wait_for(
            lambda: browser.evaluate("!document.getElementById('composer').hidden"),
            timeout=10,
            what="the composer of the second monolog",
        )
        # A reload lost the stubbed microphone: the page must record the same
        # speech as the first take, not whatever device this machine has.
        busy_mic = browser.evaluate(
            MIC_SCRIPT.replace("BASE64", base64.b64encode(speech_mp3()).decode()).replace(
                "SKIP", "0"
            ),
            await_promise=True,
        )
        print("  busy take mic:", busy_mic)
        browser.evaluate("document.getElementById('record').click()")

        def filled():
            items = daemon.api(busy_messages).get("messages", [])
            if not items:
                return None
            newest = max(items, key=lambda item: item["id"])
            return newest if newest["duration"] >= 14.0 else None

        held = wait_for(filled, timeout=40, what="the take to fill while the engine is busy")
        check(
            held["text"] == "",
            f"nothing is transcribed while the engine is busy"
            f" ({held['duration']:.1f} s of audio, {len(held['text'])} chars)",
        )
        check(
            browser.evaluate(
                "Array.from(document.querySelectorAll('.message .chip'))"
                ".some((chip) => chip.textContent === 'движок занят')"
            ),
            "the phone shows that the engine is busy before the first word",
        )
        check(
            browser.evaluate("document.getElementById('record').classList.contains('recording')"),
            "the phone keeps recording while the engine is busy",
        )
        freed = daemon.free_engine(busy_session)
        check(freed.get("ok"), f"the computer let the engine go ({freed})")

        def first_text():
            items = daemon.api(busy_messages).get("messages", [])
            if not items:
                return None
            newest = max(items, key=lambda item: item["id"])
            return newest if newest["text"].strip() else None

        # The audio already waited in the queue: once the engine is free the
        # text must appear quickly, not after the next phrase is cut.
        started = wait_for(
            first_text,
            timeout=15,
            what="the text to start once the engine is free",
        )
        check(
            len(started["text"]) > 3,
            f"the transcription starts as soon as the engine is free ({len(started['text'])} chars)",
        )
        check(
            browser.evaluate("document.getElementById('record').classList.contains('recording')"),
            "and it starts while the phone is still recording",
        )
        time.sleep(2)
        browser.evaluate("document.getElementById('record').click()")
        wait_for(
            lambda: any(
                item["stage"] == "done"
                for item in daemon.api(busy_messages).get("messages", [])
            ),
            timeout=40,
            what="the take that waited for the engine to finish",
        )
        check(True, "the take that waited for the engine finished")

        print("the scroll indicator stays under the header")
        # The indicator is only drawn while the view really scrolls, so the list
        # needs some rows first. They are made through the computer's own route.
        for index in range(14):
            daemon.api("/site/api/monologues", "POST", {"name": f"Свиток {index + 1}"})
        browser.navigate(daemon.base + "/")
        wait_for(
            lambda: browser.evaluate(
                "document.querySelectorAll('.monolog-card').length >= 14"
            ),
            timeout=25,
            what="a list long enough to scroll",
        )

        def scroll_geometry():
            """Header, viewport, thumb and the widths, in viewport pixels."""
            return json.loads(
                browser.evaluate(
                    "JSON.stringify((() => {"
                    " const header = document.querySelector('.vb-shell header');"
                    " const thumb = document.querySelector('.vb-scroll-thumb');"
                    " const view = document.getElementById('view');"
                    " const viewport = document.querySelector('.vb-viewport');"
                    " const head = header ? header.getBoundingClientRect() : null;"
                    " const bar = thumb ? thumb.getBoundingClientRect() : null;"
                    " const box = viewport ? viewport.getBoundingClientRect() : null;"
                    " return { header: head ? head.bottom : null,"
                    "   viewportTop: box ? box.top : null,"
                    "   viewportBottom: box ? box.bottom : null,"
                    "   thumb: bar ? bar.top : null,"
                    "   thumbBottom: bar ? bar.bottom : null,"
                    "   height: view.scrollHeight, room: view.clientHeight,"
                    "   top: view.scrollTop, width: view.scrollWidth,"
                    "   inner: view.clientWidth }; })())"
                )
            )

        def wake_scroll():
            # The thumb fades 900 ms after the last scroll; a scroll event makes
            # it measure and show again, the same way a finger would.
            browser.evaluate(
                "document.getElementById('view').dispatchEvent(new Event('scroll'))"
            )
            time.sleep(0.15)

        wake_scroll()
        wait_for(
            lambda: browser.evaluate("!!document.querySelector('.vb-scroll-thumb')"),
            timeout=10,
            what="the scroll indicator",
        )
        first = scroll_geometry()
        check(
            first["thumb"] is not None and first["header"] is not None,
            f"the scroll indicator is drawn ({first})",
        )
        check(
            first["thumb"] is not None
            and first["header"] is not None
            and first["thumb"] >= first["header"] - 0.5,
            f"the scroll indicator starts below the header ({first})",
        )
        check(
            first["thumb"] is not None
            and first["viewportTop"] is not None
            and first["thumb"] >= first["viewportTop"] + 3.5,
            f"the scroll indicator starts inside the viewport's top inset ({first})",
        )
        check(
            first["thumbBottom"] is not None
            and first["viewportBottom"] is not None
            and first["thumbBottom"] <= first["viewportBottom"] - 5.5,
            f"the scroll indicator keeps its bottom inset inside the viewport ({first})",
        )
        check(
            first["width"] == first["inner"],
            f"the list does not scroll sideways ({first['width']} of {first['inner']})",
        )
        browser.evaluate("document.getElementById('view').scrollTop = 420")
        time.sleep(0.4)
        middle = scroll_geometry()
        check(
            middle["thumb"] is not None
            and middle["header"] is not None
            and middle["thumb"] >= middle["header"] - 0.5,
            f"the scroll indicator stays below the header while scrolling ({middle})",
        )
        check(
            middle["thumbBottom"] is not None
            and middle["viewportBottom"] is not None
            and middle["thumbBottom"] <= middle["viewportBottom"] - 5.5,
            f"the scrolled indicator keeps its bottom inset ({middle})",
        )
        check(
            middle["width"] == middle["inner"],
            f"the scrolled list does not scroll sideways"
            f" ({middle['width']} of {middle['inner']})",
        )

        print("the phone switch applies without a restart")
        toggle = Daemon(port=free_port(), models=False, site=False)
        try:
            toggle.start()
            check(
                (toggle.site_state() or {}).get("running") is False,
                "the profile starts with the phone server off",
            )
            toggle.set_site(True)
            wait_for(
                lambda: (toggle.site_state() or {}).get("running") is True,
                timeout=30,
                what="the phone server to start after the switch",
            )
            check(True, "switching the phone server on starts it at once")
            check(
                toggle.page_status("/") == 200,
                "the phone page answers right after the switch",
            )
            toggle.set_site(False)
            wait_for(
                lambda: (toggle.site_state() or {}).get("running") is False,
                timeout=30,
                what="the phone server to stop after the switch",
            )
            check(True, "switching the phone server off stops it at once")
            check(
                toggle.page_status("/") == 0,
                "the port is closed after the switch, so nothing answers",
            )
        finally:
            toggle.stop()
            if not keep:
                shutil.rmtree(toggle.root, ignore_errors=True)

        print("retry after a missing model")
        # A profile of its own: it starts with an EMPTY models folder, so the
        # take cannot be decoded at first. The phone must still be able to send
        # all of it, the daemon must keep the raw PCM16, and after a restart
        # with the real models the «Повторить» button must finish the take.
        retry_daemon = Daemon(port=free_port(), models=False)
        try:
            retry_daemon.start()
            browser.navigate(retry_daemon.base + "/")
            wait_for(
                lambda: browser.evaluate(
                    "location.origin === "
                    + repr(retry_daemon.base)
                    + " && document.readyState === 'complete'"
                ),
                timeout=20,
                what="the retry phone page",
            )
            browser.evaluate(f"localStorage.setItem('vb_token', {retry_daemon.token!r})")
            browser.call("Page.reload", {"ignoreCache": True})
            wait_for(
                lambda: browser.evaluate(
                    "!!document.getElementById('app') && !document.getElementById('app').hidden"
                ),
                timeout=20,
                what="the retry page with a token",
            )
            check(True, "the phone page opened in the no-model profile")

            monolog = retry_daemon.api("/site/api/monologues", "POST", {"name": "Повтор"})
            check(monolog.get("ok"), "the retry monolog is created")
            browser.call("Page.reload", {"ignoreCache": True})
            wait_for(
                lambda: browser.evaluate("document.querySelectorAll('.monolog-card').length > 0"),
                timeout=20,
                what="the retry monolog list",
            )
            speech = browser.evaluate(
                MIC_SCRIPT.replace("BASE64", base64.b64encode(speech_mp3()).decode()).replace(
                    "SKIP", "0"
                ),
                await_promise=True,
            )
            print("retry mic:", speech)
            browser.evaluate("document.querySelectorAll('.monolog-card')[0].click()")
            wait_for(
                lambda: browser.evaluate("!document.getElementById('composer').hidden"),
                timeout=10,
                what="the retry composer",
            )

            browser.evaluate("document.getElementById('record').click()")
            time.sleep(0.5)
            time.sleep(7.5)
            browser.evaluate("document.getElementById('record').click()")
            time.sleep(0.5)

            def no_model():
                for message in retry_daemon.messages():
                    if message["stage"] == "no_model":
                        return message
                return None

            message = wait_for(no_model, timeout=45, what="the no-model take")
            check(
                message["stage"] == "no_model",
                f"without a model the phone sees no_model (stage {message['stage']})",
            )
            audio_path = os.path.join(
                retry_daemon.root, "site", "audio", f"{message['id']}.pcm"
            )
            check(os.path.exists(audio_path), "the daemon wrote the take's PCM file")
            size = os.path.getsize(audio_path) if os.path.exists(audio_path) else 0
            check(size > 32_000, f"the kept audio is the whole take ({size} bytes)")

            # The card offers retry while the model is missing; the page keeps
            # the last confirmed list on screen while the daemon restarts.
            wait_for(
                lambda: browser.evaluate(
                    "Array.from(document.querySelectorAll('.message button'))"
                    ".some((b) => b.textContent === 'Повторить')"
                ),
                timeout=20,
                what="the retry button before the restart",
            )

            retry_daemon.stop()
            retry_daemon.link_models()
            retry_daemon.start(pair=False)
            check(True, "the daemon restarted with the real models on the same port")

            wait_for(
                lambda: browser.evaluate(
                    "Array.from(document.querySelectorAll('.message button'))"
                    ".some((b) => b.textContent === 'Повторить')"
                ),
                timeout=60,
                what="the retry button after the restart",
            )
            clicked = browser.evaluate(
                "(() => { const b = Array.from(document.querySelectorAll('.message button'))"
                ".find((b) => b.textContent === 'Повторить');"
                "if (!b) return false; b.click(); return true; })()"
            )
            check(clicked is True, "the phone offers Повторить on the no-model card")

            def retried():
                for item in daemon_db(retry_daemon).get("messages", []):
                    if item.get("id") == message["id"] and item.get("stage") == "done":
                        return item
                return None

            done = wait_for(retried, timeout=90, what="the retried take to be done")
            check(
                len(done.get("text", "").strip()) > 0,
                f"the retried take got text ({len(done.get('text', ''))} chars)",
            )
            check(done.get("stage") == "done", "the computer's db.json says done")
        finally:
            retry_daemon.stop()
            if not keep:
                shutil.rmtree(retry_daemon.root, ignore_errors=True)

        print("offline: the phone keeps its data, creates a monolog and records")
        # A profile of its own so stopping it cannot disturb anything above.
        # The page is loaded and has a list first; then the computer goes away
        # for real (the process is stopped, not emulated), and the page must
        # stay readable and recordable.
        offline_daemon = Daemon(port=free_port())
        try:
            offline_daemon.start()
            browser.navigate(offline_daemon.base + "/")
            wait_for(
                lambda: browser.evaluate(
                    "location.origin === "
                    + repr(offline_daemon.base)
                    + " && document.readyState === 'complete'"
                ),
                timeout=20,
                what="the offline scenario page",
            )
            browser.evaluate(f"localStorage.setItem('vb_token', {offline_daemon.token!r})")
            # A normal reload, not an ignore-cache one: Chrome's cache-bypassing
            # reload also bypasses the service worker, and the offline part below
            # needs the worker to control this page.
            browser.call("Page.reload")
            wait_for(
                lambda: browser.evaluate(
                    "!!window.app && !document.getElementById('app').hidden"
                ),
                timeout=20,
                what="the offline scenario app",
            )
            check(
                offline_daemon.api(
                    "/site/api/monologues", "POST", {"name": "До связи"}
                ).get("ok"),
                "the offline scenario has a server monolog",
            )
            wait_for(
                lambda: browser.evaluate(
                    "Array.from(document.querySelectorAll('.monolog-card'))"
                    ".some((card) => card.textContent.includes('До связи'))"
                ),
                timeout=20,
                what="the monolog list before the computer goes away",
            )
            cached = browser.evaluate(
                "window.app.cache.monologPage(false, 0, 100).then((page) => page.monologs.length)",
                await_promise=True,
            )
            check(cached == 1, f"the phone cached the monolog list ({cached})")

            offline_mic = browser.evaluate(
                MIC_SCRIPT.replace("BASE64", base64.b64encode(speech_mp3()).decode()).replace(
                    "SKIP", "25"
                ),
                await_promise=True,
            )
            print("  offline mic:", offline_mic)
            wait_for(
                lambda: browser.evaluate("!!navigator.serviceWorker.controller"),
                timeout=20,
                what="the service worker to control the page",
            )
            offline_daemon.stop()
            time.sleep(2)
            check(offline_daemon.page_status("/") == 0, "the computer is really off")
            # A reload with no computer must still open the app: the shell and
            # every module come from the service worker, not from the network.
            # This must be a normal reload: `ignoreCache` would bypass the very
            # worker that has the page.
            browser.call("Page.reload")
            wait_for(
                lambda: browser.evaluate("document.getElementById('app') && !document.getElementById('app').hidden"),
                timeout=20,
                what="the app after an offline reload",
            )
            check(True, "the page reloads without a connection instead of a black screen")
            # The reload wiped the page's stubbed microphone; install it again
            # or the next offline take would record real silence.
            browser.evaluate(
                MIC_SCRIPT.replace(
                    "BASE64", base64.b64encode(speech_mp3()).decode()
                ).replace("SKIP", "25"),
                await_promise=True,
            )
            check(
                browser.evaluate(
                    "Array.from(document.querySelectorAll('.monolog-card'))"
                    ".some((card) => card.textContent.includes('До связи'))"
                ),
                "the page still shows the monolog list while the computer is off",
            )
            # Nothing is being recorded yet and the list is already on screen:
            # silence about the lost computer is correct here. The warning must
            # appear only when there is work that cannot be sent.
            time.sleep(3)
            check(
                not browser.evaluate(
                    "Array.from(document.querySelectorAll('#notes .banner'))"
                    ".some((note) => note.textContent.includes('не отвечает')"
                    " || note.textContent.includes('Нет связи'))"
                ),
                "no false warning while the phone is idle and offline",
            )

            print("creating a monolog with the computer away")
            # The new-monolog row moved into the list menu; it is no longer a
            # button in the list itself.
            browser.evaluate("document.getElementById('menu').click()")
            wait_sheet(browser)
            check(
                sheet_click(browser, "＋ Новый монолог"),
                "the list menu offers a new monolog while the computer is away",
            )
            wait_for(
                lambda: browser.evaluate("!!document.querySelector('.sheet textarea, .sheet input')"),
                timeout=10,
                what="the new monolog sheet",
            )
            browser.evaluate(
                "(() => { const box = document.querySelector('.sheet textarea, .sheet input');"
                "box.value = 'Офлайн';"
                "box.dispatchEvent(new Event('input', { bubbles: true })); })()"
            )
            check(sheet_click(browser, "Создать"), "the create sheet makes the monolog")
            shown = wait_for(
                lambda: browser.evaluate(
                    "(() => { const card = Array.from(document.querySelectorAll('.monolog-card'))"
                    ".find((c) => c.textContent.includes('Офлайн'));"
                    "return card ? card.textContent : ''; })()"
                ),
                timeout=20,
                what="the local monolog card",
            )
            check("Офлайн" in shown, f"the local monolog is in the list ({shown!r})")
            # A monolog made with no computer is a local row: it is in the
            # phone's own storage and shows up at once, with no server id yet.
            local_rows = browser.evaluate(
                "window.app.cache.localMonologs()"
                ".then((rows) => rows.filter((row) => row.name === 'Офлайн').length)",
                await_promise=True,
            )
            check(
                local_rows == 1,
                f"the monolog is stored on the phone while the computer is away ({local_rows})",
            )
            browser.evaluate(
                "Array.from(document.querySelectorAll('.monolog-card'))"
                ".find((c) => c.textContent.includes('Офлайн')).click()"
            )
            wait_for(
                lambda: browser.evaluate("!document.getElementById('composer').hidden"),
                timeout=10,
                what="the offline composer",
            )
            check(
                browser.evaluate("document.getElementById('record').disabled === false"),
                "the record button works for a local monolog",
            )

            print("recording with the computer away")
            browser.evaluate("document.getElementById('record').click()")
            wait_for(
                lambda: browser.evaluate(
                    "document.getElementById('record').classList.contains('offline')"
                ),
                timeout=10,
                what="the microphone to show the recording without a connection",
            )
            wait_for(
                lambda: browser.evaluate(
                    "Array.from(document.querySelectorAll('#notes .banner'))"
                    ".some((note) => note.textContent.includes('сохранится'))"
                ),
                timeout=30,
                what="the note that the recording will be kept",
            )
            check(True, "the phone says the recording will be kept")
            check(
                browser.evaluate(
                    "document.getElementById('record').classList.contains('recording')"
                ),
                "the microphone keeps a live take with the computer away",
            )
            check(
                browser.evaluate(
                    "getComputedStyle(document.getElementById('record')).backgroundColor"
                )
                == "rgb(199, 243, 107)",
                "the record button stays lime with the computer away",
            )
            check(
                browser.evaluate(
                    "!!document.querySelector('.vb-record-rings.live')"
                ),
                "the halo is live with the computer away",
            )
            check(
                browser.evaluate(
                    "document.getElementById('rec-title').textContent === 'Запись без связи'"
                ),
                "the wording says the take is offline",
            )
            time.sleep(4)
            browser.evaluate("document.getElementById('record').click()")
            wait_for(
                lambda: browser.evaluate("document.querySelectorAll('.message.unsent').length > 0"),
                timeout=10,
                what="the unsent take card",
            )
            check(True, "the offline take is shown as not sent")
            wait_for(
                lambda: browser.evaluate(
                    "(document.querySelector('.message.unsent .chip') || {}).textContent"
                    " === 'ждёт связи'"
                ),
                timeout=5,
                what="the waiting chip on the offline take",
            )
            check(True, "the queued take says it is waiting for a connection")
            waiting_takes = summarise_outbox(outbox_state(browser))
            check(
                len(waiting_takes) == 1 and (waiting_takes[0].get("bytes") or 0) > 0,
                f"the offline take and its audio wait in the outbox ({waiting_takes})",
            )

            print("the computer comes back")
            browser.evaluate("window.__pageKept = 'yes'")
            offline_daemon.start(pair=False)
            check(True, "the computer is back on the same port")
            prompt_started = time.time()
            wait_for(
                lambda: browser.evaluate(
                    "document.querySelectorAll('.message.unsent').length === 0"
                ),
                timeout=15,
                what="the queued take to stop waiting",
            )
            prompt_waited = time.time() - prompt_started
            check(
                prompt_waited < 12,
                "the queued take stops waiting promptly after the computer returns"
                f" ({prompt_waited:.1f} s)",
            )
            check(
                browser.evaluate("window.__pageKept === 'yes'"),
                "the queued take stops waiting without reloading the page",
            )

            def synced_offline():
                state = daemon_db(offline_daemon)
                monolog = next(
                    (
                        item
                        for item in state.get("monologues", [])
                        if item.get("name") == "Офлайн"
                    ),
                    None,
                )
                if not monolog:
                    return None
                messages = [
                    item
                    for item in state.get("messages", [])
                    if item.get("monolog") == monolog["id"]
                ]
                if not messages:
                    return None
                newest = max(messages, key=lambda item: item["id"])
                if newest.get("stage") not in ("done", "silent"):
                    return None
                return monolog, newest

            created, arrived = wait_for(
                synced_offline,
                timeout=120,
                what="the local monolog and its take on the computer",
            )
            check(created["id"] > 0, f"the local monolog got a server id ({created['id']})")
            check(
                arrived["stage"] == "done",
                f"the offline take was transcribed (stage {arrived['stage']})",
            )
            check(
                len(arrived.get("text", "").strip()) > 3,
                f"the computer has its text ({len(arrived.get('text', ''))} chars)",
            )
            wait_for(
                lambda: browser.evaluate(
                    "Array.from(document.querySelectorAll('.message .text'))"
                    ".some((node) => !node.classList.contains('empty')"
                    " && node.textContent.trim().length > 3)"
                ),
                timeout=30,
                what="the text of the offline take on the phone",
            )
            check(True, "the phone shows the text of the take that waited")
            wait_for(
                lambda: browser.evaluate(
                    "document.querySelectorAll('.message.unsent').length === 0"
                ),
                timeout=30,
                what="the unsent card to go away",
            )
            check(True, "the unsent card is gone after the sync")

            print("opening a monolog with the computer away")
            # Back to the list, stop the computer, and open the synced monolog:
            # its rows come from the cache, so its messages are on the first
            # paint instead of after a round trip to a computer that is gone.
            browser.evaluate("document.getElementById('back').click()")
            wait_for(
                lambda: browser.evaluate(
                    "document.getElementById('title').textContent === 'Монологи'"
                    " && document.querySelectorAll('.monolog-card').length > 0"
                ),
                timeout=15,
                what="the list before the cached open",
            )
            offline_daemon.stop()
            time.sleep(1)
            check(
                offline_daemon.page_status("/") == 0,
                "the computer is really off for the cached monolog",
            )
            monolog_card = json.dumps(f'.monolog-card[data-id="{created["id"]}"]')
            opened_at = time.time()
            browser.evaluate(f"document.querySelector({monolog_card}).click()")
            wait_for(
                lambda: browser.evaluate(
                    "document.getElementById('title').textContent === 'Офлайн'"
                    " && !!document.querySelector('.message')"
                ),
                timeout=5,
                what="the cached monolog",
            )
            opened_in = time.time() - opened_at
            check(
                browser.evaluate(
                    "Array.from(document.querySelectorAll('.message .text'))"
                    ".some((node) => !node.classList.contains('empty')"
                    " && node.textContent.trim().length > 3)"
                ),
                "the cached messages are on screen with the computer away",
            )
            check(
                opened_in < 2.5,
                f"opening a monolog does not wait for the computer ({opened_in:.2f} s)",
            )

            print("editing and deleting with the computer away")
            # The page promises local first: an edit and a delete made while the
            # computer cannot be reached stay on the phone and leave for it once
            # the connection returns. The take that just synced is the row.
            #
            # The computer was stopped for the cached open above; it stays down
            # for the edit. A page-level offline emulation is not enough here:
            # the service worker has a network of its own and would still get
            # fresh answers from a computer the page is supposed to have lost.
            arrived_id = int(arrived["id"])
            card = json.dumps(f'.message[data-id="{arrived_id}"]')
            edited_offline = "Правка без связи"
            browser.evaluate(f"document.querySelector({card}).click()")
            wait_sheet(browser)
            check(
                sheet_click(browser, "Изменить текст"),
                "the message menu offers an edit while the computer is away",
            )
            wait_for(
                lambda: browser.evaluate("!!document.querySelector('.vb-edit-field')"),
                timeout=10,
                what="the editor without a connection",
            )
            browser.evaluate(
                "(() => { const box = document.querySelector('.vb-edit-field');"
                "box.value = " + json.dumps(edited_offline, ensure_ascii=False) + ";"
                "box.dispatchEvent(new Event('input', { bubbles: true })); })()"
            )
            browser.evaluate("document.getElementById('edit-save').click()")
            wait_for(
                lambda: browser.evaluate(
                    "(document.querySelector(" + card + " + ' .text') || {}).textContent"
                    " === " + json.dumps(edited_offline, ensure_ascii=False)
                ),
                timeout=10,
                what="the offline edit on the phone",
            )
            check(
                browser.evaluate(
                    "Array.from(document.querySelectorAll('.message .chip'))"
                    ".some((chip) => chip.textContent.trim() === 'изменено · ждёт связи')"
                ),
                "the offline edit is marked as waiting for a connection",
            )
            before_edit = next(
                (
                    message
                    for message in daemon_db(offline_daemon).get("messages", [])
                    if message["id"] == arrived_id
                ),
                None,
            )
            check(
                before_edit is not None and before_edit["text"] != edited_offline,
                "the computer has not seen the offline edit yet",
            )
            offline_daemon.start(pair=False)
            wait_for(
                lambda: any(
                    message["id"] == arrived_id and message["text"] == edited_offline
                    for message in offline_daemon.messages(created["id"])
                ),
                timeout=30,
                what="the offline edit to reach the computer",
            )
            check(True, "the offline edit reached the computer when the connection returned")
            wait_for(
                lambda: browser.evaluate(
                    "Array.from(document.querySelectorAll('.message .chip'))"
                    ".some((chip) => chip.textContent.trim() === 'изменено')"
                ),
                timeout=20,
                what="the phone to show the edit as confirmed",
            )

            # The same row goes away while the computer is away: the phone hides
            # it at once, and the delete waits in the change queue.
            offline_daemon.stop()
            time.sleep(1)
            check(
                offline_daemon.page_status("/") == 0,
                "the computer is really off for the offline delete",
            )
            browser.evaluate(f"document.querySelector({card}).click()")
            wait_sheet(browser)
            check(
                sheet_click(browser, "Удалить сообщение"),
                "the message menu offers a delete while the computer is away",
            )
            wait_sheet(browser, "Удалить сообщение?")
            before_delete = next(
                (
                    message
                    for message in daemon_db(offline_daemon).get("messages", [])
                    if message["id"] == arrived_id
                ),
                None,
            )
            check(
                before_delete is not None and not before_delete["deleted"],
                "the message is still on the computer until the confirmation",
            )
            check(sheet_click(browser, "Удалить"), "the confirmation deletes the offline message")
            wait_for(
                lambda: browser.evaluate(
                    "document.querySelector(" + card + ") === null"
                ),
                timeout=10,
                what="the deleted message to leave the feed",
            )
            after_delete = next(
                (
                    message
                    for message in daemon_db(offline_daemon).get("messages", [])
                    if message["id"] == arrived_id
                ),
                None,
            )
            check(
                after_delete is not None and not after_delete["deleted"],
                "the offline delete waits on the phone",
            )
            offline_daemon.start(pair=False)
            wait_for(
                lambda: not any(
                    message["id"] == arrived_id for message in offline_daemon.messages(created["id"])
                ),
                timeout=30,
                what="the offline delete to reach the computer",
            )
            check(True, "the offline delete reached the computer when the connection returned")

            browser.evaluate("document.getElementById('back').click()")
            wait_for(
                lambda: browser.evaluate(
                    "Array.from(document.querySelectorAll('.monolog-card'))"
                    ".some((card) => card.textContent.includes('Офлайн')"
                    " && !card.textContent.includes('ждёт связи'))"
                ),
                timeout=30,
                what="the monolog to stop waiting",
            )
            check(True, "the monolog is no longer marked as waiting")
        finally:
            offline_daemon.stop()
            if not keep:
                shutil.rmtree(offline_daemon.root, ignore_errors=True)
    except Exception:
        traceback.print_exc()
        check(False, "the run stopped early: " + traceback.format_exc(limit=2).replace("\n", " | "))
    finally:
        # The file is the point of the run: a failing phase must still leave
        # every request, every page log line and the daemon's own log behind.
        try:
            dump_network(browser, diag, "all requests", bodies=False)
        except Exception as error:
            diag.line(f"(network dump failed: {error})")
        diag.section("page events")
        for event in browser.events:
            method = event.get("method", "")
            if method in ("Runtime.exceptionThrown", "Log.entryAdded", "Runtime.consoleAPICalled"):
                diag.line(method + " " + json.dumps(event.get("params", {}), ensure_ascii=False)[:1500])
        diag.section("daemon log")
        for entry in log_tail(daemon, 300):
            diag.line("  " + entry)
        diag.section("daemon db.json at the end")
        diag.line(json.dumps(daemon_db(daemon), ensure_ascii=False, indent=1))
        browser.stop()
        daemon.stop()
        if not keep:
            shutil.rmtree(daemon.root, ignore_errors=True)
        diag.close()
        print(f"diagnostics: {DIAG_PATH}")

    print()
    print(f"{checks - len(failures)}/{checks} checks passed")
    if failures:
        print("failed:")
        for failure in failures:
            print("  -", failure)
        sys.exit(1)


if __name__ == "__main__":
    main()
