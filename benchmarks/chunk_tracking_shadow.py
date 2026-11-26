"""Run the read-only chunk tracking pilot on an isolated Windows Purpur fixture."""

import argparse
from collections import defaultdict
import csv
import ctypes as c
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shutil
import socket
import statistics
import subprocess
import time
import zipfile


REPO = Path(__file__).resolve().parents[1]
WORLDS = {"world": "world-heavy-v24-snapshot", "world_nether": "world_nether", "world_the_end": "world_the_end"}
PHASES = ("DENSE", "SPREAD", "TELEPORT")
ORDERS = (("full", "java", "rust"), ("java", "rust", "full"), ("rust", "full", "java"))
TURNS = ORDERS + tuple(tuple(reversed(order)) for order in ORDERS)
TURN_SECONDS = 4
BACKGROUND_MATCH_PP = 5.0
POSITIONS = ((-80, 0), (80, 0), (0, -80), (0, 80), (80, 80))
PLUGIN_PACKAGE = "net/spicanetwork/rustnative"


class ProcessorTime(c.Structure):
    _fields_ = [(name, c.c_int64) for name in ("idle", "kernel", "user", "dpc", "interrupt")] + [("interrupt_count", c.c_uint32)]


def cpu_counters(mask):
    times = (ProcessorTime * os.cpu_count())()
    query = c.WinDLL("ntdll").NtQuerySystemInformation
    query.argtypes = (c.c_ulong, c.c_void_p, c.c_ulong, c.POINTER(c.c_ulong))
    written = c.c_ulong()
    if query(8, times, c.sizeof(times), c.byref(written)) != 0 or written.value != c.sizeof(times):
        raise RuntimeError("Unable to read host CPU counters")
    cores = [cpu for index, cpu in enumerate(times) if mask & (1 << index)]
    return (sum(cpu.kernel + cpu.user for cpu in cores) / 1e7,
            sum(cpu.kernel + cpu.user - cpu.idle for cpu in cores) / 1e7)


def pin(process, mask):
    kernel = c.WinDLL("kernel32", use_last_error=True)
    affinity = kernel.SetProcessAffinityMask
    affinity.argtypes = (c.c_void_p, c.c_size_t)
    read = kernel.GetProcessAffinityMask
    read.argtypes = (c.c_void_p, c.POINTER(c.c_size_t), c.POINTER(c.c_size_t))
    actual, system = c.c_size_t(), c.c_size_t()
    if not affinity(int(process._handle), mask) or not read(int(process._handle), c.byref(actual), c.byref(system)):
        raise c.WinError(c.get_last_error())
    if actual.value != mask:
        raise RuntimeError(f"Unexpected affinity {actual.value:#x}")


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def make_probe(jar):
    project = REPO / "spica-rust-native-plugin/target"
    with zipfile.ZipFile(jar, "w", zipfile.ZIP_DEFLATED) as out:
        for file in sorted((project / "test-classes" / PLUGIN_PACKAGE / "benchmark").glob("ChunkTracking*.class")):
            out.write(file, file.relative_to(project / "test-classes").as_posix())
        rust_class = f"{PLUGIN_PACKAGE}/nativebridge/RustNative.class"
        out.write(project / "classes" / rust_class, rust_class)
        out.writestr("plugin.yml", """name: SpicaChunkTrackingShadow
version: '1'
main: net.spicanetwork.rustnative.benchmark.ChunkTrackingShadow
api-version: '1.21'
commands:
  chunkshadow:
    usage: /chunkshadow <idle|dense|spread|teleport|dump> [full|java|rust]
""")


def copy_fixture(seed, destination, probe, port):
    destination.mkdir()
    for name, source in WORLDS.items():
        shutil.copytree(seed / source, destination / name, ignore=shutil.ignore_patterns("session.lock"))
    for name in ("cache", "config", "libraries", "versions"):
        shutil.copytree(seed / name, destination / name)
    for file in ("purpur-1.21.4.jar", "eula.txt", "bukkit.yml", "spigot.yml", "purpur.yml"):
        shutil.copy2(seed / file, destination / file)
    overrides = {"server-ip": "127.0.0.1", "server-port": port, "online-mode": "false", "level-name": "world",
                 "enable-query": "false", "enable-rcon": "false", "white-list": "false"}
    settings = (seed / "server.properties").read_text(encoding="utf-8").splitlines()
    settings = [line for line in settings if line.partition("=")[0] not in overrides]
    settings.extend(f"{key}={value}" for key, value in overrides.items())
    (destination / "server.properties").write_text("\n".join(settings) + "\n", encoding="utf-8")
    (destination / "plugins").mkdir()
    shutil.copy2(probe, destination / "plugins")


