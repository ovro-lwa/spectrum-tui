use core::f64;

use anyhow::Result;
use async_trait::async_trait;
use ndarray::{Array, Ix1, Ix2, Zip};

#[cfg(feature = "ovro")]
pub mod ovro;

#[cfg(feature = "lwa-na")]
pub mod north_arm;

#[derive(Debug, Clone)]
pub struct AutoSpectra {
    pub(crate) freq_min: f64,
    pub(crate) freq_max: f64,
    pub(crate) ant_names: Vec<String>,
    pub(crate) spectra: Vec<Vec<(f64, f64)>>,
    pub(crate) log_spectra: Vec<Vec<(f64, f64)>>,
    pub(crate) plot_log: bool,
}
impl AutoSpectra {
    pub fn new(
        ant_names: Vec<String>,
        freqs: Array<f64, Ix1>,
        // Spectra must be given as (ant_names, nfreqs) array
        data: Array<f64, Ix2>,
        plot_log: bool,
    ) -> Self {
        let freq_min = freqs.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let freq_max = freqs.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        let log_spectra = data
            .outer_iter()
            .map(|inner| {
                Zip::from(inner)
                    .and(&freqs)
                    .map_collect(|y, x| (*x, 10.0 * y.log10()))
                    .to_vec()
                    .into_iter()
                    .filter(|(_freq, val)| val.is_finite())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        let spectra = data
            .outer_iter()
            .map(|inner| {
                Zip::from(inner)
                    .and(&freqs)
                    .map_collect(|y, x| (*x, *y))
                    .to_vec()
            })
            .collect::<Vec<_>>();

        Self {
            freq_min,
            freq_max,
            ant_names,
            spectra,
            log_spectra,
            plot_log,
        }
    }

    pub fn ymin(&self) -> f64 {
        let data_to_min = match self.plot_log {
            true => &self.log_spectra,
            false => &self.spectra,
        };

        data_to_min.iter().fold(f64::INFINITY, |a, b| {
            a.min(b.iter().fold(f64::INFINITY, |c, &d| c.min(d.1)))
        }) - 10.0
    }

    pub fn ymax(&self) -> f64 {
        let data_to_max = match self.plot_log {
            true => &self.log_spectra,
            false => &self.spectra,
        };

        data_to_max.iter().fold(f64::NEG_INFINITY, |a, b| {
            a.max(b.iter().fold(f64::NEG_INFINITY, |c, &d| c.max(d.1)))
        }) + 10.0
    }
}

#[async_trait]
// allow dead code or complains in the test compilation mode (no-op)
#[allow(dead_code)]
pub trait SpectrumLoader {
    /// Loads autospectrum data from the underlying source and sends
    /// correlations (freq, val) pairs over the channel to the main process.
    async fn get_data(&mut self) -> Option<AutoSpectra>;

    /// Filters the antennas to be plotted based on their string names.
    fn filter_antenna(&mut self, antenna_number: &[String]) -> Result<()>;
}
