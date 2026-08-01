//! Python bindings for the wingfoil-next **augurs** adapter
//! ([`wingfoil_next::adapters::augurs`]) — time-series analytics.
//!
//! Six graph entry points, all `#[pyadapter]`-generated:
//!
//! | Python                          | Rust                     | input | output |
//! |---------------------------------|--------------------------|-------|--------|
//! | `augurs_forecast(stream, …)`    | [`AugursForecastOps`]    | float | `{point, lower, upper}` |
//! | `augurs_changepoint(stream, …)` | [`AugursChangepointOps`] | float | `{indices}` |
//! | `augurs_seasons(stream, …)`     | [`AugursSeasonsOps`]     | float | `{periods}` |
//! | `augurs_outlier(stream, …)`     | [`AugursOutlierOps`]     | list of floats | `{outlying, scores}` |
//! | `augurs_dtw(stream, …)`         | [`AugursDtwOps`]         | list of floats | `{rows}` |
//! | `augurs_cluster(stream, …)`     | [`AugursClusterOps`]     | list of floats | `{labels}` |
//!
//! # The two input shapes
//!
//! The first three analyse **one** series and take a `Stream<f64>`; the last
//! three compare **several** series against each other and take a
//! `Stream<Vec<f64>>` — one value per series, per tick. That split is the Rust
//! adapter's, not a binding choice, and it is why `augurs_outlier` on a stream
//! of plain floats is an error rather than a single-series analysis.
//!
//! Both are marshaled inside the fn from the erased stream rather than through
//! the `typed_input` seam: `Vec<f64>` has no `TryFrom<&PyElement>` edge (only
//! `Vec<u8>` does, for binary payloads), and adding one would make a Python
//! `bytes` silently acceptable where a list of floats is meant.
//!
//! # Selectors are strings
//!
//! `model`, `detector` and `metric` are strings rather than `#[pyclass]` enums,
//! the convention postgres set — a wrong value raises listing the accepted ones.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **Conversion fails loudly.** Legacy's `as_floats` / `as_series` used
//!    `unwrap_or_default()`, so a non-numeric tick became `0.0` and a non-list
//!    tick became an *empty series* — both silently feeding the model bad data.
//!    Here either aborts the run naming the op.
//! 2. **Results are dicts, not bare values.** Legacy returned only the headline
//!    number (the one-step forecast, say); the full result — prediction
//!    intervals, scores, every detected period — is now reachable.
//! 3. **New capability**: the `model` selector (`"mstl"` with seasonal periods),
//!    `level` prediction intervals, and the `min_points` / `hazard_lambda` /
//!    `min_period` / `max_period` knobs, none of which legacy exposed.

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList, PyTuple};
use wingfoil_next::adapters::augurs::{
    AugursChangepointConfig, AugursChangepointOps, AugursChangepoints, AugursClusterConfig,
    AugursClusterOps, AugursClusters, AugursDistanceMatrix, AugursDtwConfig, AugursDtwMetric,
    AugursDtwOps, AugursForecast, AugursForecastConfig, AugursForecastModel, AugursForecastOps,
    AugursOutlierConfig, AugursOutlierDetector, AugursOutlierOps, AugursOutliers, AugursSeasons,
    AugursSeasonsConfig, AugursSeasonsOps,
};
use wingfoil_next::prelude::{Stream, StreamOps};

use crate::{PyElement, pyadapter};

// ---------------------------------------------------------------------------
// Inputs: erased -> f64 / Vec<f64>.
// ---------------------------------------------------------------------------

/// One tick as a float. Fails loudly where legacy substituted `0.0`.
fn as_float(elem: &PyElement, who: &str) -> Result<f64> {
    if elem.is_none() {
        bail!("{who}: stream value is empty (the upstream produced no value)");
    }
    f64::try_from(elem).map_err(|e| anyhow!("{who}: stream value must be a float: {e}"))
}

/// One tick as a series of floats — a Python `list`/`tuple`, one entry per
/// series. Fails loudly where legacy substituted an empty series.
fn as_series(elem: &PyElement, who: &str) -> Result<Vec<f64>> {
    if elem.is_none() {
        bail!("{who}: stream value is empty (the upstream produced no value)");
    }
    Python::attach(|py| {
        let obj = elem.object().bind(py);
        if !obj.is_instance_of::<PyList>() && !obj.is_instance_of::<PyTuple>() {
            bail!(
                "{who}: stream value must be a list of floats (one per series), got {}",
                obj.get_type()
            );
        }
        obj.extract::<Vec<f64>>()
            .map_err(|e| anyhow!("{who}: series entries must be floats: {e}"))
    })
}

