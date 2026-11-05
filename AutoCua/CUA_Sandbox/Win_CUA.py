"""Second Windows: a second desktop for the agent, inside your own Windows session.

Shared with your main desktop: the same user, files, internet, installed apps
and clipboard. Its own: its windows, its focus and its picture. Nothing on it
moves your mouse or takes your keyboard, and nothing on it shows on your
screen except through the viewer window this script opens. The taskbar strip
is cropped out of every picture.

RAM: the desktop object itself is a few MB of desktop heap. Only the apps
started on it use memory, the same as opening them on your main desktop.

Standalone: Python 3 standard library and ctypes. --computer-use and --scan
also use comtypes, the UI Automation library already in the venv. No AutoCua
imports.

From D:\\AutoCua:
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\Win_CUA.py
        Make the second desktop, start Explorer (wallpaper, icons) and
        Character Map on it, and show it live in a window on your screen.
        Closing the window ends everything that runs on it.
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\Win_CUA.py --shot AutoCua\\CUA_Sandbox\\shot.png
        The same, but save one screenshot of it, print a report and exit.
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\Win_CUA.py --computer-use
        Computer use on it with no mouse and no keyboard: scan Character Map
        with UI Automation, type, click, scroll, scan again. Pictures go to
        AutoCua\\CUA_Sandbox\\cu_1_scan.png and AutoCua\\CUA_Sandbox\\cu_2_after.png.
    venv\\Scripts\\python.exe AutoCua\\CUA_Sandbox\\Win_CUA.py --run APP --scan scan.png
        Start APP on it and save the agent's scan: what UI Automation finds
        to click or type in, numbered on a picture, and print the list.

    --no-shell   no Explorer on it (no wallpaper or icons, least RAM)
    --run CMD    start CMD on it instead of Character Map (can repeat)
    --wait SEC   seconds the apps get to start before --shot or --scan (default 6)
"""
import argparse
import ctypes
import os
import struct
import sys
import threading
import time
import traceback
import zlib
from ctypes import wintypes as wt

DESKTOP = "AutoCuaAgent"   # the second desktop, inside window station WinSta0
FRAME_INTERVAL = 0.25      # seconds between two pictures for the viewer
HERE = os.path.dirname(os.path.abspath(__file__))

user32 = ctypes.WinDLL("user32", use_last_error=True)
gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
userenv = ctypes.WinDLL("userenv", use_last_error=True)
dwmapi = ctypes.WinDLL("dwmapi")

LRESULT = ctypes.c_ssize_t
WNDPROC = ctypes.WINFUNCTYPE(LRESULT, wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM)
WNDENUMPROC = ctypes.WINFUNCTYPE(wt.BOOL, wt.HWND, wt.LPARAM)

GENERIC_ALL = 0x10000000
CREATE_SUSPENDED = 0x00000004
CREATE_UNICODE_ENVIRONMENT = 0x00000400
TOKEN_DUPLICATE, TOKEN_QUERY = 0x0002, 0x0008
STARTF_FORCEOFFFEEDBACK = 0x00000080
STILL_ACTIVE = 259
JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000
JOB_PROCESS_ID_LIST = 3
JOB_EXTENDED_LIMITS = 9
PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
PROCESS_VM_READ = 0x0010
SM_CXSCREEN, SM_CYSCREEN = 0, 1
SPI_GETWORKAREA = 0x0030
GW_HWNDNEXT = 2
GWL_EXSTYLE = -20
WS_EX_LAYERED = 0x00080000
LWA_ALPHA = 0x2
DWMWA_EXTENDED_FRAME_BOUNDS = 9
DWMWA_CLOAKED = 14
PW_RENDERFULLCONTENT = 0x2
SRCCOPY = 0x00CC0020
DIB_RGB_COLORS = 0
HALFTONE = 4
BLACK_BRUSH, NULL_BRUSH = 4, 5
TRANSPARENT = 1
LABEL_COLOR = 0x00FF00FF   # magenta, as a COLORREF (0x00BBGGRR)
WS_OVERLAPPEDWINDOW = 0x00CF0000
WS_VISIBLE = 0x10000000
WM_DESTROY, WM_PAINT, WM_ERASEBKGND, WM_TIMER = 0x0002, 0x000F, 0x0014, 0x0113
WM_CHAR, WM_MOUSEMOVE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEWHEEL = 0x0102, 0x0200, 0x0201, 0x0202, 0x020A
EM_SETSEL = 0x00B1
MK_LBUTTON = 0x0001
CWP_SKIPINVISIBLE, CWP_SKIPTRANSPARENT = 0x0001, 0x0004
IDC_ARROW = 32512
DPI_PER_MONITOR_AWARE_V2 = -4


class BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [("biSize", wt.DWORD), ("biWidth", wt.LONG), ("biHeight", wt.LONG),
                ("biPlanes", wt.WORD), ("biBitCount", wt.WORD), ("biCompression", wt.DWORD),
                ("biSizeImage", wt.DWORD), ("biXPelsPerMeter", wt.LONG),
                ("biYPelsPerMeter", wt.LONG), ("biClrUsed", wt.DWORD), ("biClrImportant", wt.DWORD)]


class BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", BITMAPINFOHEADER), ("bmiColors", wt.DWORD * 3)]


class STARTUPINFOW(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("lpReserved", wt.LPWSTR), ("lpDesktop", wt.LPWSTR),
                ("lpTitle", wt.LPWSTR), ("dwX", wt.DWORD), ("dwY", wt.DWORD),
                ("dwXSize", wt.DWORD), ("dwYSize", wt.DWORD), ("dwXCountChars", wt.DWORD),
                ("dwYCountChars", wt.DWORD), ("dwFillAttribute", wt.DWORD), ("dwFlags", wt.DWORD),
                ("wShowWindow", wt.WORD), ("cbReserved2", wt.WORD), ("lpReserved2", ctypes.c_void_p),
                ("hStdInput", wt.HANDLE), ("hStdOutput", wt.HANDLE), ("hStdError", wt.HANDLE)]


class PROCESS_INFORMATION(ctypes.Structure):
    _fields_ = [("hProcess", wt.HANDLE), ("hThread", wt.HANDLE),
                ("dwProcessId", wt.DWORD), ("dwThreadId", wt.DWORD)]


class JOB_BASIC_LIMITS(ctypes.Structure):
    _fields_ = [("PerProcessUserTimeLimit", ctypes.c_longlong), ("PerJobUserTimeLimit", ctypes.c_longlong),
                ("LimitFlags", wt.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wt.DWORD),
                ("Affinity", ctypes.c_size_t), ("PriorityClass", wt.DWORD), ("SchedulingClass", wt.DWORD)]


