//! Python bindings for the execution layer (`wingfoil::adapters::execution`):
//! the OMS, parent orders, the test venue and the ceiling, the position fold,
//! reconciliation, the kill switch and the FIX replace chain.
//!
//! The layer is generic over a `Copy` instrument. Python has no such type, so
//! the binding runs it on [`Symbol`]: an instrument name of up to
//! [`SYMBOL_CAPACITY`] bytes, held inline. Every node's interior stays the
//! typed Rust node; only its edges convert.
//!
//! # Entry points
//!
//! | Python | Rust | Shape |
//! |---|---|---|
//! | `execution_oms(desired, reports, sweep, ...)` | `OmsOps::wire_oms` | `(requests, pacing)`, plus a reading with `reading=True` |
//! | `execution_algo(parents, landed, reports, clock)` | `AlgoOps::algo` | `(desired, progress)` |
//! | `execution_sim_venue(requests, touch, ...)` | `testing::SimVenue`, optionally inside `ceiling::Capped` | a `dict` of the session's seven streams |
//! | `execution_position(fills, measure, ...)` | `PositionOps::wire_position` | a `list` of positions per tick |
//! | `execution_reconcile(fills, holdings, sweep, measure, grace_nanos, ...)` | `ReconcileOps::wire_reconcile` | `(mismatches, desk)` |
//! | `execution_kill_switch(readings, assess, limits, ...)` | `KillSwitchOps::kill_switch` | an assessment `dict` per tick |
//! | `execution_replace_chain(requests, executions, mass_cancel=...)` | `FixOps::replace_chain` | `(messages, reports)` |
//! | `execution_replace_chain_sending(requests, executions, send, mass_cancel=...)` | `FixOps::replace_chain_sending` | `reports` |
//!
//! The order loop needs one feedback cut on the request wire, as in Rust.
//! `Graph.feedback()` and `Stream.feedback(sink)` are that cut.
//!
//! # Values
//!
//! A burst is a Python `list`; on the way in, a `list` or `tuple` is a burst
//! and anything else is a burst of one. A record is a `dict`, an enum is a
//! lowercase string tag, a time or a span is an `int` of nanoseconds, and a
//! price, quantity or amount of money is a `decimal.Decimal`. On the way in a
//! number may also be an `int`, a `str` or a `float`; a `float` is read by
//! its shortest round-trip text, so `0.1` is exactly `0.1`. More than nine
//! decimal places is refused, never rounded. Every record refuses a key it
//! does not know, so a misspelt optional field raises instead of being
//! ignored. The shapes are in each function's docstring.
//!
//! # What differs from the Rust surface
//!
//! There is no legacy twin, so nothing here is a deviation from legacy. These
//! are the places the Python surface is narrower or different from Rust's:
//!
//! 1. **One instrument type.** The Rust layer takes any `Copy` instrument;
//!    Python gets [`Symbol`], a short string.
//! 2. **Readings are fixed.** Rust's `reading(read)` takes a closure over the
//!    node's state. Python gets one reading per node instead: the OMS's
//!    counters, the position list, and a desk reading with each position's
//!    mark and PnL. The node state never crosses into Python.
//! 3. **Valuations and caps are tables, not closures.** A measure or a cap is
//!    one value for every instrument, or a `dict` keyed by instrument. A
//!    closure would be called inside the fold on every fill.
//! 4. **The kill switch's limits are names.** Rust takes a `Limit` enum
//!    with a `const ALL`. Python names its limits in a list of at most 64,
//!    and `assess` returns the names it found breached. `assess` cannot
//!    fail in Rust, so a raise, or a return naming an unknown limit, is
//!    logged and recorded as **every** limit breached. A cap nobody
//!    evaluated has not been respected, and the switch halts.
//! 5. **`replace_chain_sending`'s `send` cannot fail either.** A raise is
//!    logged and answered as a refusal with reason `"other"`.
//! 6. **The only venue is the test venue.** A `Venue` is implemented out of
//!    tree, in Rust, so the one Python can wire is `testing::SimVenue`. The
//!    binding feature turns on `execution-testing` for it.
//! 7. **`StanceOps::stance` is the assessment's `"stance"` key**, not a
//!    function of its own.
//!
//! # Tests
//!
//! The layer does no I/O, so every tier runs by default: the Rust marshaling
//! tests below and `tests/test_execution.py`. There is no marked tier and no
//! integration workflow.

use std::collections::BTreeMap;
use std::rc::Rc;

use anyhow::{Context, Result, anyhow, bail};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};
use wingfoil::adapters::execution::algo::{AlgoOps, Parent};
use wingfoil::adapters::execution::ceiling::{Cap, Capped};
use wingfoil::adapters::execution::edge::{
    Cashflow, Holdings, RejectReason, Report, Request, TradingState,
};
use wingfoil::adapters::execution::fix::{ExecReport, FixOps};
use wingfoil::adapters::execution::kill_switch::{
    Assessed, Breaches, ClearError, Day, KillSwitchOps, Latch, Limit, Snapshot, Stance, Switch,
};
use wingfoil::adapters::execution::oms::{Config, Desired, Lifetime, OmsOps, Passive};
use wingfoil::adapters::execution::order::Fill;
use wingfoil::adapters::execution::position::{Measure, Position, PositionOps};
use wingfoil::adapters::execution::rate_limit::{OrderRate, Ratio, Terms};
use wingfoil::adapters::execution::reconcile::node::Desk;
use wingfoil::adapters::execution::reconcile::{self, Mismatch, ReconcileOps};
use wingfoil::adapters::execution::testing::{Profile, SimVenue, Touch};
use wingfoil::adapters::execution::venue::Venue;
use wingfoil::adapters::market::{Px, Qty};
use wingfoil::prelude::{Stream as Typed, StreamOps};
use wingfoil::{Burst, NanoTime};

use crate::PyElement;
use crate::graph::PyStream;
use crate::python::Stream;

mod values;

