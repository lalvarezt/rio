"""Run fixed terminal workloads through Rio's actual Linux window and PTY."""
import ctypes as C
import ctypes.util
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

DURATION = 4.0
UPDATES = 240


def fixture(mode, folder):
    folder = Path(folder)
    actual_cols, actual_rows = os.get_terminal_size(1)
    columns, rows = 120, 32
    assert actual_cols >= columns and actual_rows >= rows
    emit = lambda text: os.write(1, text.encode())
    text = "let value = compute(input) + 42; // benchmark 0123456789 "
    line = (text * 3)[:columns]
    emit("\x1b[?1049h\x1b[?25l\x1b[2J" + "".join(f"\x1b[{row+1};1H{line}" for row in range(rows)))
    emit(f"\x1b]2;RIO_BENCH_{os.getpid()}\x07")
    (folder/f"ready-{os.getpid()}").write_text(json.dumps({"columns":actual_cols,"rows":actual_rows}))
    while not (folder/"go").exists():
        time.sleep(0.005)
    start = time.monotonic()
    for index in range(UPDATES):
        if mode == "dense":
            emit("".join(f"\x1b[{row+1};1H\x1b[{32+row%6}m{index:06} {line[7:]}" for row in range(rows)))
        elif mode == "sparse":
            emit(f"\x1b[{2+index%(rows-2)};1H{index:06} {line[7:]}")
        elif mode == "scroll":
            emit(f"\x1b[{actual_rows};1H" + "".join(f"\r\n{index:06}/{part} {line[10:]}" for part in range(4)))
        elif mode in ("cursor", "search"):
            emit(f"\x1b[?25h\x1b[{1+index%rows};{1+index*7%columns}H")
        elif mode == "unicode":
            emit(f"\x1b[{2+index%(rows-2)};1H\x1b[0m{index:06} " + "日本語 한글 λ α Ж é 👩‍💻 "*4)
        time.sleep(max(0, start+(index+1)*DURATION/UPDATES-time.monotonic()))
    (folder/f"done-{os.getpid()}").touch()
    while not (folder/"stop").exists():
        time.sleep(0.01)