class JOB_EXTENDED_LIMITS_INFO(ctypes.Structure):
    _fields_ = [("BasicLimitInformation", JOB_BASIC_LIMITS), ("IoInfo", ctypes.c_ulonglong * 6),
                ("ProcessMemoryLimit", ctypes.c_size_t), ("JobMemoryLimit", ctypes.c_size_t),
                ("PeakProcessMemoryUsed", ctypes.c_size_t), ("PeakJobMemoryUsed", ctypes.c_size_t)]


class JOB_PROCESS_IDS(ctypes.Structure):
    _fields_ = [("NumberOfAssignedProcesses", wt.DWORD), ("NumberOfProcessIdsInList", wt.DWORD),
                ("ProcessIdList", ctypes.c_size_t * 512)]


class PROCESS_MEMORY_COUNTERS_EX(ctypes.Structure):
    _fields_ = [("cb", wt.DWORD), ("PageFaultCount", wt.DWORD)] + [(name, ctypes.c_size_t) for name in (
        "PeakWorkingSetSize", "WorkingSetSize", "QuotaPeakPagedPoolUsage", "QuotaPagedPoolUsage",
        "QuotaPeakNonPagedPoolUsage", "QuotaNonPagedPoolUsage", "PagefileUsage",
        "PeakPagefileUsage", "PrivateUsage")]


class WNDCLASSEXW(ctypes.Structure):
    _fields_ = [("cbSize", wt.UINT), ("style", wt.UINT), ("lpfnWndProc", WNDPROC),
                ("cbClsExtra", ctypes.c_int), ("cbWndExtra", ctypes.c_int), ("hInstance", wt.HINSTANCE),
                ("hIcon", wt.HICON), ("hCursor", wt.HANDLE), ("hbrBackground", wt.HBRUSH),
                ("lpszMenuName", wt.LPCWSTR), ("lpszClassName", wt.LPCWSTR), ("hIconSm", wt.HICON)]


class PAINTSTRUCT(ctypes.Structure):
    _fields_ = [("hdc", wt.HDC), ("fErase", wt.BOOL), ("rcPaint", wt.RECT), ("fRestore", wt.BOOL),
                ("fIncUpdate", wt.BOOL), ("rgbReserved", ctypes.c_byte * 32)]


def _api(dll, name, restype, *argtypes):
    func = getattr(dll, name)
    func.restype, func.argtypes = restype, argtypes
    return func


P = ctypes.POINTER
VOIDP, INT = ctypes.c_void_p, ctypes.c_int

CreateDesktopW = _api(user32, "CreateDesktopW", wt.HANDLE, wt.LPCWSTR, wt.LPCWSTR, VOIDP, wt.DWORD, wt.DWORD, VOIDP)
OpenDesktopW = _api(user32, "OpenDesktopW", wt.HANDLE, wt.LPCWSTR, wt.DWORD, wt.BOOL, wt.DWORD)
CloseDesktop = _api(user32, "CloseDesktop", wt.BOOL, wt.HANDLE)
SetThreadDesktop = _api(user32, "SetThreadDesktop", wt.BOOL, wt.HANDLE)
GetTopWindow = _api(user32, "GetTopWindow", wt.HWND, wt.HWND)
GetWindow = _api(user32, "GetWindow", wt.HWND, wt.HWND, wt.UINT)
FindWindowW = _api(user32, "FindWindowW", wt.HWND, wt.LPCWSTR, wt.LPCWSTR)
FindWindowExW = _api(user32, "FindWindowExW", wt.HWND, wt.HWND, wt.HWND, wt.LPCWSTR, wt.LPCWSTR)
EnumWindows = _api(user32, "EnumWindows", wt.BOOL, WNDENUMPROC, wt.LPARAM)
EnumChildWindows = _api(user32, "EnumChildWindows", wt.BOOL, wt.HWND, WNDENUMPROC, wt.LPARAM)
IsWindowVisible = _api(user32, "IsWindowVisible", wt.BOOL, wt.HWND)
IsIconic = _api(user32, "IsIconic", wt.BOOL, wt.HWND)
GetWindowRect = _api(user32, "GetWindowRect", wt.BOOL, wt.HWND, P(wt.RECT))
GetWindowLongPtrW = _api(user32, "GetWindowLongPtrW", ctypes.c_ssize_t, wt.HWND, INT)
GetLayeredWindowAttributes = _api(user32, "GetLayeredWindowAttributes", wt.BOOL, wt.HWND, VOIDP,
                                  P(ctypes.c_ubyte), P(wt.DWORD))
GetWindowThreadProcessId = _api(user32, "GetWindowThreadProcessId", wt.DWORD, wt.HWND, P(wt.DWORD))
GetClassNameW = _api(user32, "GetClassNameW", INT, wt.HWND, wt.LPWSTR, INT)
GetWindowTextW = _api(user32, "GetWindowTextW", INT, wt.HWND, wt.LPWSTR, INT)
GetForegroundWindow = _api(user32, "GetForegroundWindow", wt.HWND)
GetCursorPos = _api(user32, "GetCursorPos", wt.BOOL, P(wt.POINT))
WindowFromPoint = _api(user32, "WindowFromPoint", wt.HWND, wt.POINT)
ChildWindowFromPointEx = _api(user32, "ChildWindowFromPointEx", wt.HWND, wt.HWND, wt.POINT, wt.UINT)
ScreenToClient = _api(user32, "ScreenToClient", wt.BOOL, wt.HWND, P(wt.POINT))
ClientToScreen = _api(user32, "ClientToScreen", wt.BOOL, wt.HWND, P(wt.POINT))
PostMessageW = _api(user32, "PostMessageW", wt.BOOL, wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM)
PrintWindow = _api(user32, "PrintWindow", wt.BOOL, wt.HWND, wt.HDC, wt.UINT)
PaintDesktop = _api(user32, "PaintDesktop", wt.BOOL, wt.HDC)
GetDC = _api(user32, "GetDC", wt.HDC, wt.HWND)
ReleaseDC = _api(user32, "ReleaseDC", INT, wt.HWND, wt.HDC)
GetSystemMetrics = _api(user32, "GetSystemMetrics", INT, INT)
SystemParametersInfoW = _api(user32, "SystemParametersInfoW", wt.BOOL, wt.UINT, wt.UINT, VOIDP, wt.UINT)
SetProcessDpiAwarenessContext = _api(user32, "SetProcessDpiAwarenessContext", wt.BOOL, ctypes.c_ssize_t)
RegisterClassExW = _api(user32, "RegisterClassExW", wt.ATOM, P(WNDCLASSEXW))
CreateWindowExW = _api(user32, "CreateWindowExW", wt.HWND, wt.DWORD, wt.LPCWSTR, wt.LPCWSTR, wt.DWORD,
                       INT, INT, INT, INT, wt.HWND, wt.HMENU, wt.HINSTANCE, VOIDP)
