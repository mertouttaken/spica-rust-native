import unittest

from chunk_tracking_shadow import stage_metrics
from netty_stage_cpu import FILETIME_UNIX_OFFSET_MS, attribute_stages, parse_stage_snapshot


class NettyStageCpuTest(unittest.TestCase):
    def test_scales_by_each_threads_cpu_per_cycle(self):
        first, second = (101, 1), (102, 1)
        base = FILETIME_UNIX_OFFSET_MS
        start = {"filetime": (base + 1_000) * 10_000, "threads": {first: 0, second: 0},
                 "thread_cycles": {first: 0, second: 0},
                 "stages": parse_stage_snapshot("1000 101:3:0:0:0:0:0:0:0 102:3:0:0:0:0:0:0:0")}
        finish = {"filetime": (base + 2_000) * 10_000, "threads": {first: 800, second: 250},
                  "thread_cycles": {first: 3_200, second: 3_200},
                  "stages": parse_stage_snapshot("1990 101:3:0:10:1:160:2:5:1 102:3:0:8:1:320:2:4:1")}

        report = attribute_stages(start, finish, {first: "Netty Server IO #1", second: "Netty Server IO #2"}, 1_000)
        self.assertAlmostEqual(report["stage_cpu_s"]["encode"], .1)
        self.assertAlmostEqual(report["stage_cpu_s"]["compression"], .065)
        self.assertAlmostEqual(report["stage_cpu_s"]["flush"], .05)
        self.assertEqual(report["stage_samples"], {"encode": 2, "compression": 4, "flush": 2})
        self.assertEqual(report["stage_boundary_skew_ms"], 10)

        summary = stage_metrics([report | {"os_elapsed_s": 1}], packet_rate=100_000, netty_cpu_per_20s=21)
        self.assertAlmostEqual(sum(summary["netty_stage_cpu_us_per_packet"].values()), 10.5)

    def test_rejects_duplicate_thread_snapshots(self):
        with self.assertRaises(ValueError):
            parse_stage_snapshot("1000 101:3:0:0:0:0:0:0:0 101:3:0:0:0:0:0:0:0")
