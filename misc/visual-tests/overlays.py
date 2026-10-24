"""Test live Rio overlays on an existing X11/Xwayland display.

Build with `cargo build -p rioterm --bin rio`. Install Pillow and python-xlib
for this Python interpreter, then run `python misc/visual-tests/overlays.py
--display :1`. Screenshots and isolated config fixtures go to --output.
Only windows belonging to the test processes receive input.
"""

import argparse
import json
import os
from pathlib import Path
import subprocess
import time

from PIL import Image, ImageChops
from Xlib import X, XK, display, protocol
from Xlib.ext import xtest

ROOT = Path(__file__).resolve().parents[2]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--display', default=os.environ.get('DISPLAY'))
parser.add_argument('--output', type=Path, default=Path('/tmp/rio-overlay-visuals'))
parser.add_argument('--binary', type=Path, default=ROOT / 'target/debug/rio')
args = parser.parse_args()
if not args.display:
    parser.error('Set DISPLAY or pass --display for the existing desktop session')
OUT = args.output.resolve()
OUT.mkdir(parents=True, exist_ok=True)
ENV = dict(os.environ, DISPLAY=args.display)
ENV.pop('WAYLAND_DISPLAY', None)
os.environ['DISPLAY'] = args.display
d = display.Display(args.display)
root = d.screen().root

def wait_window(pid):
    atom = d.intern_atom('_NET_WM_PID')
    for _ in range(100):
        for w in root.query_tree().children:
            try:
                prop = w.get_full_property(atom, X.AnyPropertyType)
                if prop is not None and int(prop.value[0]) == pid and w.get_attributes().map_state == X.IsViewable:
                    time.sleep(1)
                    w.set_input_focus(X.RevertToParent, X.CurrentTime)
                    d.sync()
                    return w
            except Exception:
                pass
        time.sleep(.1)
    raise AssertionError('Rio window did not map')

def capture(w, name):
    time.sleep(.3)
    d.sync()
    g = w.get_geometry()
    raw = w.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xffffffff)
    im = Image.frombytes('RGB', (g.width, g.height), raw.data, 'raw', 'BGRX')
    im.save(OUT / (name + '.png'))
    return im

def move(w, x, y):
    pos = w.translate_coords(root, int(x), int(y))
    xtest.fake_input(d, X.MotionNotify, x=pos.x, y=pos.y)
    d.sync()
    time.sleep(.15)

def button(state):
    xtest.fake_input(d, state, detail=1)
    d.sync()
    time.sleep(.15)

def key(name):
    code = d.keysym_to_keycode(XK.string_to_keysym(name))
    xtest.fake_input(d, X.KeyPress, detail=code)
    xtest.fake_input(d, X.KeyRelease, detail=code)
    d.sync()
    time.sleep(.2)

def close_request(w):
    w.send_event(protocol.event.ClientMessage(window=w, client_type=d.intern_atom('WM_PROTOCOLS'), data=(32, [d.intern_atom('WM_DELETE_WINDOW'), X.CurrentTime, 0, 0, 0])), event_mask=0)
    d.sync()
    time.sleep(.3)

def chord(name):
    modifiers = [d.keysym_to_keycode(XK.string_to_keysym(k)) for k in ['Control_L', 'Shift_L']]
    for code in modifiers:
        xtest.fake_input(d, X.KeyPress, detail=code)
    key(name)
    for code in reversed(modifiers):
        xtest.fake_input(d, X.KeyRelease, detail=code)
    d.sync()
    time.sleep(.3)

def changed(a, b, region):
    diff = ImageChops.difference(a.crop(region), b.crop(region))
    return diff.getbbox() is not None

config = OUT / 'config'
config.mkdir(exist_ok=True)
(config / 'config.toml').write_text('confirm-before-quit = true\n[window]\nwidth=800\nheight=600\n[renderer]\nuse-cpu=true\n')
env = dict(ENV, RIO_CONFIG_HOME=str(config))
log = open(OUT / 'rio.log', 'w')
p = subprocess.Popen([str(args.binary.resolve()), '--app-id', 'rio-overlay-visual', '-e', '/bin/sh', '-c', 'printf "Overlay visual test\\n"; exec sleep 600'], env=env, stdout=log, stderr=log, start_new_session=True)
try:
    w = wait_window(p.pid)
    base = capture(w, 'terminal')
    close_request(w)
    move(w, 5, 300)
    idle = capture(w, 'quit-idle')
    width, height = idle.size
    px, py = (width - 380) / 2, (height - 164) / 2
    assert idle.getpixel((int(px), int(py + 70))) != idle.getpixel((int(px + 3), int(py + 70))), 'missing dialog border'
    region = (int(px), int(py), int(px + 380), int(py + 164))
    move(w, px + 100, py + 128)
    hover = capture(w, 'quit-cancel-hover')
    assert changed(idle, hover, region), 'Cancel hover has no visible feedback'
    button(X.ButtonPress)
    pressed = capture(w, 'quit-cancel-pressed')
    assert changed(hover, pressed, region), 'Cancel press has no visible feedback'
    button(X.ButtonRelease)
    cancelled = capture(w, 'quit-cancelled')
    assert changed(pressed, cancelled, region), 'Cancel did not dismiss dialog'
    close_request(w)
    move(w, 5, 300)
    reopened = capture(w, 'quit-reopened')
    key('Tab')
    focused = capture(w, 'quit-keyboard-close-focus')
    assert changed(reopened, focused, region), 'Tab has no visible focus feedback'
    key('Escape')
    close_request(w)
    key('Return')
    cancelled = capture(w, 'quit-enter-cancelled')
    assert changed(reopened, cancelled, region), 'Enter did not activate the default Cancel button'
    close_request(w)
    key('space')
    cancelled = capture(w, 'quit-space-cancelled')
    assert changed(reopened, cancelled, region), 'Space did not activate the focused Cancel button'
    close_request(w)
    move(w, px + 280, py + 128)
    capture(w, 'quit-close-hover')
    button(X.ButtonPress)
    capture(w, 'quit-close-pressed')
    button(X.ButtonRelease)
    p.wait(timeout=5)
    print(json.dumps({'result':'PASS', 'size':idle.size, 'screenshots':str(OUT), 'checks':['border','Cancel hover','Cancel press','Cancel dismissal','keyboard focus','Close click exits']}))
