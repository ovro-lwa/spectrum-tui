use anyhow::Result;
use async_trait::async_trait;

#[cfg(feature = "ovro")]
pub mod ovro;

#[cfg(feature = "lwa-na")]
pub mod north_arm;

#[derive(Debug, Clone)]
pub struct AutoSpectra {
    pub freq_min: f64,
    pub freq_max: f64,
    pub ant_names: Vec<String>,
    pub spectra: Vec<Vec<(f64, f64)>>,
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