DefWindowProcW = _api(user32, "DefWindowProcW", LRESULT, wt.HWND, wt.UINT, wt.WPARAM, wt.LPARAM)
GetMessageW = _api(user32, "GetMessageW", wt.BOOL, P(wt.MSG), wt.HWND, wt.UINT, wt.UINT)
TranslateMessage = _api(user32, "TranslateMessage", wt.BOOL, P(wt.MSG))
DispatchMessageW = _api(user32, "DispatchMessageW", LRESULT, P(wt.MSG))
PostQuitMessage = _api(user32, "PostQuitMessage", None, INT)
BeginPaint = _api(user32, "BeginPaint", wt.HDC, wt.HWND, P(PAINTSTRUCT))
EndPaint = _api(user32, "EndPaint", wt.BOOL, wt.HWND, P(PAINTSTRUCT))
GetClientRect = _api(user32, "GetClientRect", wt.BOOL, wt.HWND, P(wt.RECT))
InvalidateRect = _api(user32, "InvalidateRect", wt.BOOL, wt.HWND, VOIDP, wt.BOOL)
SetTimer = _api(user32, "SetTimer", ctypes.c_size_t, wt.HWND, ctypes.c_size_t, wt.UINT, VOIDP)
LoadCursorW = _api(user32, "LoadCursorW", wt.HANDLE, wt.HINSTANCE, VOIDP)
SetWindowTextW = _api(user32, "SetWindowTextW", wt.BOOL, wt.HWND, wt.LPCWSTR)
FillRect = _api(user32, "FillRect", INT, wt.HDC, P(wt.RECT), wt.HBRUSH)

CreateCompatibleDC = _api(gdi32, "CreateCompatibleDC", wt.HDC, wt.HDC)
CreateCompatibleBitmap = _api(gdi32, "CreateCompatibleBitmap", wt.HBITMAP, wt.HDC, INT, INT)
CreateDIBSection = _api(gdi32, "CreateDIBSection", wt.HBITMAP, wt.HDC, P(BITMAPINFO), wt.UINT,
                        P(VOIDP), wt.HANDLE, wt.DWORD)
SelectObject = _api(gdi32, "SelectObject", wt.HGDIOBJ, wt.HDC, wt.HGDIOBJ)
DeleteObject = _api(gdi32, "DeleteObject", wt.BOOL, wt.HGDIOBJ)
DeleteDC = _api(gdi32, "DeleteDC", wt.BOOL, wt.HDC)
BitBlt = _api(gdi32, "BitBlt", wt.BOOL, wt.HDC, INT, INT, INT, INT, wt.HDC, INT, INT, wt.DWORD)
StretchDIBits = _api(gdi32, "StretchDIBits", INT, wt.HDC, INT, INT, INT, INT, INT, INT, INT, INT,
                     ctypes.c_char_p, P(BITMAPINFO), wt.UINT, wt.DWORD)
SetStretchBltMode = _api(gdi32, "SetStretchBltMode", INT, wt.HDC, INT)
SetBrushOrgEx = _api(gdi32, "SetBrushOrgEx", wt.BOOL, wt.HDC, INT, INT, VOIDP)
SetWindowOrgEx = _api(gdi32, "SetWindowOrgEx", wt.BOOL, wt.HDC, INT, INT, VOIDP)
GetStockObject = _api(gdi32, "GetStockObject", wt.HGDIOBJ, INT)
GdiFlush = _api(gdi32, "GdiFlush", wt.BOOL)
CreatePen = _api(gdi32, "CreatePen", wt.HGDIOBJ, INT, INT, wt.DWORD)
CreateFontW = _api(gdi32, "CreateFontW", wt.HGDIOBJ, INT, INT, INT, INT, INT, wt.DWORD, wt.DWORD, wt.DWORD,
                   wt.DWORD, wt.DWORD, wt.DWORD, wt.DWORD, wt.DWORD, wt.LPCWSTR)
Rectangle = _api(gdi32, "Rectangle", wt.BOOL, wt.HDC, INT, INT, INT, INT)
SetBkMode = _api(gdi32, "SetBkMode", INT, wt.HDC, INT)
SetTextColor = _api(gdi32, "SetTextColor", wt.DWORD, wt.HDC, wt.DWORD)
TextOutW = _api(gdi32, "TextOutW", wt.BOOL, wt.HDC, INT, INT, wt.LPCWSTR, INT)

CreateProcessW = _api(kernel32, "CreateProcessW", wt.BOOL, wt.LPCWSTR, wt.LPWSTR, VOIDP, VOIDP, wt.BOOL,
                      wt.DWORD, VOIDP, wt.LPCWSTR, P(STARTUPINFOW), P(PROCESS_INFORMATION))
ResumeThread = _api(kernel32, "ResumeThread", wt.DWORD, wt.HANDLE)
CloseHandle = _api(kernel32, "CloseHandle", wt.BOOL, wt.HANDLE)
CreateJobObjectW = _api(kernel32, "CreateJobObjectW", wt.HANDLE, VOIDP, wt.LPCWSTR)
SetInformationJobObject = _api(kernel32, "SetInformationJobObject", wt.BOOL, wt.HANDLE, INT, VOIDP, wt.DWORD)
QueryInformationJobObject = _api(kernel32, "QueryInformationJobObject", wt.BOOL, wt.HANDLE, INT, VOIDP,
                                 wt.DWORD, P(wt.DWORD))
AssignProcessToJobObject = _api(kernel32, "AssignProcessToJobObject", wt.BOOL, wt.HANDLE, wt.HANDLE)
TerminateJobObject = _api(kernel32, "TerminateJobObject", wt.BOOL, wt.HANDLE, wt.UINT)
OpenProcess = _api(kernel32, "OpenProcess", wt.HANDLE, wt.DWORD, wt.BOOL, wt.DWORD)
GetExitCodeProcess = _api(kernel32, "GetExitCodeProcess", wt.BOOL, wt.HANDLE, P(wt.DWORD))
QueryFullProcessImageNameW = _api(kernel32, "QueryFullProcessImageNameW", wt.BOOL, wt.HANDLE, wt.DWORD,
                                  wt.LPWSTR, P(wt.DWORD))
K32GetProcessMemoryInfo = _api(kernel32, "K32GetProcessMemoryInfo", wt.BOOL, wt.HANDLE, VOIDP, wt.DWORD)
GetModuleHandleW = _api(kernel32, "GetModuleHandleW", wt.HMODULE, wt.LPCWSTR)
GetCurrentProcess = _api(kernel32, "GetCurrentProcess", wt.HANDLE)
OpenProcessToken = _api(advapi32, "OpenProcessToken", wt.BOOL, wt.HANDLE, wt.DWORD, P(wt.HANDLE))
CreateEnvironmentBlock = _api(userenv, "CreateEnvironmentBlock", wt.BOOL, P(VOIDP), wt.HANDLE, wt.BOOL)
DestroyEnvironmentBlock = _api(userenv, "DestroyEnvironmentBlock", wt.BOOL, VOIDP)

