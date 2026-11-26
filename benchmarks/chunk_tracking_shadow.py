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

from windows_jvm_cpu import CPU_GROUPS, JvmCpuSampler


REPO = Path(__file__).resolve().parents[1]
WORLDS = {"world": "world-heavy-v24-snapshot", "world_nether": "world_nether", "world_the_end": "world_the_end"}
PHASES = ("DENSE", "SPREAD", "TELEPORT")
ORDERS = (("full", "java", "rust"), ("java", "rust", "full"), ("rust", "full", "java"))
TURNS = ORDERS + tuple(tuple(reversed(order)) for order in ORDERS)
DELTA_MODES = ("rust", "incremental")
MOVEMENT_MODES = ("incremental", "dirty")
HOOK_COST_MODES = ("dirty", "direct")
DELTA_SETTLE_SECONDS = 2
TURN_SECONDS = 4
ATTRIBUTION_TURN_SECONDS = 12
PACKET_TURN_SECONDS = 8
BACKGROUND_MATCH_PP = 5.0
PACKET_BOUNDARY_LIMIT_MS = 100
PACKET_SNAPSHOT = re.compile(r"SPICA_SNAPSHOT (\d+) (\d+) (\d+) (\d+) (\d+)")
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


def make_probe(jar, movement):
    project = REPO / "spica-rust-native-plugin/target"
    with zipfile.ZipFile(jar, "w", zipfile.ZIP_DEFLATED) as out:
        for file in sorted((project / "test-classes" / PLUGIN_PACKAGE / "benchmark").glob("ChunkTracking*.class")):
            if movement and file.name == "ChunkTrackingPositionFeed.class":
                continue
            out.write(file, file.relative_to(project / "test-classes").as_posix())
        rust_class = f"{PLUGIN_PACKAGE}/nativebridge/RustNative.class"
        out.write(project / "classes" / rust_class, rust_class)
        out.writestr("plugin.yml", """name: SpicaChunkTrackingShadow
version: '1'
main: net.spicanetwork.rustnative.benchmark.ChunkTrackingShadow
api-version: '1.21'
commands:
  chunkshadow:
    usage: /chunkshadow <idle|dense|spread|teleport|dump> [full|java|rust|incremental|dirty|direct]
""")


def make_movement_agent(args):
    asm = args.seed / "libraries/org/ow2/asm/asm/9.7.1/asm-9.7.1.jar"
    classes = REPO / "spica-rust-native-plugin/target/test-classes"
    built = args.output / "position-agent-classes"
    subprocess.run([str(args.java.with_name("javac.exe")), "--release", "21", "-cp", f"{classes};{asm}",
                    "-d", str(built), str(REPO / "benchmarks/java/ChunkTrackingPositionAgent.java")], check=True,
                   creationflags=subprocess.CREATE_NO_WINDOW)
    feed = args.output / "position-feed.jar"
    with zipfile.ZipFile(feed, "w", zipfile.ZIP_DEFLATED) as out:
        entry = f"{PLUGIN_PACKAGE}/benchmark/ChunkTrackingPositionFeed.class"
        out.write(classes / entry, entry)
    agent = args.output / "position-agent.jar"
    with zipfile.ZipFile(agent, "w", zipfile.ZIP_DEFLATED) as out, zipfile.ZipFile(asm) as source:
        out.writestr("META-INF/MANIFEST.MF", "Manifest-Version: 1.0\r\n"
                     "Premain-Class: net.spicanetwork.rustnative.benchmark.ChunkTrackingPositionAgent\r\n\r\n")
        for file in sorted(built.rglob("*.class")):
            out.write(file, file.relative_to(built).as_posix())
        for entry in source.namelist():
            if entry.startswith("org/objectweb/asm/") and entry.endswith(".class"):
                out.writestr(entry, source.read(entry))
    return f"-javaagent:{agent}={feed}"


def paired_modes(args):
    if args.hook_cost:
        return HOOK_COST_MODES
    if args.movement:
        return MOVEMENT_MODES
    if args.incremental:
        return DELTA_MODES
    return None


