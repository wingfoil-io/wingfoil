//! The execution vocabulary at the Python edge.
//!
//! Every value crosses as plain Python data: a `dict` with string tags for
//! enums, a `list` for a burst, `decimal.Decimal` for a price, a quantity or
//! an amount of money, and an `int` of nanoseconds for a time. Nothing here
//! holds a `Py<PyAny>` past the call that built it.
//!
//! Marshaling fails loudly. A missing key, an unknown key, a wrong type or a
//! value the Rust constructor refuses raises with the field named. Nothing is
//! defaulted silently, except the keys the module docs list as optional.

use std::fmt;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::sync::PyOnceLock;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};
use wingfoil::adapters::execution::algo::{Parent, Progress, Status, Style};
use wingfoil::adapters::execution::edge::{
    Account, Ack, Amend, Cashflow, Fired, Held, Holdings, MmpTrip, Reject, RejectReason, Report,
    Request, Retired, TradingState,
};
use wingfoil::adapters::execution::exec_id::{ExecId, VenueId};
use wingfoil::adapters::execution::fix::{ClOrdId, ExecKind, ExecReport, Message, Trade};
use wingfoil::adapters::execution::oms::{Desired, Intent, Ladder, Pacing};
use wingfoil::adapters::execution::order::{
    ClientOrderId, Epoch, Fill, Liquidity, Order, OrderKind, Reference, TimeInForce, Trigger,
    TriggerKind,
};
use wingfoil::adapters::execution::position::{Measure, Pnl, Position};
use wingfoil::adapters::execution::reconcile::Mismatch;
use wingfoil::adapters::execution::testing::Touch;
use wingfoil::adapters::market::{Level, Px, Qty, Side};
use wingfoil::{Burst, NanoTime};

pub(crate) type Obj<'py> = Bound<'py, PyAny>;

// ---------------------------------------------------------------------------
// The instrument.
// ---------------------------------------------------------------------------

/// The longest instrument name a [`Symbol`] holds, in bytes.
pub const SYMBOL_CAPACITY: usize = 23;

/// The instrument type the Python surface runs the execution layer on.
///
/// The layer is generic over a `Copy` instrument and Python has none, so
/// the binding picks one: a short string held inline. It is `Copy`, so the
/// vocabulary stays a plain value on every edge, and it orders as the string
/// does, so the layer's keyed state walks in name order. `Default` is the
/// empty name, which a Python value can never produce.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Symbol {
    // `bytes` before `len`, and NUL refused, so the derived order is the
    // string's own.
    bytes: [u8; SYMBOL_CAPACITY],
    len: u8,
}

impl Symbol {
    /// Read an instrument name, refusing what a `Symbol` cannot hold.
    pub fn new(name: &str) -> Result<Symbol> {
        if name.is_empty() {
            bail!("an instrument name is empty");
        }
        if name.len() > SYMBOL_CAPACITY {
            bail!(
                "instrument name {name:?} is {} bytes; at most {SYMBOL_CAPACITY} are held",
                name.len()
            );
        }
        if name.contains('\0') {
            bail!("instrument name {name:?} contains a NUL");
        }
        let mut bytes = [0u8; SYMBOL_CAPACITY];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        Ok(Symbol {
            bytes,
            len: name.len() as u8,
        })
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..self.len as usize])
            .expect("invariant: a Symbol holds the bytes of a &str")
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.as_str())
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// The two directions.
// ---------------------------------------------------------------------------

/// A value Python can hand the layer.
pub trait FromPy: Sized {
    /// Read one value, naming what was wrong with it.
    fn from_py(obj: &Obj<'_>) -> Result<Self>;
}

/// A value the layer hands Python.
pub trait ToPy {
    /// Build the Python form.
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>>;
}

/// A burst from Python: a `list` or `tuple` is the same-instant group, one
/// member per item, and anything else is a burst of one.
pub fn burst_from_py<T: FromPy + Default>(obj: &Obj<'_>) -> Result<Burst<T>> {
    let mut burst = Burst::default();
    if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
        for (i, item) in obj.try_iter()?.enumerate() {
            burst.push(T::from_py(&item?).with_context(|| format!("burst item {i}"))?);
        }
    } else {
        burst.push(T::from_py(obj)?);
    }
    Ok(burst)
}

/// A burst to Python: a `list`, one item per member, in order.
pub fn burst_to_py<'py, T: ToPy>(py: Python<'py>, burst: &[T]) -> Result<Obj<'py>> {
    let items = burst
        .iter()
        .map(|item| item.to_py(py))
        .collect::<Result<Vec<_>>>()?;
    Ok(PyList::new(py, items)?.into_any())
}

impl<T: ToPy> ToPy for Vec<T> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        burst_to_py(py, self)
    }
}

// ---------------------------------------------------------------------------
// Dicts.
// ---------------------------------------------------------------------------

/// A `dict` from Python, read field by field.
pub struct In<'a, 'py> {
    dict: &'a Bound<'py, PyDict>,
    what: &'static str,
}

impl<'a, 'py> In<'a, 'py> {
    /// `obj` as a dict of `what`, refusing a key outside `keys`, which
    /// catches a misspelt optional field that would otherwise be ignored.
    pub fn new(obj: &'a Obj<'py>, what: &'static str, keys: &[&str]) -> Result<Self> {
        let dict = obj
            .cast::<PyDict>()
            .map_err(|_| anyhow!("{what}: expected a dict, got {}", type_name(obj)))?;
        for key in dict.keys() {
            let key: String = key
                .extract()
                .map_err(|_| anyhow!("{what}: a key is not a str"))?;
            if !keys.contains(&key.as_str()) {
                bail!("{what}: unknown key {key:?}; the keys are {keys:?}");
            }
        }
        Ok(In { dict, what })
    }

    /// The value at `key`, which must be present and not `None`.
    pub fn req(&self, key: &str) -> Result<Obj<'py>> {
        self.opt(key)?
            .ok_or_else(|| anyhow!("{}: missing {key:?}", self.what))
    }

    /// The value at `key`, `None` where it is absent or `None`.
    pub fn opt(&self, key: &str) -> Result<Option<Obj<'py>>> {
        Ok(self.dict.get_item(key)?.filter(|v| !v.is_none()))
    }

    /// Read `key` with `read`, naming the field on failure.
    pub fn get<T>(&self, key: &str, read: impl Fn(&Obj<'py>) -> Result<T>) -> Result<T> {
        read(&self.req(key)?).with_context(|| format!("{}: {key:?}", self.what))
    }

    /// Read `key` with `read` where present.
    pub fn maybe<T>(&self, key: &str, read: impl Fn(&Obj<'py>) -> Result<T>) -> Result<Option<T>> {
        self.opt(key)?
            .map(|v| read(&v).with_context(|| format!("{}: {key:?}", self.what)))
            .transpose()
    }
}

/// A `dict` for Python, built field by field.
pub struct Out<'py>(Bound<'py, PyDict>);

impl<'py> Out<'py> {
    /// An empty dict.
    pub fn new(py: Python<'py>) -> Self {
        Out(PyDict::new(py))
    }

    /// Set `key` to `value`.
    pub fn set<V>(self, key: &str, value: V) -> Result<Self>
    where
        V: IntoPyObject<'py>,
    {
        self.0.set_item(key, value)?;
        Ok(self)
    }

    /// The dict.
    pub fn done(self) -> Obj<'py> {
        self.0.into_any()
    }
}