finally:
    if p.poll() is None:
        os.killpg(p.pid, 15)
        p.wait(timeout=5)
    log.close()

def launch_case(name, contents):
    directory = OUT / name
    directory.mkdir(exist_ok=True)
    (directory / 'config.toml').write_text(contents)
    log = open(directory / 'rio.log', 'w')
    p = subprocess.Popen([str(args.binary.resolve()), '--app-id', 'rio-visual-' + name, '-e', '/bin/sh', '-c', 'printf "Overlay visual test\\n"; exec sleep 600'], env=dict(ENV, RIO_CONFIG_HOME=str(directory)), stdout=log, stderr=log, start_new_session=True)
    return p, log

p, log = launch_case('other-overlays', '[renderer]\nuse-cpu=true\n')
try:
    w = wait_window(p.pid)
    move(w, 5, 400)
    base = capture(w, 'other-terminal')
    chord('p')
    palette = capture(w, 'palette-idle')
    width, height = palette.size
    region = (int(width/2 - 240), 80, int(width/2 + 240), 650)
    assert changed(base, palette, region), 'palette did not open'
    move(w, width/2, 250)
    hovered = capture(w, 'palette-hover')
    assert changed(palette, hovered, region), 'palette result hover has no feedback'
    key('Escape')
    chord('f')
    move(w, 5, 400)
    search = capture(w, 'search-idle')
    ox = width - 328
    region = (ox, 8, width - 8, 44)
    assert changed(base, search, region), 'search did not open'
    assert search.getpixel((ox, 26)) != search.getpixel((ox + 3, 26)), 'search has no border'
    move(w, width - 28, 26)
    hovered = capture(w, 'search-close-hover')
    assert changed(search, hovered, region), 'search close hover has no feedback'
    button(X.ButtonPress)
    button(X.ButtonRelease)
    dismissed = capture(w, 'search-dismissed')
    assert changed(hovered, dismissed, region), 'search close did not dismiss'
    print('palette/search PASS: render, borders, hover, search dismissal')
finally:
    if p.poll() is None:
        os.killpg(p.pid, 15)
        p.wait(timeout=5)
    log.close()

for name, contents in [
    ('config-error', '[fonts\nsize = "not a number"\n'),
    ('font-warning', '[renderer]\nuse-cpu=true\n[fonts]\nfamily="RioVisualTestMissingFont12345"\n'),
]:
    p, log = launch_case(name, contents)
    try:
        w = wait_window(p.pid)
        move(w, 5, 400)
        idle = capture(w, name + '-idle')
        width, height = idle.size
        ox, oy, ow = width - 488, 8, 480
        # Border mid-edge stays distinct from the panel's interior.
        assert idle.getpixel((ox, oy + 40)) != idle.getpixel((ox + 3, oy + 40)), name + ' has no border'
        region = (ox, oy, width - 8, min(height, oy + 400))
        move(w, width - 36, 24)
        hover = capture(w, name + '-close-hover')
        assert changed(idle, hover, region), name + ' close hover has no feedback'
        # Locate the docs row by hovering rows until the link highlight appears.
        move(w, ox + 60, oy + 60)
        for y in range(oy + 62, min(height, oy + 390), 9):
            move(w, ox + 65, y)
            im = capture(w, name + '-probe')
            if changed(idle, im, (ox + 10, y - 3, ox + 180, y + 3)):
                im.save(OUT / (name + '-docs-hover.png'))
                break
        else:
            raise AssertionError(name + ' docs hover has no feedback')
        move(w, width - 36, 24)
        button(X.ButtonPress)
        button(X.ButtonRelease)
        dismissed = capture(w, name + '-dismissed')
        assert changed(hover, dismissed, region), name + ' close did not dismiss'
        print(name, 'PASS: border, close hover, docs hover, dismissal')
    finally:
        if p.poll() is None:
            os.killpg(p.pid, 15)
            p.wait(timeout=5)
        log.close()