def rotation(args):
    modes = paired_modes(args)
    repeats = 5 if args.packet_attribution else (3 if args.cpu_attribution else 4)
    return (modes, modes[::-1]) * repeats if modes else TURNS


def window_seconds(args):
    if args.packet_attribution:
        return PACKET_TURN_SECONDS
    return ATTRIBUTION_TURN_SECONDS if args.cpu_attribution else TURN_SECONDS


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
        self.require_bots = False

    def send(self, command):
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()

    def wait(self, pattern, timeout=50):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline and self.process.poll() is None:
            line = self.log.readline()
            if "SHADOW_FAILED" in line:
                raise RuntimeError(line.strip())
            if self.require_bots and re.search(r"ShadowBot\d{3} lost connection:", line):
                raise RuntimeError(line.strip())
            if match := re.search(pattern, line):
                return match
            if not line:
                time.sleep(.025)
        raise RuntimeError(f"Missing {pattern!r} in {self.log.name}")

    def phase(self, name, backend=None):
        command = f"chunkshadow {name.lower()}"
        self.send(f"{command} {backend}" if backend else command)
        match = self.wait(rf"SHADOW_PHASE {name} (-?\d+) (\d+) (\d+) (-?\d+) (\d+) (\d+)")
        return tuple(int(value) for value in match.groups())


class PacketSampler:
    def __init__(self, bot, log, bot_mask, cpu_sampler):
        self.bot = bot
        self.log = log
        self.bot_mask = bot_mask
        self.cpu_sampler = cpu_sampler

    def snapshot(self):
        started = time.perf_counter_ns()
        self.bot.stdin.write(b"snapshot\n")
        self.bot.stdin.flush()
        pending = ""
        while time.perf_counter_ns() - started < 2_000_000_000 and self.bot.poll() is None:
            line = self.log.readline()
            if not line:
                time.sleep(.001)
                continue
            pending += line
            if not pending.endswith("\n"):
                continue
            if match := PACKET_SNAPSHOT.fullmatch(pending.rstrip()):
                ended = time.perf_counter_ns()
                measuring, ready, packets, wire_bytes, entity_packets = map(int, match.groups())
                if measuring != 1 or ready != 20:
                    raise RuntimeError(f"Packet window has {ready} ready bots, measuring={measuring}")
                return {"ns": (started + ended) // 2, "read_ms": (ended - started) / 1e6,
                        "packets": packets, "wire_bytes": wire_bytes, "entity_packets": entity_packets,
                        "cpu": self.cpu_sampler.process_cpu_s(self.bot), "lane": cpu_counters(self.bot_mask)}
            pending = ""
        raise RuntimeError(f"Missing packet snapshot in {self.log.name}")

    @staticmethod
    def delta(start, finish, os_start, os_finish):
        span = (finish["ns"] - start["ns"]) / 1e9
        counters = {key: finish[key] - start[key] for key in ("packets", "wire_bytes", "entity_packets")}
        if span <= 0 or counters["packets"] <= 0 or counters["wire_bytes"] < counters["packets"]:
            raise RuntimeError(f"Invalid packet counters: {counters}")
        total, busy = (finish["lane"][i] - start["lane"][i] for i in range(2))
        return counters | {"packet_elapsed_s": span, "packet_read_ms": max(start["read_ms"], finish["read_ms"]),
                           "packet_boundary_skew_ms": max(abs(start["ns"] - os_start["monotonic"]),
                                                          abs(finish["ns"] - os_finish["monotonic"])) / 1e6,
                           "bot_background_pct": 100 * max(0, busy - finish["cpu"] + start["cpu"]) / total}


