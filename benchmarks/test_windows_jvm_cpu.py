import unittest

from windows_jvm_cpu import attribute_cpu, cpu_group, hotspot_threads


class WindowsJvmCpuTest(unittest.TestCase):
    def test_hotspot_names_and_thread_groups(self):
        names = hotspot_threads('''"Server thread" #31 prio=5 nid=0x103 runnable
"GC Thread#0" os_prio=0 nid=788 runnable
"G1 Conc#0" os_prio=0 nid=0xc8 runnable
"Netty Server IO #2" #65 nid=0xabc waiting on condition
"C2 CompilerThread0" #13 nid=0xfe3 runnable
"Paper Common Worker #0" #145 nid=0xDD runnable
JNI global refs: 74
''')
        self.assertEqual({tid: cpu_group(name) for tid, name in names.items()}, {
            259: "main", 788: "gc", 200: "gc", 2748: "netty", 4067: "jit", 221: "paper_workers",
        })
        self.assertEqual(cpu_group("VM Thread"), "other")

    def test_split_includes_new_threads_and_conserves_process_time(self):
        main, old_gc, new_netty, missing = (10, 1), (20, 1), (20, 200), (30, 1)
        start = {"filetime": 100, "process": 10_000_000, "threads": {main: 3_000_000, old_gc: 1_000_000}}
        finish = {"process": 22_000_000,
                  "threads": {main: 7_000_000, old_gc: 3_000_000, new_netty: 1_000_000, missing: 9_000_000}}
        total, parts = attribute_cpu(start, finish, {
            main: "Server thread", old_gc: "GC Thread#0", new_netty: "Netty Server IO #0", missing: "G1 Conc#0",
        })

        self.assertAlmostEqual(total, 1.2)
        self.assertAlmostEqual(sum(parts.values()), total)
        self.assertAlmostEqual(parts["main"], .4)
        self.assertAlmostEqual(parts["gc"], .2)
        self.assertAlmostEqual(parts["netty"], .1)
        self.assertAlmostEqual(parts["other"], .5)