fn type_name(obj: &Obj<'_>) -> String {
    obj.get_type()
        .name()
        .map(|n| n.to_string())
        .unwrap_or_else(|_| "?".to_string())
}

// ---------------------------------------------------------------------------
// Scalars.
// ---------------------------------------------------------------------------

static DECIMAL: PyOnceLock<Py<PyType>> = PyOnceLock::new();

fn decimal_type(py: Python<'_>) -> Result<&Bound<'_, PyType>> {
    Ok(DECIMAL.import(py, "decimal", "Decimal")?)
}

/// The plain decimal text of a price or quantity from Python.
///
/// A `Decimal`, an `int` or a `str` is read exactly, and a `float` by its
/// shortest round-trip text, so `0.1` is `0.1`. A `bool` is refused rather
/// than read as `0` or `1`. The text then goes through the fixed-point
/// parse, which refuses more than nine decimal places rather than rounding.
fn decimal_text(obj: &Obj<'_>) -> Result<String> {
    let py = obj.py();
    let decimal = decimal_type(py)?;
    if obj.is_instance_of::<PyBool>() {
        bail!("expected a number, got a bool");
    }
    let value = if obj.is_instance_of::<PyFloat>() {
        decimal.call1((obj.repr()?,))?
    } else if obj.is_instance_of::<PyInt>()
        || obj.is_instance_of::<PyString>()
        || obj.is_instance(decimal)?
    {
        decimal
            .call1((obj,))
            .map_err(|e| anyhow!("{obj} is not a number: {e}"))?
    } else {
        bail!(
            "expected a Decimal, int, float or str number, got {}",
            type_name(obj)
        );
    };
    Ok(value.call_method1("__format__", ("f",))?.extract()?)
}

/// A price.
pub fn px(obj: &Obj<'_>) -> Result<Px> {
    let text = decimal_text(obj)?;
    Px::parse(&text).with_context(|| format!("{text} is not a price"))
}

/// A quantity, or an amount of money.
pub fn qty(obj: &Obj<'_>) -> Result<Qty> {
    let text = decimal_text(obj)?;
    Qty::parse(&text).with_context(|| format!("{text} is not a quantity"))
}

/// A price or quantity as a `decimal.Decimal`, exactly.
pub fn decimal<'py>(py: Python<'py>, value: impl fmt::Display) -> Result<Obj<'py>> {
    Ok(decimal_type(py)?.call1((value.to_string(),))?)
}

pub fn opt_decimal<'py>(py: Python<'py>, value: Option<impl fmt::Display>) -> Result<Obj<'py>> {
    match value {
        Some(v) => decimal(py, v),
        None => Ok(py.None().into_bound(py)),
    }
}

/// An engine time: an `int` of nanoseconds.
pub fn time(obj: &Obj<'_>) -> Result<NanoTime> {
    let nanos: u64 = int(obj).context("expected an int of nanoseconds")?;
    Ok(NanoTime::from(nanos))
}

fn nanos(t: NanoTime) -> u64 {
    u64::from(t)
}

fn opt_nanos(t: Option<NanoTime>) -> Option<u64> {
    t.map(nanos)
}

/// A span: an `int` of nanoseconds.
pub fn duration(nanos: u64) -> Duration {
    Duration::from_nanos(nanos)
}

fn boolean(obj: &Obj<'_>) -> Result<bool> {
    if !obj.is_instance_of::<PyBool>() {
        bail!("expected a bool, got {}", type_name(obj));
    }
    Ok(obj.extract()?)
}

pub fn int<T: for<'a, 'py> FromPyObject<'a, 'py>>(obj: &Obj<'_>) -> Result<T> {
    if obj.is_instance_of::<PyBool>() {
        bail!("expected an int, got a bool");
    }
    obj.extract::<T>().map_err(|_| {
        anyhow!(
            "expected a non-negative int in range, got {}",
            obj.repr().map(|r| r.to_string()).unwrap_or_default()
        )
    })
}

fn string(obj: &Obj<'_>) -> Result<String> {
    obj.extract()
        .map_err(|_| anyhow!("expected a str, got {}", type_name(obj)))
}

/// An instrument name.
pub fn symbol(obj: &Obj<'_>) -> Result<Symbol> {
    Symbol::new(&string(obj)?)
}

/// An epoch for minting order ids.
pub fn epoch(epoch: u32) -> Result<Epoch> {
    Epoch::new(epoch)
        .with_context(|| format!("epoch {epoch} is past the largest, {}", Epoch::MAX.get()))
}

fn order_id(obj: &Obj<'_>) -> Result<ClientOrderId> {
    Ok(ClientOrderId(int(obj)?))
}

fn cl_ord_id(obj: &Obj<'_>) -> Result<ClOrdId> {
    Ok(ClOrdId(int(obj)?))
}

fn exec_id(obj: &Obj<'_>) -> Result<ExecId> {
    ExecId::new(&string(obj)?).map_err(|e| anyhow!("{e}"))
}

fn venue_id(obj: &Obj<'_>) -> Result<VenueId> {
    VenueId::new(&string(obj)?).map_err(|e| anyhow!("{e}"))
}

fn opt_venue_id(id: VenueId) -> Option<String> {
    (!id.is_empty()).then(|| id.as_str().to_string())
}

/// A string tag for an enum, both ways, with the accepted set in the error.
macro_rules! tags {
    ($read:ident, $name:ident, $ty:ty, $what:literal, { $($tag:literal => $variant:expr),+ $(,)? }) => {
        /// Read the tag.
        pub fn $read(obj: &Obj<'_>) -> Result<$ty> {
            let tag = string(obj)?;
            match tag.as_str() {
                $($tag => Ok($variant),)+
                other => bail!(concat!("unknown ", $what, " {:?}; expected one of {:?}"), other, [$($tag),+]),
            }
        }

        /// The tag.
        pub fn $name(value: $ty) -> &'static str {
            $(if value == $variant { return $tag; })+
            unreachable!(concat!("invariant: every ", $what, " has a tag"))
        }
    };
}

tags!(side, side_name, Side, "side", { "bid" => Side::Bid, "ask" => Side::Ask });
tags!(order_kind, order_kind_name, OrderKind, "order kind", {
    "limit" => OrderKind::Limit,
    "post_only" => OrderKind::PostOnly,
    "market" => OrderKind::Market,
});
tags!(tif, tif_name, TimeInForce, "time in force", {
    "gtc" => TimeInForce::GoodTillCancel,
    "ioc" => TimeInForce::ImmediateOrCancel,
    "fok" => TimeInForce::FillOrKill,
    "day" => TimeInForce::Day,
});
tags!(trigger_kind, trigger_kind_name, TriggerKind, "trigger kind", {
    "stop" => TriggerKind::Stop,
    "take" => TriggerKind::Take,
});
tags!(reference, reference_name, Reference, "trigger reference", {
    "mark" => Reference::Mark,
    "index" => Reference::Index,
    "last" => Reference::Last,
});
tags!(liquidity, liquidity_name, Liquidity, "liquidity", {
    "maker" => Liquidity::Maker,
    "taker" => Liquidity::Taker,
});
tags!(reject_reason, reject_reason_name, RejectReason, "reject reason", {
    "post_only_would_cross" => RejectReason::PostOnlyWouldCross,
    "unknown_order" => RejectReason::UnknownOrder,
    "risk_limit" => RejectReason::RiskLimit,
    "rate_limited" => RejectReason::RateLimited,
    "price_band" => RejectReason::PriceBand,
    "other" => RejectReason::Other,
});
tags!(trading_state, trading_state_name, TradingState, "trading state", {
    "open" => TradingState::Open,
    "auction" => TradingState::Auction,
    "halted" => TradingState::Halted,
    "closed" => TradingState::Closed,
});
fn status_name(status: Status) -> &'static str {
    match status {
        Status::Working => "working",
        Status::Filled => "filled",
        Status::Lapsed => "lapsed",
        Status::Replaced => "replaced",
        Status::Refused => "refused",
    }
}

