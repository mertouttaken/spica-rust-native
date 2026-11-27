"""Scale sampled stage-cycle shares by each Netty thread's measured CPU time."""

STAGES = ("encode", "compression", "flush", "event_loop", "queue", "buffer")
SAMPLE_WEIGHTS = (32, 1, 32, 16, 128, 128)
FILETIME_UNIX_OFFSET_MS = 11_644_473_600_000


def parse_stage_snapshot(line):
    epoch_ms, *records = line.split()
    threads = {}
    for record in records:
        thread, overhead, errors, *counters = map(int, record.split(":"))
        if thread in threads or len(counters) != 2 * len(STAGES) or min(counters) < 0:
            raise ValueError(f"Invalid Netty snapshot {record!r}")
        threads[thread] = {"cycles": counters[::2], "samples": counters[1::2], "errors": errors, "overhead": overhead}
    return int(epoch_ms), threads


def attribute_stages(start, finish, names, ticks_per_second):
    start_ms, before = start["stages"]
    finish_ms, after = finish["stages"]
    stage_cpu = dict.fromkeys(STAGES, 0.0)
    samples = dict.fromkeys(STAGES, 0)
    stage_cycles = dict.fromkeys(STAGES, 0)
    seen = set()
    zero = {"cycles": [0] * len(STAGES), "samples": [0] * len(STAGES), "errors": 0}
    for identity, end_cycles in finish["thread_cycles"].items():
        tid, creation = identity
        if not names.get(identity, "").startswith("Netty Server IO ") or tid not in after:
            continue
        if tid in seen:
            raise RuntimeError(f"Reused Netty thread id in a measurement window: {tid}")
        seen.add(tid)
        if identity in start["thread_cycles"]:
            begin_cycles = start["thread_cycles"][identity]
            begin_cpu = start["threads"][identity]
        elif creation >= start["filetime"]:
            begin_cycles = begin_cpu = 0
        else:
            raise RuntimeError(f"No opening CPU counters for Netty worker {tid}")
        previous = before.get(tid, zero)
        current = after[tid]
        if current["errors"] != previous["errors"]:
            raise RuntimeError(f"Thread cycle clock failed on Netty worker {tid}")
        total_cycles = end_cycles - begin_cycles
        cpu_s = (finish["threads"][identity] - begin_cpu) / ticks_per_second
        for i, stage in enumerate(STAGES):
            cycle_delta = (current["cycles"][i] - previous["cycles"][i]) * SAMPLE_WEIGHTS[i]
            sample_delta = current["samples"][i] - previous["samples"][i]
            if min(cycle_delta, sample_delta, total_cycles) < 0 or (cycle_delta and not total_cycles):
                raise RuntimeError(f"Invalid cycle delta for {stage} on Netty worker {tid}")
            stage_cycles[stage] += cycle_delta
            samples[stage] += sample_delta
            if total_cycles:
                stage_cpu[stage] += cpu_s * (cycle_delta / total_cycles)
    if set(after) - seen or not sum(samples.values()):
        raise RuntimeError("Incomplete Netty worker CPU or stage counters")
    max_skew = max(abs(s["filetime"] / 10_000 - FILETIME_UNIX_OFFSET_MS - epoch)
                   for s, epoch in ((start, start_ms), (finish, finish_ms)))
    return {"stage_cpu_s": stage_cpu, "stage_weighted_cycles": stage_cycles, "stage_samples": samples,
            "stage_boundary_skew_ms": max_skew}