DwmGetWindowAttribute = _api(dwmapi, "DwmGetWindowAttribute", ctypes.c_long, wt.HWND, wt.DWORD, VOIDP, wt.DWORD)


# ---------------------------------------------------------------- the desktop

def open_desktop(name):
    """The second desktop, made on first use. A desktop is a Windows object
    of its own: its own windows and focus, on the same session and user."""
    desk = OpenDesktopW(name, 0, False, GENERIC_ALL) or CreateDesktopW(name, None, None, 0, GENERIC_ALL, None)
    if not desk:
        raise ctypes.WinError(ctypes.get_last_error())
    return desk


def kill_on_close_job():
    """A job that ends every process in it when this script ends, however it
    ends (window closed, Ctrl+C, a crash): nothing keeps running unseen."""
    job = CreateJobObjectW(None, None)
    info = JOB_EXTENDED_LIMITS_INFO()
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    if not job or not SetInformationJobObject(job, JOB_EXTENDED_LIMITS, ctypes.byref(info), ctypes.sizeof(info)):
        raise ctypes.WinError(ctypes.get_last_error())
    return job


def fresh_environment():
    """The environment Explorer gives a program, not this script's own. A
    script started from an editor carries the editor's settings, and one of
    Cursor's (ELECTRON_RUN_AS_NODE) turns any Electron app into plain Node."""
    token, block = wt.HANDLE(), VOIDP()
    if not OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_DUPLICATE, ctypes.byref(token)):
        return None
    ok = CreateEnvironmentBlock(ctypes.byref(block), token, False)
    CloseHandle(token)
    return block if ok else None


def start_on_desktop(command, desktop, job):
    """Start `command` with its windows on `desktop`. It starts suspended and
    joins the job first, so whatever it starts in turn is in the job too."""
    si = STARTUPINFOW()
    si.cb = ctypes.sizeof(si)
    si.lpDesktop = f"WinSta0\\{desktop}"
    si.dwFlags = STARTF_FORCEOFFFEEDBACK   # no busy cursor on your screen
    pi = PROCESS_INFORMATION()
    line = ctypes.create_unicode_buffer(command)
    env = fresh_environment()
    flags = CREATE_SUSPENDED | (CREATE_UNICODE_ENVIRONMENT if env else 0)
    ok = CreateProcessW(None, line, None, None, False, flags, env, None, ctypes.byref(si), ctypes.byref(pi))
    error = ctypes.get_last_error()
    if env:
        DestroyEnvironmentBlock(env)
    if not ok:
        raise ctypes.WinError(error)
    AssignProcessToJobObject(job, pi.hProcess)
    ResumeThread(pi.hThread)
    CloseHandle(pi.hThread)
    CloseHandle(pi.hProcess)
    return pi.dwProcessId


def process_state(pid):
    handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return "exited"
    code = wt.DWORD()
    ok = GetExitCodeProcess(handle, ctypes.byref(code))
    CloseHandle(handle)
    return "still running" if ok and code.value == STILL_ACTIVE else f"exited (code {code.value})"


def exe_name(command):
    first = command[1:].split('"', 1)[0] if command.startswith('"') else command.split()[0]
    return os.path.basename(first)


# ---------------------------------------------------------------- windows

TASKBARS = ("Shell_TrayWnd", "Shell_SecondaryTrayWnd")
TASKBAR_CONTENT = "Windows.UI.Composition.DesktopWindowContentBridge"


def class_of(hwnd):
    cls = ctypes.create_unicode_buffer(256)
    GetClassNameW(hwnd, cls, 256)
    return cls.value


def title_of(hwnd):
    title = ctypes.create_unicode_buffer(256)
    GetWindowTextW(hwnd, title, 256)
    return title.value


def is_shown(hwnd):
    """On show: visible, not minimized, not hidden by DWM, not fully clear."""
    if not IsWindowVisible(hwnd) or IsIconic(hwnd):
        return False
    # The Windows 11 taskbar draws with XAML, which only runs on the main
    # desktop: on the second desktop its window is there but empty (black).
    if class_of(hwnd) in TASKBARS and not FindWindowExW(hwnd, None, TASKBAR_CONTENT, None):
        return False
    cloaked = wt.DWORD()
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, ctypes.byref(cloaked), 4) == 0 and cloaked.value:
        return False
    if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_LAYERED:
        alpha, flags = ctypes.c_ubyte(), wt.DWORD()
        if (GetLayeredWindowAttributes(hwnd, None, ctypes.byref(alpha), ctypes.byref(flags))
                and flags.value & LWA_ALPHA and alpha.value == 0):
            return False
    r = wt.RECT()
    return bool(GetWindowRect(hwnd, ctypes.byref(r))) and r.right > r.left and r.bottom > r.top


def window_info(hwnd):
    pid = wt.DWORD()
    GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
    return pid.value, class_of(hwnd), title_of(hwnd)


def process_name(pid):
    handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, False, pid)
    if not handle:
        return f"pid {pid}"
    buf, size = ctypes.create_unicode_buffer(1024), wt.DWORD(1024)
    ok = QueryFullProcessImageNameW(handle, 0, buf, ctypes.byref(size))
    CloseHandle(handle)
    return os.path.basename(buf.value) if ok else f"pid {pid}"


def main_desktop_windows():
    """The windows on show on your main desktop (this thread's desktop)."""
    found = {}

    @WNDENUMPROC
    def collect(hwnd, _):
        if is_shown(hwnd):
            found[hwnd] = window_info(hwnd)
        return True

    EnumWindows(collect, 0)
    return found


def child_windows(hwnd):
    found = []

    @WNDENUMPROC
    def collect(child, _):
        found.append(child)
        return True

    EnumChildWindows(hwnd, collect, 0)
    return found


def job_memory(job):
    """(name, private MB) for every process running in the job."""
    ids = JOB_PROCESS_IDS()
    if not QueryInformationJobObject(job, JOB_PROCESS_ID_LIST, ctypes.byref(ids), ctypes.sizeof(ids), None):
        return []
    rows = []
    for pid in ids.ProcessIdList[:ids.NumberOfProcessIdsInList]:
        handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, False, pid)
        if not handle:
            continue
        mem = PROCESS_MEMORY_COUNTERS_EX()
        mem.cb = ctypes.sizeof(mem)
        if K32GetProcessMemoryInfo(handle, ctypes.byref(mem), mem.cb):
            rows.append((process_name(pid), mem.PrivateUsage / 2 ** 20))
        CloseHandle(handle)
    return rows


