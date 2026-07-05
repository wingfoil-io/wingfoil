//! Python bindings for the augurs adapter.
//!
//! augurs is a pure-Rust compute library, so all bindings are stream transforms
//! exposed as methods on [`PyStream`] rather than source functions. Each yields
//! a dict per tick:
//!
//! - `.augurs_forecast(...)` — floats in → `{"point", "lower", "upper"}`.
//! - `.augurs_outlier(...)` — `list[float]` in → `{"outlying", "scores"}`.
//! - `.augurs_changepoint(...)` — floats in → `{"indices"}`.
//! - `.augurs_seasons(...)` — floats in → `{"periods"}`.
//! - `.augurs_dtw(...)` — `list[float]` in → `{"rows"}` (distance matrix).
//! - `.augurs_cluster(...)` — `list[float]` in → `{"labels"}`.

use std::rc::Rc;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use wingfoil::adapters::augurs::{
    AugursChangepointConfig, AugursChangepointOperators, AugursClusterConfig,
    AugursClusterOperators, AugursDtwConfig, AugursDtwMetric, AugursDtwOperators,
    AugursForecastConfig, AugursForecastOperators, AugursOutlierConfig, AugursOutlierOperators,
    AugursSeasonsConfig, AugursSeasonsOperators,
};
use wingfoil::{Stream, StreamOperators};

use crate::py_element::PyElement;

/// Map a `PyElement` stream to an `f64` stream, failing the graph run with a
/// contextual error on a value that is not a float. This mirrors
/// [`PyStream::extract`] used by `.average()` / `.sum()`; substituting `NaN`
/// instead would silently poison the model window and abort the run later with
/// an augurs-internal message that never points back at the offending value.
fn as_floats(stream: &Rc<dyn Stream<PyElement>>, op: &'static str) -> Rc<dyn Stream<f64>> {
    stream.try_map(move |elem: PyElement| {
        Python::attach(|py| {
            elem.as_ref().extract::<f64>(py).map_err(|e| {
                anyhow::Error::new(e).context(format!("{op}: expected a float input value"))
            })
        })
    })
}

/// Map a `PyElement` stream to a `Vec<f64>` (per-series readings) stream,
/// failing the graph run with a contextual error on a value that is not a
/// `list[float]`.
fn as_series(stream: &Rc<dyn Stream<PyElement>>, op: &'static str) -> Rc<dyn Stream<Vec<f64>>> {
    stream.try_map(move |elem: PyElement| {
        Python::attach(|py| {
            elem.as_ref().extract::<Vec<f64>>(py).map_err(|e| {
                anyhow::Error::new(e).context(format!("{op}: expected a list[float] input value"))
            })
        })
    })
}

fn metric_from_str(metric: &str) -> PyResult<AugursDtwMetric> {
    match metric.to_ascii_lowercase().as_str() {
        "manhattan" | "l1" => Ok(AugursDtwMetric::Manhattan),
        "euclidean" | "l2" => Ok(AugursDtwMetric::Euclidean),
        other => Err(pyo3::exceptions::PyValueError::new_err(format!(
            "unknown metric '{other}' (expected 'euclidean' or 'manhattan')"
        ))),
    }
}

/// Inner implementation for the `.augurs_forecast()` stream method.
pub fn py_augurs_forecast_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: usize,
    horizon: usize,
    level: Option<f64>,
    min_points: usize,
    periods: Option<Vec<usize>>,
) -> Rc<dyn Stream<PyElement>> {
    let floats = as_floats(stream, "augurs_forecast");

    let mut config = AugursForecastConfig::new(window, horizon).with_min_points(min_points);
    if let Some(level) = level {
        config = config.with_level(level);
    }
    if let Some(periods) = periods.filter(|p| !p.is_empty()) {
        config = config.mstl(periods);
    }

    floats.augurs_forecast(config).map(|forecast| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("point", forecast.point)
                .and_then(|()| dict.set_item("lower", forecast.lower))
                .and_then(|()| dict.set_item("upper", forecast.upper))
                .expect("invariant: inserting list values into a dict cannot fail");
            PyElement::new(dict.into_any().unbind())
        })
    })
}

