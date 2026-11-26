"""Attribute the benchmark JVM's cumulative Windows CPU time to named HotSpot threads."""

import ctypes as c
from ctypes import wintypes as w
from pathlib import Path
import re
import subprocess
import time


CPU_GROUPS = ("main", "gc", "netty", "jit", "paper_workers", "other")
THREAD_HEADER = re.compile(r'^"([^"\r\n]+)"[^\r\n]*?\bnid=(0x[\da-f]+|\d+)\b', re.I | re.M)
TICKS_PER_SECOND = 10_000_000


def hotspot_threads(dump):
    return {int(nid, 16 if nid.lower().startswith("0x") else 10): name
            for name, nid in THREAD_HEADER.findall(dump)}


def cpu_group(name):
    if name == "Server thread":
        return "main"
    if name.startswith(("GC Thread#", "G1 ")):
        return "gc"
    if name.startswith("Netty Server IO "):
        return "netty"
    if name.startswith(("C1 CompilerThread", "C2 CompilerThread")):
        return "jit"
    if name.startswith(("Paper Common Worker", "Paper I/O Worker", "Moonrise Worker")):
        return "paper_workers"
    return "other"


def attribute_cpu(start, finish, thread_names):
    parts = dict.fromkeys(CPU_GROUPS, 0)
    for identity, end_cpu in finish["threads"].items():
        if identity in start["threads"]:
            before = start["threads"][identity]
        elif identity[1] >= start["filetime"]:
            before = 0
        else:
            continue
        group = cpu_group(thread_names.get(identity, ""))
        parts[group] += end_cpu - before
    total = finish["process"] - start["process"]
    # Includes unsampled or unclassified threads and the short boundary read skew.
    parts["other"] = total - sum(parts[g] for g in CPU_GROUPS if g != "other")
    return total / TICKS_PER_SECOND, {group: ticks / TICKS_PER_SECOND for group, ticks in parts.items()}


class ThreadEntry(c.Structure):
    _fields_ = [("size", w.DWORD), ("usage", w.DWORD), ("thread_id", w.DWORD), ("owner_pid", w.DWORD),
                ("base_priority", w.LONG), ("delta_priority", w.LONG), ("flags", w.DWORD)]


class JvmCpuSampler:
    """Keep native handles until the end so exiting threads retain their CPU counters."""

    def __init__(self, process, java):
        self.process = process
        self.jcmd = Path(java).with_name("jcmd.exe")
        self.handles = {}
        self.names = {}
        self.boundaries = {}
        kernel = c.WinDLL("kernel32", use_last_error=True)
        entry = c.POINTER(ThreadEntry)
        filetime = c.POINTER(w.FILETIME)
        self.toolhelp = self._bind(kernel.CreateToolhelp32Snapshot, w.HANDLE, w.DWORD, w.DWORD)
        self.first = self._bind(kernel.Thread32First, w.BOOL, w.HANDLE, entry)
        self.next = self._bind(kernel.Thread32Next, w.BOOL, w.HANDLE, entry)
        self.open_thread = self._bind(kernel.OpenThread, w.HANDLE, w.DWORD, w.BOOL, w.DWORD)
        self.owner = self._bind(kernel.GetProcessIdOfThread, w.DWORD, w.HANDLE)
        self.close_handle = self._bind(kernel.CloseHandle, w.BOOL, w.HANDLE)
        self.thread_times = self._bind(kernel.GetThreadTimes, w.BOOL, w.HANDLE, *([filetime] * 4))
        self.process_times = self._bind(kernel.GetProcessTimes, w.BOOL, w.HANDLE, *([filetime] * 4))
        self.system_time = self._bind(kernel.GetSystemTimePreciseAsFileTime, None, filetime)

    @staticmethod
    def _bind(function, result, *arguments):
        function.argtypes = arguments
        function.restype = result
        return function

    @staticmethod
    def _ticks(value):
        return (value.dwHighDateTime << 32) | value.dwLowDateTime

    def _times(self, function, handle):
        created, exited, kernel, user = (w.FILETIME() for _ in range(4))
        if not function(handle, c.byref(created), c.byref(exited), c.byref(kernel), c.byref(user)):
            return None
        return self._ticks(created), self._ticks(kernel) + self._ticks(user)

    def _discover(self):
        snapshot = self.toolhelp(0x00000004, 0)  # TH32CS_SNAPTHREAD
        if snapshot == c.c_void_p(-1).value:
            raise c.WinError(c.get_last_error())
        current = {}
        try:
            item = ThreadEntry()
            item.size = c.sizeof(item)
            ok = self.first(snapshot, c.byref(item))
            if not ok:
                raise c.WinError(c.get_last_error())
            while ok:
                if item.owner_pid == self.process.pid:
                    tid = item.thread_id
                    handle = self.open_thread(0x0800, False, tid)  # THREAD_QUERY_LIMITED_INFORMATION
                    if handle:
                        timing = self._times(self.thread_times, handle) if self.owner(handle) == self.process.pid else None
                        if timing is None:
                            self.close_handle(handle)
                        else:
                            identity = tid, timing[0]
                            if identity in self.handles:
                                self.close_handle(handle)
                            else:
                                self.handles[identity] = handle
                            current[tid] = identity
                item.size = c.sizeof(item)
                ok = self.next(snapshot, c.byref(item))
        finally:
            self.close_handle(snapshot)
        return current

    def discover_names(self):
        # Thread.print can pause the JVM. Run it during setup or after measurement.
        dump = subprocess.run([str(self.jcmd), str(self.process.pid), "Thread.print"], check=True,
                              capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=30,
                              creationflags=subprocess.CREATE_NO_WINDOW)
        threads = self._discover()
        for tid, name in hotspot_threads(dump.stdout).items():
            identity = threads.get(tid)
            if identity is not None:
                self.names[identity] = name
        groups = {cpu_group(self.names.get(identity, "")) for identity in threads.values()}
        if not {"main", "gc", "netty"}.issubset(groups):
            raise RuntimeError(f"Missing required live JVM thread names: {sorted(groups)}")

    def snapshot(self):
        self._discover()
        began = time.perf_counter_ns()
        wall = w.FILETIME()
        self.system_time(c.byref(wall))
        total = self._process_ticks(self.process)
        readings = {}
        for identity, handle in self.handles.items():
            timing = self._times(self.thread_times, handle)
            if timing is None:
                raise c.WinError(c.get_last_error())
            readings[identity] = timing[1]
        return {"monotonic": began, "filetime": self._ticks(wall), "process": total, "threads": readings,
                "read_ms": (time.perf_counter_ns() - began) / 1e6}

    def _process_ticks(self, process):
        timing = self._times(self.process_times, int(process._handle))
        if timing is None:
            raise c.WinError(c.get_last_error())
        return timing[1]

    def process_cpu_s(self, process):
        return self._process_ticks(process) / TICKS_PER_SECOND

    def record(self, window, start, finish):
        self.boundaries[window] = start, finish

    def finish(self, windows):
        for window in windows:
            start, finish = self.boundaries[window["window"]]
            cpu_s, parts = attribute_cpu(start, finish, self.names)
            window.update(os_cpu_s=cpu_s, thread_cpu_s=parts,
                          os_elapsed_s=(finish["monotonic"] - start["monotonic"]) / 1e9,
                          thread_read_ms=max(start["read_ms"], finish["read_ms"]))

    def catalogue(self):
        return [{"tid": tid, "creation_filetime": created, "name": name, "group": cpu_group(name)}
                for (tid, created), name in sorted(self.names.items())]

    def close(self):
        for handle in self.handles.values():
            self.close_handle(handle)
        self.handles.clear()
