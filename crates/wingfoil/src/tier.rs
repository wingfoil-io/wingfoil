//! [`Tier`]: which of a `nitro!` module's engines executes the graph.
//!
//! A `nitro!` block emits both an interpreted runner and a fully monomorphized
//! compiled one from the same tokens, but their entry points have different
//! shapes — `interpreted()` hands back a [`Runner`](crate::interp::Runner) plus
//! handles you read *after* running, while `compiled(run_mode, run_for)`
//! returns the output values directly. That difference is what stops a caller
//! from swapping engines with a flag.
//!
//! `Tier` closes it: the macro also emits
//! `run(tier, run_mode, run_for) -> Result<(Out, ..)>`, which resolves to
//! either engine and returns the same tuple either way. The intended
//! development workflow — interpreted while debugging (per-node error context,
//! `instrument-*` spans, dynamic graph surgery), compiled in production — is
//! then one argument rather than two call shapes.

use std::str::FromStr;

/// Which of a `nitro!` module's engines executes the graph.
///
/// Passed to the generated `run(tier, run_mode, run_for)`. The two tiers are
/// required to agree on values *and* tick times — that is what the parity
/// tests assert — so this only ever changes *how* the graph runs, never what
/// it produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// The dirty-list engine built through `wire`. Slower, and the one to
    /// develop against: it carries per-node error context, honours the
    /// `instrument-*` span features, and supports dynamic graph surgery.
    Interpreted,
    /// The monomorphized straight-line schedule. Every `Op::cycle` — closures
    /// included — is visible to the optimizer across node boundaries.
    Compiled,
}

impl Tier {
    /// The environment variable [`Tier::preferred`] consults: `WINGFOIL_TIER`.
    pub const ENV: &'static str = "WINGFOIL_TIER";

    /// The tier named by `WINGFOIL_TIER`, or `None` when it is unset.
    ///
    /// An unrecognised value is a mistake worth hearing about but not worth
    /// aborting a run over, so it logs a warning (through the `log` crate,
    /// target `"wingfoil"`) and returns `None` rather than failing. Use
    /// [`Tier::from_str`] directly if you want the error.
    pub fn from_env() -> Option<Tier> {
        let raw = std::env::var(Self::ENV).ok()?;
        match raw.parse() {
            Ok(tier) => Some(tier),
            Err(e) => {
                log::warn!(target: "wingfoil", "{}={raw:?}: {e} — falling back to the default tier", Self::ENV);
                None
            }
        }
    }

    /// The tier to use when the caller has no opinion: `WINGFOIL_TIER` if it
    /// names one, otherwise [`Interpreted`](Tier::Interpreted) in a debug
    /// build and [`Compiled`](Tier::Compiled) in a release build.
    ///
    /// Both engines are compiled into the binary regardless, so the
    /// environment variable flips tiers **without a rebuild** — which is the
    /// point of it: a release binary misbehaving in the field can be re-run
    /// once under `WINGFOIL_TIER=interpreted` to get node-labelled errors and
    /// engine spans out of the same executable.
    ///
    /// The `debug_assertions` fallback is evaluated where this crate was
    /// compiled, not the caller's crate. Cargo applies one profile across a
    /// build, so the two agree under any standard configuration; pass a `Tier`
    /// explicitly if you are doing something unusual with per-package
    /// profiles.
    pub fn preferred() -> Tier {
        Self::from_env().unwrap_or(if cfg!(debug_assertions) {
            Tier::Interpreted
        } else {
            Tier::Compiled
        })
    }
}

/// Delegates to [`Tier::preferred`] — so `run(Tier::default(), ..)` picks up
/// `WINGFOIL_TIER` and the build profile. Note that this reads the
/// environment; construct the variant directly to pin a tier.
impl Default for Tier {
    fn default() -> Self {
        Tier::preferred()
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tier::Interpreted => "interpreted",
            Tier::Compiled => "compiled",
        })
    }
}

/// Case-insensitive: `"interpreted"` / `"compiled"`.
impl FromStr for Tier {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Tier> {
        match s.trim().to_ascii_lowercase().as_str() {
            "interpreted" => Ok(Tier::Interpreted),
            "compiled" => Ok(Tier::Compiled),
            other => Err(anyhow::anyhow!(
                "unknown tier {other:?} (expected \"interpreted\" or \"compiled\")"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_spellings_case_insensitively() {
        assert_eq!("interpreted".parse::<Tier>().unwrap(), Tier::Interpreted);
        assert_eq!("  Compiled ".parse::<Tier>().unwrap(), Tier::Compiled);
        assert!("nitro".parse::<Tier>().is_err());
    }

    #[test]
    fn display_round_trips_through_from_str() {
        for tier in [Tier::Interpreted, Tier::Compiled] {
            assert_eq!(tier.to_string().parse::<Tier>().unwrap(), tier);
        }
    }
}