use values::{
    FromPy, In, Obj, Out, ToPy, burst_from_py, burst_to_py, decimal, duration, measure, pnl_to,
    position_to, qty, reject_reason, time,
};
pub use values::{SYMBOL_CAPACITY, Symbol};

// ---------------------------------------------------------------------------
// The seams.
// ---------------------------------------------------------------------------

/// One value per tick, read off a Python stream.
fn input<T>(stream: &PyStream, what: &'static str) -> Typed<T>
where
    T: FromPy + Clone + Default + 'static,
{
    stream
        .typed_input::<PyElement>()
        .try_map(move |e: &PyElement| {
            Python::attach(|py| {
                let obj = e.value();
                T::from_py(obj.bind(py)).with_context(|| format!("{what} input"))
            })
        })
}

/// One burst per tick, read off a Python stream.
fn input_burst<T>(stream: &PyStream, what: &'static str) -> Typed<Burst<T>>
where
    T: FromPy + Clone + Default + 'static,
{
    stream
        .typed_input::<PyElement>()
        .try_map(move |e: &PyElement| {
            Python::attach(|py| {
                let obj = e.value();
                burst_from_py(obj.bind(py)).with_context(|| format!("{what} input"))
            })
        })
}

/// A tick, whatever the value: the sweep, a clock, a cancel-all, a clear.
fn input_tick(stream: &PyStream) -> Typed<()> {
    stream.typed_input::<PyElement>().map(|_: &PyElement| ())
}

/// Each tick's value, built into Python, on `anchor`'s graph.
fn output<T>(anchor: &PyStream, typed: &Typed<T>) -> Stream
where
    T: ToPy + 'static,
{
    let erased = typed.try_map(|v: &T| Python::attach(|py| Ok(PyElement::from(v.to_py(py)?))));
    Stream::from(anchor.erased_output(erased))
}

/// Each tick's burst, built into a Python `list`, on `anchor`'s graph.
fn output_burst<T>(anchor: &PyStream, typed: &Typed<Burst<T>>) -> Stream
where
    T: ToPy + Clone + Default + 'static,
{
    let erased =
        typed.try_map(|b: &Burst<T>| Python::attach(|py| Ok(PyElement::from(burst_to_py(py, b)?))));
    Stream::from(anchor.erased_output(erased))
}

fn obj_err(err: anyhow::Error) -> PyErr {
    PyValueError::new_err(format!("{err:#}"))
}

fn read_arg<T>(
    obj: &Bound<'_, PyAny>,
    what: &str,
    read: impl Fn(&Obj<'_>) -> Result<T>,
) -> PyResult<T> {
    read(obj).with_context(|| what.to_string()).map_err(obj_err)
}

// ---------------------------------------------------------------------------
// The OMS.
// ---------------------------------------------------------------------------

/// What `execution_oms(..., reading=True)` emits: the OMS's counters after
/// each diff.
#[derive(Clone, Copy, Default)]
struct OmsReading {
    refused: u64,
    rejected: u64,
    adjusted: u64,
    cancel_all_refused: u64,
    unrouted: u64,
    trading: TradingState,
    ratio: Option<(u64, u64)>,
}

impl ToPy for OmsReading {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(Out::new(py)
            .set("refused", self.refused)?
            .set("rejected", self.rejected)?
            .set("adjusted", self.adjusted)?
            .set("cancel_all_refused", self.cancel_all_refused)?
            .set("unrouted", self.unrouted)?
            .set("trading", self.trading.to_py(py)?)?
            .set("ratio", self.ratio)?
            .done())
    }
}

fn terms(stated: Option<(u32, u32)>) -> Terms {
    stated.map_or(Terms::UNMETERED, |(rate, burst)| {
        Terms::per_second(rate, burst)
    })
}

#[allow(clippy::too_many_arguments)]
fn oms_config(
    rate: Option<(u32, u32)>,
    cancel_all_rate: Option<(u32, u32)>,
    headroom: Option<f64>,
    ratio: Option<(u32, u32)>,
    passive: Option<&str>,
    lifetime: Option<&str>,
    max_desired_age_nanos: Option<u64>,
    retake_nanos: Option<u64>,
    min_requote: Option<Px>,
) -> Result<Config> {
    let mut config = Config::unmetered();
    if rate.is_none() && cancel_all_rate.is_none() {
        if headroom.is_some() {
            bail!("headroom needs a rate or a cancel_all_rate");
        }
    } else {
        let rate = OrderRate::new(terms(rate), terms(cancel_all_rate), headroom.unwrap_or(1.0))
            .map_err(|e| anyhow!("rate: {e}"))?;
        config = config.with_rate(rate);
    }
    if let Some((messages_per_fill, free)) = ratio {
        config = config.with_ratio(Ratio {
            messages_per_fill,
            free,
        });
    }
    if let Some(passive) = passive {
        config = config.with_passive(match passive {
            "post_only" => Passive::PostOnly,
            "limit" => Passive::Limit,
            other => bail!("unknown passive {other:?}; expected \"post_only\" or \"limit\""),
        });
    }
    if let Some(lifetime) = lifetime {
        config = config.with_lifetime(match lifetime {
            "gtc" => Lifetime::GoodTillCancel,
            "day" => Lifetime::Day,
            other => bail!("unknown lifetime {other:?}; expected \"gtc\" or \"day\""),
        });
    }
    if let Some(age) = max_desired_age_nanos {
        config = config.with_max_desired_age(duration(age));
    }
    if let Some(retake) = retake_nanos {
        config = config.with_retake(duration(retake));
    }
    if let Some(min) = min_requote {
        config = config.with_min_requote(min);
    }
    Ok(config)
}

/// The OMS: closes the gap between what the strategy wants showing and what
/// is working.
///
/// - `desired`: a stream of desireds, one `dict` or a `list` of them per
///   tick: `{"instrument": str, "as_of": int, "bids": [(price, qty), ...],
///   "asks": [...], "intent": "rest" | "cross" | "trigger", "trigger":
///   {...}, "reduce_only": bool}`. Only `instrument` and `as_of` are
///   required. Ladders are best first, up to eight levels. A cross carries
///   one level on one side. A `"trigger"` intent carries one level and a
///   trigger `{"kind": "stop" | "take", "reference": "mark" | "index" |
///   "last", "price": ...}`; with no levels and no trigger it withdraws the
///   triggered decision. A key absent from a burst wants what it wanted last
///   time; `max_desired_age_nanos` withdraws a silent one.
/// - `reports`: the venue's reports, as `execution_sim_venue` emits them.
/// - `sweep`: any ticking stream; the OMS re-plans on it. Required.
/// - `trading`: optional stream of `"open"`, `"auction"`, `"halted"` or
///   `"closed"`. While not open, only cancels go out.
/// - `cancel_all`: optional stream; each tick pulls everything first.
/// - `epoch`: the process's id epoch, `0` for a backtest.
/// - `rate` and `cancel_all_rate`: the venue's stated `(per_second, burst)`
///   limits. Both absent is unmetered; one absent is unmetered on that side.
///   `headroom` is the fraction of them spent, `1.0` by default.
/// - `ratio`: a message-to-fill limit, `(messages_per_fill, free)`.
/// - `passive`: `"post_only"` (default) or `"limit"`; `lifetime`: `"gtc"`
///   (default) or `"day"`.
/// - `max_desired_age_nanos` (default 5s), `retake_nanos` (default 1s) and
///   `min_requote` (default 0) as in the Rust `Config`.
///
/// Returns `(requests, pacing)`. Requests tick as a `list` only on a cycle
/// that sent any: `{"type": "place", "id", "instrument", "side", "qty",
/// "price", "kind", "tif", "reduce_only", "trigger", "created"}`, `{"type":
/// "amend", "order", "instrument", "price", "qty", "trigger"}`, `{"type":
/// "cancel", "order"}` or `{"type": "cancel_all"}`. Pacing is `{"deferred",
/// "superseded", "waiting"}`. With `reading=True` a third stream carries
/// the OMS's counters after each diff. Feed the requests back through one
/// `Graph.feedback()` cut before they reach the venue. Bad arguments raise
/// here; a bad value aborts the run.
#[pyfunction]
#[pyo3(signature = (
    desired, reports, sweep, *, epoch=0, rate=None, cancel_all_rate=None, headroom=None,
    ratio=None, passive=None, lifetime=None, max_desired_age_nanos=None, retake_nanos=None,
    min_requote=None, trading=None, cancel_all=None, reading=false
))]
#[allow(clippy::too_many_arguments)]
pub fn execution_oms<'py>(
    py: Python<'py>,
    desired: PyRef<'_, Stream>,
    reports: PyRef<'_, Stream>,
    sweep: PyRef<'_, Stream>,
    epoch: u32,
    rate: Option<(u32, u32)>,
    cancel_all_rate: Option<(u32, u32)>,
    headroom: Option<f64>,
    ratio: Option<(u32, u32)>,
    passive: Option<&str>,
    lifetime: Option<&str>,
    max_desired_age_nanos: Option<u64>,
    retake_nanos: Option<u64>,
    min_requote: Option<Bound<'py, PyAny>>,
    trading: Option<PyRef<'_, Stream>>,
    cancel_all: Option<PyRef<'_, Stream>>,
    reading: bool,
) -> PyResult<Bound<'py, PyTuple>> {
    let min_requote = min_requote
        .map(|m| read_arg(&m, "min_requote", values::px))
        .transpose()?;
    let config = oms_config(
        rate,
        cancel_all_rate,
        headroom,
        ratio,
        passive,
        lifetime,
        max_desired_age_nanos,
        retake_nanos,
        min_requote,
    )
    .map_err(obj_err)?;
    let epoch = values::epoch(epoch).map_err(obj_err)?;
    let anchor = desired.object();
    let desired = input_burst::<Desired<Symbol>>(anchor, "execution_oms desired");
    let reports = input_burst::<Report<Symbol>>(reports.object(), "execution_oms reports");
    let mut wiring = desired
        .wire_oms(config, epoch, &reports)
        .sweep(&input_tick(sweep.object()));
    if let Some(trading) = trading {
        wiring = wiring.trading(&input::<TradingState>(
            trading.object(),
            "execution_oms trading",
        ));
    }
    if let Some(cancel_all) = cancel_all {
        wiring = wiring.cancel_all(&input_tick(cancel_all.object()));
    }
    let streams = if reading {
        let (requests, pacing, counters) = wiring
            .reading(|oms| OmsReading {
                refused: oms.refused(),
                rejected: oms.rejected(),
                adjusted: oms.adjusted(),
                cancel_all_refused: oms.cancel_all_refused(),
                unrouted: oms.unrouted(),
                trading: oms.trading_state(),
                ratio: oms.ratio(),
            })
            .build();
        vec![
            output_burst(anchor, &requests),
            output(anchor, &pacing),
            output(anchor, &counters),
        ]
    } else {
        let (requests, pacing) = wiring.build();
        vec![output_burst(anchor, &requests), output(anchor, &pacing)]
    };
    PyTuple::new(py, streams)
}