# ---------------------------------------------------------------- the picture

def bitmap_info(width, height):
    bmi = BITMAPINFO()
    h = bmi.bmiHeader
    h.biSize, h.biWidth, h.biHeight = ctypes.sizeof(BITMAPINFOHEADER), width, -height   # top-down rows
    h.biPlanes, h.biBitCount = 1, 32
    return bmi


def visible_windows():
    """Top-level windows of this thread's desktop on show, top of the Z order first."""
    found, hwnd = [], GetTopWindow(None)
    for _ in range(5000):
        if not hwnd:
            break
        if is_shown(hwnd):
            found.append(hwnd)
        hwnd = GetWindow(hwnd, GW_HWNDNEXT)
    return found


def paint_window(canvas, screen, hwnd):
    """Ask Windows to paint one window into `canvas` at its place on screen.
    PrintWindow works for a window nobody can see, which is every window here."""
    r = wt.RECT()
    if not GetWindowRect(hwnd, ctypes.byref(r)):
        return
    w, h = r.right - r.left, r.bottom - r.top
    if not (0 < w <= 16384 and 0 < h <= 16384):
        return
    f = wt.RECT()   # the visible frame, without the invisible resize borders
    if DwmGetWindowAttribute(hwnd, DWMWA_EXTENDED_FRAME_BOUNDS, ctypes.byref(f), ctypes.sizeof(f)) != 0:
        f = r
    dc = CreateCompatibleDC(screen)
    bmp = CreateCompatibleBitmap(screen, w, h)
    old = SelectObject(dc, bmp)
    if PrintWindow(hwnd, dc, PW_RENDERFULLCONTENT) or PrintWindow(hwnd, dc, 0):
        BitBlt(canvas, f.left, f.top, f.right - f.left, f.bottom - f.top,
               dc, f.left - r.left, f.top - r.top, SRCCOPY)
    SelectObject(dc, old)
    DeleteObject(bmp)
    DeleteDC(dc)


def draw_boxes(canvas, boxes):
    """Numbered magenta boxes, the way the agent's screenshot marks elements."""
    pen = CreatePen(0, 2, LABEL_COLOR)
    font = CreateFontW(-22, 0, 0, 0, 700, 0, 0, 0, 1, 0, 0, 4, 0, "Arial")
    old_pen = SelectObject(canvas, pen)
    old_brush = SelectObject(canvas, GetStockObject(NULL_BRUSH))
    old_font = SelectObject(canvas, font)
    SetBkMode(canvas, TRANSPARENT)
    SetTextColor(canvas, LABEL_COLOR)
    for index, (left, top, right, bottom) in boxes:
        Rectangle(canvas, left, top, right, bottom)
        label = f"[{index}]"
        TextOutW(canvas, left + 3, top + 2, label, len(label))
    SelectObject(canvas, old_font)
    SelectObject(canvas, old_brush)
    SelectObject(canvas, old_pen)
    DeleteObject(pen)
    DeleteObject(font)


def capture(x0, y0, width, height, boxes=()):
    """One picture of this thread's desktop, from (x0, y0) and width x height
    of it: the wallpaper, then every window on show from the bottom up, then
    `boxes` as (number, (left, top, right, bottom)). Returns (top-down BGRA
    bytes, windows)."""
    screen = GetDC(None)
    canvas = CreateCompatibleDC(screen)
    bits = VOIDP()
    dib = CreateDIBSection(screen, ctypes.byref(bitmap_info(width, height)), DIB_RGB_COLORS,
                           ctypes.byref(bits), None, 0)
    old = SelectObject(canvas, dib)
    SetWindowOrgEx(canvas, x0, y0, None)   # draw in screen coordinates
    PaintDesktop(canvas)
    windows = visible_windows()
    for hwnd in reversed(windows):
        paint_window(canvas, screen, hwnd)
    if boxes:
        draw_boxes(canvas, boxes)
    GdiFlush()
    pixels = ctypes.string_at(bits, width * height * 4)
    SelectObject(canvas, old)
    DeleteObject(dib)
    DeleteDC(canvas)
    ReleaseDC(None, screen)
    return pixels, windows


class Frames:
    """The latest picture of the second desktop, shared with the viewer."""

    def __init__(self, x0, y0, width, height):
        self.x0, self.y0, self.width, self.height = x0, y0, width, height
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.pixels, self.windows, self.count, self.error = None, [], 0, None


def capture_loop(desk, frames):
    """Runs on a thread of its own that moves onto the second desktop: window
    messages, and so PrintWindow, only reach windows on the caller's desktop."""
    if not SetThreadDesktop(desk):
        frames.error = f"could not join the second desktop (error {ctypes.get_last_error()})"
        return
    while not frames.stop.is_set():
        try:
            pixels, windows = capture(frames.x0, frames.y0, frames.width, frames.height)
            infos = [window_info(hwnd) for hwnd in windows]
        except Exception as e:
            frames.error = repr(e)
            return
        with frames.lock:
            frames.pixels, frames.windows = pixels, infos
            frames.count += 1
        frames.stop.wait(FRAME_INTERVAL)


def write_png(path, width, height, bgra):
    """Top-down BGRA pixels to a PNG file, with the standard library's zlib."""
    rgb = bytearray(width * height * 3)
    rgb[0::3], rgb[1::3], rgb[2::3] = bgra[2::4], bgra[1::4], bgra[0::4]
    row = width * 3
    raw = b"".join(b"\x00" + rgb[y * row:(y + 1) * row] for y in range(height))

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n"
                + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


# ---------------------------------------------------------------- input without mouse or keyboard

def lparam_point(x, y):
    return ((y & 0xFFFF) << 16) | (x & 0xFFFF)


def deepest_window_at(x, y):
    """The innermost window under a screen point of this thread's desktop."""
    hwnd = WindowFromPoint(wt.POINT(x, y))
    while hwnd:
        local = wt.POINT(x, y)
        ScreenToClient(hwnd, ctypes.byref(local))
        child = ChildWindowFromPointEx(hwnd, local, CWP_SKIPINVISIBLE | CWP_SKIPTRANSPARENT)
        if not child or child == hwnd:
            break
        hwnd = child
    return hwnd


def click_at(x, y):
    """A left click at a screen point, sent as messages to the window under
    it. For things UI Automation cannot press. The real cursor never moves."""
    hwnd = deepest_window_at(x, y)
    if hwnd:
        local = wt.POINT(x, y)
        ScreenToClient(hwnd, ctypes.byref(local))
        lp = lparam_point(local.x, local.y)
        PostMessageW(hwnd, WM_MOUSEMOVE, 0, lp)
        PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON, lp)
        PostMessageW(hwnd, WM_LBUTTONUP, 0, lp)
    return hwnd


