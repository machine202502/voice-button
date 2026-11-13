"""The Telegram bot against a fake Bot API: no real token, no real Telegram.

The daemon is the real one, the recognizer is the real one; only the Bot API is
a local HTTP server. This checks the path a real voice message takes: /start,
media, 👀, transcription, reply in the thread, ✅, history and the desktop chat.

    python tests/telegram-test.py
"""

import ctypes
import importlib.util
import json
import os
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.stdout.reconfigure(encoding="utf-8")
ROOT = r"C:\shared-data\reps\voice-button"
spec = importlib.util.spec_from_file_location("e2e", os.path.join(ROOT, "tests", "phone-e2e.py"))
e2e = importlib.util.module_from_spec(spec)
spec.loader.exec_module(e2e)

checks = 0
failures = []


def check(condition, message):
    global checks
    checks += 1
    if condition:
        print("  ok    " + message)
    else:
        failures.append(message)
        print("  FAIL  " + message)


class Blob(ctypes.Structure):
    _fields_ = [("cbData", ctypes.c_uint32), ("pbData", ctypes.POINTER(ctypes.c_char))]


def write_secure(path, token, proxy):
    payload = json.dumps({"token": token, "proxy": proxy}).encode("utf-8")
    buffer = ctypes.create_string_buffer(payload)
    data_in = Blob(len(payload), buffer)
    data_out = Blob()
    crypt32 = ctypes.windll.crypt32
    kernel32 = ctypes.windll.kernel32
    if not crypt32.CryptProtectData(
        ctypes.byref(data_in), None, None, None, None, 1, ctypes.byref(data_out)
    ):
        raise OSError("CryptProtectData failed")
    try:
        encrypted = ctypes.string_at(data_out.pbData, data_out.cbData)
        with open(path, "wb") as handle:
            handle.write(encrypted)
    finally:
        kernel32.LocalFree(data_out.pbData)


class FakeBot(BaseHTTPRequestHandler):
    server_version = "FakeBot/1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length)
        path = self.path
        if path.endswith("/getUpdates"):
            updates = self.server.updates
            self.server.updates = []
            self._json({"ok": True, "result": updates})
            return
        if path.endswith("/getFile"):
            self._json(
                {
                    "ok": True,
                    "result": {
                        "file_path": "files/voice.opus",
                        "file_size": len(self.server.media),
                    },
                }
            )
            return
        if path.endswith("/sendMessage"):
            parsed = json.loads(body.decode("utf-8"))
            self.server.sent.append(parsed)
            self._json({"ok": True, "result": {"message_id": 1}})
            return
        if path.endswith("/sendDocument"):
            self.server.documents.append(len(body))
            self._json({"ok": True, "result": {"message_id": 1}})
            return
        if path.endswith("/setMessageReaction"):
            parsed = json.loads(body.decode("utf-8"))
            self.server.reactions.append(parsed)
            self._json({"ok": True, "result": True})
            return
        self._json({"ok": False, "description": "unknown method " + path}, status=404)

    def do_GET(self):
        if "/file/bot" in self.path:
            self.send_response(200)
            self.send_header("Content-Type", "audio/ogg")
            self.send_header("Content-Length", str(len(self.server.media)))
            self.end_headers()
            self.wfile.write(self.server.media)
            return
        self.send_error(404)

    def _json(self, value, status=200):
        data = json.dumps(value).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)


def read_json(path, attempts=40):
    """The daemon replaces the file atomically; a reader can catch the gap."""
    last = None
    for _ in range(attempts):
        try:
            with open(path, encoding="utf-8") as handle:
                return json.load(handle)
        except (PermissionError, FileNotFoundError, json.JSONDecodeError) as error:
            last = error
            time.sleep(0.1)
    raise last


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), FakeBot)
    server.sent = []
    server.reactions = []
    server.documents = []
    server.media = open(os.path.join(ROOT, "tests", "assets", "voice.opus"), "rb").read()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    port = server.server_address[1]
    print("fake bot api on", port)

    daemon = e2e.Daemon(models=True)
    try:
        daemon.start()
        root = daemon.root
        write_secure(os.path.join(root, "telegram.secure"), "123:fake", "")
        config_path = os.path.join(root, "config.json")
        with open(config_path, encoding="utf-8") as handle:
            config = json.load(handle)
        config["telegram_enabled"] = True
        config["telegram_proxy"] = ""
        config["telegram_api_base"] = "http://127.0.0.1:%d" % port
        with open(config_path, "w", encoding="utf-8") as handle:
            json.dump(config, handle)

        owner = 777
        server.updates = [
            {
                "update_id": 1,
                "message": {
                    "message_id": 11,
                    "chat": {"id": owner, "type": "private"},
                    "from": {"id": owner, "is_bot": False},
                    "text": "/start",
                },
            },
            {
                "update_id": 2,
                "message": {
                    "message_id": 12,
                    "chat": {"id": owner, "type": "private"},
                    "from": {"id": owner, "is_bot": False},
                    "voice": {
                        "file_id": "voice-1",
                        "file_size": len(server.media),
                        "duration": 8,
                        "mime_type": "audio/ogg",
                    },
                },
            },
        ]

        def answered():
            return any(text.get("reply_to_message_id") == 12 for text in server.sent)

        e2e.wait_for(answered, timeout=120, what="the transcript reply")
        reply = [text for text in server.sent if text.get("reply_to_message_id") == 12][-1]
        check(bool(reply.get("text", "").strip()), "the bot replies with the transcript")
        check(
            "Не удалось" not in reply.get("text", ""),
            "the reply is a transcript, not an error",
        )
        check(
            any(
                reaction.get("message_id") == 12
                and any(item.get("emoji") == "👀" for item in reaction.get("reaction", []))
                for reaction in server.reactions
            ),
            "the bot acknowledges with 👀",
        )
        check(
            any(
                reaction.get("message_id") == 12
                and any(item.get("emoji") == "✅" for item in reaction.get("reaction", []))
                for reaction in server.reactions
            ),
            "the bot finishes with ✅",
        )
        state_path = os.path.join(root, "telegram.state.json")
        state = read_json(state_path)
        check(state.get("owner") == owner, "the owner was stored")
        check(
            any(entry.get("message_id") == 12 and entry.get("text") for entry in state.get("chat", [])),
            "the desktop chat got the entry",
        )
        history = os.path.join(root, "history.jsonl")
        with open(history, encoding="utf-8") as handle:
            entries = [json.loads(line) for line in handle if line.strip()]
        check(
            any(entry.get("source") == "telegram" for entry in entries),
            "the result is in History with the telegram source",
        )
    finally:
        daemon.stop()
        server.shutdown()

    print("")
    if failures:
        print("failed:")
        for failure in failures:
            print("  - " + failure)
        sys.exit(1)
    print(f"{checks}/{checks} checks passed")


if __name__ == "__main__":
    main()
