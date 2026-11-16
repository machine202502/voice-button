"""The Telegram bot against a fake Bot API: no real token, no real Telegram.

The daemon is the real one, the recognizer is the real one; only the Bot API is
a local HTTP server. This checks the path a real voice message takes from a new
account: /start becomes a request, the window allows it, media is transcribed,
👀 and ✅ mark the message, the reply lands in the thread, the history and the
desktop chat; then access is taken back and the account is ignored.

    python tests/telegram-test.py
"""

import ctypes
import importlib.util
import json
import os
import sqlite3
import re
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
    payload = (token + "\n" + proxy).encode("utf-8")
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
        if path.endswith("/setMyCommands"):
            parsed = json.loads(body.decode("utf-8"))
            self.server.commands.append(parsed)
            self._json({"ok": True, "result": True})
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


def start_update(update_id, user, first_name="Guest", username="guest"):
    return {
        "update_id": update_id,
        "message": {
            "message_id": update_id,
            "chat": {"id": user, "type": "private"},
            "from": {"id": user, "is_bot": False, "first_name": first_name, "username": username},
            "date": int(time.time()),
            "text": "/start",
        },
    }


def voice_update(update_id, owner, media):
    return {
        "update_id": update_id,
        "message": {
            "message_id": update_id,
            "chat": {"id": owner, "type": "private"},
            "from": {"id": owner, "is_bot": False, "first_name": "Owner", "username": "owner"},
            "date": int(time.time()),
            "voice": {
                "file_id": "voice-%d" % update_id,
                "file_size": len(media),
                "duration": 8,
                "mime_type": "audio/ogg",
            },
        },
    }


def read_state(root, attempts=40):
    """Read the bot state from SQLite (the daemon owns the connection)."""
    last = None
    for _ in range(attempts):
        try:
            conn = sqlite3.connect(os.path.join(root, "voice-button.sqlite3"), timeout=5)
            conn.row_factory = sqlite3.Row
            kv = {row["key"]: row["value"] for row in conn.execute("SELECT key, value FROM kv")}
            current = None
            row = conn.execute("SELECT * FROM telegram_current WHERE id = 1").fetchone()
            if row:
                current = {key: row[key] for key in row.keys() if key != "id"}
            columns = (
                "update_id, chat_id, message_id, file_id, file_name, kind, size, seconds, "
                "done_seconds, stage, progress, text, error, sender, original_at, "
                "received_at, started_at"
            )
            queue = [
                {key: row[key] for key in row.keys()}
                for row in conn.execute(
                    f"SELECT {columns} FROM telegram_queue ORDER BY position"
                )
            ]
            chat = [
                {key: row[key] for key in row.keys()}
                for row in conn.execute(
                    "SELECT chat_id, message_id, at, sender, kind, file_name, seconds, "
                    "text, error FROM telegram_chat ORDER BY id"
                )
            ]
            accounts = [
                {key: row[key] for key in row.keys()}
                for row in conn.execute(
                    "SELECT user_id, status, name, username, added_at, last_seen_at "
                    "FROM telegram_account ORDER BY user_id"
                )
            ]
            conn.close()
            return {
                "offset": int(kv["telegram.offset"]) if kv.get("telegram.offset") else 0,
                "accounts": accounts,
                "current": current,
                "queue": queue,
                "chat": chat,
            }
        except (sqlite3.Error, OSError, ValueError, KeyError) as error:
            last = error
            time.sleep(0.1)
    raise last


def account_of(state, user):
    return next(
        (item for item in state.get("accounts", []) if item["user_id"] == user), None
    )


def write_access(root, action, user=0):
    """A decision from the window, exactly as ipc::request_telegram_access writes it."""
    conn = sqlite3.connect(os.path.join(root, "voice-button.sqlite3"), timeout=5)
    conn.execute(
        "INSERT INTO kv(key, value) VALUES('request.telegram_access', ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        ("action\t%s\nuser\t%d" % (action, user),),
    )
    conn.commit()
    conn.close()


def read_history(root, attempts=40):
    last = None
    for _ in range(attempts):
        try:
            conn = sqlite3.connect(os.path.join(root, "voice-button.sqlite3"), timeout=5)
            conn.row_factory = sqlite3.Row
            rows = []
            for row in conn.execute(
                "SELECT at, text, model, source, state, audio, group_name, message_id "
                "FROM history ORDER BY id"
            ):
                entry = {key: row[key] for key in row.keys()}
                entry["group"] = entry.pop("group_name")
                entry["message"] = entry.pop("message_id")
                entry["parts"] = []
                rows.append(entry)
            conn.close()
            return rows
        except (sqlite3.Error, OSError, ValueError) as error:
            last = error
            time.sleep(0.1)
    raise last