// ---------------------------------------------------------------------------
// The vocabulary.
// ---------------------------------------------------------------------------

impl FromPy for TradingState {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        trading_state(obj)
    }
}

impl ToPy for TradingState {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(PyString::new(py, trading_state_name(*self)).into_any())
    }
}

fn trigger_from(obj: &Obj<'_>) -> Result<Trigger> {
    let d = In::new(obj, "trigger", &["kind", "reference", "price"])?;
    Ok(Trigger {
        kind: d.get("kind", trigger_kind)?,
        reference: d.get("reference", reference)?,
        price: d.get("price", px)?,
    })
}

fn trigger_to<'py>(py: Python<'py>, trigger: Option<Trigger>) -> Result<Obj<'py>> {
    match trigger {
        None => Ok(py.None().into_bound(py)),
        Some(t) => Ok(Out::new(py)
            .set("kind", trigger_kind_name(t.kind))?
            .set("reference", reference_name(t.reference))?
            .set("price", decimal(py, t.price)?)?
            .done()),
    }
}

fn level_from(obj: &Obj<'_>) -> Result<Level> {
    if obj.is_instance_of::<PyDict>() {
        let d = In::new(obj, "level", &["price", "qty"])?;
        return Ok(Level::new(d.get("price", px)?, d.get("qty", qty)?));
    }
    let pair: (Obj<'_>, Obj<'_>) = obj.extract().map_err(|_| {
        anyhow!(
            "a level is a (price, qty) pair or a dict, got {}",
            type_name(obj)
        )
    })?;
    Ok(Level::new(
        px(&pair.0).context("level price")?,
        qty(&pair.1).context("level qty")?,
    ))
}

fn level_to<'py>(py: Python<'py>, level: Level) -> Result<Obj<'py>> {
    Ok(PyTuple::new(py, [decimal(py, level.price)?, decimal(py, level.qty)?])?.into_any())
}

const ORDER_KEYS: &[&str] = &[
    "type",
    "id",
    "instrument",
    "side",
    "qty",
    "price",
    "kind",
    "tif",
    "reduce_only",
    "trigger",
    "created",
];

fn order_from(d: &In<'_, '_>) -> Result<Order<Symbol>> {
    let order = Order {
        id: d.get("id", order_id)?,
        instrument: d.get("instrument", symbol)?,
        side: d.get("side", side)?,
        qty: d.get("qty", qty)?,
        price: d.maybe("price", px)?,
        kind: d.get("kind", order_kind)?,
        tif: d.get("tif", tif)?,
        reduce_only: d.maybe("reduce_only", boolean)?.unwrap_or(false),
        trigger: d.maybe("trigger", trigger_from)?,
        created: d.get("created", time)?,
    };
    order.validate().map_err(|e| anyhow!("order: {e}"))?;
    Ok(order)
}

fn order_to<'py>(py: Python<'py>, out: Out<'py>, o: &Order<Symbol>) -> Result<Out<'py>> {
    out.set("id", o.id.0)?
        .set("instrument", o.instrument.as_str())?
        .set("side", side_name(o.side))?
        .set("qty", decimal(py, o.qty)?)?
        .set("price", opt_decimal(py, o.price)?)?
        .set("kind", order_kind_name(o.kind))?
        .set("tif", tif_name(o.tif))?
        .set("reduce_only", o.reduce_only)?
        .set("trigger", trigger_to(py, o.trigger)?)?
        .set("created", nanos(o.created))
}

impl FromPy for Request<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let kind = obj
            .cast::<PyDict>()
            .ok()
            .and_then(|d| d.get_item("type").ok().flatten())
            .ok_or_else(|| anyhow!("request: expected a dict with a \"type\""))?;
        let request = match string(&kind)?.as_str() {
            "place" => Request::Place(order_from(&In::new(obj, "place", ORDER_KEYS)?)?),
            "amend" => {
                let d = In::new(
                    obj,
                    "amend",
                    &["type", "order", "instrument", "price", "qty", "trigger"],
                )?;
                Request::Amend(Amend {
                    order: d.get("order", order_id)?,
                    instrument: d.get("instrument", symbol)?,
                    price: d.get("price", px)?,
                    qty: d.get("qty", qty)?,
                    trigger: d.maybe("trigger", trigger_from)?,
                })
            }
            "cancel" => {
                let d = In::new(obj, "cancel", &["type", "order"])?;
                Request::Cancel(d.get("order", order_id)?)
            }
            "cancel_all" => {
                In::new(obj, "cancel_all", &["type"])?;
                Request::CancelAll
            }
            other => bail!(
                "unknown request type {other:?}; expected one of [\"place\", \"amend\", \"cancel\", \"cancel_all\"]"
            ),
        };
        request.validate().map_err(|e| anyhow!("request: {e}"))?;
        Ok(request)
    }
}

impl ToPy for Request<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        let out = Out::new(py);
        Ok(match self {
            Request::Place(o) => order_to(py, out.set("type", "place")?, o)?.done(),
            Request::Amend(a) => out
                .set("type", "amend")?
                .set("order", a.order.0)?
                .set("instrument", a.instrument.as_str())?
                .set("price", decimal(py, a.price)?)?
                .set("qty", decimal(py, a.qty)?)?
                .set("trigger", trigger_to(py, a.trigger)?)?
                .done(),
            Request::Cancel(id) => out.set("type", "cancel")?.set("order", id.0)?.done(),
            Request::CancelAll => out.set("type", "cancel_all")?.done(),
        })
    }
}

const FILL_KEYS: &[&str] = &[
    "type",
    "order",
    "exec_id",
    "instrument",
    "side",
    "qty",
    "filled",
    "remaining",
    "price",
    "fee",
    "liquidity",
    "venue_time",
    "recv_time",
];

fn fill_from(d: &In<'_, '_>) -> Result<Fill<Symbol>> {
    Ok(Fill {
        order: d.get("order", order_id)?,
        exec_id: d.get("exec_id", exec_id)?,
        instrument: d.get("instrument", symbol)?,
        side: d.get("side", side)?,
        qty: d.get("qty", qty)?,
        filled: d.get("filled", qty)?,
        remaining: d.get("remaining", qty)?,
        price: d.get("price", px)?,
        fee: d.get("fee", qty)?,
        liquidity: d.get("liquidity", liquidity)?,
        venue_time: d.maybe("venue_time", time)?,
        recv_time: d.get("recv_time", time)?,
    })
}

fn fill_to<'py>(py: Python<'py>, out: Out<'py>, f: &Fill<Symbol>) -> Result<Out<'py>> {
    out.set("order", f.order.0)?
        .set("exec_id", f.exec_id.as_str())?
        .set("instrument", f.instrument.as_str())?
        .set("side", side_name(f.side))?
        .set("qty", decimal(py, f.qty)?)?
        .set("filled", decimal(py, f.filled)?)?
        .set("remaining", decimal(py, f.remaining)?)?
        .set("price", decimal(py, f.price)?)?
        .set("fee", decimal(py, f.fee)?)?
        .set("liquidity", liquidity_name(f.liquidity))?
        .set("venue_time", opt_nanos(f.venue_time))?
        .set("recv_time", nanos(f.recv_time))
}

