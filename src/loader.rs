use std::{collections::HashSet, path::PathBuf, time::SystemTime};

use anyhow::{Context, Result};
use async_trait::async_trait;
use etcd_client::{Client, WatchOptions};
use futures::StreamExt;
use itertools::Itertools;
use log::info;
use ndarray::{concatenate, Array, Axis, Ix2, Zip};
use ndarray_npy::read_npy;

use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct AutoSpectra {
    pub freq_min: f64,
    pub freq_max: f64,
    pub ant_names: Vec<String>,
    pub spectra: Vec<Vec<(f64, f64)>>,
}

#[async_trait]
pub trait SpectrumLoader {
    /// Loads autospectrum data from the underlying source and sends
    /// correlations (freq, val) pairs over the channel to the main process.
    async fn get_data(&mut self) -> Option<AutoSpectra>;
}

pub(crate) struct DiskLoader {
    n_spectra: usize,
    file: PathBuf,
}
impl DiskLoader {
    pub fn new(n_spectra: usize, file: PathBuf) -> Self {
        Self { n_spectra, file }
    }
}
#[async_trait]
impl SpectrumLoader for DiskLoader {
    async fn get_data(&mut self) -> Option<AutoSpectra> {
        let data: Array<f64, Ix2> = read_npy(&self.file).expect("unabe to read.");

        let len = data.shape()[1];
        let xs = Array::linspace(0.0, 98.3, len);
        let xmin = xs.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let xmax = xs.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
        let spectra = data
            .outer_iter()
            .filter(|inner| !inner.iter().all(|y| y.is_nan()))
            .take(2 * self.n_spectra)
            .map(|inner| {
                Zip::from(inner)
                    .and(&xs)
                    .map_collect(|y, x| (*x, 10.0 * y.log10()))
                    .to_vec()
            })
            .collect::<Vec<_>>();
        let ant_names = (0..(2 * self.n_spectra))
            .map(|x| match x % 2 == 0 {
                true => (x / 2).to_string() + "A",
                false => (x / 2).to_string() + "B",
            })
            .collect::<Vec<_>>();

        let data = AutoSpectra {
            freq_min: xmin,
            freq_max: xmax,
            ant_names,
            spectra,
        };

        Some(data)
    }
}

const ETCD_RESP_KEY: &str = "/resp/snap/";
const ETCD_CMD_ROOT: &str = "/cmd/snap/";

#[derive(Debug, Clone)]
struct AntInfo {
    antname: String,
    snap2_location: i64,
    pola_fpga_num: i64,
    polb_fpga_num: i64,
}
impl core::cmp::PartialEq for AntInfo {
    fn eq(&self, other: &Self) -> bool {
        self.snap2_location == other.snap2_location
    }
}
impl core::cmp::Eq for AntInfo {}
impl core::cmp::PartialOrd for AntInfo {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.snap2_location.cmp(&other.snap2_location))
    }
}
impl core::cmp::Ord for AntInfo {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.snap2_location.cmp(&other.snap2_location)
    }
}

pub(crate) struct EtcdLoader {
    /// etcd3 client to communicate with correlator
    client: Client,
    /// Antenna configuration matrix
    ant_info: Vec<AntInfo>,
    /// Antenna Filter to apply on FGPA call
    /// Filter consists of [Antenna Number, FPGA number, polA index, polB index]
    filter: Option<Vec<AntInfo>>,
}
impl EtcdLoader {
    pub async fn new<T: AsRef<str>>(address: T) -> Result<Self> {
        let mut client = Client::connect(&[address.as_ref()], None)
            .await
            .context("Error connecting to etcd server.")?;

        let config = client.get("/cfg/system", None).await?;
        let full_json = serde_json::from_str::<Value>(config.kvs()[0].value_str()?)
            .context("Error generating JSON from etcd respose.")?;

        let dict = full_json.get("lwacfg").unwrap().as_object().unwrap();

        let ant_info = match dict.keys().find(|x| x.eq(&"snap2_location")) {
            Some(_) => {
                let ants = dict
                    .values()
                    .flat_map(|val| val.as_object().unwrap().keys())
                    .collect::<HashSet<_>>();
                let mut all_series = vec![];
                for ant in ants.iter() {
                    all_series.push(AntInfo {
                        antname: dict
                            .get("antname")
                            .and_then(|name| {
                                name.as_object()
                                    .and_then(|next| next.get(*ant).and_then(|val| val.as_str()))
                            })
                            .unwrap_or("null")
                            .to_owned(),
                        snap2_location: dict
                            .get("snap2_location")
                            .and_then(|name| {
                                name.as_object()
                                    .and_then(|next| next.get(*ant).and_then(|val| val.as_i64()))
                            })
                            .unwrap_or(-1),
                        pola_fpga_num: dict
                            .get("pola_fpga_num")
                            .and_then(|name| {
                                name.as_object()
                                    .and_then(|next| next.get(*ant).and_then(|val| val.as_i64()))
                            })
                            .unwrap_or(-1),
                        polb_fpga_num: dict
                            .get("polb_fpga_num")
                            .and_then(|name| {
                                name.as_object()
                                    .and_then(|next| next.get(*ant).and_then(|val| val.as_i64()))
                            })
                            .unwrap_or(-1),
                    });
                }
                all_series
            }
            None => {
                let mut all_series = vec![];

                for ant_dict in dict.values() {
                    all_series.push(AntInfo {
                        antname: ant_dict
                            .get("antname")
                            .and_then(|name| name.as_str())
                            .unwrap_or("null")
                            .to_owned(),
                        snap2_location: ant_dict
                            .get("snap2_location")
                            .and_then(|snap| snap.as_i64())
                            .unwrap_or(-1),
                        pola_fpga_num: ant_dict
                            .get("pola_fpga_num")
                            .and_then(|fpga| fpga.as_i64())
                            .unwrap_or(-1),
                        polb_fpga_num: ant_dict
                            .get("polb_fpga_num")
                            .and_then(|fpga| fpga.as_i64())
                            .unwrap_or(-1),
                    });
                }
                all_series
            }
        };
        info!("Configuration loaded.");

        Ok(Self {
            client,
            ant_info,
            filter: None,
        })
    }