// ---------------------------------------------------------------------------
// Parent orders.
// ---------------------------------------------------------------------------

/// Parent orders: a quantity worked over time, above the OMS.
///
/// - `parents`: a stream of parents, one `dict` or a `list` per tick:
///   `{"instrument", "side": "bid" | "ask", "total", "cap", "by": int,
///   "style": "twap" | "iceberg" | "sweep", "clips": int (twap only),
///   "show": qty (iceberg only), "reduce_only": bool}`. `cap` is the
///   iceberg's price. A new parent on an instrument and side replaces the
///   live one.
/// - `landed`: the requests as they land, after the feedback cut.
/// - `reports`: the venue's reports.
/// - `clock`: any ticking stream; the beat the parents decide on.
///
/// Returns `(desired, progress)`: desireds for `execution_oms`, and a `list`
/// of `{"parent", "start", "filled", "remaining", "clips", "status",
/// "at"}` per tick that moved a parent. `status` is `"working"`,
/// `"filled"`, `"lapsed"`, `"replaced"` or `"refused"`.
#[pyfunction]
pub fn execution_algo(
    parents: PyRef<'_, Stream>,
    landed: PyRef<'_, Stream>,
    reports: PyRef<'_, Stream>,
    clock: PyRef<'_, Stream>,
) -> (Stream, Stream) {
    let anchor = parents.object();
    let parents = input_burst::<Parent<Symbol>>(anchor, "execution_algo parents");
    let landed = input_burst::<Request<Symbol>>(landed.object(), "execution_algo landed");
    let reports = input_burst::<Report<Symbol>>(reports.object(), "execution_algo reports");
    let (desired, progress) = parents.algo(&landed, &reports, &input_tick(clock.object()));
    (
        output_burst(anchor, &desired),
        output_burst(anchor, &progress),
    )
}