// ---------------------------------------------------------------------------
// Outputs: results -> Python dicts.
// ---------------------------------------------------------------------------

/// Build a dict from `(key, value)` pairs, where inserting cannot fail.
fn dict_of(py: Python<'_>, items: &[(&str, Bound<'_, PyAny>)]) -> PyElement {
    let dict = PyDict::new(py);
    for (key, value) in items {
        dict.set_item(key, value)
            .expect("invariant: inserting a str key into a fresh PyDict cannot fail");
    }
    PyElement::new(dict.into_any().unbind())
}

/// Box a slice of scalars as a Python list. Primitive conversions are
/// infallible, and `PyList::new` over an exact-size iterator cannot fail.
fn list_of<'py, T>(py: Python<'py>, values: &[T]) -> Bound<'py, PyAny>
where
    T: Copy + IntoPyObject<'py>,
{
    PyList::new(py, values.iter().copied())
        .expect("invariant: PyList::new over an exact-size iterator cannot fail")
        .into_any()
}

impl From<AugursForecast> for PyElement {
    fn from(f: AugursForecast) -> Self {
        Python::attach(|py| {
            dict_of(
                py,
                &[
                    ("point", list_of(py, &f.point)),
                    ("lower", list_of(py, &f.lower)),
                    ("upper", list_of(py, &f.upper)),
                ],
            )
        })
    }
}

impl From<AugursOutliers> for PyElement {
    fn from(o: AugursOutliers) -> Self {
        Python::attach(|py| {
            dict_of(
                py,
                &[
                    ("outlying", list_of(py, &o.outlying)),
                    ("scores", list_of(py, &o.scores)),
                ],
            )
        })
    }
}

impl From<AugursChangepoints> for PyElement {
    fn from(c: AugursChangepoints) -> Self {
        Python::attach(|py| dict_of(py, &[("indices", list_of(py, &c.indices))]))
    }
}

impl From<AugursSeasons> for PyElement {
    fn from(s: AugursSeasons) -> Self {
        Python::attach(|py| dict_of(py, &[("periods", list_of(py, &s.periods))]))
    }
}

impl From<AugursDistanceMatrix> for PyElement {
    fn from(m: AugursDistanceMatrix) -> Self {
        Python::attach(|py| {
            let rows = PyList::new(py, m.rows.iter().map(|row| list_of(py, row)))
                .expect("invariant: PyList::new over an exact-size iterator cannot fail");
            dict_of(py, &[("rows", rows.into_any())])
        })
    }
}

impl From<AugursClusters> for PyElement {
    fn from(c: AugursClusters) -> Self {
        Python::attach(|py| dict_of(py, &[("labels", list_of(py, &c.labels))]))
    }
}

// ---------------------------------------------------------------------------
// String selectors.
// ---------------------------------------------------------------------------

fn detector(name: &str) -> Result<AugursOutlierDetector> {
    match name {
        "mad" => Ok(AugursOutlierDetector::Mad),
        "dbscan" => Ok(AugursOutlierDetector::Dbscan),
        other => bail!("augurs_outlier: unknown detector '{other}'; expected 'mad' or 'dbscan'"),
    }
}

fn metric(who: &str, name: &str) -> Result<AugursDtwMetric> {
    match name {
        "euclidean" => Ok(AugursDtwMetric::Euclidean),
        "manhattan" => Ok(AugursDtwMetric::Manhattan),
        other => bail!("{who}: unknown metric '{other}'; expected 'euclidean' or 'manhattan'"),
    }
}

// ---------------------------------------------------------------------------
// Entry points.
// ---------------------------------------------------------------------------