def place(console, phase):
    if phase == "SPREAD":
        for index in range(20):
            x, z = POSITIONS[index // 4]
            console.send(f"minecraft:tp ShadowBot{index + 1:03d} {x} -54 {z}")
        console.wait(r"Teleported ShadowBot020 to")
    else:
        teleport_all(console, 0)


def teleport_all(console, coord):
    console.send(f"minecraft:tp @a {coord} -54 {coord}")
    console.wait(r"Teleported 20 entities to")


def measure(console, phase, seconds, server_mask, mode, cpu_sampler=None, packet_sampler=None):
    start_ns, start_cpu, window, start_main, start_gc_count, start_gc_ms = console.phase(
        phase, mode if console.interleaved else None
    )
    start_bg = cpu_counters(server_mask)
    os_start = cpu_sampler.snapshot() if cpu_sampler else None
    packet_start = packet_sampler.snapshot() if packet_sampler else None
    started = time.monotonic()
    if phase == "TELEPORT":
        for wave in range(seconds // 2):
            time.sleep(max(0, started + 2 * wave + 1 - time.monotonic()))
            coord = 80 if wave % 2 == 0 else -80
            teleport_all(console, coord)
    elif phase == "DENSE" and mode == "verify":
        lifecycle = (
            (1, 'minecraft:summon minecraft:pig 32 -54 -20 {Tags:["spica_shadow_lifecycle"],NoAI:1b,NoGravity:1b}',
             r"Summoned new Pig"),
            (3, "minecraft:tp @e[tag=spica_shadow_lifecycle,limit=1] -65 -54 0", r"Teleported Pig to"),
            (5, "chunkshadow movement-coverage", r"SHADOW_MOVEMENT_GAP"),
            (8, "minecraft:kill @e[tag=spica_shadow_lifecycle,limit=1]", r"Killed Pig"),
        )
        for offset, command, confirmation in lifecycle:
            time.sleep(max(0, started + offset - time.monotonic()))
            console.send(command)
            console.wait(confirmation)
    time.sleep(max(0, started + seconds - time.monotonic()))
    end_ns, end_cpu, _, end_main, end_gc_count, end_gc_ms = console.phase("IDLE")
    os_finish = cpu_sampler.snapshot() if cpu_sampler else None
    packets = packet_sampler.delta(packet_start, packet_sampler.snapshot(), os_start, os_finish) if packet_sampler else {}
    finish_bg = cpu_counters(server_mask)
    if cpu_sampler:
        cpu_sampler.record(window, os_start, os_finish)
    cpu = (end_cpu - start_cpu) / 1e9
    total, busy = (finish_bg[i] - start_bg[i] for i in range(2))
    return {"phase": phase, "backend": mode, "window": window, "elapsed_s": (end_ns - start_ns) / 1e9, "cpu_s": cpu,
            "main_cpu_s": (end_main - start_main) / 1e9 if start_main >= 0 and end_main >= 0 else None,
            "gc_count": end_gc_count - start_gc_count, "gc_ms": end_gc_ms - start_gc_ms,
            "background_pct": 100 * max(0, busy - cpu) / total} | packets


def summarize_rows(run, windows, minimum_samples, keep_short=False):
    by_window = defaultdict(list)
    with (run / "chunk-tracking.csv").open(newline="", encoding="utf-8") as stream:
        for row in csv.DictReader(stream):
            by_window[int(row["window"])].append(row)
    for metrics in windows:
        samples = by_window[metrics["window"]]
        if any(int(row["players"]) != 20 or row["backend"].lower() != metrics["backend"] for row in samples):
            raise RuntimeError(f"Invalid {metrics['phase']} tick window {metrics['window']} in {run}")
        metrics["ticks"] = len(samples)
        if len(samples) < minimum_samples:
            if keep_short:
                continue
            raise RuntimeError(f"Short {metrics['phase']} tick window {metrics['window']} in {run}")
        if not all(int(row["mobs"]) > 0 and int(row["matches"]) > 0 for row in samples):
            raise RuntimeError(f"Empty workload in {run}")
        tick_ms = sorted(float(row["tick_ms"]) for row in samples)
        metrics.update(tick_p99_ms=tick_ms[math.ceil(.99 * len(samples)) - 1],
                       tick_mean_ms=statistics.mean(tick_ms), cpu_s_per_20s=20 * metrics["cpu_s"] / metrics["elapsed_s"],
                       mobs=statistics.median(int(row["mobs"]) for row in samples),
                       pairs=statistics.median(int(row["matches"]) for row in samples),
                       median_upserts=statistics.median(int(row["upserts"]) for row in samples),
                       median_mob_checks=statistics.median(int(row["mob_checks"]) for row in samples),
                       removals=sum(int(row["removals"]) for row in samples))
        for field in ("total_ns", "index_ns", "evaluation_ns"):
            metrics[field.replace("_ns", "_us")] = statistics.median(int(row[field]) for row in samples) / 1000


def interleave(console, args, checkpoint, cpu_sampler=None, packet_sampler=None):
    windows = []
    turns = rotation(args)
    for phase in PHASES:
        print(f"interleaved {phase.lower()}", flush=True)
        place(console, phase)
        if phase == "TELEPORT":
            teleport_all(console, -80)
        for mode in turns[0]:
            console.phase("IDLE", mode)
            time.sleep(6)
        if cpu_sampler:
            cpu_sampler.discover_names()
        for turn, order in enumerate(turns, start=1):
            for mode in order:
                if paired_modes(args):
                    console.phase("IDLE", mode)
                    time.sleep(DELTA_SETTLE_SECONDS)
                windows.append(measure(console, phase, window_seconds(args), args.server_mask, mode,
                                       cpu_sampler, packet_sampler) | {"turn": turn})
        if cpu_sampler:
            cpu_sampler.discover_names()
            cpu_sampler.finish(windows)
        checkpoint.write_text(json.dumps(windows, indent=2), encoding="utf-8")
    return windows


def packet_metrics(windows, netty_cpu_rate):
    count = sum(w["packets"] for w in windows)
    span = sum(w["packet_elapsed_s"] for w in windows)
    packet_rate = count / span
    bot_background = [w["bot_background_pct"] for w in windows]
    return {"packets": count, "packets_per_s": packet_rate,
            "wire_bytes_per_packet": sum(w["wire_bytes"] for w in windows) / count,
            "entity_packet_pct": 100 * sum(w["entity_packets"] for w in windows) / count,
            "netty_cpu_us_per_packet": 1e6 * netty_cpu_rate / packet_rate,
            "max_packet_read_ms": max(w["packet_read_ms"] for w in windows),
            "max_packet_boundary_skew_ms": max(w["packet_boundary_skew_ms"] for w in windows),
            "bot_background_pct_range": [min(bot_background), max(bot_background)]}


def packet_cpu_effects(baseline, candidate):
    rates = [report["packets_per_s"] for report in (baseline, candidate)]
    costs = [report["netty_cpu_us_per_packet"] for report in (baseline, candidate)]
    # Attribute the interaction equally to both factors so the two effects sum to the observed change.
    return {"volume_cpu_s_per_20s": 20e-6 * (rates[1] - rates[0]) * statistics.mean(costs),
            "cost_cpu_s_per_20s": 20e-6 * (costs[1] - costs[0]) * statistics.mean(rates)}


def interleaved_summary(run, windows, modes):
    with (run / "chunk-tracking.csv").open(newline="", encoding="utf-8") as stream:
        samples = list(csv.DictReader(stream))
    report = {}
    for phase in PHASES:
        phase_windows = [window for window in windows if window["phase"] == phase]
        by_turn = defaultdict(list)
        has_packets = "packets" in phase_windows[0]
        short_turns = {window["turn"] for window in phase_windows if "tick_p99_ms" not in window}
        skewed_turns = {
            w["turn"] for w in phase_windows
            if has_packets and w["packet_boundary_skew_ms"] + w["packet_read_ms"] / 2 > PACKET_BOUNDARY_LIMIT_MS
        }
        excluded_turns = short_turns | skewed_turns
        for window in phase_windows:
            if window["turn"] not in excluded_turns:
                by_turn[window["turn"]].append(window)
        load_keys = ("background_pct", "bot_background_pct") if has_packets else ("background_pct",)
        matched = []
        for turn, pair in by_turn.items():
            if all(max(w[key] for w in pair) - min(w[key] for w in pair) <= BACKGROUND_MATCH_PP for key in load_keys):
                matched.append(turn)
        report[phase] = {"short_turns": sorted(short_turns), "matched_turns": matched, "all": {}, "matched": {}}
        if has_packets:
            report[phase]["packet_skew_turns"] = sorted(skewed_turns)
        for label, turns in (("all", by_turn), ("matched", matched)):
            if not turns:
                continue
            for mode in modes:
                selected = [window for window in phase_windows if window["backend"] == mode and window["turn"] in turns]
                ids = {str(window["window"]) for window in selected}
                ticks = [sample for sample in samples if sample["window"] in ids]
                lengths = sorted(float(sample["tick_ms"]) for sample in ticks)
                elapsed = sum(w["elapsed_s"] for w in selected)
                main_cpu = [w["main_cpu_s"] for w in selected]
                metrics = {
                    "ticks": len(ticks), "tick_p99_ms": lengths[math.ceil(.99 * len(ticks)) - 1],
                    "cpu_s_per_20s": 20 * sum(w["cpu_s"] for w in selected) / elapsed,
                    "main_cpu_s_per_20s": 20 * sum(main_cpu) / elapsed if None not in main_cpu else None,
                    "gc_count": sum(w["gc_count"] for w in selected),
                    "gc_ms": sum(w["gc_ms"] for w in selected),
                    "background_pct_range": [min(w["background_pct"] for w in selected),
                                             max(w["background_pct"] for w in selected)],
                    "mobs": statistics.median(int(tick["mobs"]) for tick in ticks),
                    "median_upserts": statistics.median(int(tick["upserts"]) for tick in ticks),
                    "median_mob_checks": statistics.median(int(tick["mob_checks"]) for tick in ticks),
                    "median_notifications": statistics.median(int(tick["notifications"]) for tick in ticks),
                    "median_hash_lookups": statistics.median(int(tick["hash_lookups"]) for tick in ticks),
                    "removals": sum(int(tick["removals"]) for tick in ticks),
                }
                for field in ("total_ns", "tick_path_ns", "event_ns", "index_ns", "evaluation_ns"):
                    metrics[field.replace("_ns", "_us")] = statistics.median(int(tick[field]) for tick in ticks) / 1000
                if "thread_cpu_s" in selected[0]:
                    os_elapsed = sum(w["os_elapsed_s"] for w in selected)
                    metrics["os_cpu_s_per_20s"] = 20 * sum(w["os_cpu_s"] for w in selected) / os_elapsed
                    metrics["thread_cpu_s_per_20s"] = {
                        group: 20 * sum(w["thread_cpu_s"][group] for w in selected) / os_elapsed for group in CPU_GROUPS
                    }
                    metrics["os_elapsed_s"] = os_elapsed
                    metrics["max_thread_read_ms"] = max(w["thread_read_ms"] for w in selected)
                if has_packets:
                    metrics.update(packet_metrics(selected, metrics["thread_cpu_s_per_20s"]["netty"] / 20))
                report[phase][label][mode] = metrics
            if has_packets:
                report[phase].setdefault("netty_cpu_effects", {})[label] = packet_cpu_effects(
                    *(report[phase][label][mode] for mode in modes)
                )
    return report


def stop(process, console):
    if process.poll() is None:
        try:
            console.send("stop")
            process.wait(timeout=45)
        except (OSError, subprocess.TimeoutExpired):
            process.kill()
            process.wait(timeout=10)


def run(args, probe, backend, name, verify_dirty=False):
    game_port = free_port()
    work = args.output / name
    copy_fixture(args.seed, work, probe, game_port)
    print(f"{name} starting", flush=True)
    verify = backend == "verify"
    interleaved = backend == "interleaved"
    seconds, warmup = (10, 5) if verify else (args.seconds, 8)
    java = [str(args.java), "-Xms2048M", "-Xmx2048M", "-XX:+UseG1GC",
            f"-XX:ActiveProcessorCount={args.server_mask.bit_count()}", "-Dterminal.jline=false", "-Dterminal.ansi=false",
            f"-Dspica.shadow.backend={backend}", f"-Dspica.shadow.library={args.native}"]
    if args.movement or args.hook_cost:
        java.append(args.movement_agent)
        checks_dirty = verify_dirty or not args.hook_cost
        java.append("-Dspica.shadow.verifyDirty=true" if checks_dirty else "-Dspica.shadow.verifyDirect=true")
    if interleaved and args.hook_cost:
        java.append("-Dpaper.playerconnection.keepalive=120")
    if verify:
        java.append("-Xcheck:jni")
    java.extend(("-jar", "purpur-1.21.4.jar", "nogui"))
    console_file = work / "console.log"
    bot = None
    process_flags = subprocess.CREATE_NO_WINDOW
    if interleaved and args.hook_cost:
        process_flags |= subprocess.ABOVE_NORMAL_PRIORITY_CLASS
    with console_file.open("w", encoding="utf-8") as output, console_file.open(encoding="utf-8", errors="replace") as log:
        server = subprocess.Popen(java, cwd=work, stdin=subprocess.PIPE, stdout=output, stderr=subprocess.STDOUT,
                                  text=True, encoding="utf-8", creationflags=process_flags)
        console = Console(server, log, interleaved)
        cpu_sampler = None
        try:
            pin(server, args.server_mask)
            console.wait(f"SHADOW_READY {backend.upper()}", 120)
            console.wait(r"Done \(", 90)
            initial_warmup = 0 if verify else 12
            bot_duration = 3 * (seconds + warmup) + initial_warmup + 12
            if interleaved:
                turns = rotation(args)
                settle = DELTA_SETTLE_SECONDS if paired_modes(args) else 0
                scenario_duration = len(turns) * len(turns[0]) * (window_seconds(args) + settle) + 6 * len(turns[0])
                bot_duration = len(PHASES) * scenario_duration + 22
                if args.cpu_attribution:
                    cpu_sampler = JvmCpuSampler(server, args.java)
                    bot_duration += 12
            packet_mode = interleaved and args.packet_attribution
            if packet_mode:
                bot_duration += 180  # Safety deadline; the runner stops the bots after the last sample.
            bot_log = work / "bots.log"
            with ((work / "bots.json").open("w", encoding="utf-8") as out, bot_log.open("w", encoding="utf-8") as err,
                  bot_log.open(encoding="utf-8", errors="replace") as snapshots):
                bot = subprocess.Popen([str(args.bots), "--host", "127.0.0.1", "--port", str(game_port), "--bots", "20",
                                        "--prefix", "ShadowBot", "--warmup", "2", "--duration", str(bot_duration),
                                        "--stagger-ms", "60", "--min-ready-pct", "100", "--json"]
                                       + (["--snapshot-stdin"] if packet_mode else []), cwd=work, stdout=out,
                                       stdin=subprocess.PIPE if packet_mode else None, stderr=err, creationflags=process_flags)
                pin(bot, args.bot_mask)
                packet_sampler = PacketSampler(bot, snapshots, args.bot_mask, cpu_sampler) if packet_mode else None
                outstanding = {f"{index:03d}" for index in range(1, 21)}
                join_deadline = time.monotonic() + 60
                while outstanding:
                    joined = console.wait(r"ShadowBot(\d{3}) joined the game", join_deadline - time.monotonic())
                    outstanding.discard(joined.group(1))
                console.require_bots = True
                console.send("minecraft:gamemode spectator @a")
                console.wait(r"Set ShadowBot\d{3}'s game mode to Spectator Mode")
                if interleaved:
                    windows = interleave(console, args, work / "windows.json", cpu_sampler, packet_sampler)
                    if cpu_sampler:
                        (work / "thread-catalogue.json").write_text(json.dumps(cpu_sampler.catalogue(), indent=2), encoding="utf-8")
                else:
                    windows = []
                    for phase in PHASES:
                        print(f"{name} {phase.lower()}", flush=True)
                        place(console, phase)
                        time.sleep(warmup + (initial_warmup if phase == "DENSE" else 0))
                        windows.append(measure(console, phase, seconds, args.server_mask, backend))
                console.require_bots = False
                console.send("chunkshadow dump")
                console.wait(r"SHADOW_WRITTEN \d+")
                if packet_mode:
                    bot.stdin.write(b"finish\n")
                    bot.stdin.close()
                if bot.wait(timeout=45) != 0:
                    raise RuntimeError(f"Bot run failed in {work}")
        finally:
            if cpu_sampler:
                cpu_sampler.close()
            if bot and bot.poll() is None:
                bot.terminate()
                bot.wait(timeout=10)
            stop(server, console)
    if server.returncode != 0:
        raise RuntimeError(f"Purpur exited with {server.returncode} in {work}")
    bots = json.loads((work / "bots.json").read_text(encoding="utf-8"))
    if bots["readyPlay"] != 20 or bots["disconnectedEarly"] != 0:
        raise RuntimeError(f"Expected 20 ready bots and zero early drops in {work}")
    (work / "windows.json").write_text(json.dumps(windows, indent=2), encoding="utf-8")
    summarize_rows(work, windows, 10 * window_seconds(args) if interleaved else (60 if verify else 200), keep_short=interleaved)
    if verify and not any(window["removals"] > 0 for window in windows if window["phase"] == "DENSE"):
        raise RuntimeError(f"Lifecycle probe did not observe a removal in {work}")
    modes = paired_modes(args) or ORDERS[0]
    metrics = ({"windows": windows, "summary": interleaved_summary(work, windows, modes)} if interleaved
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
    comparison = parser.add_mutually_exclusive_group()
    comparison.add_argument("--incremental", action="store_true", help="Pair the current Rust pilot with the incremental pilot")
    comparison.add_argument("--movement", action="store_true", help="Pair incremental polling with the pinned test movement hook")
    comparison.add_argument("--hook-cost", action="store_true", help="Pair the original movement listener with direct slot lookup")
    comparison.add_argument("--cpu-attribution", action="store_true", help="Repeat the hook A/B with long windows and OS thread CPU")
    comparison.add_argument("--packet-attribution", action="store_true", help="Attribute Netty CPU to packet volume and cost")
    args = parser.parse_args()
    args.cpu_attribution = args.cpu_attribution or args.packet_attribution
    args.hook_cost = args.hook_cost or args.cpu_attribution
    if os.name != "nt" or not args.server_mask or not args.bot_mask or args.server_mask & args.bot_mask or args.seconds < 20:
        parser.error("Requires Windows, disjoint nonempty masks and at least twenty measurement seconds")
    args.output.mkdir(parents=True)
    probe = args.output / "ChunkTrackingShadow.jar"
    make_probe(probe, args.movement or args.hook_cost)
    if args.movement or args.hook_cost:
        args.movement_agent = make_movement_agent(args)
    raw = {"server_mask": args.server_mask, "bot_mask": args.bot_mask, "fixture": stable_fixture(args.seed), "rounds": []}
    if not args.skip_verify:
        raw["verify"] = run(args, probe, "verify", "verify")
        if args.packet_attribution:
            raw["verify_dirty"] = run(args, probe, "verify", "verify-dirty", verify_dirty=True)
        (args.output / "results.json").write_text(json.dumps(raw, indent=2), encoding="utf-8")
    if args.interleave or paired_modes(args):
        raw["interleaved"] = run(args, probe, "interleaved", "interleaved")
    for index, order in enumerate(ORDERS[:args.rounds], start=1):
        raw["rounds"].append({mode: run(args, probe, mode, f"round-{index}-{mode}") for mode in order})
        (args.output / "results.json").write_text(json.dumps(raw, indent=2), encoding="utf-8")
    if stable_fixture(args.seed) != raw["fixture"]:
        raise RuntimeError("Fixture changed during the benchmark")
    (args.output / "results.json").write_text(json.dumps(raw, indent=2), encoding="utf-8")


if __name__ == "__main__":
    main()