// ---------------------------------------------------------------------------
// The test venue and the ceiling.
// ---------------------------------------------------------------------------

/// The largest order on each contract.
#[derive(Clone, Debug)]
enum TableCap {
    Every(Qty),
    Each(BTreeMap<Symbol, Qty>),
}

impl Cap<Symbol> for TableCap {
    fn of(&self, instrument: &Symbol) -> Option<Qty> {
        match self {
            TableCap::Every(cap) => Some(*cap),
            TableCap::Each(caps) => caps.get(instrument).copied(),
        }
    }
}

fn table_cap(obj: &Obj<'_>) -> Result<TableCap> {
    match obj.cast::<PyDict>() {
        Ok(dict) => dict
            .iter()
            .map(|(instrument, cap)| {
                let instrument = values::symbol(&instrument)?;
                Ok((
                    instrument,
                    qty(&cap).with_context(|| format!("cap for {instrument}"))?,
                ))
            })
            .collect::<Result<_>>()
            .map(TableCap::Each),
        Err(_) => qty(obj).map(TableCap::Every),
    }
}

/// The test venue: a FIX-shaped harness that matches against a touch you
/// state, with unlimited depth behind it. It is not a model of any venue.
///
/// - `requests`: the requests as they land, after the feedback cut.
/// - `touch`: a stream of `{"instrument", "bid", "ask"}`, one or a `list`
///   per tick. `bid` and `ask` are optional.
/// - `post_only` and `mass_cancel`: whether the venue takes each; both
///   `True` by default. Without one, it refuses it.
/// - `epoch`: the epoch the venue's FIX ids are minted in.
/// - `trading`: optional stream of trading states for a session venue.
/// - `cap`: optional ceiling, the largest order as one quantity for every
///   instrument or a `dict` of instrument to quantity. A place or amend
///   over it, or on an instrument the dict leaves out, aborts the run.
///
/// Returns a `dict` of the session's streams: `"reports"` (a `list` of
/// `{"type": "ack" | "reject" | "fill" | "cancelled" | "expired" |
/// "triggered", ...}`), `"settlements"` (fills), `"account"`,
/// `"holdings"`, `"cashflows"`, `"mmp"` and `"trading"`. The test venue
/// states only the reports, and the trading state it was handed.
#[pyfunction]
#[pyo3(signature = (requests, touch, *, post_only=true, mass_cancel=true, epoch=0, trading=None, cap=None))]
#[allow(clippy::too_many_arguments)]
pub fn execution_sim_venue<'py>(
    py: Python<'py>,
    requests: PyRef<'_, Stream>,
    touch: PyRef<'_, Stream>,
    post_only: bool,
    mass_cancel: bool,
    epoch: u32,
    trading: Option<PyRef<'_, Stream>>,
    cap: Option<Bound<'py, PyAny>>,
) -> PyResult<Bound<'py, PyDict>> {
    let cap = cap.map(|c| read_arg(&c, "cap", table_cap)).transpose()?;
    let epoch = values::epoch(epoch).map_err(obj_err)?;
    let anchor = requests.object();
    let requests = input_burst::<Request<Symbol>>(anchor, "execution_sim_venue requests");
    let touch = input_burst::<Touch<Symbol>>(touch.object(), "execution_sim_venue touch");
    let mut venue = SimVenue::new(
        Profile {
            post_only,
            mass_cancel,
        },
        epoch,
        &touch,
    );
    if let Some(trading) = trading {
        venue = venue.with_trading(&input::<TradingState>(
            trading.object(),
            "execution_sim_venue trading",
        ));
    }
    let session = match cap {
        Some(cap) => Capped::new(&venue, cap).wire(&requests),
        None => venue.wire(&requests),
    };
    let out = PyDict::new(py);
    out.set_item("reports", output_burst(anchor, &session.reports))?;
    out.set_item("settlements", output_burst(anchor, &session.settlements))?;
    out.set_item("account", output(anchor, &session.account))?;
    out.set_item("holdings", output(anchor, &session.holdings))?;
    out.set_item("cashflows", output_burst(anchor, &session.cashflows))?;
    out.set_item("mmp", output_burst(anchor, &session.mmp))?;
    out.set_item("trading", output(anchor, &session.trading))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Positions and reconciliation.
// ---------------------------------------------------------------------------

/// The caller's valuation, as a table.
#[derive(Clone)]
enum Valuation {
    Nothing,
    Every(Measure),
    Each(BTreeMap<Symbol, Measure>),
}

impl Valuation {
    fn of(&self, instrument: &Symbol) -> Option<Measure> {
        match self {
            Valuation::Nothing => None,
            Valuation::Every(m) => Some(*m),
            Valuation::Each(table) => table.get(instrument).copied(),
        }
    }
}

fn valuation(obj: &Obj<'_>) -> Result<Valuation> {
    if obj.is_none() {
        return Ok(Valuation::Nothing);
    }
    match obj.cast::<PyDict>() {
        Ok(dict) => dict
            .iter()
            .map(|(instrument, spec)| {
                let instrument = values::symbol(&instrument)?;
                Ok((
                    instrument,
                    measure(&spec).with_context(|| format!("measure for {instrument}"))?,
                ))
            })
            .collect::<Result<_>>()
            .map(Valuation::Each),
        Err(_) => measure(obj).map(Valuation::Every),
    }
}