/// Forecast a single series over a rolling window.
///
/// Each tick yields `{"point": [float], "lower": [float], "upper": [float]}` —
/// `horizon` point forecasts, with the interval bounds populated only when
/// `level` is given (e.g. `0.95`). The stream must carry floats.
///
/// `model` is `"ets"` (non-seasonal, the default) or `"mstl"`, which needs
/// `periods` — seasonal period lengths in samples, e.g. `[24]` for hourly data
/// with a daily cycle — and a window of at least a couple of full periods.
/// `min_points` is how many samples must accumulate before forecasting starts.
#[pyadapter(name = augurs_forecast)]
#[pyo3(signature = (
    window, horizon, min_points = 12, level = None, model = "ets".to_string(), periods = None,
))]
fn forecast(
    stream: &Stream<PyElement>,
    window: usize,
    horizon: usize,
    min_points: usize,
    level: Option<f64>,
    model: String,
    periods: Option<Vec<usize>>,
) -> Result<Stream<AugursForecast>> {
    let model = match model.as_str() {
        "ets" => AugursForecastModel::Ets,
        "mstl" => AugursForecastModel::Mstl {
            periods: periods.ok_or_else(|| {
                anyhow!("augurs_forecast: model='mstl' needs periods=[…] (seasonal lengths)")
            })?,
        },
        other => bail!("augurs_forecast: unknown model '{other}'; expected 'ets' or 'mstl'"),
    };
    let values = stream.try_map(|e: &PyElement| as_float(e, "augurs_forecast"));
    Ok(values.augurs_forecast(AugursForecastConfig {
        window,
        min_points,
        horizon,
        level,
        model,
    }))
}

/// Detect changepoints in a single series over a rolling window.
///
/// Each tick yields `{"indices": [int]}` — positions within the current window
/// at which the series' behaviour changed. `hazard_lambda` is the expected run
/// length between changepoints: larger means fewer, more confident detections.
#[pyadapter(name = augurs_changepoint)]
#[pyo3(signature = (window, min_points = 8, hazard_lambda = 250.0))]
fn changepoint(
    stream: &Stream<PyElement>,
    window: usize,
    min_points: usize,
    hazard_lambda: f64,
) -> Result<Stream<AugursChangepoints>> {
    let values = stream.try_map(|e: &PyElement| as_float(e, "augurs_changepoint"));
    Ok(values.augurs_changepoint(AugursChangepointConfig {
        window,
        min_points,
        hazard_lambda,
    }))
}

/// Detect seasonal periods in a single series over a rolling window.
///
/// Each tick yields `{"periods": [int]}` — detected period lengths in samples,
/// longest signal first. `min_period` / `max_period` bound the search.
#[pyadapter(name = augurs_seasons)]
#[pyo3(signature = (window, min_points = None, min_period = None, max_period = None))]
fn seasons(
    stream: &Stream<PyElement>,
    window: usize,
    min_points: Option<usize>,
    min_period: Option<u32>,
    max_period: Option<u32>,
) -> Result<Stream<AugursSeasons>> {
    let values = stream.try_map(|e: &PyElement| as_float(e, "augurs_seasons"));
    Ok(values.augurs_seasons(AugursSeasonsConfig {
        window,
        // The Rust default is window-derived, so `None` reproduces it rather
        // than pinning a constant here that would drift from the adapter's.
        min_points: min_points.unwrap_or_else(|| (window / 2).max(16)),
        min_period,
        max_period,
    }))
}

/// Flag outlying series among several, over a rolling window.
///
/// Each tick's value is a `list` of floats — **one per series** — and yields
/// `{"outlying": [int], "scores": [float]}`, indices into that list plus the
/// latest score per series.
///
/// `detector` is `"mad"` (median absolute deviation, works with any number of
/// series) or `"dbscan"` (density clustering, which needs at least three series
/// and reports nothing with fewer). `sensitivity` is in `0.0..=1.0`.
#[pyadapter(name = augurs_outlier)]
#[pyo3(signature = (window, sensitivity, detector = "mad".to_string()))]
fn outlier(
    stream: &Stream<PyElement>,
    window: usize,
    sensitivity: f64,
    detector: String,
) -> Result<Stream<AugursOutliers>> {
    let detector = self::detector(&detector)?;
    let series = stream.try_map(|e: &PyElement| as_series(e, "augurs_outlier"));
    Ok(series.augurs_outlier(AugursOutlierConfig {
        window,
        sensitivity,
        detector,
    }))
}

/// Dynamic-time-warping distances between several series, over a rolling window.
///
/// Each tick's value is a `list` of floats — one per series — and yields
/// `{"rows": [[float]]}`, a row-major `n × n` matrix where `rows[i][j]` is the
/// distance between series `i` and `j`.
#[pyadapter(name = augurs_dtw)]
#[pyo3(signature = (window, metric = "euclidean".to_string()))]
fn dtw(
    stream: &Stream<PyElement>,
    window: usize,
    metric: String,
) -> Result<Stream<AugursDistanceMatrix>> {
    let metric = self::metric("augurs_dtw", &metric)?;
    let series = stream.try_map(|e: &PyElement| as_series(e, "augurs_dtw"));
    Ok(series.augurs_dtw(AugursDtwConfig { window, metric }))
}

