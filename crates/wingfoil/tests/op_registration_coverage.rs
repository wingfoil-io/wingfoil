//! Registration-coverage guard: an op in the catalog reaches **every** surface
//! it owes, or names the reason it does not.
//!
//! [`op_completeness`](op_completeness.rs) already guards the *engine* axis —
//! a combinator used inside a `nitro!` block only compiles if it has both a
//! fluent method and a `__wf_op_<name>_*` forwarder, so dropping either side
//! breaks that build. This file guards the axis that one cannot see: the
//! **fluent** and [`Signal`] facade surfaces, neither of which any compiler
//! checks for *absence*. Nothing fails to build when an op is simply never
//! registered — it just quietly does not exist for callers.
//!
//! That is not hypothetical. The same omission has landed three times, and
//! each time at *trait* granularity, which is why per-op review kept missing
//! it:
//!
//! 1. The `Signal` facade was hand-forwarded and fell **15 of 41** methods
//!    behind `StreamOps` (`signal.rs` module docs). Fixed by generating the
//!    forwarding — but generating it only means the macro *exists*; the
//!    invocation is still a line somebody has to write.
//! 2. `op_completeness.rs` was written against `StreamOps`, so the whole of
//!    `StatisticsOps` — 36 methods — sat in neither its `nitro!` blocks nor
//!    its allowlist for the surface's entire life.
//! 3. All 35 statistics ops are absent from the `Signal` facade to this day.
//!    That one turns out to be *principled* (see `signal_exempt` below) — but
//!    it was nowhere written down, so it read exactly like the first two.
//!
//! The test is a source scan rather than a compile-time trick because absence
//! is what it looks for, and absence does not generate a token to trip over.

/// The op catalog and the three files that publish it.
const OPS_RS: &str = include_str!("../src/ops.rs");
const FLUENT_RS: &str = include_str!("../src/fluent.rs");
const STATS_RS: &str = include_str!("../src/stats.rs");
const SIGNAL_RS: &str = include_str!("../src/signal.rs");

/// Every `#[op(build = <name> …)]` in the catalog, paired with whether that
/// attribute also carries the `fluent` flag.
fn catalog_ops() -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let mut rest = OPS_RS;
    while let Some(at) = rest.find("#[op(build") {
        rest = &rest[at..];
        let Some(end) = rest.find(")]") else { break };
        let attr = &rest[..end];
        rest = &rest[end..];
        let Some(eq) = attr.find('=') else { continue };
        let after = attr[eq + 1..].trim_start();
        let name: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            // `fluent` as a flag, not as a substring of some other token.
            let has_fluent = attr[eq + 1..].split(',').any(|tok| tok.trim() == "fluent");
            out.push((name, has_fluent));
        }
    }
    out
}

/// Does `src` invoke `macro_stem<name>!`, or declare the method by hand?
///
/// Both count as registered: an op whose fluent or `Signal` signature is not a
/// plain forward keeps a hand-written method on purpose (`with_time`,
/// `delay_with_reset`, `logged`), and this guard is about the surface
/// existing, not about how it was produced.
fn registered(src: &str, macro_stem: &str, name: &str) -> bool {
    if src.contains(&format!("{macro_stem}{name}!")) {
        return true;
    }
    // A hand-written `fn <name>(` / `fn <name><`, at a word boundary so
    // `join` never matches inside `join_passive`.
    let mut rest = src;
    while let Some(at) = rest.find(&format!("fn {name}")) {
        let after = &rest[at + 3 + name.len()..];
        if after.starts_with('(') || after.starts_with('<') {
            return true;
        }
        rest = &rest[at + 3 + name.len()..];
    }
    false
}

/// Ops exempt from the [`Signal`] facade, and why.
///
/// **The statistics surface is exempt, and the reason is structural rather
/// than an oversight.** `Signal`'s generated combinators are *inherent*
/// methods (`impl<T: 'static> Signal<T>` in `signal.rs`), whereas
/// `StatisticsOps` is deliberately kept **out of the prelude** so callers opt
/// in with `use wingfoil::stats::StatisticsOps;` (see `CLAUDE.md`, "Working
/// conventions"). An inherent method cannot be opted into — it is in scope on
/// the type unconditionally — so invoking `__wf_signal_<stat>!` would put all
/// 35 statistics combinators permanently on `Signal<f64>` and quietly overturn
/// that convention.
///
/// So the exemption is derived, not a hand-maintained name list: an op whose
/// fluent surface lives in `stats.rs` is exempt, and one whose surface lives in
/// `fluent.rs` is not. A new statistics op inherits the exemption; a new
/// `StreamOps` op inherits the obligation. A list of 35 names would have to be
/// edited to stay true, which is the failure mode this file exists to catch.
fn signal_exempt(name: &str) -> bool {
    registered(STATS_RS, "__wf_fluent_", name)
}

#[test]
fn every_fluent_op_has_a_fluent_surface() {
    let missing: Vec<_> = catalog_ops()
        .into_iter()
        .filter(|(_, fluent)| *fluent)
        .map(|(name, _)| name)
        .filter(|n| {
            !registered(FLUENT_RS, "__wf_fluent_", n) && !registered(STATS_RS, "__wf_fluent_", n)
        })
        .collect();
    assert!(
        missing.is_empty(),
        "these ops declare `#[op(build = …, fluent)]` but no fluent surface \
         invokes `__wf_fluent_<name>!` (and no method of that name is written \
         by hand) in `fluent.rs` or `stats.rs`, so they are unreachable for \
         callers: {missing:?}\n\
         Add the one-line invocation to the extension trait's `impl` block — \
         see `/new-op` step 4."
    );
}

#[test]
fn every_fluent_op_reaches_the_signal_facade() {
    let missing: Vec<_> = catalog_ops()
        .into_iter()
        .filter(|(_, fluent)| *fluent)
        .map(|(name, _)| name)
        .filter(|n| !registered(SIGNAL_RS, "__wf_signal_", n) && !signal_exempt(n))
        .collect();
    assert!(
        missing.is_empty(),
        "these ops declare `#[op(build = …, fluent)]` but never reach the \
         `Signal` facade — `__wf_signal_<name>!` is emitted for each and never \
         invoked, so `signal.rs` silently lags the catalog exactly as it did \
         when it fell 15 methods behind: {missing:?}\n\
         Add the one line to the generated `impl<T: 'static> Signal<T>` block — \
         see `/new-op` step 4b. A source is not registered this way (it enters \
         the facade as a free function); if this op is one, or its `Signal` \
         signature is genuinely not a plain forward, write the method by hand \
         instead."
    );
}

/// The guard is only worth its coverage — so pin the coverage itself.
///
/// Without this, `catalog_ops` silently returning nothing (a parse that
/// stopped matching after some future edit to the attribute spelling) would
/// make both tests above pass vacuously, which is precisely the shape of
/// failure they exist to prevent.
#[test]
fn the_scan_actually_finds_the_catalog() {
    let ops = catalog_ops();
    let fluent = ops.iter().filter(|(_, f)| *f).count();
    assert!(
        ops.len() > 60 && fluent > 60,
        "the source scan found only {} ops ({fluent} fluent) in `ops.rs`; it \
         has almost certainly stopped parsing the `#[op(build = …)]` \
         attribute rather than the catalog having shrunk by half. Fix the \
         scan — a vacuous pass here disables both guards in this file.",
        ops.len()
    );
    assert!(
        ops.iter().any(|(n, _)| n == "map"),
        "the scan did not find `map`, so it is not reading the catalog correctly"
    );
}
