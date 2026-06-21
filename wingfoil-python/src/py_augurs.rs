//! Python bindings for the augurs adapter.
//!
//! augurs is a pure-Rust compute library, so both bindings are stream
//! transforms exposed as methods on [`PyStream`] rather than source functions:
//!
//! - `.augurs_forecast(...)` consumes a stream of floats and yields a dict
//!   `{"point": list[float], "lower": list[float], "upper": list[float]}`.
//! - `.augurs_outlier(...)` consumes a stream of `list[float]` (one value per
//!   series per tick) and yields `{"outlying": list[int], "scores": list[float]}`.

use std::rc::Rc;

use pyo3::prelude::*;
use pyo3::types::PyDict;
use wingfoil::adapters::augurs::{
    AugursForecastConfig, AugursForecastOperators, AugursOutlierConfig, AugursOutlierOperators,
};
use wingfoil::{Stream, StreamOperators};

use crate::py_element::PyElement;

/// Inner implementation for the `.augurs_forecast()` stream method.
pub fn py_augurs_forecast_inner(
    stream: &Rc<dyn Stream<PyElement>>,
    window: usize,
    horizon: usize,
    level: Option<f64>,
    min_points: usize,
) -> Rc<dyn Stream<PyElement>> {
    let floats: Rc<dyn Stream<f64>> = stream.map(|elem| {
        Python::attach(|py| {
            elem.as_ref().extract::<f64>(py).unwrap_or_else(|e| {
                log::error!("augurs_forecast: expected a float input value: {e}");
                f64::NAN
            })
        })
    });

    let mut config = AugursForecastConfig::new(window, horizon).with_min_points(min_points);
    if let Some(level) = level {
        config = config.with_level(level);
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
) -> Rc<dyn Stream<PyElement>> {
    let series: Rc<dyn Stream<Vec<f64>>> = stream.map(|elem| {
        Python::attach(|py| {
            elem.as_ref().extract::<Vec<f64>>(py).unwrap_or_else(|e| {
                log::error!("augurs_outlier: expected a list[float] input value: {e}");
                Vec::new()
            })
        })
    });

    series
        .augurs_outlier(AugursOutlierConfig::new(window, sensitivity))
        .map(|outliers| {
            Python::attach(|py| {
                let dict = PyDict::new(py);
                dict.set_item("outlying", outliers.outlying)
                    .and_then(|()| dict.set_item("scores", outliers.scores))
                    .expect("invariant: inserting list values into a dict cannot fail");
                PyElement::new(dict.into_any().unbind())
            })
        })
}
