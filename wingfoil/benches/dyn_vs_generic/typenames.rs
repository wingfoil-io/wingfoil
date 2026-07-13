// Demonstrates the "type explosion" that motivates type erasure (`dyn`).
// Each generic combinator wraps the previous type, so a pipeline's type grows
// with its length. `dyn` collapses all of them to one name.
//
// Not a registered cargo target (kept out of bench autodiscovery on purpose).
// Run standalone:  rustc -O benches/dyn_vs_generic/typenames.rs -o /tmp/tn && /tmp/tn

fn tn<T>(_: &T) -> &'static str { std::any::type_name::<T>() }

fn main() {
    let base = 0u64..10;

    // 1 stage
    let s1 = base.clone().map(|x| x + 1);
    println!("1 stage : {}\n", tn(&s1));

    // 3 stages
    let s3 = base.clone().map(|x| x + 1).filter(|x| x % 2 == 0).map(|x| x * 3);
    println!("3 stages: {}\n", tn(&s3));

    // 6 stages
    let s6 = base
        .clone()
        .map(|x| x + 1)
        .filter(|x| x % 2 == 0)
        .map(|x| x * 3)
        .map(|x| x ^ 7)
        .filter(|x| *x > 2)
        .map(|x| x.wrapping_mul(11));
    println!("6 stages: {}\n", tn(&s6));

    // The same stages, but type-erased after each step (what a `dyn`-based
    // fluent API returns). One stable name regardless of depth.
    let mut it: Box<dyn Iterator<Item = u64>> = Box::new(base);
    it = Box::new(it.map(|x| x + 1));
    it = Box::new(it.filter(|x| x % 2 == 0));
    it = Box::new(it.map(|x| x * 3));
    println!("erased  : {}", tn(&it));
}
