"""POC: a wingfoil-next *compiled* graph called from Python.

`wingfoil.compiled_squares` runs a fixed, fully-monomorphized graph
(ticker -> count -> map(i*i) -> accumulate) via wingfoil-next's `compiled()`
static-schedule runner. `wingfoil.interpreted_squares` runs the *same* graph
through the dynamically dispatched interpreted engine. Both are generated from
one wiring definition, so they must agree exactly — this test asserts that and
prints a rough timing comparison.

This is deliberately narrow: ONE fixed graph, not general Python-defined-graph
codegen. See wingfoil-python/src/py_compiled.rs.
"""

import time
import unittest

import wingfoil


def _expected_squares(n):
    # count ticks 1..n, each squared
    return [i * i for i in range(1, n + 1)]


class TestCompiledGraph(unittest.TestCase):
    def test_compiled_matches_expected(self):
        out = wingfoil.compiled_squares(realtime=False, cycles=6)
        self.assertEqual(out, _expected_squares(6))

    def test_compiled_matches_interpreted(self):
        # The load-bearing property: both engines, generated from the same
        # wiring, produce identical output for the same RunMode/RunFor.
        for n in (1, 5, 20, 100):
            compiled = wingfoil.compiled_squares(realtime=False, cycles=n)
            interpreted = wingfoil.interpreted_squares(realtime=False, cycles=n)
            self.assertEqual(compiled, interpreted, f"engines disagree at n={n}")
            self.assertEqual(compiled, _expected_squares(n))

    def test_duration_bound(self):
        # A duration-bounded historical run (10ms period) also agrees.
        compiled = wingfoil.compiled_squares(realtime=False, duration=0.055)
        interpreted = wingfoil.interpreted_squares(realtime=False, duration=0.055)
        self.assertTrue(len(compiled) > 0)
        self.assertEqual(compiled, interpreted)

    def test_timing_comparison(self):
        # Nice-to-have: rough wall-clock comparison of the two engines over a
        # large historical run. NOT an assertion on speed. For this small,
        # ticker-bound graph the two timings sit close to each other: the
        # per-cycle cost is dominated by shared work (the kernel's ticker
        # scheduling and the `accumulate` Vec growth) plus the GIL
        # detach/reacquire, so the compiled path's inlined dispatch is a small
        # slice of the total and the ratio hovers around 1.0 with noise.
        # The load-bearing result of this POC is the exact-parity assertion
        # above, not the timing.
        n = 50_000
        runs = 5

        def bench(fn):
            best = float("inf")
            for _ in range(runs):
                t0 = time.perf_counter()
                fn(realtime=False, cycles=n)
                best = min(best, time.perf_counter() - t0)
            return best

        c = bench(wingfoil.compiled_squares)
        i = bench(wingfoil.interpreted_squares)
        ratio = i / c if c > 0 else float("nan")
        print(
            f"\n[compiled POC] {n} cycles x best-of-{runs} (release): "
            f"compiled={c * 1e3:.1f}ms interpreted={i * 1e3:.1f}ms "
            f"interpreted/compiled={ratio:.2f}x "
            f"(both engines run the identical graph; timing is roughly neutral "
            f"for this ticker-bound shape)"
        )
        # Correctness under the large run, too.
        self.assertEqual(
            wingfoil.compiled_squares(realtime=False, cycles=n),
            wingfoil.interpreted_squares(realtime=False, cycles=n),
        )


if __name__ == "__main__":
    unittest.main()