def type_text(hwnd, text):
    """Type into a window as character messages. The real keyboard is never used."""
    for ch in text:
        PostMessageW(hwnd, WM_CHAR, ord(ch), 1)


def scroll_at(x, y, notches):
    """Mouse-wheel scroll at a screen point, as a message (down when notches > 0)."""
    hwnd = deepest_window_at(x, y)
    if hwnd:
        PostMessageW(hwnd, WM_MOUSEWHEEL, ((-120 * notches) & 0xFFFF) << 16, lparam_point(x, y))
    return hwnd


# ---------------------------------------------------------------- computer use

CUIAUTOMATION = "{ff48dba4-60ef-4201-aa87-54103eef594e}"
UIA_INVOKE, UIA_VALUE, UIA_TOGGLE = 10000, 10002, 10015
CONTROL_TYPES = {50000: "Button", 50002: "CheckBox", 50003: "ComboBox", 50004: "Edit", 50005: "Hyperlink",
                 50006: "Image", 50007: "ListItem", 50008: "List", 50009: "Menu", 50010: "MenuBar",
                 50011: "MenuItem", 50013: "RadioButton", 50014: "ScrollBar", 50015: "Slider", 50018: "Tab",
                 50019: "TabItem", 50020: "Text", 50021: "ToolBar", 50023: "Tree", 50024: "TreeItem",
                 50025: "Custom", 50026: "Group", 50027: "Thumb", 50028: "DataGrid", 50029: "DataItem",
                 50030: "Document", 50031: "SplitButton", 50032: "Window", 50033: "Pane", 50037: "TitleBar"}
UIA_RECT, UIA_TYPE, UIA_NAME, UIA_FOCUSABLE, UIA_OFFSCREEN = 30001, 30003, 30005, 30009, 30022
CLICKABLE = {50000, 50002, 50003, 50004, 50005, 50007, 50011, 50013, 50015, 50019, 50024, 50029, 50031}
WHOLE_AREAS = {50030, 50032, 50033}   # Document, Window, Pane: not things to click


def uia_client():
    """UI Automation for the calling thread, which is already on the second desktop."""
    sys.coinit_flags = 0   # COM for this thread only, multithreaded: comtypes reads it at import
    import comtypes.client
    lib = comtypes.client.GetModule("UIAutomationCore.dll")
    return comtypes.client.CreateObject(CUIAUTOMATION, interface=lib.IUIAutomation), lib


def changed(before, after, width, rect, x0, y0):
    """Share of pixels that differ inside `rect` between two pictures."""
    left, top, right, bottom = rect[0] - x0, rect[1] - y0, rect[2] - x0, rect[3] - y0
    diff = total = 0
    for y in range(top, bottom, 4):
        a = before[(y * width + left) * 4:(y * width + right) * 4]
        b = after[(y * width + left) * 4:(y * width + right) * 4]
        total += len(a)
        diff += sum(1 for i in range(0, len(a), 16) if a[i:i + 3] != b[i:i + 3]) * 16
    return diff / max(1, total)


def computer_use(desk, frames):
    """The agent's loop on the second desktop, with no mouse and no keyboard:
    scan with UI Automation, act on the elements directly, scan again."""
    if not SetThreadDesktop(desk):
        print("could not join the second desktop, error", ctypes.get_last_error())
        return
    uia, uia_lib = uia_client()
    every = uia.CreateTrueCondition()

    def pattern(element, pattern_id, interface):
        found = element.GetCurrentPattern(pattern_id)
        return found.QueryInterface(interface) if found else None

    tops = uia.GetRootElement().FindAll(uia_lib.TreeScope_Children, every)
    names = [tops.GetElement(i).CurrentName for i in range(tops.Length)]
    print(f"\n1. UI Automation, from the second desktop, sees {len(names)} windows: "
          + ", ".join(repr(n) for n in names))

    hwnd = None
    for _ in range(100):
        hwnd = FindWindowW(None, "Character Map")
        if hwnd:
            break
        time.sleep(0.1)
    if not hwnd:
        print("Character Map did not open on the second desktop")
        return
    time.sleep(1.0)
    window = uia.ElementFromHandle(hwnd)

    def scan(png):
        found = window.FindAll(uia_lib.TreeScope_Descendants, every)
        items = []
        for i in range(found.Length):
            e = found.GetElement(i)
            r = e.CurrentBoundingRectangle
            if r.right > r.left and r.bottom > r.top and not e.CurrentIsOffscreen:
                items.append((e, CONTROL_TYPES.get(e.CurrentControlType, "Other"), e.CurrentName or "",
                              (r.left, r.top, r.right, r.bottom)))
        pixels, _ = capture(frames.x0, frames.y0, frames.width, frames.height,
                            [(n, item[3]) for n, item in enumerate(items, 1)])
        path = os.path.join(HERE, png)
        write_png(path, frames.width, frames.height, pixels)
        return items, path

    items, path = scan("cu_1_scan.png")
    print(f"2. Scan: {len(items)} elements, numbered the way the agent sees them: {path}")
    for n, (_, kind, name, _) in enumerate(items, 1):
        print(f"   [{n}] {kind} {name!r}")

    def pick(kind, words=""):
        for element, k, name, rect in items:
            if k == kind and words.lower() in name.lower():
                return element, rect
        raise LookupError(f"no {kind} {words!r} in the scan")

    print("3. Act, with no mouse and no keyboard:")
    edit, _ = pick("Edit")
    value = pattern(edit, UIA_VALUE, uia_lib.IUIAutomationValuePattern)
    edit_hwnd = edit.CurrentNativeWindowHandle

    type_text(edit_hwnd, "Hi")
    time.sleep(0.4)
    print(f"   typewrite 'Hi' (character messages)          -> field: {value.CurrentValue!r}")

    text = "Hello from the second desktop "
    value.SetValue(text)
    time.sleep(0.3)
    print(f"   input (UI Automation sets the field)         -> field: {value.CurrentValue!r}")

    grid = next((c for c in child_windows(hwnd) if "grid" in class_of(c).lower()), None)
    if grid:
        r, origin = wt.RECT(), wt.POINT(0, 0)
        GetClientRect(grid, ctypes.byref(r))
        ClientToScreen(grid, ctypes.byref(origin))
        cell_w, cell_h = r.right / 20, r.bottom / 10
        x, y = origin.x + int(12.5 * cell_w), origin.y + int(1.5 * cell_h)   # the "A" cell
        target = click_at(x, y)
        time.sleep(0.4)
        PostMessageW(edit_hwnd, EM_SETSEL, len(text), len(text))   # the caret to the end
        select, _ = pick("Button", "Select")
        pattern(select, UIA_INVOKE, uia_lib.IUIAutomationInvokePattern).Invoke()
        time.sleep(0.4)
        print(f"   left_click at a point, cell 'A' (messages)   -> went to {class_of(target)!r}")
        print(f"   left_click 'Select' (UI Automation Invoke)   -> field: {value.CurrentValue!r}")

        grid_rect = (origin.x, origin.y, origin.x + r.right, origin.y + r.bottom)
        before, _ = capture(frames.x0, frames.y0, frames.width, frames.height)
        scroll_at(x, y, 3)
        time.sleep(0.6)
        after, _ = capture(frames.x0, frames.y0, frames.width, frames.height)
        share = changed(before, after, frames.width, grid_rect, frames.x0, frames.y0)
        print(f"   scroll down 3 (wheel messages)               -> {share:.0%} of the grid changed")
        if share < 0.01:   # this grid ignores the wheel: press its scroll bar's button instead
            down, _ = pick("Button", "Line down")
            press = pattern(down, UIA_INVOKE, uia_lib.IUIAutomationInvokePattern)
            for _ in range(3):
                press.Invoke()
                time.sleep(0.1)
            time.sleep(0.5)
            after, _ = capture(frames.x0, frames.y0, frames.width, frames.height)
            share = changed(before, after, frames.width, grid_rect, frames.x0, frames.y0)
            print(f"   scroll down 3 ('Line down', UI Automation)   -> {share:.0%} of the grid changed")
    else:
        print("   (no character grid found, point click and scroll skipped)")

    box, _ = pick("CheckBox", "Advanced")
    toggle = pattern(box, UIA_TOGGLE, uia_lib.IUIAutomationTogglePattern)
    toggle.Toggle()
    time.sleep(0.8)
    print(f"   left_click 'Advanced view' (UI Automation)   -> {'on' if toggle.CurrentToggleState else 'off'}")

    items, path = scan("cu_2_after.png")
    print(f"4. Scan again: {len(items)} elements: {path}")