    pub fn filter_antenna(&mut self, antenna_number: Vec<String>) -> Result<()> {
        self.filter = antenna_number
            .iter()
            .map(|ant| {
                self.ant_info
                    .iter()
                    .find(|info| info.antname == *ant)
                    .cloned()
            })
            // this sorts them by snap location
            .sorted()
            .collect();

        Ok(())
    }

    fn get_snaps(&self) -> Option<Vec<i64>> {
        self.filter.as_ref().map(|ants| {
            ants.iter()
                .map(|a| a.snap2_location)
                .unique()
                .sorted()
                .collect()
        })
    }

    async fn get_spectra_for_snap(
        &mut self,
        snap_location: Option<i64>,
    ) -> Result<Array<f64, Ix2>> {
        let cmd_key = snap_location
            .as_ref()
            .map_or(format!("{ETCD_CMD_ROOT}0"), |info| {
                format!("{ETCD_CMD_ROOT}{:0>2}", info)
            });
        let mut spectra = Array::<f64, Ix2>::zeros((64, 4096));

        for (signal_block, mut chunk) in
            spectra.exact_chunks_mut((16, 4096)).into_iter().enumerate()
        {
            let timestamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .context("Unable to convert Sytem time to unix epoch")?
                .as_micros() as f64
                * 1e-6_f64;

            let seq_id = format!("{}", (timestamp * 1e6).round() as i64);
            let command = serde_json::to_string(&json!({
                "cmd": "get_new_spectra",
                "val": {
                    "block": "autocorr",
                    "timestamp": timestamp,
                    "kwargs": {"signal_block": signal_block},
                    },
                "id": seq_id,
            }))
            .context("Unable to format request JSON")?;

            let (_watcher, mut stream) = self
                .client
                .watch(ETCD_RESP_KEY, Some(WatchOptions::new().with_prefix()))
                .await
                .context("Unable to watch ETCD response key")?;

            // send command
            self.client
                .put(cmd_key.clone(), command, None)
                .await
                .context("Unable to put spectrum request.")?;

            'while_loop: while let Some(Ok(response)) = stream.next().await {
                for event in response.events() {
                    if let Some(Ok(dict)) = event
                        .kv()
                        .map(|keyval| serde_json::from_slice::<Value>(keyval.value()))
                    {
                        if let Some(id) = dict.get("id").and_then(|val| val.as_str()) {
                            if id == seq_id {
                                let spectra = dict["val"]["response"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .flat_map(|spec| {
                                        spec.as_array().unwrap().iter().map(|x| x.as_f64().unwrap())
                                    })
                                    .collect::<Vec<f64>>();
                                {
                                    chunk.assign(
                                        &Array::from_shape_vec((16, 4096), spectra)
                                            .context("Cannot fit spectra in to shape (16, 4096)")?,
                                    );
                                    break 'while_loop;
                                }
                            }
                        }
                    }
                }
            }
        }
        Ok(spectra)
    }

    pub async fn request_autos(&mut self) -> Result<Array<f64, Ix2>> {
        if let Some(snaps) = self.get_snaps() {
            let mut all_sectra = Array::zeros((0, 4096));

            for snap in snaps {
                let mut spectra = self.get_spectra_for_snap(Some(snap)).await?;

                if let Some(all_info) = self.filter.as_ref() {
                    let mut axes = vec![];
                    for info in all_info {
                        if info.snap2_location == snap {
                            axes.extend([info.pola_fpga_num as usize, info.polb_fpga_num as usize]);
                        }
                    }
                    spectra = Array::from_iter(
                        spectra
                            .outer_iter()
                            .enumerate()
                            .filter_map(|(cnt, ax)| {
                                if axes.contains(&cnt) {
                                    Some(ax.to_vec())
                                } else {
                                    None
                                }
                            })
                            .flatten(),
                    )
                    .into_shape((2, 4096))?;
                    all_sectra = concatenate![Axis(0), all_sectra.view(), spectra.view()];
                }
            }
            Ok(all_sectra)
        } else {
            Ok(self.get_spectra_for_snap(None).await?)
        }
    }
}
#[async_trait]
impl SpectrumLoader for EtcdLoader {
    async fn get_data(&mut self) -> Option<AutoSpectra> {
        let spectra = self.request_autos().await.ok()?;

        let xs = Array::linspace(0.0, 98.3, spectra.shape()[1]);
        let xmin = xs.iter().fold(f64::INFINITY, |a, &b| a.min(b));
        let xmax = xs.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));

        let spectra = spectra
            .outer_iter()
            .filter(|inner| !inner.iter().all(|y| y.is_nan()))
            .map(|inner| {
                Zip::from(inner)
                    .and(&xs)
                    .map_collect(|y, x| (*x, 10.0 * y.log10()))
                    .to_vec()
            })
            .collect::<Vec<_>>();

        let ant_names = if let Some(all_info) = self.filter.as_ref() {
            all_info
                .iter()
                .flat_map(|info| [format!("{}a", info.antname), format!("{}b", info.antname)])
                .collect()
        } else {
            (0..spectra.len()).map(|x| format!("{x}")).collect()
        };

        let data = AutoSpectra {
            freq_min: xmin,
            freq_max: xmax,
            ant_names,
            spectra,
        };

        Some(data)
    }
}