impl FromPy for Fill<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        fill_from(&In::new(obj, "fill", FILL_KEYS)?)
    }
}

impl ToPy for Fill<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(fill_to(py, Out::new(py).set("type", "fill")?, self)?.done())
    }
}

fn retired_from(d: &In<'_, '_>) -> Result<Retired> {
    Ok(Retired {
        order: d.get("order", order_id)?,
        remaining: d.get("remaining", qty)?,
        venue_time: d.maybe("venue_time", time)?,
        recv_time: d.get("recv_time", time)?,
    })
}

const RETIRED_KEYS: &[&str] = &["type", "order", "remaining", "venue_time", "recv_time"];

impl FromPy for Report<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let kind = obj
            .cast::<PyDict>()
            .ok()
            .and_then(|d| d.get_item("type").ok().flatten())
            .ok_or_else(|| anyhow!("report: expected a dict with a \"type\""))?;
        Ok(match string(&kind)?.as_str() {
            "ack" => {
                let d = In::new(
                    obj,
                    "ack",
                    &[
                        "type",
                        "order",
                        "price",
                        "remaining",
                        "venue_id",
                        "venue_time",
                        "recv_time",
                    ],
                )?;
                Report::Ack(Ack {
                    order: d.get("order", order_id)?,
                    price: d.maybe("price", px)?,
                    remaining: d.maybe("remaining", qty)?,
                    venue_id: d.maybe("venue_id", venue_id)?.unwrap_or_default(),
                    venue_time: d.maybe("venue_time", time)?,
                    recv_time: d.get("recv_time", time)?,
                })
            }
            "reject" => {
                let d = In::new(
                    obj,
                    "reject",
                    &["type", "order", "reason", "venue_time", "recv_time"],
                )?;
                Report::Reject(Reject {
                    order: d.maybe("order", order_id)?,
                    reason: d.get("reason", reject_reason)?,
                    venue_time: d.maybe("venue_time", time)?,
                    recv_time: d.get("recv_time", time)?,
                })
            }
            "fill" => Report::Fill(fill_from(&In::new(obj, "fill", FILL_KEYS)?)?),
            "cancelled" => {
                Report::Cancelled(retired_from(&In::new(obj, "cancelled", RETIRED_KEYS)?)?)
            }
            "expired" => Report::Expired(retired_from(&In::new(obj, "expired", RETIRED_KEYS)?)?),
            "triggered" => {
                let d = In::new(
                    obj,
                    "triggered",
                    &["type", "order", "venue_id", "venue_time", "recv_time"],
                )?;
                Report::Triggered(Fired {
                    order: d.get("order", order_id)?,
                    venue_id: d.maybe("venue_id", venue_id)?.unwrap_or_default(),
                    venue_time: d.maybe("venue_time", time)?,
                    recv_time: d.get("recv_time", time)?,
                })
            }
            other => bail!(
                "unknown report type {other:?}; expected one of [\"ack\", \"reject\", \"fill\", \"cancelled\", \"expired\", \"triggered\"]"
            ),
        })
    }
}

impl ToPy for Report<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        let out = Out::new(py);
        let retired = |out: Out<'py>, tag: &str, r: &Retired| -> Result<Obj<'py>> {
            Ok(out
                .set("type", tag)?
                .set("order", r.order.0)?
                .set("remaining", decimal(py, r.remaining)?)?
                .set("venue_time", opt_nanos(r.venue_time))?
                .set("recv_time", nanos(r.recv_time))?
                .done())
        };
        Ok(match self {
            Report::Ack(a) => out
                .set("type", "ack")?
                .set("order", a.order.0)?
                .set("price", opt_decimal(py, a.price)?)?
                .set("remaining", opt_decimal(py, a.remaining)?)?
                .set("venue_id", opt_venue_id(a.venue_id))?
                .set("venue_time", opt_nanos(a.venue_time))?
                .set("recv_time", nanos(a.recv_time))?
                .done(),
            Report::Reject(r) => out
                .set("type", "reject")?
                .set("order", r.order.map(|id| id.0))?
                .set("reason", reject_reason_name(r.reason))?
                .set("venue_time", opt_nanos(r.venue_time))?
                .set("recv_time", nanos(r.recv_time))?
                .done(),
            Report::Fill(f) => fill_to(py, out.set("type", "fill")?, f)?.done(),
            Report::Cancelled(r) => retired(out, "cancelled", r)?,
            Report::Expired(r) => retired(out, "expired", r)?,
            Report::Triggered(f) => out
                .set("type", "triggered")?
                .set("order", f.order.0)?
                .set("venue_id", opt_venue_id(f.venue_id))?
                .set("venue_time", opt_nanos(f.venue_time))?
                .set("recv_time", nanos(f.recv_time))?
                .done(),
        })
    }
}

impl FromPy for Holdings<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let d = In::new(obj, "holdings", &["whole", "held", "as_of"])?;
        let held = d.get("held", |held| {
            let held = held.cast::<PyDict>().map_err(|_| {
                anyhow!(
                    "expected a dict of instrument to net, got {}",
                    type_name(held)
                )
            })?;
            held.iter()
                .map(|(instrument, net)| {
                    let instrument = symbol(&instrument)?;
                    Ok(Held {
                        instrument,
                        net: qty(&net).with_context(|| format!("net for {instrument}"))?,
                    })
                })
                .collect::<Result<Burst<Held<Symbol>>>>()
        })?;
        Ok(Holdings {
            whole: d.get("whole", boolean)?,
            held,
            as_of: d.get("as_of", time)?,
        })
    }
}

impl ToPy for Holdings<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        let held = PyDict::new(py);
        for h in &self.held {
            held.set_item(h.instrument.as_str(), decimal(py, h.net)?)?;
        }
        Ok(Out::new(py)
            .set("whole", self.whole)?
            .set("held", held)?
            .set("as_of", nanos(self.as_of))?
            .done())
    }
}

impl FromPy for Cashflow<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let d = In::new(
            obj,
            "cashflow",
            &["instrument", "amount", "venue_time", "recv_time"],
        )?;
        Ok(Cashflow {
            instrument: d.get("instrument", symbol)?,
            amount: d.get("amount", qty)?,
            venue_time: d.maybe("venue_time", time)?,
            recv_time: d.get("recv_time", time)?,
        })
    }
}

impl ToPy for Cashflow<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(Out::new(py)
            .set("instrument", self.instrument.as_str())?
            .set("amount", decimal(py, self.amount)?)?
            .set("venue_time", opt_nanos(self.venue_time))?
            .set("recv_time", nanos(self.recv_time))?
            .done())
    }
}

impl ToPy for Account {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(Out::new(py)
            .set("equity", opt_decimal(py, self.equity)?)?
            .set("utilisation", self.utilisation)?
            .set("as_of", nanos(self.as_of))?
            .done())
    }
}

impl ToPy for MmpTrip {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(Out::new(py)
            .set("frozen_until", opt_nanos(self.frozen_until))?
            .set("venue_time", opt_nanos(self.venue_time))?
            .set("recv_time", nanos(self.recv_time))?
            .done())
    }
}