def scan_desktop(desk, frames, png):
    """The agent's scan of the second desktop: what UI Automation finds to
    click or type in, in every window on show, numbered on a picture of it.
    Returns the windows on show as (pid, class, title), top first."""
    if not SetThreadDesktop(desk):
        raise ctypes.WinError(ctypes.get_last_error())
    uia, lib = uia_client()
    every = uia.CreateTrueCondition()
    cache = uia.CreateCacheRequest()   # these come back with the elements, in one call per window
    for prop in (UIA_RECT, UIA_TYPE, UIA_NAME, UIA_FOCUSABLE, UIA_OFFSCREEN):
        cache.AddProperty(prop)
    windows = visible_windows()

    def read():
        items, above = [], []
        for hwnd in windows:   # top first: what a window covers is dropped from the ones below
            w = wt.RECT()
            GetWindowRect(hwnd, ctypes.byref(w))
            try:
                found = uia.ElementFromHandle(hwnd).FindAllBuildCache(lib.TreeScope_Descendants, every, cache)
            except Exception as e:
                print(f"  could not read {class_of(hwnd)!r}: {e!r}")
                found = None
            for i in range(found.Length if found else 0):
                e = found.GetElement(i)
                kind = e.CachedControlType
                if e.CachedIsOffscreen or kind in WHOLE_AREAS:
                    continue
                if kind not in CLICKABLE and not e.CachedIsKeyboardFocusable:
                    continue
                r = e.CachedBoundingRectangle
                box = (max(r.left, w.left), max(r.top, w.top), min(r.right, w.right), min(r.bottom, w.bottom))
                cx, cy = (box[0] + box[2]) // 2, (box[1] + box[3]) // 2
                if box[2] - box[0] < 4 or box[3] - box[1] < 4 or any(
                        a.left <= cx < a.right and a.top <= cy < a.bottom for a in above):
                    continue
                items.append((CONTROL_TYPES.get(kind, "Other"), e.CachedName or "", box))
            above.append(w)
        one_per_place = {}
        for item in items:
            one_per_place.setdefault(item[2], item)
        return list(one_per_place.values())

    # Chromium (so WebView2 and Electron) builds its element tree only when
    # someone first asks: read until two reads in a row agree.
    items = read()
    for _ in range(8):
        time.sleep(1.5)
        start = time.monotonic()
        again = read()
        took = time.monotonic() - start
        if len(again) == len(items):
            break
        items = again
    items = again
    pixels, _ = capture(frames.x0, frames.y0, frames.width, frames.height,
                        [(n, box) for n, (_, _, box) in enumerate(items, 1)])
    write_png(png, frames.width, frames.height, pixels)
    print(f"\nScan of the second desktop: {len(items)} elements, read in {took:.2f} s, "
          f"numbered on {os.path.abspath(png)}")
    for n, (kind, name, _) in enumerate(items, 1):
        print(f"  [{n}] {kind} {name[:50]!r}")
    return [window_info(hwnd) for hwnd in windows]


# ---------------------------------------------------------------- shot and viewer

def take_shot(path, wait, frames, job, before, started):
    time.sleep(wait)
    seen, deadline = frames.count, time.monotonic() + 15
    while frames.count == seen and frames.error is None and time.monotonic() < deadline:
        time.sleep(0.05)   # a picture taken after the wait
    if frames.error or frames.pixels is None:
        raise SystemExit(f"no picture: {frames.error or 'timed out'}")
    with frames.lock:
        pixels, windows = frames.pixels, frames.windows
    write_png(path, frames.width, frames.height, pixels)
    print(f"\nScreenshot of the second desktop: {os.path.abspath(path)} ({frames.width}x{frames.height})")
    report(windows, job, before, started)


def report(windows, job, before, started):
    """What runs on the second desktop, its RAM, and whether your desktop changed."""
    print("Programs started on it:")
    for command, pid in started:
        print(f"  {exe_name(command):<24} {process_state(pid)}")
    print("Windows on it, top first:")
    for pid, cls, title in windows:
        print(f"  {process_name(pid):<24} {cls:<34} {title}")
    if not windows:
        print("  (none)")
    rows = job_memory(job)
    print(f"RAM of the apps on it: {sum(mb for _, mb in rows):.0f} MB private")
    for name, mb in rows:
        print(f"  {name:<24} {mb:7.1f} MB")
    report_main_desktop(before)


def report_main_desktop(before):
    windows, fg_before, fg_title, cursor = before
    now, point = main_desktop_windows(), wt.POINT()
    GetCursorPos(ctypes.byref(point))
    new = {h: v for h, v in now.items() if h not in windows}
    if new:
        print("New windows on your main desktop while it ran:")
        for pid, cls, title in new.values():
            print(f"  {process_name(pid):<24} {cls:<34} {title}")
    else:
        print("Your main desktop: no new windows.")
    fg = GetForegroundWindow()
    front = "unchanged" if fg == fg_before else f"changed from {fg_title!r} to {title_of(fg)!r}"
    print(f"Your front window: {front}. Mouse: {'not moved' if (point.x, point.y) == cursor else 'moved (by you?)'}.")