/// The position fold: a book of fills keyed by instrument.
///
/// - `fills`: a stream of fills, one `dict` or a `list` per tick, each as a
///   report's `"fill"` (the `"type"` key may be left out).
/// - `measure`: how each instrument's PnL runs. `"linear"`, `"inverse"`, a
///   contract multiplier, or a `dict` of instrument to one of those; an
///   instrument the dict leaves out is folded but not valued. `None` values
///   nothing, and must be passed explicitly.
/// - `settlements` and `cashflows`: optional streams; a cashflow is
///   `{"instrument", "amount", "venue_time", "recv_time"}`.
///
/// Returns a stream of the book every cycle the node runs: a `list` of
/// `{"instrument", "measure", "net", "cost", "realised", "fees",
/// "cashflows", "entry"}` in instrument order. Realised is gross of fees.
#[pyfunction]
#[pyo3(signature = (fills, measure, *, settlements=None, cashflows=None))]
pub fn execution_position(
    fills: PyRef<'_, Stream>,
    measure: Bound<'_, PyAny>,
    settlements: Option<PyRef<'_, Stream>>,
    cashflows: Option<PyRef<'_, Stream>>,
) -> PyResult<Stream> {
    let valuation = read_arg(&measure, "measure", valuation)?;
    let anchor = fills.object();
    let fills = input_burst::<Fill<Symbol>>(anchor, "execution_position fills");
    let mut wiring = fills.wire_position(move |i: &Symbol| valuation.of(i));
    if let Some(s) = settlements {
        wiring = wiring.settlements(&input_burst(s.object(), "execution_position settlements"));
    }
    if let Some(c) = cashflows {
        wiring = wiring.cashflows(&input_burst::<Cashflow<Symbol>>(
            c.object(),
            "execution_position cashflows",
        ));
    }
    Ok(output(anchor, &wiring.build()))
}

/// What `execution_reconcile` reads off the desk after each reconcile.
#[derive(Clone, Default)]
struct DeskReading {
    heard: bool,
    disagreeing: usize,
    rebased: u64,
    unvalued: u64,
    positions: Vec<(Position<Symbol>, Option<Px>)>,
    mismatches: Vec<Mismatch<Symbol>>,
}

fn read_desk(desk: &Desk<Symbol>) -> DeskReading {
    let (book, reconciler) = (desk.book(), desk.reconciler());
    DeskReading {
        heard: reconciler.has_heard(),
        disagreeing: reconciler.disagreeing(),
        rebased: reconciler.rebased(),
        unvalued: book.unvalued(),
        positions: book
            .iter()
            .map(|p| (*p, desk.mark(&p.instrument)))
            .collect(),
        mismatches: desk.mismatches().to_vec(),
    }
}

impl ToPy for DeskReading {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        let positions = self
            .positions
            .iter()
            .map(|(p, mark)| {
                let pnl = mark.and_then(|m| p.pnl(m));
                Ok(position_to(py, Out::new(py), p)?
                    .set("mark", values::opt_decimal(py, *mark)?)?
                    .set("pnl", pnl_to(py, pnl)?)?
                    .done())
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Out::new(py)
            .set("heard", self.heard)?
            .set("disagreeing", self.disagreeing)?
            .set("rebased", self.rebased)?
            .set("unvalued", self.unvalued)?
            .set("positions", positions)?
            .set("mismatches", self.mismatches.to_py(py)?)?
            .done())
    }
}

/// Reconciliation: the position fold, checked against the venue's
/// holdings. The venue is the truth: a difference that outlives the grace
/// is a mismatch, and the fold is re-based on the venue.
///
/// - `fills`: as for `execution_position`.
/// - `holdings`: a stream of `{"whole": bool, "held": {instrument: net},
///   "as_of": int}`, one or a `list` per tick. With `whole`, anything not
///   named is flat.
/// - `sweep`: any ticking stream; the grace is judged on it. Required.
/// - `measure`: as for `execution_position`.
/// - `grace_nanos`: how long a difference may stand before it is a
///   mismatch.
/// - `settlements`, `cashflows`: optional, as for `execution_position`.
/// - `marks`: optional stream of `{instrument: price}` dicts, the price a
///   re-base is booked at and each position is valued at.
///
/// Returns `(mismatches, desk)`. Mismatches tick as a `list` of
/// `{"instrument", "ours", "venue", "since"}`. The desk ticks every cycle:
/// `{"heard", "disagreeing", "rebased", "unvalued", "positions",
/// "mismatches"}`, each position carrying `"mark"` and `"pnl"` (`None`
/// without a mark or a measure) beside the `execution_position` fields.
/// PnL is per position; currencies are never summed.
#[pyfunction]
#[pyo3(signature = (fills, holdings, sweep, measure, grace_nanos, *, settlements=None, cashflows=None, marks=None))]
#[allow(clippy::too_many_arguments)]
pub fn execution_reconcile(
    fills: PyRef<'_, Stream>,
    holdings: PyRef<'_, Stream>,
    sweep: PyRef<'_, Stream>,
    measure: Bound<'_, PyAny>,
    grace_nanos: u64,
    settlements: Option<PyRef<'_, Stream>>,
    cashflows: Option<PyRef<'_, Stream>>,
    marks: Option<PyRef<'_, Stream>>,
) -> PyResult<(Stream, Stream)> {
    let valuation = read_arg(&measure, "measure", valuation)?;
    let anchor = fills.object();
    let fills = input_burst::<Fill<Symbol>>(anchor, "execution_reconcile fills");
    let holdings =
        input_burst::<Holdings<Symbol>>(holdings.object(), "execution_reconcile holdings");
    let mut wiring = fills
        .wire_reconcile(
            reconcile::Config {
                grace: duration(grace_nanos),
            },
            move |i: &Symbol| valuation.of(i),
            &holdings,
        )
        .sweep(&input_tick(sweep.object()));
    if let Some(s) = settlements {
        wiring = wiring.settlements(&input_burst(s.object(), "execution_reconcile settlements"));
    }
    if let Some(c) = cashflows {
        wiring = wiring.cashflows(&input_burst::<Cashflow<Symbol>>(
            c.object(),
            "execution_reconcile cashflows",
        ));
    }
    if let Some(m) = marks {
        let marks = m
            .object()
            .typed_input::<PyElement>()
            .try_map(|e: &PyElement| {
                Python::attach(|py| {
                    let obj = e.value();
                    values::marks(obj.bind(py)).context("execution_reconcile marks input")
                })
            });
        wiring = wiring.marks(&marks);
    }
    let (mismatches, desk) = wiring.reading(read_desk).build();
    Ok((output_burst(anchor, &mismatches), output(anchor, &desk)))
}