impl ToPy for Pacing {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(Out::new(py)
            .set("deferred", self.deferred)?
            .set("superseded", self.superseded)?
            .set("waiting", self.waiting)?
            .done())
    }
}

fn ladder_from(side: Side, obj: Option<Obj<'_>>) -> Result<Ladder> {
    let Some(obj) = obj else {
        return Ok(Ladder::none());
    };
    let levels = obj
        .try_iter()
        .map_err(|_| anyhow!("expected a list of levels, got {}", type_name(&obj)))?
        .enumerate()
        .map(|(i, level)| level_from(&level?).with_context(|| format!("level {i}")))
        .collect::<Result<Vec<_>>>()?;
    Ladder::from_levels(side, &levels).map_err(|e| anyhow!("{e}"))
}

fn ladder_to<'py>(py: Python<'py>, ladder: Ladder) -> Result<Obj<'py>> {
    let levels = ladder
        .iter()
        .map(|level| level_to(py, level))
        .collect::<Result<Vec<_>>>()?;
    Ok(PyList::new(py, levels)?.into_any())
}

/// The one level on one side a cross or a triggered desired carries.
fn single(bids: Ladder, asks: Ladder, what: &str) -> Result<(Side, Level)> {
    match (bids.depth(), asks.depth()) {
        (1, 0) => Ok((Side::Bid, bids.best().expect("invariant: depth 1"))),
        (0, 1) => Ok((Side::Ask, asks.best().expect("invariant: depth 1"))),
        (b, a) => bail!("a {what} carries one level on one side, got {b} bid(s) and {a} ask(s)"),
    }
}

impl FromPy for Desired<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let d = In::new(
            obj,
            "desired",
            &[
                "instrument",
                "as_of",
                "bids",
                "asks",
                "intent",
                "trigger",
                "reduce_only",
            ],
        )?;
        let instrument = d.get("instrument", symbol)?;
        let as_of = d.get("as_of", time)?;
        let bids = ladder_from(Side::Bid, d.opt("bids")?).context("desired: \"bids\"")?;
        let asks = ladder_from(Side::Ask, d.opt("asks")?).context("desired: \"asks\"")?;
        let intent = d
            .maybe("intent", string)?
            .unwrap_or_else(|| "rest".to_string());
        let trigger = d.maybe("trigger", trigger_from)?;
        if trigger.is_some() && intent != "trigger" {
            bail!("desired: a \"trigger\" needs intent \"trigger\"");
        }
        let desired = match intent.as_str() {
            "rest" => Desired::quote(instrument, as_of, bids, asks),
            "cross" => {
                let (side, level) = single(bids, asks, "cross")?;
                Desired::cross(instrument, as_of, side, level)
            }
            "trigger" => match trigger {
                None if bids.is_empty() && asks.is_empty() => {
                    Desired::no_trigger(instrument, as_of)
                }
                None => bail!("desired: intent \"trigger\" with levels needs a \"trigger\""),
                Some(trigger) => {
                    let (side, level) = single(bids, asks, "triggered desired")?;
                    Desired::stop(instrument, as_of, side, trigger, level)
                }
            },
            other => bail!(
                "desired: unknown intent {other:?}; expected one of [\"rest\", \"cross\", \"trigger\"]"
            ),
        };
        Ok(if d.maybe("reduce_only", boolean)?.unwrap_or(false) {
            desired.reduce_only()
        } else {
            desired
        })
    }
}

impl ToPy for Desired<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        let (intent, trigger) = match self.intent {
            Intent::Rest => ("rest", None),
            Intent::Cross => ("cross", None),
            // A withdrawn triggered decision carries a placeholder trigger.
            Intent::Trigger(_) if self.is_empty() => ("trigger", None),
            Intent::Trigger(t) => ("trigger", Some(t)),
        };
        Ok(Out::new(py)
            .set("instrument", self.instrument.as_str())?
            .set("as_of", nanos(self.as_of))?
            .set("bids", ladder_to(py, self.bids)?)?
            .set("asks", ladder_to(py, self.asks)?)?
            .set("intent", intent)?
            .set("trigger", trigger_to(py, trigger)?)?
            .set("reduce_only", self.reduce_only)?
            .done())
    }
}

impl FromPy for Parent<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let d = In::new(
            obj,
            "parent",
            &[
                "instrument",
                "side",
                "total",
                "cap",
                "by",
                "style",
                "clips",
                "show",
                "reduce_only",
            ],
        )?;
        let instrument = d.get("instrument", symbol)?;
        let side = d.get("side", side)?;
        let total = d.get("total", qty)?;
        let cap = d.get("cap", px)?;
        let by = d.get("by", time)?;
        let style = d.get("style", string)?;
        let clips = d.maybe("clips", int::<u32>)?;
        let show = d.maybe("show", qty)?;
        let parent = match (style.as_str(), clips, show) {
            ("twap", Some(clips), None) => Parent::twap(instrument, side, total, cap, by, clips),
            ("iceberg", None, Some(show)) => {
                Parent::iceberg(instrument, side, total, cap, by, show)
            }
            ("sweep", None, None) => Parent::sweep(instrument, side, total, cap, by),
            ("twap", _, _) => bail!("parent: a twap takes \"clips\" and no \"show\""),
            ("iceberg", _, _) => bail!("parent: an iceberg takes \"show\" and no \"clips\""),
            ("sweep", _, _) => bail!("parent: a sweep takes neither \"clips\" nor \"show\""),
            (other, _, _) => bail!(
                "parent: unknown style {other:?}; expected one of [\"twap\", \"iceberg\", \"sweep\"]"
            ),
        }
        .map_err(|e| anyhow!("parent: {e}"))?;
        Ok(if d.maybe("reduce_only", boolean)?.unwrap_or(false) {
            parent.reduce_only()
        } else {
            parent
        })
    }
}

impl ToPy for Parent<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        let out = Out::new(py)
            .set("instrument", self.instrument.as_str())?
            .set("side", side_name(self.side))?
            .set("total", decimal(py, self.total)?)?
            .set("cap", decimal(py, self.cap)?)?
            .set("by", nanos(self.by))?
            .set("reduce_only", self.reduce_only)?;
        Ok(match self.style {
            Style::Twap { clips } => out.set("style", "twap")?.set("clips", clips)?,
            Style::Iceberg { show } => out
                .set("style", "iceberg")?
                .set("show", decimal(py, show)?)?,
            Style::Sweep => out.set("style", "sweep")?,
        }
        .done())
    }
}

impl ToPy for Progress<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(Out::new(py)
            .set("parent", self.parent.to_py(py)?)?
            .set("start", nanos(self.start))?
            .set("filled", decimal(py, self.filled)?)?
            .set("remaining", decimal(py, self.remaining)?)?
            .set("clips", self.clips)?
            .set("status", status_name(self.status))?
            .set("at", nanos(self.at))?
            .done())
    }
}