class X11:
    error_callback = C.CFUNCTYPE(C.c_int,C.c_void_p,C.c_void_p)(lambda _display,_error:0)
    class Key(C.Structure):
        _fields_ = [("type",C.c_int),("serial",C.c_ulong),("send_event",C.c_int),("display",C.c_void_p),
                    ("window",C.c_ulong),("root",C.c_ulong),("subwindow",C.c_ulong),("time",C.c_ulong),
                    ("x",C.c_int),("y",C.c_int),("x_root",C.c_int),("y_root",C.c_int),
                    ("state",C.c_uint),("keycode",C.c_uint),("same_screen",C.c_int)]
    class Image(C.Structure):
        _fields_ = [("width",C.c_int),("height",C.c_int),("xoffset",C.c_int),("format",C.c_int),
                    ("data",C.c_void_p),("byte_order",C.c_int),("bitmap_unit",C.c_int),("bitmap_bit_order",C.c_int),
                    ("bitmap_pad",C.c_int),("depth",C.c_int),("bytes_per_line",C.c_int),("bits_per_pixel",C.c_int)]

    def __init__(self, display):
        self.x = C.CDLL(ctypes.util.find_library("X11"))
        self.x.XSetErrorHandler.argtypes = [type(self.error_callback)]
        self.x.XSetErrorHandler(self.error_callback)
        definitions = {
            "XOpenDisplay":([C.c_char_p],C.c_void_p), "XDefaultRootWindow":([C.c_void_p],C.c_ulong),
            "XQueryTree":([C.c_void_p,C.c_ulong,C.POINTER(C.c_ulong),C.POINTER(C.c_ulong),C.POINTER(C.POINTER(C.c_ulong)),C.POINTER(C.c_uint)],C.c_int),
            "XInternAtom":([C.c_void_p,C.c_char_p,C.c_int],C.c_ulong),
            "XGetWindowProperty":([C.c_void_p,C.c_ulong,C.c_ulong,C.c_long,C.c_long,C.c_int,C.c_ulong,C.POINTER(C.c_ulong),C.POINTER(C.c_int),C.POINTER(C.c_ulong),C.POINTER(C.c_ulong),C.POINTER(C.c_void_p)],C.c_int),
            "XFree":([C.c_void_p],C.c_int), "XStringToKeysym":([C.c_char_p],C.c_ulong),
            "XKeysymToKeycode":([C.c_void_p,C.c_ulong],C.c_uint),
            "XSendEvent":([C.c_void_p,C.c_ulong,C.c_int,C.c_long,C.c_void_p],C.c_int),
            "XSetInputFocus":([C.c_void_p,C.c_ulong,C.c_int,C.c_ulong],C.c_int),
            "XFlush":([C.c_void_p],C.c_int), "XSync":([C.c_void_p,C.c_int],C.c_int),
            "XGetImage":([C.c_void_p,C.c_ulong,C.c_int,C.c_int,C.c_uint,C.c_uint,C.c_ulong,C.c_int],C.POINTER(self.Image)),
            "XDestroyImage":([C.POINTER(self.Image)],C.c_int), "XCloseDisplay":([C.c_void_p],C.c_int),
        }
        for name,(arguments,result) in definitions.items():
            function = getattr(self.x,name); function.argtypes = arguments; function.restype = result
        self.display = self.x.XOpenDisplay(display.encode())
        if not self.display:
            raise RuntimeError("Cannot open benchmark display")
        self.root = self.x.XDefaultRootWindow(self.display)
        self.pid_atom = self.x.XInternAtom(self.display,b"_NET_WM_PID",0)

    def children(self, window):
        root,parent,count = C.c_ulong(),C.c_ulong(),C.c_uint()
        children = C.POINTER(C.c_ulong)()
        self.x.XQueryTree(self.display,window,C.byref(root),C.byref(parent),C.byref(children),C.byref(count))
        try:
            return list(children[:count.value])
        finally:
            if children: self.x.XFree(children)

    def find(self, pid):
        todo = self.children(self.root)
        while todo:
            window = todo.pop()
            actual,fmt,count,remaining,data = C.c_ulong(),C.c_int(),C.c_ulong(),C.c_ulong(),C.c_void_p()
            self.x.XGetWindowProperty(self.display,window,self.pid_atom,0,1,0,0,C.byref(actual),C.byref(fmt),C.byref(count),C.byref(remaining),C.byref(data))
            try:
                if data and count.value and C.cast(data,C.POINTER(C.c_ulong))[0] == pid:
                    return window
            finally:
                if data: self.x.XFree(data)
            todo += self.children(window)
        return None

    def key(self, window, name):
        code = self.x.XKeysymToKeycode(self.display,self.x.XStringToKeysym(name.encode()))
        for kind,mask in [(2,1),(3,2)]:
            key = self.Key(type=kind,display=self.display,window=window,root=self.root,keycode=code,same_screen=1)
            event = (C.c_long*24)(); C.memmove(C.byref(event),C.byref(key),C.sizeof(key))
            self.x.XSendEvent(self.display,window,0,mask,C.byref(event))
        self.x.XFlush(self.display)

    def image(self, window):
        image = self.x.XGetImage(self.display,window,0,0,1024,640,C.c_ulong(-1).value,2)
        if not image:
            raise RuntimeError("No window image for UI verification")
        try:
            value = image.contents
            return hashlib.sha256(C.string_at(value.data,value.bytes_per_line*value.height)).hexdigest()
        finally:
            self.x.XDestroyImage(image)

    def close(self):
        self.x.XCloseDisplay(self.display)


def cpu_ns(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")",1)[1].split()
    return (int(fields[11])+int(fields[12])) * 1_000_000_000 // os.sysconf("SC_CLK_TCK")


