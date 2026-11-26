from io import BytesIO
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from chunk_tracking_shadow import PacketSampler, packet_cpu_effects, packet_metrics


class PacketAttributionTest(unittest.TestCase):
    def test_snapshot_reads_line_split_across_file_appends(self):
        bot = SimpleNamespace(stdin=BytesIO(), poll=lambda: None)
        chunks = iter(["SPICA_SNAPSHOT 1 ", "", "20 14 150 12\n"])
        log = SimpleNamespace(readline=lambda: next(chunks))
        cpu_sampler = SimpleNamespace(process_cpu_s=lambda process: 3)
        with patch("chunk_tracking_shadow.cpu_counters", return_value=(5, 4)):
            snapshot = PacketSampler(bot, log, bot_mask=0xc0, cpu_sampler=cpu_sampler).snapshot()

        self.assertEqual(bot.stdin.getvalue(), b"snapshot\n")
        self.assertEqual((snapshot["packets"], snapshot["wire_bytes"], snapshot["entity_packets"]), (14, 150, 12))

    def test_packet_rates_use_elapsed_time_and_packet_weighted_bytes(self):
        common = {"packet_read_ms": 2, "packet_boundary_skew_ms": 3, "bot_background_pct": 4}
        report = packet_metrics([
            common | {"packets": 10, "packet_elapsed_s": 1, "wire_bytes": 120, "entity_packets": 8},
            common | {"packets": 30, "packet_elapsed_s": 4, "wire_bytes": 280, "entity_packets": 12},
        ], netty_cpu_rate=.4)

        self.assertEqual(report["packets_per_s"], 8)
        self.assertEqual(report["wire_bytes_per_packet"], 10)
        self.assertEqual(report["entity_packet_pct"], 50)
        self.assertEqual(report["netty_cpu_us_per_packet"], 50_000)

    def test_packet_and_cost_effects_sum_to_observed_netty_cpu_change(self):
        baseline = {"packets_per_s": 100_000, "netty_cpu_us_per_packet": 15}
        candidate = {"packets_per_s": 95_000, "netty_cpu_us_per_packet": 12}
        effects = packet_cpu_effects(baseline, candidate)

        self.assertAlmostEqual(effects["volume_cpu_s_per_20s"], -1.35)
        self.assertAlmostEqual(effects["cost_cpu_s_per_20s"], -5.85)
        self.assertAlmostEqual(sum(effects.values()), 22.8 - 30)