// ---------------------------------------------------------------------------
// The kill switch.
// ---------------------------------------------------------------------------

/// One of the caller's named limits, by its index in the names list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot(u8);

const SLOTS: [Slot; 64] = {
    let mut slots = [Slot(0); 64];
    let mut i = 0;
    while i < 64 {
        slots[i] = Slot(i as u8);
        i += 1;
    }
    slots
};

impl Limit for Slot {
    const ALL: &'static [Slot] = &SLOTS;

    fn index(self) -> u32 {
        u32::from(self.0)
    }
}

/// The caller's limit names, in the order given.
#[derive(Clone)]
struct Limits(Rc<Vec<String>>);

impl Limits {
    fn new(names: Vec<String>) -> Result<Limits> {
        if names.is_empty() {
            bail!("limits: name at least one");
        }
        if names.len() > SLOTS.len() {
            bail!(
                "limits: at most {} are held, got {}",
                SLOTS.len(),
                names.len()
            );
        }
        for (i, name) in names.iter().enumerate() {
            if name.is_empty() {
                bail!("limits: name {i} is empty");
            }
            if names[..i].contains(name) {
                bail!("limits: {name:?} is named twice");
            }
        }
        Ok(Limits(Rc::new(names)))
    }

    fn every(&self) -> Breaches<Slot> {
        SLOTS[..self.0.len()].iter().copied().collect()
    }

    /// The breaches an iterable of names states. A bare `str` is refused:
    /// it would read as one limit per character.
    fn parse(&self, obj: &Obj<'_>) -> Result<Breaches<Slot>> {
        if obj.is_instance_of::<PyString>() {
            bail!("expected an iterable of limit names, got a str");
        }
        let mut breaches = Breaches::NONE;
        for name in obj
            .try_iter()
            .map_err(|_| anyhow!("expected an iterable of limit names"))?
        {
            let name: String = name?
                .extract()
                .map_err(|_| anyhow!("a limit name is not a str"))?;
            let index =
                self.0.iter().position(|n| *n == name).ok_or_else(|| {
                    anyhow!("unknown limit {name:?}; the limits are {:?}", self.0)
                })?;
            breaches.set(SLOTS[index]);
        }
        Ok(breaches)
    }

    fn names(&self, breaches: Breaches<Slot>) -> Vec<&str> {
        breaches
            .iter()
            .map(|slot| self.0[slot.0 as usize].as_str())
            .collect()
    }
}

/// The switch as `assess` sees it: `daily_loss` and the day's opening
/// equity. A copy, written back once `assess` returns.
#[pyclass(name = "KillSwitch", unsendable)]
pub struct PyKillSwitch(Switch<Slot>);

#[pymethods]
impl PyKillSwitch {
    /// The fraction of the day's opening equity given back, positive being a
    /// loss, and the amount: `(fraction, Decimal)`. The first equity of a day
    /// anchors it. `None` where `equity` is `None` or no fraction is a number,
    /// which the caller should record as a breach.
    fn daily_loss<'py>(
        &mut self,
        py: Python<'py>,
        now: u64,
        equity: Option<Bound<'py, PyAny>>,
    ) -> PyResult<Option<(f64, Bound<'py, PyAny>)>> {
        let equity = equity.map(|e| read_arg(&e, "equity", qty)).transpose()?;
        self.0
            .daily_loss(NanoTime::from(now), equity)
            .map(|(fraction, lost)| Ok((fraction, decimal(py, lost).map_err(obj_err)?)))
            .transpose()
    }

    /// The day's opening equity, once a day is anchored.
    #[getter]
    fn opening_equity<'py>(&self, py: Python<'py>) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.0
            .opening_equity()
            .map(|e| decimal(py, e).map_err(obj_err))
            .transpose()
    }
}

fn snapshot_from(limits: &Limits, obj: &Obj<'_>) -> Result<Snapshot<Slot>> {
    let d = In::new(obj, "snapshot", &["day", "latch", "standing"])?;
    let day = d.maybe("day", |day| {
        let day = In::new(day, "snapshot day", &["window", "opening"])?;
        Ok(Day {
            window: day.get("window", |w| Ok(w.extract::<u64>()?))?,
            opening: day.get("opening", qty)?,
        })
    })?;
    let latch = d.maybe("latch", |latch| {
        let latch = In::new(latch, "snapshot latch", &["at", "breaches"])?;
        Ok(Latch {
            at: latch.get("at", time)?,
            breaches: latch.get("breaches", |b| limits.parse(b))?,
        })
    })?;
    Ok(Snapshot {
        day,
        latch,
        standing: d
            .maybe("standing", |s| limits.parse(s))?
            .unwrap_or(Breaches::NONE),
    })
}

fn snapshot_to<'py>(py: Python<'py>, limits: &Limits, s: &Snapshot<Slot>) -> Result<Obj<'py>> {
    let day = match s.day {
        None => py.None().into_bound(py),
        Some(day) => Out::new(py)
            .set("window", day.window)?
            .set("opening", decimal(py, day.opening)?)?
            .done(),
    };
    let latch = match s.latch {
        None => py.None().into_bound(py),
        Some(latch) => Out::new(py)
            .set("at", u64::from(latch.at))?
            .set("breaches", limits.names(latch.breaches))?
            .done(),
    };
    Ok(Out::new(py)
        .set("day", day)?
        .set("latch", latch)?
        .set("standing", limits.names(s.standing))?
        .done())
}