def run(binary, mode, folder, env, cpu):
    folder.mkdir(parents=True,exist_ok=True)
    for pattern in ["ready-*","done-*","go","stop"]:
        for path in folder.glob(pattern): path.unlink()
    config = folder/"config"; config.mkdir(exist_ok=True)
    (config/"config.toml").write_text(f"""[renderer]
backend = 'Vulkan'
[window]
width = 1440
height = 900
[fonts]
size = 14
[navigation]
hide-if-single = false
[cursor]
blinking = false
[shell]
program = '{sys.executable}'
args = ['{Path(__file__).resolve()}', '--fixture', '{mode}', '{folder}']
[bindings]
keys = [{{key='F6',action='CreateTab'}},{{key='F7',action='OpenCommandPalette'}},{{key='F8',action='SearchForward'}},{{key='F9',action='SelectNextTab'}}]
""")
    env = dict(env,RIO_CONFIG_HOME=str(config))
    def pin(): os.sched_setaffinity(0,{cpu})
    x = X11(env["DISPLAY"])
    with (folder/"application.log").open("w") as log:
        process = subprocess.Popen([binary],env=env,stdout=log,stderr=log,preexec_fn=pin)
        try:
            deadline = time.monotonic()+20
            while not list(folder.glob("ready-*")):
                if process.poll() is not None or time.monotonic()>deadline:
                    raise RuntimeError(f"No terminal readiness; see {folder}")
                time.sleep(0.01)
            window = x.find(process.pid)
            if window is None: raise RuntimeError("No Rio client window")
            x.x.XSetInputFocus(x.display,window,1,0); x.x.XSync(x.display,0)
            tabs = 4 if mode in ("tabs","tab_repeat") else 1
            for count in range(2,tabs+1):
                x.key(window,"F6")
                while len(list(folder.glob("ready-*")))<count:
                    if process.poll() is not None or time.monotonic()>deadline: raise RuntimeError("Tab creation did not reach the fixture")
                    time.sleep(0.01)
            time.sleep(0.3)
            if mode in ("tabs","tab_repeat"):
                before = x.image(window)
                x.key(window,"F9")
                time.sleep(0.2)
                if x.image(window)==before: raise RuntimeError("Tab navigation did not change the rendered UI")
            if mode in ("palette","search"):
                before = x.image(window)
                x.key(window,"F7" if mode=="palette" else "F8")
                time.sleep(0.15)
                if mode=="search":
                    for key in "compute": x.key(window,key)
                    time.sleep(0.15)
                if x.image(window)==before: raise RuntimeError(f"{mode} input did not change the rendered UI")
            start_cpu = cpu_ns(process.pid)
            start = time.monotonic()
            (folder/"go").touch()
            if mode in ("tabs","tab_repeat","palette"):
                rate = 120 if mode=="tab_repeat" else 30
                for index in range(int(DURATION*rate)):
                    x.key(window,("Down" if index%2==0 else "Up") if mode=="palette" else "F9")
                    time.sleep(max(0,start+(index+1)/rate-time.monotonic()))
            while len(list(folder.glob("done-*")))<tabs:
                if process.poll() is not None or time.monotonic()>start+DURATION+10: raise RuntimeError("Fixture did not finish")
                time.sleep(0.01)
            time.sleep(0.15)
            elapsed_cpu = cpu_ns(process.pid)-start_cpu
            elapsed_wall = time.monotonic()-start
            (folder/"stop").touch()
            process.wait(timeout=10)
            if process.returncode: raise RuntimeError(f"Rio exited {process.returncode}")
            return {"kind":"bench","case":f"application/{mode}","sample":0,"ns":elapsed_cpu,
                    "wall_ms":elapsed_wall*1000,"updates":UPDATES,"tabs":tabs,"metric":"process_cpu_ns_per_replay"}
        finally:
            if process.poll() is None:
                (folder/"stop").touch(); process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: process.kill(); process.wait()
            x.close()


if __name__ == "__main__":
    if len(sys.argv)==4 and sys.argv[1]=="--fixture": fixture(sys.argv[2],sys.argv[3])
    else: raise SystemExit("Use run.py to launch the application benchmarks")
