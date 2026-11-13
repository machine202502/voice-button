"""The recording scenarios that used to be checked by hand.

Every case starts a real daemon, opens the real page in a browser, records with
a stubbed microphone and toggles the network. Short timings are used on purpose:
the shape of the case is what matters, not the exact minutes.

    python tests/mic-scenarios-test.py
"""

import base64
import importlib.util
import json
import os
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
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


class Case:
    """One daemon, one browser, one monolog per scenario."""

    def __init__(self, name):
        self.name = name
        self.daemon = e2e.Daemon(models=True)
        self.browser = e2e.Browser()

    def __enter__(self):
        self.daemon.start()
        self.browser.connect()
        self.browser.call(
            "Emulation.setDeviceMetricsOverride",
            {"width": 412, "height": 915, "deviceScaleFactor": 1, "mobile": True},
        )
        self.browser.navigate(self.daemon.base + "/")
        e2e.wait_for(
            lambda: self.browser.evaluate(
                "location.origin === " + repr(self.daemon.base)
                + " && document.readyState === 'complete'"
            ),
            timeout=20,
            what="the page",
        )
        self.browser.evaluate("localStorage.setItem('vb_token', %r)" % self.daemon.token)
        self.browser.call("Page.reload", {"ignoreCache": True})
        e2e.wait_for(
            lambda: self.browser.evaluate("!document.getElementById('app').hidden"),
            timeout=20,
            what="the app",
        )
        self.browser.evaluate(
            e2e.MIC_SCRIPT.replace(
                "BASE64", base64.b64encode(e2e.speech_mp3()).decode()
            ).replace("SKIP", "0"),
            await_promise=True,
        )
        return self

    def __exit__(self, *exc):
        self.browser.stop()
        self.daemon.stop()

    def monolog(self, name):
        return self.daemon.api("/site/api/monologues", "POST", {"name": name})["monolog"]["id"]

    def open(self, name):
        e2e.wait_for(
            lambda: self.browser.evaluate(
                "Array.from(document.querySelectorAll('.monolog-card'))"
                ".some((card) => card.textContent.includes(%r))" % name
            ),
            timeout=20,
            what="the card " + name,
        )
        self.browser.evaluate(
            "Array.from(document.querySelectorAll('.monolog-card'))"
            ".find((card) => card.textContent.includes(%r)).click()" % name
        )
        e2e.wait_for(
            lambda: self.browser.evaluate("!document.getElementById('composer').hidden"),
            timeout=10,
            what="the monolog " + name,
        )

    def back(self):
        self.browser.evaluate("document.getElementById('back').click()")
        e2e.wait_for(
            lambda: self.browser.evaluate("document.getElementById('title').textContent === 'Монологи'"),
            timeout=10,
            what="the list",
        )

    def start(self):
        self.browser.evaluate("document.getElementById('record').click()")
        e2e.wait_for(
            lambda: self.browser.evaluate(
                "document.getElementById('record').classList.contains('recording')"
                " || document.getElementById('record').classList.contains('offline')"
            ),
            timeout=15,
            what="the recording state",
        )

    def stop(self):
        self.browser.evaluate("document.getElementById('record').click()")
        e2e.wait_for(
            lambda: self.browser.evaluate(
                "!document.getElementById('record').classList.contains('recording')"
                " && !document.getElementById('record').classList.contains('offline')"
            ),
            timeout=15,
            what="the microphone to go quiet",
        )
        check(True, "the microphone is white again after the stop")

    def messages(self):
        found = []
        for monolog in self.daemon.api("/site/api/monologues").get("monologs", []):
            found.extend(
                self.daemon.api("/site/api/monologues/%d/messages" % monolog["id"]).get(
                    "messages", []
                )
            )
        return found

    def newest(self):
        messages = self.messages()
        return max(messages, key=lambda item: item["id"]) if messages else None

    def wait_done(self, timeout=90):
        def finished():
            message = self.newest()
            return message if message and message["stage"] in ("done", "silent") else None

        return e2e.wait_for(finished, timeout=timeout, what="the take to be finished")


def scenario(name):
    print(name)
    return Case(name)