fn assessed_to<'py>(py: Python<'py>, limits: &Limits, a: &Assessed<Slot>) -> Result<Obj<'py>> {
    let cleared = match a.cleared {
        None => py.None().into_bound(py),
        Some(Ok(())) => true.into_pyobject(py)?.to_owned().into_any(),
        Some(Err(ClearError::StillBreached(standing))) => Out::new(py)
            .set("still_breached", limits.names(standing))?
            .done(),
    };
    Ok(Out::new(py)
        .set(
            "stance",
            match a.stance {
                Stance::Open => "open",
                Stance::Halted => "halted",
            },
        )?
        .set("standing", limits.names(a.standing))?
        .set("latched", limits.names(a.latched))?
        .set("latched_since", a.latched_since.map(u64::from))?
        .set("opening_equity", values::opt_decimal(py, a.opening_equity)?)?
        .set("cleared", cleared)?
        .set("snapshot", snapshot_to(py, limits, &a.snapshot)?)?
        .done())
}

/// The kill switch: a latch over your named limits.
///
/// - `readings`: a stream of whatever `assess` reads, one value or a `list`
///   of them per tick. Each is assessed and recorded, in order.
/// - `assess`: `assess(switch, now, reading)` returns the names of the limits
///   the reading breaches, as a list or set. `switch.daily_loss(now,
///   equity)` measures the day's loss against its opening equity. Read
///   every cap so a value that is not a number is outside it. A raise, or a
///   name not in `limits`, is logged and recorded as every limit breached.
/// - `limits`: the limit names, at most 64.
/// - `clear`: optional stream; each tick is the operator's clear, refused
///   while a breach stands.
/// - `day_start_nanos`: where days start, into the UTC day.
/// - `restored`: an earlier assessment's `"snapshot"`, carried across a
///   restart so it does not clear the latch.
///
/// Returns a stream of `{"stance": "open" | "halted", "standing",
/// "latched", "latched_since", "opening_equity", "cleared", "snapshot"}`
/// every cycle the switch runs. A breach latches until an operator's clear.
/// `cleared` is `None`, `True`, or `{"still_breached": [...]}` for a refusal.
/// The stance before the first assessment is `"halted"`.
#[pyfunction]
#[pyo3(signature = (readings, assess, limits, *, clear=None, day_start_nanos=0, restored=None))]
pub fn execution_kill_switch(
    readings: PyRef<'_, Stream>,
    assess: Bound<'_, PyAny>,
    limits: Vec<String>,
    clear: Option<PyRef<'_, Stream>>,
    day_start_nanos: u64,
    restored: Option<Bound<'_, PyAny>>,
) -> PyResult<Stream> {
    if !assess.is_callable() {
        return Err(PyValueError::new_err("assess: expected a callable"));
    }
    let limits = Limits::new(limits).map_err(obj_err)?;
    let restored = restored
        .map(|r| read_arg(&r, "restored", |o| snapshot_from(&limits, o)))
        .transpose()?;
    let anchor = readings.object();
    let readings = anchor.typed_burst_input::<PyElement>();
    let clear = match clear {
        Some(clear) => input_tick(clear.object()),
        None => readings.filter_map(|_: &Burst<PyElement>| None::<()>),
    };
    let callable = assess.unbind();
    let judge = limits.clone();
    let assessed = readings.kill_switch(
        &clear,
        duration(day_start_nanos),
        restored,
        move |switch: &mut Switch<Slot>, now: NanoTime, reading: &PyElement| {
            let judged = Python::attach(|py| -> Result<Breaches<Slot>> {
                let view = Py::new(py, PyKillSwitch(switch.clone()))?;
                let found = callable.call1(py, (view.clone_ref(py), u64::from(now), reading.value()))?;
                *switch = view.borrow(py).0.clone();
                judge.parse(found.bind(py))
            });
            judged.unwrap_or_else(|err| {
                log::error!(
                    "execution_kill_switch: assess failed at {now}, recording every limit as breached: {err:#}"
                );
                judge.every()
            })
        },
    );
    let erased = assessed.try_map(move |a: &Assessed<Slot>| {
        Python::attach(|py| Ok(PyElement::from(assessed_to(py, &limits, a)?)))
    });
    Ok(Stream::from(anchor.erased_output(erased)))
}

// ---------------------------------------------------------------------------
// The FIX replace chain.
// ---------------------------------------------------------------------------

/// The FIX replace chain: the OMS's requests in FIX's shape, each with its
/// own `ClOrdId`, and the venue's execution reports back as the OMS's
/// reports. The OMS never learns a venue id.
///
/// - `requests`: the requests as they land, after the feedback cut.
/// - `executions`: a stream of execution reports, one or a `list` per tick:
///   `{"cl_ord_id": int, "orig": int | None, "kind": "new" | "replaced" |
///   "canceled" | "expired" | "rejected" | "trade", "venue_time": int,
///   ...}`. `"replaced"` adds `price` and `qty`; `"canceled"` and
///   `"expired"` add `remaining`; `"rejected"` adds `reason`; `"trade"` adds
///   `exec_id`, `instrument`, `side`, `qty`, `filled`, `remaining`, `price`
///   and `liquidity`.
/// - `mass_cancel`: whether the venue takes a mass cancel. Without one, a
///   cancel-all goes out per order. Required: it is the venue's.
/// - `epoch`: the epoch the `ClOrdId`s are minted in.
///
/// Returns `(messages, reports)`. Messages tick as a `list` of `{"type":
/// "new" | "replace" | "cancel" | "mass_cancel", "cl_ord_id", ...}`.
/// Reports are as `execution_sim_venue` emits them.
#[pyfunction]
#[pyo3(signature = (requests, executions, *, mass_cancel, epoch=0))]
pub fn execution_replace_chain(
    requests: PyRef<'_, Stream>,
    executions: PyRef<'_, Stream>,
    mass_cancel: bool,
    epoch: u32,
) -> PyResult<(Stream, Stream)> {
    let epoch = values::epoch(epoch).map_err(obj_err)?;
    let anchor = requests.object();
    let requests = input_burst::<Request<Symbol>>(anchor, "execution_replace_chain requests");
    let executions = input_burst::<ExecReport<Symbol>>(
        executions.object(),
        "execution_replace_chain executions",
    );
    let (messages, reports) = requests.replace_chain(mass_cancel, epoch, &executions);
    Ok((
        output_burst(anchor, &messages),
        output_burst(anchor, &reports),
    ))
}

