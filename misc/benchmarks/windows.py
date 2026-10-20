#!/usr/bin/env python3
"""Windows-only lifecycle and native tab-motion benchmarks for rio-profile."""
import argparse
import ctypes as C
from ctypes import wintypes as W
import json
import os
from pathlib import Path
import subprocess
import sys
import time

MINIMIZE_SETTLE_MS = 10_000

def motion(executable, output, level, minimize=False):
    from desktop_windows import Window, cpu_ns
    user = C.WinDLL("user32",use_last_error=True)
    class Keyboard(C.Structure):
        _fields_ = [("vk",W.WORD),("scan",W.WORD),("flags",W.DWORD),("time",W.DWORD),("extra",C.c_size_t)]
    class Mouse(C.Structure):
        _fields_ = [("x",W.LONG),("y",W.LONG),("data",W.DWORD),("flags",W.DWORD),("time",W.DWORD),("extra",C.c_size_t)]
    class Payload(C.Union):
        _fields_ = [("keyboard",Keyboard),("mouse",Mouse)]
    class Input(C.Structure):
        _anonymous_ = ("payload",)
        _fields_ = [("type",W.DWORD),("payload",Payload)]
    user.SendInput.argtypes = [W.UINT,C.POINTER(Input),C.c_int]
    user.SendInput.restype = W.UINT
    user.GetForegroundWindow.restype = W.HWND
    user.ShowWindow.argtypes = [W.HWND,C.c_int]
    user.GetClientRect.argtypes = [W.HWND,C.POINTER(W.RECT)]
    folder = output/f"motion-{level}"; folder.mkdir(parents=True,exist_ok=True)
    config = folder/"config"; config.mkdir(exist_ok=True)
    (config/"config.toml").write_text(f"""[navigation]
mode = 'Tab'
animation = '{level}'
[cursor]
blinking = false
[bindings]
keys = [{{key='F9',action='SelectNextTab'}}]
""")
    report = folder/"capture.json"
    env = dict(os.environ,RIO_CONFIG_HOME=str(config.resolve()),RIO_PROFILE_OUTPUT=str(report.resolve()),
               RIO_PROFILE_TABS="4",RIO_PROFILE_WINDOWS="1",RIO_PROFILE_FRAMES="0")
    if minimize: env["RIO_PROFILE_SETTLE_MS"] = str(MINIMIZE_SETTLE_MS)
    with (folder/"process.log").open("w") as log:
        launched = time.monotonic()
        process = subprocess.Popen([str(executable)],env=env,stdout=log,stderr=log,
                                   creationflags=subprocess.NORMAL_PRIORITY_CLASS)
        try:
            from desktop_windows import priority_class
            priority = {"process_priority_class":priority_class(process), "driver_priority_class":priority_class()}
            desktop = Window()
            window = None
            deadline = time.monotonic()+8
            while window is None:
                window = desktop.find(process.pid)
                if process.poll() is not None or time.monotonic()>deadline: raise RuntimeError("No profiling window")
                time.sleep(0.01)
            desktop.focus(window)
            time.sleep(0.4)
            original = W.RECT()
            if not user.GetClientRect(window,C.byref(original)): raise C.WinError(C.get_last_error())
            for _ in range(20):
                if minimize:
                    user.ShowWindow(window,6); time.sleep(0.015)
                    user.ShowWindow(window,9); time.sleep(0.035)
                    restored = W.RECT()
                    if not user.GetClientRect(window,C.byref(restored)): raise C.WinError(C.get_last_error())
                    if (restored.right,restored.bottom)!=(original.right,original.bottom): raise RuntimeError("Window dimensions changed after restore")
                    continue
                if user.GetForegroundWindow()!=window: raise RuntimeError("Profiling window is not foreground; no input sent")
                for flags in [0,2]:
                    event = Input(type=1,keyboard=Keyboard(vk=0x78,flags=flags))
                    if user.SendInput(1,C.byref(event),C.sizeof(event))!=1: raise C.WinError(C.get_last_error())
                time.sleep(0.045)
            process.wait(timeout=15)
            capture = json.loads(report.read_text())
            minimum = 15 if minimize or level=="none" else 30
            if process.returncode or capture["ready_tabs"]!=4 or capture["frames"]<minimum:
                raise RuntimeError("Tab motion was not exercised; do not use this capture as a timing result")
            capture["process_cpu_ms"] = cpu_ns(process)/1_000_000
            capture["process_ms"] = (time.monotonic()-launched)*1000
            return {"case":"windows/minimize_restore" if minimize else f"windows/motion/{level}","capture":capture, **priority}
        finally:
            if process.poll() is None: process.terminate(); process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("executable",type=Path)
    parser.add_argument("--output",type=Path,default=Path("target/branch-performance/windows"))
    parser.add_argument("--runs",type=int,default=5)
    args = parser.parse_args()
    if sys.platform!="win32": raise SystemExit("Windows lifecycle and display-clock benches cannot run on Linux")
    args.output.mkdir(parents=True,exist_ok=True)
    config = args.output/"config"; config.mkdir(exist_ok=True)
    (config/"config.toml").write_text("[navigation]\nmode = 'Tab'\n[cursor]\nblinking = false\n")
    driver = Path(__file__).resolve().parents[1]/"profiling/run-windows.py"
    for case,options in {
        "startup_exit":[],
        "second_window":["--windows","2"],
        "many_tabs":["--tabs","8"],
        "dense":["--frames","300","--full-redraw"],
    }.items():
        subprocess.run([sys.executable,str(driver),str(args.executable.resolve()),"--runs",str(args.runs),
                        "--output",str(args.output/case),"--env",f"RIO_CONFIG_HOME={config.resolve()}",*options],check=True)
    rows = [motion(args.executable.resolve(),args.output/f"run-{repeat}",level)
            for repeat in range(args.runs) for level in ["none","medium","full"]]
    rows += [motion(args.executable.resolve(),args.output/f"minimize-{repeat}","full",minimize=True) for repeat in range(args.runs)]
    (args.output/"motion.json").write_text(json.dumps(rows,indent=2)+"\n")


if __name__=="__main__": main()
