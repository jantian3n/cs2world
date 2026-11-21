"""Measure packaged GUI dispatch from a process independent of the traffic fixtures."""
import contextlib
import ctypes as C
from ctypes import wintypes as W
import math
import multiprocessing
import subprocess
import time


class WindowApi:
    def __init__(self):
        self.user = C.WinDLL("user32", use_last_error=True)
        functions = {
            "FindWindowExW": (W.HWND, [W.HWND, W.HWND, W.LPCWSTR, W.LPCWSTR]),
            "GetWindowThreadProcessId": (W.DWORD, [W.HWND, C.POINTER(W.DWORD)]),
            "GetDlgItem": (W.HWND, [W.HWND, C.c_int]),
            "IsWindowEnabled": (W.BOOL, [W.HWND]),
            "IsWindowVisible": (W.BOOL, [W.HWND]),
            "IsIconic": (W.BOOL, [W.HWND]),
            "PostMessageW": (W.BOOL, [W.HWND, W.UINT, W.WPARAM, W.LPARAM]),
            "WaitForInputIdle": (W.DWORD, [W.HANDLE, W.DWORD]),
            "SendMessageTimeoutW": (W.LPARAM, [W.HWND, W.UINT, W.WPARAM, W.LPARAM,
                                                W.UINT, W.UINT, C.POINTER(C.c_size_t)]),
        }
        for name, (result, args) in functions.items():
            f = getattr(self.user, name)
            f.argtypes, f.restype = args, result

    def find(self, pid):
        hwnd = None
        while True:
            hwnd = self.user.FindWindowExW(None, hwnd, "CS2WorldWin32XPMain", None)
            if not hwnd:
                return None
            owner = W.DWORD()
            self.user.GetWindowThreadProcessId(hwnd, C.byref(owner))
            if owner.value == pid:
                return hwnd

    def visible(self, hwnd):
        return self.user.IsWindowVisible(hwnd) and not self.user.IsIconic(hwnd)

    def post(self, hwnd, msg, wparam=0):
        if not self.user.PostMessageW(hwnd, msg, wparam, 0):
            raise C.WinError(C.get_last_error())

    def send(self, hwnd, msg=0, wparam=0, lparam=0):
        result = C.c_size_t()
        return self.user.SendMessageTimeoutW(hwnd, msg, wparam, lparam, 3, 250, C.byref(result))

    def connect_caption(self, hwnd):
        text = C.create_unicode_buffer(80)
        button = self.user.GetDlgItem(hwnd, 102)
        if not self.send(button, 0x000D, len(text), C.addressof(text)):  # WM_GETTEXT
            raise AssertionError("connect button did not respond in 250 ms")
        return text.value


def wait_for(check, description, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.005)
    raise TimeoutError(description)


@contextlib.contextmanager
def visible_window(exe, label):
    api = WindowApi()
    start = time.perf_counter_ns()
    proc = subprocess.Popen([str(exe)])
    try:
        if api.user.WaitForInputIdle(proc._handle, 8000) != 0:
            raise RuntimeError("GUI did not reach input idle")
        hwnd = wait_for(lambda: api.find(proc.pid), "main window did not appear")
        wait_for(lambda: api.visible(hwnd), "main window did not become visible")
        # The connect control exists during WM_CREATE. Observe the effect of queued
        # commands on the main thread before starting a workload or its stopwatch.
        api.post(hwnd, 0x0010)  # WM_CLOSE hides to tray
        wait_for(lambda: not api.user.IsWindowVisible(hwnd), "tray hide was not dispatched")
        api.post(hwnd, 0x0111, 901)  # existing tray Show command
        wait_for(lambda: api.visible(hwnd) and api.user.IsWindowEnabled(api.user.GetDlgItem(hwnd, 102)),
                 "tray restore or core initialization failed")
        ready_ms = (time.perf_counter_ns() - start) / 1e6
        print(f"Win32 {label} launch to visible ready (incl. tray round trip) {ready_ms:.2f} ms", flush=True)
        yield api, hwnd
        api.post(hwnd, 0x0010)
        wait_for(lambda: not api.user.IsWindowVisible(hwnd), "WM_CLOSE did not hide the window")
        if proc.poll() is not None:
            raise AssertionError("WM_CLOSE exited the application")
        api.post(hwnd, 0x0111, 903)  # tray Quit
        if proc.wait(timeout=10):
            raise RuntimeError("tray quit failed")
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait()


def sample(hwnd, pipe):
    try:
        api = WindowApi()
        samples = []
        pipe.send("armed")
        while not pipe.poll():
            if not api.visible(hwnd):
                raise AssertionError("window was hidden or minimized during workload")
            started = time.perf_counter_ns()
            replied = bool(api.send(hwnd))
            samples.append((started, time.perf_counter_ns(), replied))
            time.sleep(.01)
        pipe.send(samples)
    except Exception as exc:
        pipe.send(str(exc))
    finally:
        pipe.close()


def exercise_window(exe, label, workload=None):
    with visible_window(exe, label) as (api, hwnd):
        if workload is None:
            return
        ctx = multiprocessing.get_context("spawn")
        parent, child = ctx.Pipe()
        sampler = ctx.Process(target=sample, args=(hwnd, child))
        sampler.start()
        child.close()
        try:
            if not parent.poll(8) or parent.recv() != "armed":
                raise RuntimeError("latency sampler failed to start")
            start, end = workload(api, hwnd)
            parent.send("done")
            if not parent.poll(5):
                raise TimeoutError("latency sampler did not finish")
            samples = parent.recv()
            if not isinstance(samples, list):
                raise RuntimeError(samples)
        finally:
            parent.close()
            sampler.join(1)
            if sampler.is_alive():
                sampler.terminate()
                sampler.join()
        # QPC uses the same clock in both Windows processes. Include messages
        # overlapping either boundary so that a stall crossing one is counted.
        active = [(b - a, replied) for a, b, replied in samples if a <= end and b >= start]
        if any(not replied for _, replied in active):
            raise AssertionError(f"{label}: window did not reply within 250 ms")
        ms = sorted(ns / 1e6 for ns, _ in active)
        if len(ms) < 150:
            raise AssertionError(f"{label}: only {len(ms)} workload samples")
        p95, p99 = (ms[math.ceil(p * len(ms)) - 1] for p in (.95, .99))
        print(f"Win32 {label} active={(end-start)/1e6:.2f} ms n={len(ms)} "
              f"p95={p95:.2f} ms p99={p99:.2f} ms max={ms[-1]:.2f} ms", flush=True)
        if p99 > 100 or ms[-1] > 250:
            raise AssertionError(f"{label}: window latency exceeded design threshold")
