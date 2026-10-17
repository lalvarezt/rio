"""Child session for the benchmark runner's isolated compositor."""
import json
import os
from pathlib import Path
import signal

Path(os.environ["RIO_BENCH_DISPLAY_FILE"]).write_text(json.dumps({
    key: os.environ[key] for key in ["DISPLAY", "WAYLAND_DISPLAY", "XDG_RUNTIME_DIR", "XAUTHORITY"] if key in os.environ
}))
signal.pause()
