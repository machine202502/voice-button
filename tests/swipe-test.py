"""Swipe a monolog card into the archive and back, in a real browser.

The chat and its menu are covered by phone-e2e.py. This test is deliberately
small: one monolog, two pointer gestures, and the API checked on both sides so
a card cannot disappear only visually.
"""

import importlib.util
import os
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
_SPEC = importlib.util.spec_from_file_location(
    "phone_e2e", os.path.join(HERE, "phone-e2e.py")
)
e2e = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(e2e)


def wait_for(condition, timeout, what):
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            if condition():
                return
        except Exception:
            pass
        time.sleep(0.1)
    raise TimeoutError("timed out waiting for " + what)


def main():
    if len(sys.argv) > 1:
        sys.exit("swipe-test.py takes no arguments")
    e2e.sync_page()
    daemon = e2e.Daemon(models=False)
    browser = e2e.Browser()
    checks = 0
    failures = []

    def check(condition, label):
        nonlocal checks
        checks += 1
        if condition:
            print("  ok   ", label)
        else:
            failures.append(label)
            print("  FAIL ", label)

    try:
        daemon.start()
        browser.connect()
        for _ in range(4):
            browser.navigate("https://127.0.0.1:%d/" % daemon.port)
            try:
                wait_for(
                    lambda: browser.evaluate(
                        "document.readyState === 'complete'"
                        " && location.origin === 'https://127.0.0.1:%d'"
                        % daemon.port
                    ),
                    timeout=10,
                    what="the phone page",
                )
                break
            except TimeoutError:
                continue
        else:
            raise TimeoutError("the phone page never loaded")

        browser.evaluate("localStorage.setItem('vb_token', %r)" % daemon.token)
        browser.call("Page.reload", {"ignoreCache": True})
        wait_for(
            lambda: browser.evaluate(
                "!!document.getElementById('app') && !document.getElementById('app').hidden"
            ),
            timeout=20,
            what="the app",
        )
        wait_for(lambda: daemon.api("/site/api/me").get("paired"), 10, "pairing")
        daemon.api("/site/api/monologues", "POST", {"name": "Свайп"})
        browser.call("Page.reload", {"ignoreCache": True})
        wait_for(
            lambda: browser.evaluate(
                "document.querySelectorAll('.monolog-card').length === 1"
            ),
            timeout=20,
            what="the monolog list",
        )

        swipe = (
            "(dx) => { const card = document.querySelector('.monolog-card');"
            " const rect = card.getBoundingClientRect();"
            " const y = rect.top + rect.height / 2;"
            " const x = rect.left + rect.width - 30;"
            " const make = (type, at) => new PointerEvent(type, {bubbles: true,"
            " cancelable: true, pointerId: 7, pointerType: 'touch',"
            " isPrimary: true, clientX: at, clientY: y, buttons: 1});"
            " card.dispatchEvent(make('pointerdown', x));"
            " card.dispatchEvent(make('pointermove', x + dx));"
            " card.dispatchEvent(make('pointerup', x + dx)); }"
        )

        # Hold the card in the pulled-out state across a sync tick: the list
        # must not redraw under the finger and snap back.
        active_height = browser.evaluate(
            "document.querySelector('.monolog-card').getBoundingClientRect().height"
        )
        start_swipe = (
            "() => { const card = document.querySelector('.monolog-card');"
            " const rect = card.getBoundingClientRect();"
            " window.__swipeY = rect.top + rect.height / 2;"
            " window.__swipeX = rect.left + rect.width - 30;"
            " const make = (type, at) => new PointerEvent(type, {bubbles: true,"
            " cancelable: true, pointerId: 7, pointerType: 'touch',"
            " isPrimary: true, clientX: at, clientY: window.__swipeY, buttons: 1});"
            " card.dispatchEvent(make('pointerdown', window.__swipeX));"
            " card.dispatchEvent(make('pointermove', window.__swipeX - 110)); }"
        )
        end_swipe = (
            "() => { const card = document.querySelector('.monolog-card');"
            " const make = (type, at) => new PointerEvent(type, {bubbles: true,"
            " cancelable: true, pointerId: 7, pointerType: 'touch',"
            " isPrimary: true, clientX: at, clientY: window.__swipeY, buttons: 1});"
            " card.dispatchEvent(make('pointerup', window.__swipeX - 110)); }"
        )
        browser.evaluate("(%s)()" % start_swipe)
        time.sleep(1.4)
        check(
            browser.evaluate(
                "!!document.querySelector('.monolog-card')"
                " && document.querySelector('.monolog-card').style.transform !== ''"
            ),
            "a long swipe survives a sync redraw",
        )
        check(
            browser.evaluate(
                "(() => { const view = document.querySelector('main');"
                " return view.scrollWidth <= view.clientWidth + 1; })()"
            ),
            "a swipe does not create a horizontal scroll stripe",
        )
        browser.evaluate("(%s)()" % end_swipe)
        wait_for(
            lambda: browser.evaluate(
                "document.querySelectorAll('.monolog-card').length === 0"
            ),
            timeout=10,
            what="the card to leave after a left swipe",
        )
        check(
            len(daemon.api("/site/api/monologues?archived=1").get("monologs", [])) == 1,
            "a left swipe archives the card on the computer",
        )

        browser.evaluate("document.getElementById('archive').click()")
        wait_for(
            lambda: browser.evaluate(
                "window.app && window.app.archived === true"
                " && document.querySelectorAll('.monolog-card').length === 1"
            ),
            timeout=10,
            what="the archive",
        )
        archived_height = browser.evaluate(
            "document.querySelector('.monolog-card').getBoundingClientRect().height"
        )
        check(
            abs(active_height - archived_height) < 1,
            "archiving does not change the card height",
        )
        browser.evaluate("(%s)(110)" % swipe)
        wait_for(
            lambda: browser.evaluate(
                "document.querySelectorAll('.monolog-card').length === 0"
            ),
            timeout=10,
            what="the card to leave the archive after a right swipe",
        )
        check(
            len(daemon.api("/site/api/monologues").get("monologs", [])) == 1,
            "a right swipe returns the card to the open list",
        )
        check(
            len(daemon.api("/site/api/monologues?archived=1").get("monologs", [])) == 0,
            "the archive is empty after the return swipe",
        )
    finally:
        try:
            browser.stop()
        finally:
            daemon.stop()

    print()
    print("%d/%d checks passed" % (checks - len(failures), checks))
    if failures:
        print("failed:")
        for failure in failures:
            print("  -", failure)
        sys.exit(1)


if __name__ == "__main__":
    main()