impl ToPy for Message<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        let out = Out::new(py);
        Ok(match *self {
            Message::New(o) => out
                .set("type", "new")?
                .set("cl_ord_id", o.cl_ord_id.0)?
                .set("instrument", o.instrument.as_str())?
                .set("side", side_name(o.side))?
                .set("qty", decimal(py, o.qty)?)?
                .set("price", opt_decimal(py, o.price)?)?
                .set("kind", order_kind_name(o.kind))?
                .set("tif", tif_name(o.tif))?
                .done(),
            Message::Replace {
                cl_ord_id,
                orig,
                instrument,
                side,
                kind,
                tif,
                price,
                qty,
            } => out
                .set("type", "replace")?
                .set("cl_ord_id", cl_ord_id.0)?
                .set("orig", orig.0)?
                .set("instrument", instrument.as_str())?
                .set("side", side_name(side))?
                .set("kind", order_kind_name(kind))?
                .set("tif", tif_name(tif))?
                .set("price", decimal(py, price)?)?
                .set("qty", decimal(py, qty)?)?
                .done(),
            Message::Cancel {
                cl_ord_id,
                orig,
                instrument,
                side,
                qty,
            } => out
                .set("type", "cancel")?
                .set("cl_ord_id", cl_ord_id.0)?
                .set("orig", orig.0)?
                .set("instrument", instrument.as_str())?
                .set("side", side_name(side))?
                .set("qty", decimal(py, qty)?)?
                .done(),
            Message::MassCancel { cl_ord_id } => out
                .set("type", "mass_cancel")?
                .set("cl_ord_id", cl_ord_id.0)?
                .done(),
        })
    }
}

impl FromPy for ExecReport<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let kind = obj
            .cast::<PyDict>()
            .ok()
            .and_then(|d| d.get_item("kind").ok().flatten())
            .ok_or_else(|| anyhow!("execution report: expected a dict with a \"kind\""))?;
        let kind = string(&kind)?;
        let extra: &[&str] = match kind.as_str() {
            "new" => &[],
            "replaced" => &["price", "qty"],
            "canceled" | "expired" => &["remaining"],
            "rejected" => &["reason"],
            "trade" => &[
                "exec_id",
                "instrument",
                "side",
                "qty",
                "filled",
                "remaining",
                "price",
                "liquidity",
            ],
            other => bail!(
                "execution report: unknown kind {other:?}; expected one of [\"new\", \"replaced\", \"canceled\", \"expired\", \"rejected\", \"trade\"]"
            ),
        };
        let keys: Vec<&str> = ["cl_ord_id", "orig", "kind", "venue_time"]
            .into_iter()
            .chain(extra.iter().copied())
            .collect();
        let d = In::new(obj, "execution report", &keys)?;
        let kind = match kind.as_str() {
            "new" => ExecKind::New,
            "replaced" => ExecKind::Replaced {
                price: d.get("price", px)?,
                qty: d.get("qty", qty)?,
            },
            "canceled" => ExecKind::Canceled {
                remaining: d.get("remaining", qty)?,
            },
            "expired" => ExecKind::Expired {
                remaining: d.get("remaining", qty)?,
            },
            "rejected" => ExecKind::Rejected(d.get("reason", reject_reason)?),
            _ => ExecKind::Trade(Trade {
                exec_id: d.get("exec_id", exec_id)?,
                instrument: d.get("instrument", symbol)?,
                side: d.get("side", side)?,
                qty: d.get("qty", qty)?,
                filled: d.get("filled", qty)?,
                remaining: d.get("remaining", qty)?,
                price: d.get("price", px)?,
                liquidity: d.get("liquidity", liquidity)?,
            }),
        };
        Ok(ExecReport {
            cl_ord_id: d.get("cl_ord_id", cl_ord_id)?,
            orig: d.maybe("orig", cl_ord_id)?,
            kind,
            venue_time: d.get("venue_time", time)?,
        })
    }
}

/// A measure, as a valuation argument names one: `"linear"`, `"inverse"`,
/// or a contract multiplier.
pub fn measure(obj: &Obj<'_>) -> Result<Measure> {
    if obj.is_instance_of::<PyString>() {
        match string(obj)?.as_str() {
            "linear" => return Ok(Measure::Linear),
            "inverse" => return Ok(Measure::Inverse),
            other if qty(obj).is_err() => bail!(
                "unknown measure {other:?}; expected \"linear\", \"inverse\" or a positive multiplier"
            ),
            _ => {}
        }
    }
    let multiplier = qty(obj)?;
    Measure::scaled(multiplier)
        .with_context(|| format!("a multiplier must be positive, got {multiplier}"))
}

fn measure_to<'py>(py: Python<'py>, measure: Option<Measure>) -> Result<Obj<'py>> {
    Ok(match measure {
        None => py.None().into_bound(py),
        Some(Measure::Linear) => PyString::new(py, "linear").into_any(),
        Some(Measure::Inverse) => PyString::new(py, "inverse").into_any(),
        Some(Measure::Scaled { multiplier }) => decimal(py, multiplier)?,
    })
}

/// The fields every position reading carries, onto `out`.
pub fn position_to<'py>(py: Python<'py>, out: Out<'py>, p: &Position<Symbol>) -> Result<Out<'py>> {
    out.set("instrument", p.instrument.as_str())?
        .set("measure", measure_to(py, p.measure)?)?
        .set("net", decimal(py, p.net())?)?
        .set("cost", decimal(py, p.cost())?)?
        .set("realised", decimal(py, p.realised())?)?
        .set("fees", decimal(py, p.fees())?)?
        .set("cashflows", decimal(py, p.cashflows())?)?
        .set("entry", opt_decimal(py, p.entry())?)
}

impl ToPy for Position<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(position_to(py, Out::new(py), self)?.done())
    }
}

/// A PnL, or `None` where it was withheld.
pub fn pnl_to<'py>(py: Python<'py>, pnl: Option<Pnl>) -> Result<Obj<'py>> {
    let Some(pnl) = pnl else {
        return Ok(py.None().into_bound(py));
    };
    Ok(Out::new(py)
        .set("realised", decimal(py, pnl.realised)?)?
        .set("unrealised", decimal(py, pnl.unrealised)?)?
        .set("fees", decimal(py, pnl.fees)?)?
        .set("cashflows", decimal(py, pnl.cashflows)?)?
        .set("net", decimal(py, pnl.net())?)?
        .done())
}

impl ToPy for Mismatch<Symbol> {
    fn to_py<'py>(&self, py: Python<'py>) -> Result<Obj<'py>> {
        Ok(Out::new(py)
            .set("instrument", self.instrument.as_str())?
            .set("ours", decimal(py, self.ours)?)?
            .set("venue", decimal(py, self.venue)?)?
            .set("since", nanos(self.since))?
            .done())
    }
}

/// One instant's marks: a `dict` of instrument to price, or a `list` of
/// them for several. A pair is not accepted, because a `tuple` is already
/// a burst.
pub fn marks(obj: &Obj<'_>) -> Result<Burst<(Symbol, Px)>> {
    let mut burst = Burst::default();
    let mut one = |obj: &Obj<'_>| -> Result<()> {
        let dict = obj.cast::<PyDict>().map_err(|_| {
            anyhow!(
                "marks: expected a dict of instrument to price, got {}",
                type_name(obj)
            )
        })?;
        for (instrument, price) in dict.iter() {
            let instrument = symbol(&instrument).context("marks")?;
            burst.push((
                instrument,
                px(&price).with_context(|| format!("marks: price for {instrument}"))?,
            ));
        }
        Ok(())
    };
    if obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>() {
        for item in obj.try_iter()? {
            one(&item?)?;
        }
    } else {
        one(obj)?;
    }
    Ok(burst)
}

