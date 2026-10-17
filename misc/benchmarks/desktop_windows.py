"""Win32 controls for benchmark-owned windows and processes."""
import ctypes as C
from ctypes import wintypes as W
import hashlib
import time

user = C.WinDLL("user32",use_last_error=True)
kernel = C.WinDLL("kernel32",use_last_error=True)
gdi = C.WinDLL("gdi32",use_last_error=True)
callback_type = C.WINFUNCTYPE(W.BOOL,W.HWND,W.LPARAM)


def signature(library, name, arguments, result):
    function = getattr(library,name)
    function.argtypes, function.restype = arguments, result
    return function


signature(user,"EnumWindows",[callback_type,W.LPARAM],W.BOOL)
signature(user,"GetWindowThreadProcessId",[W.HWND,C.POINTER(W.DWORD)],W.DWORD)
signature(user,"IsWindowVisible",[W.HWND],W.BOOL)
signature(user,"SetForegroundWindow",[W.HWND],W.BOOL)
signature(user,"BringWindowToTop",[W.HWND],W.BOOL)
signature(user,"AttachThreadInput",[W.DWORD,W.DWORD,W.BOOL],W.BOOL)
signature(user,"SetWindowPos",[W.HWND,W.HWND,C.c_int,C.c_int,C.c_int,C.c_int,W.UINT],W.BOOL)
signature(user,"GetForegroundWindow",[],W.HWND)
signature(user,"ShowWindow",[W.HWND,C.c_int],W.BOOL)
signature(user,"GetClientRect",[W.HWND,C.POINTER(W.RECT)],W.BOOL)
signature(user,"ClientToScreen",[W.HWND,C.POINTER(W.POINT)],W.BOOL)
signature(user,"PostMessageW",[W.HWND,W.UINT,W.WPARAM,W.LPARAM],W.BOOL)
signature(user,"MapVirtualKeyW",[W.UINT,W.UINT],W.UINT)
signature(user,"GetDC",[W.HWND],W.HDC)
signature(user,"ReleaseDC",[W.HWND,W.HDC],C.c_int)
signature(user,"SetProcessDpiAwarenessContext",[C.c_void_p],W.BOOL)
signature(kernel,"GetCurrentProcess",[],W.HANDLE)
signature(kernel,"GetCurrentThreadId",[],W.DWORD)
signature(kernel,"GetProcessAffinityMask",[W.HANDLE,C.POINTER(C.c_size_t),C.POINTER(C.c_size_t)],W.BOOL)
signature(kernel,"SetProcessAffinityMask",[W.HANDLE,C.c_size_t],W.BOOL)
signature(kernel,"GetProcessTimes",[W.HANDLE,*([C.POINTER(W.FILETIME)]*4)],W.BOOL)
signature(gdi,"CreateCompatibleDC",[W.HDC],W.HDC)
signature(gdi,"CreateCompatibleBitmap",[W.HDC,C.c_int,C.c_int],W.HBITMAP)
signature(gdi,"SelectObject",[W.HDC,W.HANDLE],W.HANDLE)
signature(gdi,"DeleteObject",[W.HANDLE],W.BOOL)
signature(gdi,"DeleteDC",[W.HDC],W.BOOL)
signature(gdi,"BitBlt",[W.HDC,C.c_int,C.c_int,C.c_int,C.c_int,W.HDC,C.c_int,C.c_int,W.DWORD],W.BOOL)
signature(gdi,"GetDIBits",[W.HDC,W.HBITMAP,W.UINT,W.UINT,C.c_void_p,C.c_void_p,W.UINT],C.c_int)
user.SetProcessDpiAwarenessContext(C.c_void_p(-4))


def available_cpus():
    current,system = C.c_size_t(),C.c_size_t()
    if not kernel.GetProcessAffinityMask(kernel.GetCurrentProcess(),C.byref(current),C.byref(system)):
        raise C.WinError(C.get_last_error())
    return {index for index in range(current.value.bit_length()) if current.value & (1<<index)}


def pin_process(process, cpu):
    if not kernel.SetProcessAffinityMask(int(process._handle),1<<cpu):
        raise C.WinError(C.get_last_error())