def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), FakeBot)
    server.sent = []
    server.reactions = []
    server.documents = []
    server.commands = []
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
                    "from": {"id": owner, "is_bot": False, "first_name": "Owner", "username": "owner"},
            "date": int(time.time()),
                    "text": "/start",
                },
            },
        ]
        e2e.wait_for(
            lambda: account_of(read_state(root), owner) is not None,
            timeout=30,
            what="the /start request to be recorded",
        )
        check(
            account_of(read_state(root), owner)["status"] == "pending",
            "a new account waits for the window",
        )
        check(not server.sent, "a new account gets no answer")
        check(not read_state(root)["queue"], "and cannot queue anything")

        write_access(root, "allow", owner)
        e2e.wait_for(
            lambda: (account_of(read_state(root), owner) or {}).get("status") == "allowed",
            timeout=30,
            what="the window's decision to reach the bot",
        )
        check(True, "the window opens the bot to the account")
        e2e.wait_for(
            lambda: any(
                item.get("chat_id") == owner and item.get("reply_to_message_id") is None
                for item in server.sent
            ),
            timeout=30,
            what="the access notice",
        )
        check(True, "the account is told that access is open")

        server.updates = [
            {
                "update_id": 2,
                "message": {
                    "message_id": 12,
                    "chat": {"id": owner, "type": "private"},
                    "from": {"id": owner, "is_bot": False, "first_name": "Owner", "username": "owner"},
            "date": int(time.time()),
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
        print("  reply text:", reply.get("text", ""))
        check(
            bool(re.match(r"^\[\d{2}\.\d{2}\.\d{2} \d{2}:\d{2} UTC[+-]\d{2}:\d{2}\] Owner \(@owner\): ", reply.get("text", ""))),
            "the reply names the sender and the original date",
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
                and any(item.get("emoji") == "👍" for item in reaction.get("reaction", []))
                for reaction in server.reactions
            ),
            "the bot finishes with 👍",
        )
        state = read_state(root)
        check(
            account_of(state, owner)["status"] == "allowed",
            "the account stays allowed",
        )
        check(
            any(entry.get("message_id") == 12 and entry.get("text") for entry in state.get("chat", [])),
            "the desktop chat got the entry",
        )
        entries = read_history(root)
        check(
            any(entry.get("source") == "telegram" for entry in entries),
            "the result is in History with the telegram source",
        )

        e2e.wait_for(
            lambda: not daemon.local("/api/status").get("background"),
            timeout=20,
            what="the violet background to clear after the reply",
        )
        check(True, "the tray background clears when Telegram is done")

        # /status answers only the owner and names the queue.
        server.updates = [
            {
                "update_id": 30,
                "message": {
                    "message_id": 30,
                    "chat": {"id": owner, "type": "private"},
                    "from": {"id": owner, "is_bot": False, "first_name": "Owner", "username": "owner"},
            "date": int(time.time()),
                    "text": "/status",
                },
            }
        ]
        e2e.wait_for(
            lambda: any(item.get("reply_to_message_id") == 30 for item in server.sent),
            timeout=30,
            what="the /status reply",
        )
        status_reply = [item for item in server.sent if item.get("reply_to_message_id") == 30][-1]
        status_text = status_reply.get("text", "")
        print("  status reply:", status_text.replace("\n", " | "))
        check(
            any(
                word in status_text
                for word in ("Queue", "Очередь", "队列", "File", "Файл", "文件")
            ),
            "the /status answer describes the queue",
        )

        # A stranger is recorded for the window, never answered and never served.
        stranger = 888
        server.updates = [
            {
                "update_id": 25,
                "message": {
                    "message_id": 25,
                    "chat": {"id": stranger, "type": "private"},
                    "from": {"id": stranger, "is_bot": False, "first_name": "Guest", "username": "guest"},
                    "date": int(time.time()),
                    "text": "/start",
                },
            },
        ]
        e2e.wait_for(
            lambda: account_of(read_state(root), stranger) is not None,
            timeout=30,
            what="the second request to be recorded",
        )
        check(
            account_of(read_state(root), stranger)["status"] == "pending",
            "a stranger only asks",
        )
        check(
            not any(item.get("chat_id") == stranger for item in server.sent),
            "and is not answered",
        )
        server.updates = [voice_update(26, stranger, server.media)]
        e2e.wait_for(
            lambda: read_state(root)["offset"] > 26,
            timeout=30,
            what="the stranger's file to be ignored",
        )
        check(
            read_state(root)["current"] is None and not read_state(root)["queue"],
            "a pending account cannot queue files",
        )
        check(
            not any(item.get("reply_to_message_id") == 26 for item in server.sent),
            "and gets no transcript",
        )
        write_access(root, "decline", stranger)
        e2e.wait_for(
            lambda: account_of(read_state(root), stranger) is None,
            timeout=30,
            what="the declined request to disappear",
        )
        check(True, "a declined request is forgotten")

        # Several strangers at once: one button refuses them all, and a refused
        # account may ask again later.
        server.updates = [
            start_update(27, 889, "Guest1", "guest1"),
            start_update(28, 890, "Guest2", "guest2"),
        ]
        e2e.wait_for(
            lambda: account_of(read_state(root), 889) is not None
            and account_of(read_state(root), 890) is not None,
            timeout=30,
            what="two more requests",
        )
        write_access(root, "decline_all")
        e2e.wait_for(
            lambda: account_of(read_state(root), 889) is None
            and account_of(read_state(root), 890) is None,
            timeout=30,
            what="every request to be refused",
        )
        check(
            account_of(read_state(root), owner)["status"] == "allowed",
            "refusing every request keeps the account with access",
        )
        server.updates = [start_update(29, 889, "Guest1", "guest1")]
        e2e.wait_for(
            lambda: account_of(read_state(root), 889) is not None,
            timeout=30,
            what="the repeated request",
        )
        check(
            account_of(read_state(root), 889)["status"] == "pending",
            "a refused account may ask again",
        )
        write_access(root, "decline", 889)
        e2e.wait_for(
            lambda: account_of(read_state(root), 889) is None,
            timeout=30,
            what="the repeated request to be refused",
        )

        e2e.wait_for(
            lambda: len(server.commands) >= 4,
            timeout=20,
            what="the command menu",
        )
        default_menu = server.commands[0]
        check(
            [item["command"] for item in default_menu["commands"]] == ["start", "status"],
            "the bot menu has exactly start and status",
        )
        described = {
            item.get("language_code")
            for item in server.commands
            if item.get("language_code")
        }
        check(
            {"ru", "en", "zh"}.issubset(described),
            "the command menu has descriptions in ru, en and zh",
        )
        check(
            all(
                len(item["commands"]) == 2
                and all(command.get("description") for command in item["commands"])
                for item in server.commands
            ),
            "every command has a description",
        )

        # A recording at the computer pauses Telegram at the segment boundary.
        session = daemon.take_engine()
        check(session is not None, "the computer took the engine")
        server.updates = [voice_update(40, owner, server.media)]
        e2e.wait_for(
            lambda: (read_state(root).get("current") or {}).get("stage")
            in ("engine busy", "движок занят"),
            timeout=30,
            what="Telegram to report the paused engine",
        )
        check(
            daemon.local("/api/status").get("background") is True,
            "the violet background is on while Telegram waits",
        )
        check(
            not any(item.get("reply_to_message_id") == 40 for item in server.sent),
            "nothing is sent while the engine is taken",
        )
        daemon.free_engine(session)
        e2e.wait_for(
            lambda: any(item.get("reply_to_message_id") == 40 for item in server.sent),
            timeout=120,
            what="Telegram to resume after the engine is free",
        )
        check(True, "Telegram resumes the paused file")
        e2e.wait_for(
            lambda: not daemon.local("/api/status").get("background"),
            timeout=20,
            what="the background to clear after the paused file",
        )
        check(True, "the violet background clears after the resumed file")

        # An album is several messages: each file is its own job and reply.
        server.updates = [
            voice_update(50, owner, server.media),
            voice_update(51, owner, server.media),
        ]
        e2e.wait_for(
            lambda: all(
                any(item.get("reply_to_message_id") == message for item in server.sent)
                for message in (50, 51)
            ),
            timeout=180,
            what="both album files to be answered",
        )
        check(True, "an album becomes one job and one reply per file")
        media_dir = os.path.join(root, "telegram")
        leftovers = os.listdir(media_dir) if os.path.isdir(media_dir) else []
        check(not leftovers, f"downloaded audio is deleted after processing (left {leftovers})")
        state_files = [
            name
            for name in os.listdir(root)
            if name.endswith(".state.json") or name in ("session.json", "live.json")
        ]
        check(not state_files, f"no runtime JSON files in the data directory (got {state_files})")

        # Access can be taken back: the bot ignores the account from that moment.
        before_removal = len(server.sent)
        write_access(root, "remove", owner)
        e2e.wait_for(
            lambda: account_of(read_state(root), owner) is None,
            timeout=30,
            what="the account to be removed",
        )
        check(True, "the window closes the bot to the account")
        e2e.wait_for(
            lambda: len(server.sent) > before_removal,
            timeout=30,
            what="the removal notice",
        )
        notice = server.sent[before_removal]
        check(
            notice.get("chat_id") == owner and notice.get("reply_to_message_id") is None,
            "the account is told once",
        )
        server.updates = [
            {
                "update_id": 60,
                "message": {
                    "message_id": 60,
                    "chat": {"id": owner, "type": "private"},
                    "from": {"id": owner, "is_bot": False, "first_name": "Owner", "username": "owner"},
                    "date": int(time.time()),
                    "text": "/status",
                },
            },
        ]
        e2e.wait_for(
            lambda: read_state(root)["offset"] > 60,
            timeout=30,
            what="the removed account to be ignored",
        )
        check(
            not any(item.get("reply_to_message_id") == 60 for item in server.sent),
            "and is ignored afterwards",
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