class Console:
    def __init__(self, process, log, interleaved):
        self.process = process
        self.log = log
        self.interleaved = interleaved

    def send(self, command):
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()

    def wait(self, pattern, timeout=50):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and self.process.poll() is None:
            line = self.log.readline()
            if "SHADOW_FAILED" in line:
                raise RuntimeError(line.strip())
            if match := re.search(pattern, line):
                return match
            if not line:
                time.sleep(.025)
        raise RuntimeError(f"Missing {pattern!r} in {self.log.name}")

    def phase(self, name, backend=None):
        command = f"chunkshadow {name.lower()}"
        self.send(f"{command} {backend}" if backend else command)
        match = self.wait(rf"SHADOW_PHASE {name} (-?\d+) (\d+) (\d+)")
        return tuple(int(value) for value in match.groups())


def place(console, phase):
    if phase == "SPREAD":
        for index in range(20):
            x, z = POSITIONS[index // 4]
            console.send(f"minecraft:tp ShadowBot{index + 1:03d} {x} -54 {z}")
    else:
        console.send("minecraft:tp @a 0 -54 0")


def measure(console, phase, seconds, server_mask, mode):
    start_ns, start_cpu, window = console.phase(phase, mode if console.interleaved else None)
    start_bg = cpu_counters(server_mask)
    started = time.monotonic()
    if phase == "TELEPORT":
        for wave in range(seconds // 2):
            time.sleep(max(0, started + 2 * wave + 1 - time.monotonic()))
            coord = 80 if wave % 2 == 0 else -80
            console.send(f"minecraft:tp @a {coord} -54 {coord}")
    time.sleep(max(0, started + seconds - time.monotonic()))
    end_ns, end_cpu, _ = console.phase("IDLE")
    finish_bg = cpu_counters(server_mask)
    cpu = (end_cpu - start_cpu) / 1e9
    total, busy = (finish_bg[i] - start_bg[i] for i in range(2))
    return {"phase": phase, "backend": mode, "window": window, "elapsed_s": (end_ns - start_ns) / 1e9, "cpu_s": cpu,
            "background_pct": 100 * max(0, busy - cpu) / total}


def summarize_rows(run, windows, minimum_samples):
    by_window = defaultdict(list)
    with (run / "chunk-tracking.csv").open(newline="", encoding="utf-8") as stream:
        for row in csv.DictReader(stream):
            by_window[int(row["window"])].append(row)
    for metrics in windows:
        samples = by_window[metrics["window"]]
        if len(samples) < minimum_samples or any(
            int(row["players"]) != 20 or row["backend"].lower() != metrics["backend"] for row in samples
        ):
            raise RuntimeError(f"Invalid {metrics['phase']} tick window {metrics['window']} in {run}")
        if not all(int(row["mobs"]) > 0 and int(row["matches"]) > 0 for row in samples):
            raise RuntimeError(f"Empty workload in {run}")
        tick_ms = sorted(float(row["tick_ms"]) for row in samples)
        metrics.update(ticks=len(samples), tick_p99_ms=tick_ms[math.ceil(.99 * len(samples)) - 1],
                       tick_mean_ms=statistics.mean(tick_ms), cpu_s_per_20s=20 * metrics["cpu_s"] / metrics["elapsed_s"],
                       mobs=statistics.median(int(row["mobs"]) for row in samples),
                       pairs=statistics.median(int(row["matches"]) for row in samples))
        for field in ("total_ns", "index_ns", "evaluation_ns"):
            metrics[field.replace("_ns", "_us")] = statistics.median(int(row[field]) for row in samples) / 1000


def interleave(console, args):
    windows = []
    for phase in PHASES:
        print(f"interleaved {phase.lower()}", flush=True)
        place(console, phase)
        if phase == "TELEPORT":
            console.send("minecraft:tp @a -80 -54 -80")
        for mode in ORDERS[0]:
            console.phase("IDLE", mode)
            time.sleep(6)
        for turn, order in enumerate(TURNS, start=1):
            for mode in order:
                windows.append(measure(console, phase, TURN_SECONDS, args.server_mask, mode) | {"turn": turn})
    return windows


def interleaved_summary(run, windows):
    with (run / "chunk-tracking.csv").open(newline="", encoding="utf-8") as stream:
        samples = list(csv.DictReader(stream))
    report = {}
    for phase in PHASES:
        phase_windows = [window for window in windows if window["phase"] == phase]
        by_turn = defaultdict(list)
        for window in phase_windows:
            by_turn[window["turn"]].append(window["background_pct"])
        matched = [turn for turn, load in by_turn.items() if max(load) - min(load) <= BACKGROUND_MATCH_PP]
        report[phase] = {"matched_turns": matched, "all": {}, "matched": {}}
        for label, turns in (("all", by_turn), ("matched", matched)):
            if not turns:
                continue
            for mode in ORDERS[0]:
                selected = [window for window in phase_windows if window["backend"] == mode and window["turn"] in turns]
                ids = {str(window["window"]) for window in selected}
                ticks = [sample for sample in samples if sample["window"] in ids]
                lengths = sorted(float(sample["tick_ms"]) for sample in ticks)
                metrics = {
                    "ticks": len(ticks), "tick_p99_ms": lengths[math.ceil(.99 * len(ticks)) - 1],
                    "cpu_s_per_20s": 20 * sum(w["cpu_s"] for w in selected) / sum(w["elapsed_s"] for w in selected),
                    "background_pct_range": [min(w["background_pct"] for w in selected),
                                             max(w["background_pct"] for w in selected)],
                }
                for field in ("total_ns", "index_ns", "evaluation_ns"):
                    metrics[field.replace("_ns", "_us")] = statistics.median(int(tick[field]) for tick in ticks) / 1000
                report[phase][label][mode] = metrics
    return report


def stop(process, console):
    if process.poll() is None:
        try:
            console.send("stop")
            process.wait(timeout=45)
        except (OSError, subprocess.TimeoutExpired):
            process.kill()
            process.wait(timeout=10)


def run(args, probe, backend, name):
    game_port = free_port()
    work = args.output / name
    copy_fixture(args.seed, work, probe, game_port)
    print(f"{name} starting", flush=True)
    verify = backend == "verify"
    interleaved = backend == "interleaved"
    seconds, warmup = (6, 3) if verify else (args.seconds, 8)
    java = [str(args.java), "-Xms2048M", "-Xmx2048M", "-XX:+UseG1GC",
            f"-XX:ActiveProcessorCount={args.server_mask.bit_count()}", "-Dterminal.jline=false", "-Dterminal.ansi=false",
            f"-Dspica.shadow.backend={backend}", f"-Dspica.shadow.library={args.native}"]
    if verify:
        java.append("-Xcheck:jni")
    java.extend(("-jar", "purpur-1.21.4.jar", "nogui"))
    console_file = work / "console.log"
    bot = None
    with console_file.open("w", encoding="utf-8") as output, console_file.open(encoding="utf-8", errors="replace") as log:
        server = subprocess.Popen(java, cwd=work, stdin=subprocess.PIPE, stdout=output, stderr=subprocess.STDOUT,
                                  text=True, encoding="utf-8", creationflags=subprocess.CREATE_NO_WINDOW)
        console = Console(server, log, interleaved)
        try:
            pin(server, args.server_mask)
            console.wait(f"SHADOW_READY {backend.upper()}", 120)
            console.wait(r"Done \(", 90)
            initial_warmup = 0 if verify else 12
            bot_duration = 3 * (seconds + warmup) + initial_warmup + 12
            if interleaved:
                bot_duration = 3 * (len(TURNS) * 3 * TURN_SECONDS + 18) + 18
            with (work / "bots.json").open("w", encoding="utf-8") as out, (work / "bots.log").open("w", encoding="utf-8") as err:
                bot = subprocess.Popen([str(args.bots), "--host", "127.0.0.1", "--port", str(game_port), "--bots", "20",
                                        "--prefix", "ShadowBot", "--warmup", "2", "--duration", str(bot_duration),
                                        "--stagger-ms", "60", "--min-ready-pct", "100", "--json"], cwd=work, stdout=out,
                                       stderr=err, creationflags=subprocess.CREATE_NO_WINDOW)
                pin(bot, args.bot_mask)
                console.wait(r"ShadowBot020 joined the game", 60)
                console.send("minecraft:gamemode spectator @a")
                console.wait(r"Set ShadowBot\d{3}'s game mode to Spectator Mode")
                if interleaved:
                    windows = interleave(console, args)
                else:
                    windows = []
                    for phase in PHASES:
                        print(f"{name} {phase.lower()}", flush=True)
                        place(console, phase)
                        time.sleep(warmup + (initial_warmup if phase == "DENSE" else 0))
                        windows.append(measure(console, phase, seconds, args.server_mask, backend))
                console.send("chunkshadow dump")
                console.wait(r"SHADOW_WRITTEN \d+")
                if bot.wait(timeout=45) != 0:
                    raise RuntimeError(f"Bot run failed in {work}")
        finally:
            if bot and bot.poll() is None:
                bot.terminate()
                bot.wait(timeout=10)
            stop(server, console)
    if server.returncode != 0:
        raise RuntimeError(f"Purpur exited with {server.returncode} in {work}")
    bots = json.loads((work / "bots.json").read_text(encoding="utf-8"))
    if bots["readyPlay"] != 20 or bots["disconnectedEarly"] != 0:
        raise RuntimeError(f"Expected 20 ready bots and zero early drops in {work}")
    summarize_rows(work, windows, 40 if interleaved else (60 if verify else 200))
    metrics = ({"windows": windows, "summary": interleaved_summary(work, windows)} if interleaved
               else {window["phase"]: window for window in windows})
    (work / "result.json").write_text(json.dumps(metrics, indent=2), encoding="utf-8")
    return metrics


def stable_fixture(seed):
    hashes = {}
    for directory in WORLDS.values():
        for file in sorted((seed / directory).rglob("*")):
            if file.is_file() and file.name != "session.lock":
                with file.open("rb") as stream:
                    hashes[file.relative_to(seed).as_posix()] = hashlib.file_digest(stream, "sha256").hexdigest()
    return hashes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for option in ("seed", "output", "java", "native", "bots"):
        parser.add_argument(f"--{option}", type=lambda value: Path(value).resolve(), required=True)
    for option in ("server-mask", "bot-mask"):
        parser.add_argument(f"--{option}", type=lambda value: int(value, 0), required=True)
    parser.add_argument("--seconds", type=int, default=24)
    parser.add_argument("--rounds", type=int, choices=range(0, 4), default=3)
    parser.add_argument("--skip-verify", action="store_true")
    parser.add_argument("--interleave", action="store_true")
    args = parser.parse_args()
    if os.name != "nt" or not args.server_mask or not args.bot_mask or args.server_mask & args.bot_mask or args.seconds < 20:
        parser.error("Requires Windows, disjoint nonempty masks and at least twenty measurement seconds")
    args.output.mkdir(parents=True)
    probe = args.output / "ChunkTrackingShadow.jar"
    make_probe(probe)
    raw = {"server_mask": args.server_mask, "bot_mask": args.bot_mask, "fixture": stable_fixture(args.seed), "rounds": []}
    if not args.skip_verify:
        raw["verify"] = run(args, probe, "verify", "verify")
    if args.interleave:
        raw["interleaved"] = run(args, probe, "interleaved", "interleaved")
    for index, order in enumerate(ORDERS[:args.rounds], start=1):
        raw["rounds"].append({mode: run(args, probe, mode, f"round-{index}-{mode}") for mode in order})
        (args.output / "results.json").write_text(json.dumps(raw, indent=2), encoding="utf-8")
    if stable_fixture(args.seed) != raw["fixture"]:
        raise RuntimeError("Fixture changed during the benchmark")
    (args.output / "results.json").write_text(json.dumps(raw, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