def show_viewer(frames, job):
    """A window on your screen that shows the second desktop live, view only."""
    hinst = GetModuleHandleW(None)
    bmi = bitmap_info(frames.width, frames.height)
    ticks = [0]

    def paint(hwnd):
        ps = PAINTSTRUCT()
        hdc = BeginPaint(hwnd, ctypes.byref(ps))
        rc = wt.RECT()
        GetClientRect(hwnd, ctypes.byref(rc))
        cw, ch = rc.right, rc.bottom
        black = GetStockObject(BLACK_BRUSH)
        with frames.lock:
            pixels = frames.pixels
        if pixels is None or cw <= 0 or ch <= 0:
            FillRect(hdc, ctypes.byref(rc), black)
        else:
            scale = min(cw / frames.width, ch / frames.height)
            dw, dh = max(1, int(frames.width * scale)), max(1, int(frames.height * scale))
            dx, dy = (cw - dw) // 2, (ch - dh) // 2
            for bar in ((0, 0, dx, ch), (dx + dw, 0, cw, ch), (dx, 0, dx + dw, dy), (dx, dy + dh, dx + dw, ch)):
                FillRect(hdc, ctypes.byref(wt.RECT(*bar)), black)
            SetStretchBltMode(hdc, HALFTONE)
            SetBrushOrgEx(hdc, 0, 0, None)
            StretchDIBits(hdc, dx, dy, dw, dh, 0, 0, frames.width, frames.height,
                          pixels, ctypes.byref(bmi), DIB_RGB_COLORS, SRCCOPY)
        EndPaint(hwnd, ctypes.byref(ps))

    def wndproc(hwnd, msg, wparam, lparam):
        try:
            if msg == WM_PAINT:
                paint(hwnd)
                return 0
            if msg == WM_ERASEBKGND:
                return 1
            if msg == WM_TIMER:
                InvalidateRect(hwnd, None, False)
                ticks[0] += 1
                if ticks[0] % 8 == 1:
                    mb = sum(m for _, m in job_memory(job))
                    SetWindowTextW(hwnd, f"Second Windows - {len(frames.windows)} windows on it - "
                                         f"its apps use {mb:.0f} MB RAM - view only")
                return 0
            if msg == WM_DESTROY:
                PostQuitMessage(0)
                return 0
        except Exception as e:
            print("viewer:", repr(e))
        return DefWindowProcW(hwnd, msg, wparam, lparam)

    proc = WNDPROC(wndproc)   # kept alive for the window's whole life
    wc = WNDCLASSEXW(cbSize=ctypes.sizeof(WNDCLASSEXW), lpfnWndProc=proc, hInstance=hinst,
                     hCursor=LoadCursorW(None, IDC_ARROW), lpszClassName="AutoCuaSecondWindows")
    if not RegisterClassExW(ctypes.byref(wc)):
        raise ctypes.WinError(ctypes.get_last_error())
    sw, sh = GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)
    w = int(sw * 0.6)
    h = int(w * frames.height / frames.width) + 40
    hwnd = CreateWindowExW(0, wc.lpszClassName, "Second Windows - starting", WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                           (sw - w) // 2, (sh - h) // 2, w, h, None, None, hinst, None)
    if not hwnd:
        raise ctypes.WinError(ctypes.get_last_error())
    SetTimer(hwnd, 1, int(FRAME_INTERVAL * 1000), None)
    msg = wt.MSG()
    while GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
        TranslateMessage(ctypes.byref(msg))
        DispatchMessageW(ctypes.byref(msg))


def main():
    parser = argparse.ArgumentParser(description="Second Windows: a second desktop for the agent.")
    parser.add_argument("--shot", metavar="PNG", help="save one screenshot and exit")
    parser.add_argument("--computer-use", action="store_true", help="scan and act on Character Map there")
    parser.add_argument("--no-shell", action="store_true", help="no Explorer on it")
    parser.add_argument("--run", action="append", metavar="CMD", help="program to start on it")
    parser.add_argument("--scan", metavar="PNG", help="save the agent's scan of it and exit")
    parser.add_argument("--wait", type=float, default=6.0, help="seconds before --shot or --scan")
    args = parser.parse_args()
    sys.stdout.reconfigure(errors="replace")   # window titles can hold characters the console lacks

    SetProcessDpiAwarenessContext(DPI_PER_MONITOR_AWARE_V2)   # real pixels, not scaled ones
    work = wt.RECT()   # the screen without the taskbar strip: every picture is cropped to it
    SystemParametersInfoW(SPI_GETWORKAREA, 0, ctypes.byref(work), 0)
    frames = Frames(work.left, work.top, work.right - work.left, work.bottom - work.top)
    windir = os.environ.get("SystemRoot", r"C:\Windows")
    charmap = os.path.join(windir, "System32", "charmap.exe")
    commands = [] if args.no_shell else [os.path.join(windir, "explorer.exe")]
    commands += [charmap] if args.computer_use or not args.run else args.run

    cursor = wt.POINT()
    GetCursorPos(ctypes.byref(cursor))
    front = GetForegroundWindow()
    before = (main_desktop_windows(), front, title_of(front), (cursor.x, cursor.y))
    desk = open_desktop(DESKTOP)
    job = kill_on_close_job()
    thread = threading.Thread(target=capture_loop, args=(desk, frames), daemon=True)
    try:
        started = []
        for command in commands:
            try:
                started.append((command, start_on_desktop(command, DESKTOP, job)))
            except OSError as e:
                print(f"could not start on the second desktop: {command}: {e}")
                continue
            print(f"started on the second desktop: {command} (pid {started[-1][1]})")
        if args.computer_use or args.scan:
            if args.scan:
                time.sleep(args.wait)   # the apps get to start
            result = []

            def work():   # on a thread of its own, which moves onto the second desktop
                try:
                    result.append(scan_desktop(desk, frames, args.scan) if args.scan else computer_use(desk, frames))
                except Exception:
                    traceback.print_exc()
            worker = threading.Thread(target=work)
            worker.start()
            worker.join()
            if args.scan and result:
                report(result[0], job, before, started)
            else:
                report_main_desktop(before)
        elif args.shot:
            thread.start()
            take_shot(args.shot, args.wait, frames, job, before, started)
        else:
            thread.start()
            show_viewer(frames, job)
    finally:
        frames.stop.set()
        if thread.is_alive():
            thread.join(timeout=3)
        TerminateJobObject(job, 0)
        CloseHandle(job)
        CloseDesktop(desk)


if __name__ == "__main__":
    main()
