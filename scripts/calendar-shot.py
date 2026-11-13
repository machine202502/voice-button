"""Photograph the period calendar and save one rectangle of the screen.

One run, about two seconds:

1. start the window binary, told to open its calendar at once and to sit at a
   fixed place on the screen;
2. wait until the window says the calendar is open (a line in its own log), then
   one short pause for the frame to reach the screen;
3. save that rectangle of the screen as a PNG;
4. stop the process it started.

No process lookup, no window handle, no polling for windows: the rectangle is a
constant here, and it is the same constant the window is given. Every wait is
bounded, so nothing can hang.
"""

import ctypes
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from PIL import ImageGrab

ROOT = Path(__file__).resolve().parent.parent
# The committed snapshots live in one place; the calendar joins them.
OUT = ROOT / "tests" / "snapshots" / "calendar.png"

# The chosen zone, in logical points: where the window is put and how wide it is.
# In shot mode the window has no frame, so the zone starts at its top-left corner;
# the height stops under the calendar, above the empty history list.
X, Y, W, H = 120, 120, 560, 430
# How long the window may take to open its calendar, and how long the frame then
# needs to be on the screen before the copy is taken.
READY_TIMEOUT = 8.0
PAINT = 1.0
MARKER = "calendar shot: period picker opened"


def ui_exe() -> Path:
    """The window binary, wherever the build directory points."""
    target = ROOT / "target"
    config = ROOT / ".cargo" / "config.toml"
    if config.exists():
        for line in config.read_text(encoding="utf-8").splitlines():
            if line.strip().startswith("target-dir"):
                target = Path(line.split("=", 1)[1].strip().strip('"'))
    return target / "debug" / "voice-button-ui.exe"


def pixels_per_point() -> float:
    """How many screen pixels one logical point is worth on this display."""
    try:
        ctypes.windll.shcore.SetProcessDpiAwareness(2)
    except OSError:
        pass
    return ctypes.windll.user32.GetDpiForSystem() / 96.0


def wait_for_marker(log: Path) -> bool:
    """True once the window has written the calendar line, False on timeout."""
    deadline = time.monotonic() + READY_TIMEOUT
    while time.monotonic() < deadline:
        if log.exists() and MARKER in log.read_text(encoding="utf-8", errors="ignore"):
            return True
        time.sleep(0.05)
    return False


def main() -> int:
    exe = ui_exe()
    if not exe.exists():
        print(f"not built: {exe}", file=sys.stderr)
        print("run: cargo build --bin voice-button-ui", file=sys.stderr)
        return 2
    scale = pixels_per_point()
    OUT.parent.mkdir(parents=True, exist_ok=True)
    # A scratch data directory: the shot never touches the real history and does
    # not need the daemon to be running.
    data = Path(tempfile.mkdtemp(prefix="vb-calendar-shot-"))
    # The real settings file, so the calendar is shown in the person's language,
    # and the translation catalogs, which the build directory does not carry.
    real = Path(os.environ.get("APPDATA", "")) / "voice-button" / "config.json"
    if real.exists():
        (data / "config.json").write_bytes(real.read_bytes())
    shutil.copytree(ROOT / "locales", data / "locales")
    env = dict(
        os.environ,
        VB_DATA_DIR=str(data),
        VB_SHOT_AT=f"{X},{Y}",
        VB_OPEN_CALENDAR="1",
    )
    child = subprocess.Popen(
        [str(exe), "--ui", "history"],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        if not wait_for_marker(data / "log.txt"):
            print(
                f"the calendar did not open within {READY_TIMEOUT:.0f}s",
                file=sys.stderr,
            )
            return 1
        time.sleep(PAINT)
        box = (
            round(X * scale),
            round(Y * scale),
            round((X + W) * scale),
            round((Y + H) * scale),
        )
        image = ImageGrab.grab(bbox=box, all_screens=True)
        image.save(OUT)
        print(f"{OUT}  {image.width}x{image.height}  zone {box}")
        return 0
    finally:
        child.terminate()


if __name__ == "__main__":
    sys.exit(main())