/// Inner implementation for the `.augurs_outlier()` stream method.
pub fn py_augurs_outlier_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: usize,
    sensitivity: f64,
    detector: &str,
) -> PyResult<Rc<dyn Stream<PyElement>>> {
    let series = as_series(stream, "augurs_outlier");
    let config = match detector.to_ascii_lowercase().as_str() {
        "dbscan" => AugursOutlierConfig::dbscan(window, sensitivity),
        "mad" => AugursOutlierConfig::mad(window, sensitivity),
        other => {
            return Err(pyo3::exceptions::PyValueError::new_err(format!(
                "unknown detector '{other}' (expected 'mad' or 'dbscan')"
            )));
        }
    };

    Ok(series.augurs_outlier(config).map(|outliers| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("outlying", outliers.outlying)
                .and_then(|()| dict.set_item("scores", outliers.scores))
                .expect("invariant: inserting list values into a dict cannot fail");
            PyElement::new(dict.into_any().unbind())
        })
    }))
}

/// Inner implementation for the `.augurs_changepoint()` stream method.
pub fn py_augurs_changepoint_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: usize,
    min_points: usize,
    hazard: f64,
) -> Rc<dyn Stream<PyElement>> {
    let floats = as_floats(stream, "augurs_changepoint");
    let config = AugursChangepointConfig::new(window)
        .with_min_points(min_points)
        .with_hazard(hazard);

    floats.augurs_changepoint(config).map(|changes| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("indices", changes.indices)
                .expect("invariant: inserting a list into a dict cannot fail");
            PyElement::new(dict.into_any().unbind())
        })
    })
}

/// Inner implementation for the `.augurs_seasons()` stream method.
pub fn py_augurs_seasons_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: usize,
    min_points: Option<usize>,
    min_period: Option<u32>,
    max_period: Option<u32>,
) -> Rc<dyn Stream<PyElement>> {
    let floats = as_floats(stream, "augurs_seasons");
    let mut config = AugursSeasonsConfig::new(window);
    if let Some(min_points) = min_points {
        config = config.with_min_points(min_points);
    }
    // Apply each period bound independently — the Python signature documents
    // them as separately optional, so passing only one must not be discarded.
    config.min_period = min_period;
    config.max_period = max_period;

    floats.augurs_seasons(config).map(|seasons| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("periods", seasons.periods)
                .expect("invariant: inserting a list into a dict cannot fail");
            PyElement::new(dict.into_any().unbind())
        })
    })
}

/// Inner implementation for the `.augurs_dtw()` stream method.
pub fn py_augurs_dtw_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: usize,
    metric: &str,
) -> PyResult<Rc<dyn Stream<PyElement>>> {
    let series = as_series(stream, "augurs_dtw");
    let config = AugursDtwConfig::new(window).with_metric(metric_from_str(metric)?);

    Ok(series.augurs_dtw(config).map(|matrix| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("rows", matrix.rows)
                .expect("invariant: inserting a list into a dict cannot fail");
            PyElement::new(dict.into_any().unbind())
        })
    }))
}

/// Inner implementation for the `.augurs_cluster()` stream method.
pub fn py_augurs_cluster_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: usize,
    epsilon: f64,
    min_cluster_size: usize,
    metric: &str,
) -> PyResult<Rc<dyn Stream<PyElement>>> {
    let series = as_series(stream, "augurs_cluster");
    let config = AugursClusterConfig::new(window, epsilon, min_cluster_size)
        .with_metric(metric_from_str(metric)?);

    Ok(series.augurs_cluster(config).map(|clusters| {
        Python::attach(|py| {
            let dict = PyDict::new(py);
            dict.set_item("labels", clusters.labels)
                .expect("invariant: inserting a list into a dict cannot fail");
            PyElement::new(dict.into_any().unbind())
        })
    }))
}