/// Cluster several series by DTW distance, over a rolling window.
///
/// Each tick's value is a `list` of floats — one per series — and yields
/// `{"labels": [int]}`, a cluster label per series where **`-1` means noise**
/// (unclustered) and non-negative labels group series together.
///
/// `epsilon` is the neighbourhood radius and `min_cluster_size` the minimum
/// members for a cluster (DBSCAN parameters).
#[pyadapter(name = augurs_cluster)]
#[pyo3(signature = (window, epsilon, min_cluster_size, metric = "euclidean".to_string()))]
fn cluster(
    stream: &Stream<PyElement>,
    window: usize,
    epsilon: f64,
    min_cluster_size: usize,
    metric: String,
) -> Result<Stream<AugursClusters>> {
    let metric = self::metric("augurs_cluster", &metric)?;
    let series = stream.try_map(|e: &PyElement| as_series(e, "augurs_cluster"));
    Ok(series.augurs_cluster(AugursClusterConfig {
        window,
        epsilon,
        min_cluster_size,
        metric,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn float_element(py: Python<'_>, v: f64) -> PyElement {
        PyElement::new(v.into_pyobject(py).unwrap().into_any().unbind())
    }

    #[test]
    fn a_forecast_erases_to_a_dict_of_lists() {
        let element = PyElement::from(AugursForecast {
            point: vec![1.0, 2.0],
            lower: vec![0.5],
            upper: vec![2.5],
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let get = |k: &str| dict.get_item(k).unwrap().unwrap();
            assert_eq!(vec![1.0, 2.0], get("point").extract::<Vec<f64>>().unwrap());
            assert_eq!(vec![0.5], get("lower").extract::<Vec<f64>>().unwrap());
            assert_eq!(vec![2.5], get("upper").extract::<Vec<f64>>().unwrap());
        });
    }

    #[test]
    fn a_distance_matrix_erases_to_nested_lists() {
        let element = PyElement::from(AugursDistanceMatrix {
            rows: vec![vec![0.0, 1.5], vec![1.5, 0.0]],
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let rows = dict.get_item("rows").unwrap().unwrap();
            assert_eq!(
                vec![vec![0.0, 1.5], vec![1.5, 0.0]],
                rows.extract::<Vec<Vec<f64>>>().unwrap()
            );
        });
    }

    #[test]
    fn clusters_keep_the_noise_label() {
        let element = PyElement::from(AugursClusters {
            labels: vec![0, -1, 1],
        });
        Python::attach(|py| {
            let dict = element.object().bind(py).cast::<PyDict>().unwrap().clone();
            let labels = dict.get_item("labels").unwrap().unwrap();
            assert_eq!(vec![0, -1, 1], labels.extract::<Vec<i32>>().unwrap());
        });
    }

    #[test]
    fn a_float_tick_converts_and_a_non_float_errors() {
        Python::attach(|py| {
            assert_eq!(1.5, as_float(&float_element(py, 1.5), "op").unwrap());
            let text = PyElement::from("nope");
            assert!(as_float(&text, "op").is_err());
            assert!(as_float(&PyElement::none(), "op").is_err());
        });
    }

    #[test]
    fn a_series_tick_converts_and_a_non_list_errors() {
        Python::attach(|py| {
            let list = PyList::new(py, [1.0_f64, 2.0]).unwrap();
            let element = PyElement::new(list.into_any().unbind());
            assert_eq!(vec![1.0, 2.0], as_series(&element, "op").unwrap());

            // A bare float is not a series — legacy turned this into an empty one.
            let scalar = float_element(py, 1.0);
            let err = as_series(&scalar, "op").unwrap_err();
            assert!(err.to_string().contains("must be a list"), "got: {err}");
        });
    }

    #[test]
    fn selectors_reject_unknown_names() {
        assert!(detector("mad").is_ok());
        assert!(detector("dbscan").is_ok());
        let err = detector("iqr").unwrap_err();
        assert!(err.to_string().contains("unknown detector 'iqr'"), "{err}");

        assert!(metric("augurs_dtw", "euclidean").is_ok());
        assert!(metric("augurs_dtw", "manhattan").is_ok());
        assert!(metric("augurs_dtw", "cosine").is_err());
    }
}