/// The FIX replace chain, sending from the node: `send(message, now)` is
/// called for each message as it is minted, and returns `None` once it is
/// sent or a reject reason string if the transport refused it. A refusal
/// is answered to the OMS in the same instant. A raise is logged and
/// answered as a refusal with reason `"other"`.
///
/// Arguments are as for `execution_replace_chain`. Returns the reports.
#[pyfunction]
#[pyo3(signature = (requests, executions, send, *, mass_cancel, epoch=0))]
pub fn execution_replace_chain_sending(
    requests: PyRef<'_, Stream>,
    executions: PyRef<'_, Stream>,
    send: Bound<'_, PyAny>,
    mass_cancel: bool,
    epoch: u32,
) -> PyResult<Stream> {
    if !send.is_callable() {
        return Err(PyValueError::new_err("send: expected a callable"));
    }
    let epoch = values::epoch(epoch).map_err(obj_err)?;
    let anchor = requests.object();
    let requests =
        input_burst::<Request<Symbol>>(anchor, "execution_replace_chain_sending requests");
    let executions = input_burst::<ExecReport<Symbol>>(
        executions.object(),
        "execution_replace_chain_sending executions",
    );
    let send = send.unbind();
    let reports = requests.replace_chain_sending(mass_cancel, epoch, &executions, move |message, now| {
        let sent = Python::attach(|py| -> Result<Option<RejectReason>> {
            let answer = send.call1(py, (message.to_py(py)?, u64::from(now)))?;
            let answer = answer.bind(py);
            if answer.is_none() {
                Ok(None)
            } else {
                reject_reason(answer).map(Some)
            }
        });
        match sent {
            Ok(None) => Ok(()),
            Ok(Some(reason)) => Err(reason),
            Err(err) => {
                log::error!(
                    "execution_replace_chain_sending: send failed at {now}, refusing the message: {err:#}"
                );
                Err(RejectReason::Other)
            }
        }
    });
    Ok(output_burst(anchor, &reports))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits::new(vec!["loss".into(), "mismatch".into(), "gross".into()]).expect("three names")
    }

    #[test]
    fn limit_names_are_checked_at_wiring() {
        assert!(Limits::new(vec![]).is_err());
        assert!(Limits::new(vec!["a".into(), "a".into()]).is_err());
        assert!(Limits::new(vec![String::new()]).is_err());
        assert!(Limits::new((0..65).map(|i| i.to_string()).collect()).is_err());
        assert!(Limits::new((0..64).map(|i| i.to_string()).collect()).is_ok());
        assert_eq!(
            limits().names(limits().every()),
            ["loss", "mismatch", "gross"]
        );
    }

    #[test]
    fn assess_names_breaches_by_name() {
        Python::initialize();
        Python::attach(|py| {
            let limits = limits();
            let read = |code: &str| {
                limits.parse(
                    &py.eval(&std::ffi::CString::new(code).expect("no NUL"), None, None)
                        .expect("eval"),
                )
            };
            assert_eq!(
                limits.names(read("['gross', 'loss']").expect("list")),
                ["loss", "gross"]
            );
            assert_eq!(
                limits.names(read("{'mismatch'}").expect("set")),
                ["mismatch"]
            );
            assert!(read("set()").expect("empty").is_empty());
            assert!(read("'loss'").is_err());
            assert!(read("['lost']").is_err());
            assert!(read("None").is_err());
        });
    }

    #[test]
    fn a_snapshot_round_trips_through_python() {
        Python::initialize();
        Python::attach(|py| {
            let limits = limits();
            let mut breaches = Breaches::NONE;
            breaches.set(SLOTS[1]);
            let snapshot = Snapshot {
                day: Some(Day {
                    window: 3,
                    opening: Qty::parse("1000").expect("ok"),
                }),
                latch: Some(Latch {
                    at: NanoTime::from(7),
                    breaches,
                }),
                standing: breaches,
            };
            let obj = snapshot_to(py, &limits, &snapshot).expect("builds");
            assert_eq!(snapshot_from(&limits, &obj).expect("reads back"), snapshot);
        });
    }

    #[test]
    fn the_oms_config_starts_unmetered_and_states_what_differs() {
        let unmetered =
            oms_config(None, None, None, None, None, None, None, None, None).expect("unmetered");
        assert_eq!(unmetered, Config::unmetered());
        let stated = oms_config(
            Some((50, 50)),
            Some((4, 4)),
            Some(0.5),
            Some((10, 5)),
            Some("limit"),
            Some("day"),
            Some(60),
            Some(2),
            None,
        )
        .expect("stated");
        assert_eq!(stated.passive, Passive::Limit);
        assert_eq!(stated.lifetime, Lifetime::Day);
        assert_eq!(stated.rate.headroom(), 0.5);
        assert!(oms_config(None, None, Some(0.5), None, None, None, None, None, None).is_err());
        assert!(
            oms_config(
                Some((50, 50)),
                None,
                Some(2.0),
                None,
                None,
                None,
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            oms_config(
                None,
                None,
                None,
                None,
                Some("aggressive"),
                None,
                None,
                None,
                None
            )
            .is_err()
        );
        assert!(
            oms_config(
                None,
                None,
                None,
                None,
                None,
                Some("forever"),
                None,
                None,
                None
            )
            .is_err()
        );
    }
}