def cpu_ns(process):
    values = [W.FILETIME() for _ in range(4)]
    if not kernel.GetProcessTimes(int(process._handle),*(C.byref(value) for value in values)):
        raise C.WinError(C.get_last_error())
    return sum((value.dwHighDateTime<<32)|value.dwLowDateTime for value in values[2:])*100


class Window:
    def find(self, pid):
        found = []
        @callback_type
        def visit(window,_):
            owner = W.DWORD()
            user.GetWindowThreadProcessId(window,C.byref(owner))
            if owner.value==pid and user.IsWindowVisible(window):
                rect = W.RECT()
                if user.GetClientRect(window,C.byref(rect)) and rect.right>0 and rect.bottom>0:
                    found.append((rect.right*rect.bottom,window))
            return True
        user.EnumWindows(visit,0)
        return max(found,key=lambda item:item[0])[1] if found else None

    def focus(self, window):
        user.ShowWindow(window,9)
        user.SetWindowPos(window,None,0,0,0,0,0x0015)
        deadline = time.monotonic()+2
        while time.monotonic()<deadline:
            foreground = user.GetForegroundWindow()
            if foreground==window: return
            thread = user.GetWindowThreadProcessId(foreground,None) if foreground else 0
            current = kernel.GetCurrentThreadId()
            attached = bool(thread and thread!=current and user.AttachThreadInput(current,thread,True))
            try:
                user.BringWindowToTop(window)
                user.SetForegroundWindow(window)
            finally:
                if attached: user.AttachThreadInput(current,thread,False)
            time.sleep(0.05)
        raise RuntimeError("Benchmark window is not foreground")

    def key(self, window, name):
        # Target only the benchmark window, even if a user changes focus.
        code = {"F6":0x75,"F7":0x76,"F8":0x77,"F9":0x78,"Down":0x28,"Up":0x26}.get(name)
        if code is None: code = ord(name.upper())
        flags = 1 | (user.MapVirtualKeyW(code,0)<<16) | ((1<<24) if name in ("Down","Up") else 0)
        events = [(0x100,code,flags),(0x101,code,flags|0xC0000000)]
        for message,value,flags in events:
            if not user.PostMessageW(window,message,value,flags): raise C.WinError(C.get_last_error())

    def image(self, window):
        if user.GetForegroundWindow()!=window:
            raise RuntimeError("Benchmark window lost foreground during raster verification")
        rect,point = W.RECT(),W.POINT()
        if not user.GetClientRect(window,C.byref(rect)) or not user.ClientToScreen(window,C.byref(point)):
            raise C.WinError(C.get_last_error())
        width,height = rect.right,rect.bottom
        if width<=0 or height<=0: raise RuntimeError("Empty benchmark window")
        screen = user.GetDC(None)
        memory = gdi.CreateCompatibleDC(screen)
        bitmap = gdi.CreateCompatibleBitmap(screen,width,height)
        previous = gdi.SelectObject(memory,bitmap)
        try:
            if not gdi.BitBlt(memory,0,0,width,height,screen,point.x,point.y,0x00CC0020):
                raise C.WinError(C.get_last_error())
            gdi.SelectObject(memory,previous)
            # BITMAPINFOHEADER, top-down 32-bit RGB, followed by one unused color entry.
            import struct
            header = C.create_string_buffer(struct.pack("<IiiHHIIiiII",40,width,-height,1,32,0,0,0,0,0,0)+bytes(4))
            pixels = C.create_string_buffer(width*height*4)
            if gdi.GetDIBits(memory,bitmap,0,height,pixels,header,0)!=height:
                raise C.WinError(C.get_last_error())
            return hashlib.sha256(pixels.raw).hexdigest()
        finally:
            gdi.SelectObject(memory,previous)
            gdi.DeleteObject(bitmap); gdi.DeleteDC(memory); user.ReleaseDC(None,screen)

    def close(self):
        pass

    def minimize(self, window):
        user.ShowWindow(window,6)

    def restore(self, window):
        user.ShowWindow(window,9)