impl FromPy for Touch<Symbol> {
    fn from_py(obj: &Obj<'_>) -> Result<Self> {
        let d = In::new(obj, "touch", &["instrument", "bid", "ask"])?;
        Ok(Touch {
            instrument: d.get("instrument", symbol)?,
            bid: d.maybe("bid", px)?,
            ask: d.maybe("ask", px)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::types::PyDict;

    fn eval<'py>(py: Python<'py>, code: &str) -> Obj<'py> {
        let globals = PyDict::new(py);
        py.run(
            &std::ffi::CString::new("from decimal import Decimal as D").expect("no NUL"),
            Some(&globals),
            None,
        )
        .expect("import Decimal");
        py.eval(
            &std::ffi::CString::new(code).expect("no NUL"),
            Some(&globals),
            None,
        )
        .expect("a valid expression")
    }

    fn roundtrip<T: FromPy + ToPy + PartialEq + fmt::Debug>(py: Python<'_>, code: &str) -> T {
        let value = T::from_py(&eval(py, code)).expect("reads");
        let again = T::from_py(&value.to_py(py).expect("builds")).expect("reads back");
        assert_eq!(value, again, "{code}");
        value
    }

    fn refusal<T: FromPy>(py: Python<'_>, code: &str) -> String {
        match T::from_py(&eval(py, code)) {
            Ok(_) => panic!("{code} was read"),
            Err(e) => format!("{e:#}"),
        }
    }

    #[test]
    fn a_symbol_holds_a_short_name_and_orders_as_text() {
        assert_eq!(Symbol::new("ES").expect("short").as_str(), "ES");
        assert!(Symbol::new("").is_err());
        assert!(Symbol::new("a\0b").is_err());
        assert!(Symbol::new(&"x".repeat(SYMBOL_CAPACITY)).is_ok());
        assert!(Symbol::new(&"x".repeat(SYMBOL_CAPACITY + 1)).is_err());
        let names = ["B", "AB", "A", "BA"];
        let mut symbols: Vec<Symbol> = names.iter().map(|n| Symbol::new(n).expect("ok")).collect();
        symbols.sort();
        let sorted: Vec<&str> = symbols.iter().map(Symbol::as_str).collect();
        assert_eq!(sorted, ["A", "AB", "B", "BA"]);
    }

    #[test]
    fn numbers_are_read_exactly_and_refused_loudly() {
        Python::initialize();
        Python::attach(|py| {
            for code in ["D('5000.25')", "'5000.25'", "5000.25", "D('5.00025E+3')"] {
                assert_eq!(
                    px(&eval(py, code)).expect(code),
                    Px::parse("5000.25").expect("ok")
                );
            }
            assert_eq!(
                qty(&eval(py, "0.1")).expect("float"),
                Qty::parse("0.1").expect("ok")
            );
            assert_eq!(
                qty(&eval(py, "7")).expect("int"),
                Qty::parse("7").expect("ok")
            );
            for code in ["True", "'abc'", "D('1.0000000001')", "float('nan')", "[1]"] {
                assert!(px(&eval(py, code)).is_err(), "{code} was read");
            }
            let back = decimal(py, Px::parse("4999.75").expect("ok")).expect("builds");
            assert_eq!(back.str().expect("str").to_string(), "4999.75");
        });
    }

    #[test]
    fn every_request_round_trips() {
        Python::initialize();
        Python::attach(|py| {
            let place: Request<Symbol> = roundtrip(
                py,
                "{'type': 'place', 'id': 7, 'instrument': 'ES', 'side': 'bid', 'qty': 1, \
                 'price': D('4999.75'), 'kind': 'post_only', 'tif': 'gtc', 'created': 10}",
            );
            assert!(
                matches!(place, Request::Place(o) if o.id == ClientOrderId(7) && !o.reduce_only)
            );
            roundtrip::<Request<Symbol>>(
                py,
                "{'type': 'place', 'id': 8, 'instrument': 'ES', 'side': 'ask', 'qty': 2, \
                 'price': 10, 'kind': 'limit', 'tif': 'gtc', 'created': 0, 'reduce_only': True, \
                 'trigger': {'kind': 'stop', 'reference': 'last', 'price': 9}}",
            );
            roundtrip::<Request<Symbol>>(
                py,
                "{'type': 'amend', 'order': 7, 'instrument': 'ES', 'price': 1, 'qty': 2}",
            );
            roundtrip::<Request<Symbol>>(py, "{'type': 'cancel', 'order': 7}");
            let all: Request<Symbol> = roundtrip(py, "{'type': 'cancel_all'}");
            assert_eq!(all, Request::CancelAll);
        });
    }

    #[test]
    fn a_request_is_refused_where_it_is_wrong() {
        Python::initialize();
        Python::attach(|py| {
            assert!(
                refusal::<Request<Symbol>>(py, "{'type': 'cancel', 'order': 7, 'oder': 1}")
                    .contains("unknown key \"oder\"")
            );
            assert!(
                refusal::<Request<Symbol>>(py, "{'type': 'cancel'}").contains("missing \"order\"")
            );
            assert!(
                refusal::<Request<Symbol>>(py, "{'type': 'kill'}").contains("unknown request type")
            );
            assert!(
                refusal::<Request<Symbol>>(py, "{'type': 'cancel', 'order': True}")
                    .contains("bool")
            );
            // A market order carries no price: the order's own validation.
            let err = refusal::<Request<Symbol>>(
                py,
                "{'type': 'place', 'id': 1, 'instrument': 'ES', 'side': 'bid', 'qty': 1, \
                 'price': 5, 'kind': 'market', 'tif': 'ioc', 'created': 0}",
            );
            assert!(err.contains("order:"), "{err}");
            assert!(
                refusal::<Request<Symbol>>(
                    py,
                    "{'type': 'place', 'id': 1, 'instrument': 'ES', 'side': 'buy', 'qty': 1, \
                 'kind': 'market', 'tif': 'ioc', 'created': 0}",
                )
                .contains("unknown side \"buy\"")
            );
        });
    }

    #[test]
    fn every_report_round_trips() {
        Python::initialize();
        Python::attach(|py| {
            for code in [
                "{'type': 'ack', 'order': 1, 'recv_time': 5}",
                "{'type': 'ack', 'order': 1, 'price': 2, 'remaining': 3, 'venue_id': 'v1', \
                 'venue_time': 4, 'recv_time': 5}",
                "{'type': 'reject', 'order': None, 'reason': 'unknown_order', 'recv_time': 5}",
                "{'type': 'reject', 'order': 3, 'reason': 'post_only_would_cross', 'recv_time': 5}",
                "{'type': 'fill', 'order': 1, 'exec_id': 'x1', 'instrument': 'ES', 'side': 'bid', \
                 'qty': 1, 'filled': 1, 'remaining': 0, 'price': D('4999.75'), 'fee': D('-0.1'), \
                 'liquidity': 'maker', 'recv_time': 5}",
                "{'type': 'cancelled', 'order': 1, 'remaining': 1, 'recv_time': 5}",
                "{'type': 'expired', 'order': 1, 'remaining': 1, 'venue_time': 2, 'recv_time': 5}",
                "{'type': 'triggered', 'order': 1, 'venue_id': 'v', 'recv_time': 5}",
            ] {
                roundtrip::<Report<Symbol>>(py, code);
            }
            assert!(
                refusal::<Report<Symbol>>(py, "{'type': 'ack', 'order': 1}").contains("recv_time")
            );
            assert!(
                refusal::<Report<Symbol>>(
                    py,
                    "{'type': 'ack', 'order': 1, 'venue_id': '', 'recv_time': 0}"
                )
                .contains("empty")
            );
        });
    }

    #[test]
    fn a_desired_is_built_through_the_constructor_for_its_shape() {
        Python::initialize();
        Python::attach(|py| {
            let two_way: Desired<Symbol> = roundtrip(
                py,
                "{'instrument': 'ES', 'as_of': 1, 'bids': [(4999, 1)], 'asks': [{'price': 5001, 'qty': 1}]}",
            );
            assert_eq!(two_way.intent, Intent::Rest);
            let nothing: Desired<Symbol> = roundtrip(py, "{'instrument': 'ES', 'as_of': 1}");
            assert!(nothing.is_empty() && nothing.intent == Intent::Rest);
            let cross: Desired<Symbol> = roundtrip(
                py,
                "{'instrument': 'ES', 'as_of': 1, 'asks': [(5001, 2)], 'intent': 'cross', 'reduce_only': True}",
            );
            assert!(cross.intent == Intent::Cross && cross.reduce_only);
            let stop: Desired<Symbol> = roundtrip(
                py,
                "{'instrument': 'ES', 'as_of': 1, 'bids': [(5010, 1)], 'intent': 'trigger', \
                 'trigger': {'kind': 'stop', 'reference': 'mark', 'price': 5005}}",
            );
            assert!(matches!(stop.intent, Intent::Trigger(_)));
            let withdrawn: Desired<Symbol> =
                roundtrip(py, "{'instrument': 'ES', 'as_of': 1, 'intent': 'trigger'}");
            assert_eq!(
                withdrawn,
                Desired::no_trigger(withdrawn.instrument, NanoTime::from(1))
            );

            for (code, says) in [
                (
                    "{'instrument': 'ES', 'as_of': 1, 'bids': [(1, 1), (2, 1)]}",
                    "strictly behind",
                ),
                (
                    "{'instrument': 'ES', 'as_of': 1, 'bids': [(1, 1)], 'asks': [(2, 1)], 'intent': 'cross'}",
                    "one level on one side",
                ),
                (
                    "{'instrument': 'ES', 'as_of': 1, 'bids': [(1, 1)], 'intent': 'trigger'}",
                    "needs a \"trigger\"",
                ),
                (
                    "{'instrument': 'ES', 'as_of': 1, 'trigger': {'kind': 'stop', 'reference': 'mark', 'price': 1}}",
                    "intent \"trigger\"",
                ),
                (
                    "{'instrument': 'ES', 'as_of': 1, 'intent': 'sit'}",
                    "unknown intent",
                ),
                ("{'instrument': 'ES'}", "missing \"as_of\""),
            ] {
                let err = refusal::<Desired<Symbol>>(py, code);
                assert!(err.contains(says), "{code}: {err}");
            }
        });
    }

    #[test]
    fn a_parent_takes_the_fields_its_style_needs() {
        Python::initialize();
        Python::attach(|py| {
            let base = "'instrument': 'ES', 'side': 'bid', 'total': 10, 'cap': 5001, 'by': 100";
            let read =
                |extra: &str| Parent::<Symbol>::from_py(&eval(py, &format!("{{{base}, {extra}}}")));
            assert!(matches!(
                read("'style': 'twap', 'clips': 5").expect("twap").style,
                Style::Twap { clips: 5 }
            ));
            assert!(read("'style': 'iceberg', 'show': 2").is_ok());
            assert!(
                read("'style': 'sweep', 'reduce_only': True")
                    .expect("sweep")
                    .reduce_only
            );
            assert!(read("'style': 'twap'").is_err());
            assert!(read("'style': 'twap', 'clips': 0").is_err());
            assert!(read("'style': 'sweep', 'show': 2").is_err());
            let progress = Progress {
                parent: read("'style': 'iceberg', 'show': 2").expect("iceberg"),
                start: NanoTime::from(1),
                filled: Qty::ZERO,
                remaining: Qty::parse("10").expect("ok"),
                clips: 0,
                status: Status::Working,
                at: NanoTime::from(2),
            };
            let back = progress.to_py(py).expect("builds");
            assert_eq!(
                back.get_item("status").expect("status").to_string(),
                "working"
            );
            Parent::<Symbol>::from_py(&back.get_item("parent").expect("parent"))
                .expect("reads back");
        });
    }

    #[test]
    fn holdings_and_cashflows_round_trip() {
        Python::initialize();
        Python::attach(|py| {
            let holdings: Holdings<Symbol> = roundtrip(
                py,
                "{'whole': True, 'held': {'ES': 2, 'NQ': D('-1.5')}, 'as_of': 3}",
            );
            assert_eq!(holdings.held.len(), 2);
            roundtrip::<Cashflow<Symbol>>(
                py,
                "{'instrument': 'ES', 'amount': D('-1.25'), 'recv_time': 4}",
            );
            assert!(
                refusal::<Holdings<Symbol>>(py, "{'whole': 1, 'held': {}, 'as_of': 0}")
                    .contains("bool")
            );
        });
    }

    #[test]
    fn an_execution_report_is_read_per_kind() {
        Python::initialize();
        Python::attach(|py| {
            let trade = ExecReport::<Symbol>::from_py(&eval(
                py,
                "{'cl_ord_id': 3, 'kind': 'trade', 'venue_time': 9, 'exec_id': 'e', \
                 'instrument': 'ES', 'side': 'ask', 'qty': 1, 'filled': 1, 'remaining': 0, \
                 'price': 10, 'liquidity': 'taker'}",
            ))
            .expect("trade");
            assert!(matches!(trade.kind, ExecKind::Trade(t) if t.liquidity == Liquidity::Taker));
            let refused = ExecReport::<Symbol>::from_py(&eval(
                py,
                "{'cl_ord_id': 3, 'orig': 2, 'kind': 'rejected', 'reason': 'rate_limited', 'venue_time': 9}",
            ))
            .expect("rejected");
            assert_eq!(refused.orig, Some(ClOrdId(2)));
            assert!(
                ExecReport::<Symbol>::from_py(&eval(
                    py,
                    "{'cl_ord_id': 3, 'kind': 'replaced', 'venue_time': 9}"
                ))
                .is_err()
            );
        });
    }

    #[test]
    fn marks_and_measures_are_read_from_tables() {
        Python::initialize();
        Python::attach(|py| {
            assert_eq!(
                marks(&eval(py, "{'ES': 1, 'NQ': 2}")).expect("dict").len(),
                2
            );
            assert_eq!(
                marks(&eval(py, "[{'ES': 1}, {'ES': 2}]"))
                    .expect("list")
                    .len(),
                2
            );
            assert!(marks(&eval(py, "('ES', 1)")).is_err());
            assert_eq!(
                measure(&eval(py, "'linear'")).expect("linear"),
                Measure::Linear
            );
            assert_eq!(
                measure(&eval(py, "1")).expect("one is linear"),
                Measure::Linear
            );
            assert!(matches!(
                measure(&eval(py, "50")).expect("scaled"),
                Measure::Scaled { .. }
            ));
            assert!(measure(&eval(py, "0")).is_err());
            assert!(measure(&eval(py, "'cubic'")).is_err());
            assert!(matches!(
                measure(&eval(py, "'50'")).expect("a str number"),
                Measure::Scaled { .. }
            ));
            assert!(time(&eval(py, "True")).is_err());
        });
    }
}