def run_scenarios():
    # 1. Internet off, several takes in several monologs, then it comes back.
    with scenario("several takes in several monologs while offline") as case:
        first = case.monolog("Первый")
        second = case.monolog("Второй")
        # The page must know both monologs before the network goes away.
        e2e.wait_for(
            lambda: case.browser.evaluate(
                "Array.from(document.querySelectorAll('.monolog-card'))"
                ".filter((card) => card.textContent.includes('Первый')"
                " || card.textContent.includes('Второй')).length === 2"
            ),
            timeout=20,
            what="both monologs on the list",
        )
        case.browser.offline(True)
        case.open("Первый")
        case.start()
        time.sleep(4)
        case.stop()
        case.back()
        case.open("Второй")
        case.start()
        time.sleep(4)
        case.stop()
        case.browser.offline(False)
        try:
            e2e.wait_for(
                lambda: len(case.messages()) >= 2
                and all(item["stage"] in ("done", "silent") for item in case.messages()),
                timeout=240,
                what="both takes to be transcribed",
            )
        except TimeoutError:
            print("  daemon messages:", json.dumps(case.messages(), ensure_ascii=True)[:700])
            print("  outbox:", json.dumps(e2e.summarise_outbox(e2e.outbox_state(case.browser)), ensure_ascii=True)[:500])
            print("  page:", case.browser.evaluate(
                "JSON.stringify({ reachable: window.app.reachable,"
                " running: window.app.loop.running, scheduled: window.app.loop.scheduled,"
                " trace: (window.__trace || []).slice(-10) })"
            ))
            raise
        messages = case.messages()
        check(len(messages) >= 2, f"both takes reached the computer ({len(messages)})")
        check(
            all(len(item["text"].strip()) > 0 or item["stage"] == "silent" for item in messages),
            "both takes have their text",
        )
        check(
            any(item["monolog"] == first and item["stage"] in ("done", "silent") for item in messages),
            "the first monolog was transcribed too",
        )

    # 2. Online, a pause in the middle, then more speech.
    with scenario("a pause in the middle of a take") as case:
        case.monolog("Пауза")
        case.open("Пауза")
        case.start()
        time.sleep(4)
        time.sleep(8)
        time.sleep(4)
        case.stop()
        done = case.wait_done()
        check(done["stage"] in ("done", "silent"), "the take with a pause is finished")
        check(len(case.messages()) == 1, "the pause did not split the take into several messages")

    # 3. Online, the connection drops for a while, then returns while recording.
    with scenario("the connection returns while still recording") as case:
        case.monolog("Возврат")
        case.open("Возврат")
        case.start()
        time.sleep(3)
        case.browser.offline(True)
        time.sleep(8)
        case.browser.offline(False)
        time.sleep(5)
        case.stop()
        done = case.wait_done()
        check(done["stage"] in ("done", "silent"), "the take after the drop is finished")
        check(len(case.messages()) == 1, "one take stayed one message")

    # 4. The connection drops, the user stops offline, then the connection returns.
    with scenario("the take is stopped while offline") as case:
        case.monolog("Стоп офлайн")
        case.open("Стоп офлайн")
        case.start()
        time.sleep(3)
        case.browser.offline(True)
        time.sleep(8)
        case.stop()
        time.sleep(3)
        case.browser.offline(False)
        done = case.wait_done()
        check(done["stage"] in ("done", "silent"), "the take stopped offline is finished when the connection returns")
        check(len(case.messages()) == 1, "it is still one message")

    # 5. Start without a connection, it returns, recording continues.
    with scenario("recording starts without a connection") as case:
        case.monolog("Офлайн старт")
        case.open("Офлайн старт")
        case.browser.offline(True)
        case.start()
        time.sleep(8)
        case.browser.offline(False)
        time.sleep(5)
        case.stop()
        done = case.wait_done()
        check(done["stage"] in ("done", "silent"), "the take started offline is finished")
        check(len(case.messages()) == 1, "it is one message")


def run_engine_busy():
    print("the engine is taken while the phone records")
    with scenario("engine busy") as case:
        case.monolog("Движок")
        case.open("Движок")
        case.start()
        time.sleep(2)
        session = case.daemon.take_engine()
        check(session is not None, "the computer took the engine")
        time.sleep(4)
        paused = case.newest()
        check(
            paused is None or paused["stage"] not in ("done", "silent"),
            "nothing was finished while the engine was taken",
        )
        case.daemon.free_engine(session)
        time.sleep(4)
        case.stop()
        done = case.wait_done()
        check(done["stage"] in ("done", "silent"), "the take finished after the engine came back")


def main():
    run_scenarios()
    run_engine_busy()
    print("")
    if failures:
        print("failed:")
        for failure in failures:
            print("  - " + failure)
        sys.exit(1)
    print(f"{checks}/{checks} checks passed")


if __name__ == "__main__":
    main()
