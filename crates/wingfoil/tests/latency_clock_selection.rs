//! Pin the context-to-clock choice separately from StageSet's read-count tests.
//! A supplied older cycle snap makes the two paths distinguishable even when
//! consecutive fresh clock reads are equal; no sleep or clock advance is needed.

use wingfoil::latency::{StampAll, StampAllEach, Traced, latency_stages};
use wingfoil::op::{Ctx, Op, Tick};
use wingfoil::runtime::time_queue::TimeQueue;
use wingfoil::{NanoTime, burst};

latency_stages! {
    pub ClockLatency { first, second, untouched }
}

type Msg = Traced<u64, ClockLatency>;
type Stages = (clock_latency::first, clock_latency::second);

#[test]
fn stamp_all_selects_the_requested_context_clock() {
    for mut precise in [false, true] {
        let before = u64::from(NanoTime::now());
        let snap = before.checked_sub(1).unwrap();
        let mut queue = TimeQueue::new();
        let mut ctx = Ctx::nested(
            NanoTime::ZERO,
            NanoTime::new(snap),
            NanoTime::ZERO,
            &mut queue,
            0,
        );
        let input = Msg::new(7);
        let Tick::Value(output) =
            StampAll::<Msg, Stages>::cycle(&mut precise, &mut (), (&input,), &mut ctx).unwrap()
        else {
            panic!("stamping must forward the payload with a tick");
        };
        let after = u64::from(NanoTime::now());

        assert_eq!(output.payload, 7);
        assert_eq!(output.latency.untouched, 0);
        assert_eq!(input.latency.first, 0);
        assert_eq!(input.latency.second, 0);
        for stamp in [output.latency.first, output.latency.second] {
            if precise {
                assert!(
                    (before..=after).contains(&stamp),
                    "precise stamp {stamp} must use a fresh read, not cycle snap {snap}",
                );
            } else {
                assert_eq!(stamp, snap, "cycle mode must use the supplied snap");
            }
        }
    }
}

#[test]
fn stamp_all_each_selects_the_requested_context_clock() {
    for mut precise in [false, true] {
        let before = u64::from(NanoTime::now());
        let snap = before.checked_sub(1).unwrap();
        let mut queue = TimeQueue::new();
        let mut ctx = Ctx::nested(
            NanoTime::ZERO,
            NanoTime::new(snap),
            NanoTime::ZERO,
            &mut queue,
            0,
        );
        let input = burst![Msg::new(7), Msg::new(8), Msg::new(9)];
        let Tick::Value(output) =
            StampAllEach::<Msg, Stages>::cycle(&mut precise, &mut (), (&input,), &mut ctx).unwrap()
        else {
            panic!("stamping must forward the whole burst with a tick");
        };
        let after = u64::from(NanoTime::now());

        assert_eq!(
            output.iter().map(|m| m.payload).collect::<Vec<_>>(),
            [7, 8, 9]
        );
        for (original, stamped) in input.iter().zip(output.iter()) {
            assert_eq!(original.latency.first, 0);
            assert_eq!(original.latency.second, 0);
            assert_eq!(stamped.latency.untouched, 0);
            assert_eq!(stamped.latency.first, output[0].latency.first);
            assert_eq!(stamped.latency.second, output[0].latency.second);
            for stamp in [stamped.latency.first, stamped.latency.second] {
                if precise {
                    assert!(
                        (before..=after).contains(&stamp),
                        "precise stamp {stamp} must use a fresh read, not cycle snap {snap}",
                    );
                } else {
                    assert_eq!(stamp, snap, "cycle mode must use the supplied snap");
                }
            }
        }
    }
}
