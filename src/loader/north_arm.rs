use std::{
    fs,
    io::{BufReader, Read},
    path::Path,
};

// adapted from https://github.com/lwa-project/lsl/blob/main/lsl/reader/drspec.cpp
use anyhow::{anyhow, bail, Context, Result};
use byteorder::{LittleEndian, ReadBytesExt};
use hifitime::Epoch;
use ndarray::{Array, Axis, Ix1, Ix2, Ix3};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PolarizationType {
    LinearXX = 0x01,
    LinearXYReRe = 0x02,
    LinearXYIm = 0x04,
    LinearYY = 0x08,
    LinearRealHalf = 0x01 | 0x08,
    LinearOtherHalf = 0x02 | 0x04,
    LinearFull = 0x0f,
    StokesI = 0x10,
    StokesQ = 0x20,
    StokesU = 0x40,
    StokesV = 0x80,
    StokesRealHalf = 0x10 | 0x8,
    StokesOtherHalf = 0x20 | 0x40,
    StokesFull = 0xf0,
}
impl PolarizationType {
    fn from_u8(val: u8) -> Option<Self> {
        match val {
            0x01 => Some(Self::LinearXX),
            0x02 => Some(Self::LinearXYReRe),
            0x04 => Some(Self::LinearXYIm),
            0x08 => Some(Self::LinearYY),
            0x0f => Some(Self::LinearFull),
            0x10 => Some(Self::StokesI),
            0x20 => Some(Self::StokesQ),
            0x40 => Some(Self::StokesU),
            0x80 => Some(Self::StokesV),
            0xf0 => Some(Self::StokesFull),
            _ => None,
        }
    }
    #[inline(always)]
    fn pol_count(self) -> u8 {
        let mut v = self as u8;
        let mut c = 0;
        while v != 0 {
            v &= v - 1;
            c += 1;
        }
        c
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DRHeader {
    /// time tag of first frame in ``block''
    /// Time stamp is calculated from number of clocks as
    /// (timetag (read from file)  - time_offset) / [Self::CLOCK_SPEED]
    pub timestamp: Epoch,

    /// time offset reported by DP
    pub time_offset: u16,

    /// decimation factor
    pub decimation_factor: u16,

    /// DP frequencies for each tuning in Hz
    ///   Frequencies are calculated from the
    ///   tuning words in each file as: word * [Self::CLOCK_SPEED] / 2^32
    ///   indexing: 0..1 = Tuning 1..2
    pub frequencies: [f64; 2],

    /// fills for each pol/tuning combination
    ///   indexing: 0..3 = X0, Y0 X1, Y1
    pub fills: [u32; 4],

    /// error flag for each pol/tuning combo
    ///   indexing: 0..3 = X0, Y0 X1, Y1
    pub errors: [u8; 4],

    /// beam number
    pub beam: u8,

    /// ouptut format
    pub stokes_format: PolarizationType,

    /// version of the spectrometer data file
    pub specrometer_version: u8,

    /// flag bit-field
    pub flags: u8,

    /// <Transform Length>
    pub n_freqs: u32,

    /// <Integration Count>
    pub n_ints: u32,

    /// saturation count for each pol/tuning combo
    ///   indexing: 0..3 = X0, Y0 X1, Y1
    pub saturation_count: [u32; 4],
}
impl DRHeader {
    const SYNC_HEADER: u32 = 0xC0DEC0DE_u32;
    const SYNC_FOOTER: u32 = 0xED0CED0C_u32;

    const CLOCK_SPEED: f64 = 196.0e6;

    pub fn from_bytes<R: Read>(buffer: &mut R) -> Result<Self> {
        let header = buffer.read_u32::<LittleEndian>()?;
        if header != Self::SYNC_HEADER {
            bail!(
                "DR File Header leading MAGIC Code error. Expected {:#08X} != Recovered {:#08X}",
                Self::SYNC_HEADER,
                header
            )
        }

        let time_tag = buffer.read_u64::<LittleEndian>()?;
        let time_offset = buffer.read_u16::<LittleEndian>()?;

        let me = Self {
            timestamp: Self::calc_epoch(time_tag, time_offset),
            time_offset,
            decimation_factor: buffer.read_u16::<LittleEndian>()?,
            frequencies: (0..2)
                .map(|_| buffer.read_u32::<LittleEndian>().map(Self::calc_freq))
                .collect::<std::result::Result<Vec<_>, std::io::Error>>()?
                .try_into()
                .expect("Unable to initialize frequenceies as len 2 array."),
            fills: (0..4)
                .map(|_| buffer.read_u32::<LittleEndian>())
                .collect::<std::result::Result<Vec<_>, std::io::Error>>()?
                .try_into()
                .expect("Unable to initialize fills as len 4 array."),
            errors: (0..4)
                .map(|_| buffer.read_u8())
                .collect::<std::result::Result<Vec<_>, std::io::Error>>()?
                .try_into()
                .expect("Unable to initialize errors as len 4 array."),
            beam: buffer.read_u8()?,
            stokes_format: {
                let pol = buffer.read_u8()?;
                PolarizationType::from_u8(pol)
                    .ok_or_else(|| anyhow!("Unkown polarization type value: {pol}"))?
            },
            specrometer_version: buffer.read_u8()?,
            flags: buffer.read_u8()?,
            n_freqs: buffer.read_u32::<LittleEndian>()?,
            n_ints: buffer.read_u32::<LittleEndian>()?,
            saturation_count: (0..4)
                .map(|_| buffer.read_u32::<LittleEndian>())
                .collect::<std::result::Result<Vec<_>, std::io::Error>>()?
                .try_into()
                .expect("Unable to initialize saturation_count as len 4 array."),
        };

        let footer = buffer.read_u32::<LittleEndian>()?;
        if footer != Self::SYNC_FOOTER {
            bail!(
                "DR File Header trailing MAGIC Code error. Expected {:#08X} != Recovered {:#08X}",
                Self::SYNC_FOOTER,
                footer
            )
        }

        Ok(me)
    }

    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();

        // header is only 76 bytes, we don't need to read more than that
        let mut buffer = BufReader::with_capacity(
            76,
            fs::OpenOptions::new()
                .read(true)
                .open(path)
                .with_context(|| format!("Unable to open {}", path.display()))?,
        );

        Self::from_bytes(&mut buffer)
    }

    fn calc_freq(tunings: u32) -> f64 {
        tunings as f64 * Self::CLOCK_SPEED / 2_f64.powi(32)
    }

    fn calc_tuning(freq: f64) -> u32 {
        (freq * 2_f64.powi(32) / Self::CLOCK_SPEED).round() as u32
    }

    fn calc_epoch(time_tag: u64, offset: u16) -> Epoch {
        let tt = time_tag - offset as u64;
        Epoch::from_unix_seconds(tt as f64 / Self::CLOCK_SPEED)
    }

    fn calc_timetag(&self) -> u64 {
        let seconds = self.timestamp.to_unix_seconds().floor() as u64;

        let sec_frac = self.timestamp - hifitime::Epoch::from_unix_seconds(seconds as f64);

        let mut tt = seconds * Self::CLOCK_SPEED as u64;
        tt += (sec_frac.to_unit(hifitime::Unit::Millisecond) * Self::CLOCK_SPEED).floor() as u64
            / 1000;
        tt
    }

    pub(crate) fn sample_rate(&self) -> f64 {
        Self::CLOCK_SPEED / self.decimation_factor as f64
    }

    pub(crate) fn get_freqs(&self) -> Array<f64, Ix2> {
        let fmin1 = self.frequencies[0] - self.sample_rate() / 2.0;
        let fmax1 = self.frequencies[0] + self.sample_rate() / 2.0;

        let fmin2 = self.frequencies[1] - self.sample_rate() / 2.0;
        let fmax2 = self.frequencies[1] + self.sample_rate() / 2.0;

        ndarray::stack![
            Axis(0),
            Array::<f64, Ix1>::linspace(fmin1, fmax1, self.n_freqs as usize),
            Array::<f64, Ix1>::linspace(fmin2, fmax2, self.n_freqs as usize)
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DRSpectrum {
    /// Metadata information about this spectrum
    pub header: DRHeader,

    pub data: Array<f32, Ix3>,
}
impl DRSpectrum {
    pub fn from_bytes<R: Read>(file_handle: &mut R) -> Result<Self> {
        let header = DRHeader::from_bytes(file_handle)?;

        let n_pols = header.stokes_format.pol_count();

        // (n_tunings, nfreqs, npols) array
        let data_shape = (2, header.n_freqs as usize, n_pols as usize);
        let mut data = {
            // 4 to account for bit depth
            // 2 to accound for the tunings
            let mut tmp = vec![0_u8; 4 * header.n_freqs as usize * 2 * n_pols as usize];
            file_handle.read_exact(&mut tmp)?;
            Array::from_iter(tmp.chunks_exact(4).map(|chunk| {
                f32::from_le_bytes(
                    chunk
                        .try_into()
                        .expect("Unable to coerce len 4 slice into array."),
                )
            }))
            .into_shape(data_shape)
            .with_context(|| format!("Unable to coerce data vec into shape: {data_shape:?}"))?
        };

        // an (n_tunings, 1, npols)  conversion factor
        let data_norms = {
            let tmp_norms = header
                .fills
                .iter()
                .map(|f| *f as f32 * header.n_freqs as f32)
                .collect::<Vec<f32>>();

            let pre_array = match header.stokes_format {
                PolarizationType::LinearXX => vec![tmp_norms[0], tmp_norms[2]],
                PolarizationType::LinearYY => vec![tmp_norms[1], tmp_norms[2]],
                PolarizationType::LinearXYReRe | PolarizationType::LinearXYIm => vec![
                    tmp_norms[0].min(tmp_norms[1]),
                    tmp_norms[2].min(tmp_norms[3]),
                ],
                PolarizationType::LinearRealHalf => tmp_norms,
                PolarizationType::LinearOtherHalf => {
                    let norm0 = tmp_norms[0].min(tmp_norms[1]);
                    let norm1 = tmp_norms[2].min(tmp_norms[3]);
                    vec![norm0, norm0, norm1, norm1]
                }
                PolarizationType::LinearFull => {
                    let norm0 = tmp_norms[0].min(tmp_norms[1]);
                    let norm1 = tmp_norms[2].min(tmp_norms[3]);
                    vec![
                        tmp_norms[0],
                        norm0,
                        norm0,
                        tmp_norms[1],
                        tmp_norms[2],
                        norm1,
                        norm1,
                        tmp_norms[3],
                    ]
                }
                PolarizationType::StokesI
                | PolarizationType::StokesQ
                | PolarizationType::StokesU
                | PolarizationType::StokesV => vec![
                    tmp_norms[0].min(tmp_norms[1]),
                    tmp_norms[2].min(tmp_norms[3]),
                ],
                PolarizationType::StokesRealHalf | PolarizationType::StokesOtherHalf => {
                    let norm0 = tmp_norms[0].min(tmp_norms[1]);
                    let norm1 = tmp_norms[2].min(tmp_norms[3]);
                    vec![norm0, norm0, norm1, norm1]
                }
                PolarizationType::StokesFull => {
                    let norm0 = tmp_norms[0].min(tmp_norms[1]);
                    let norm1 = tmp_norms[2].min(tmp_norms[3]);
                    vec![norm0, norm0, norm0, norm0, norm1, norm1, norm1, norm1]
                }
            };

            let pre_shape = pre_array.len();

            Array::from_shape_vec((2_usize, n_pols as usize), pre_array)
                .with_context(|| {
                    format!(
                        "Cannot convert vec with length {} into array shape {:?}",
                        pre_shape,
                        (2, n_pols)
                    )
                })?
                .insert_axis(Axis(1))
        };

        // divide out the normalization factors
        data = data / data_norms;

        Ok(Self { header, data })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn read_north_arm() {
        let data_file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("two_spectra");

        let normalized_data_file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("normalized_data.npy");

        let mut file_handle = BufReader::new(
            fs::OpenOptions::new()
                .read(true)
                .open(&data_file)
                .unwrap_or_else(|_| panic!("Unable to open {}", data_file.display())),
        );

        let spectrum = DRSpectrum::from_bytes(&mut file_handle).expect("Unable to read test data");

        let expected_header = DRHeader {
            timestamp: Epoch::from_gregorian(
                2024,
                10,
                25,
                00,
                25,
                23,
                312430336,
                hifitime::TimeScale::UTC,
            ),
            time_offset: 0,
            decimation_factor: 10,
            frequencies: [51999999.984167516, 69999999.98044223],
            fills: [768_u32; 4],
            errors: [0_u8; 4],
            beam: 1,
            stokes_format: PolarizationType::LinearFull,
            specrometer_version: 2,
            flags: 0,
            n_freqs: 1024,
            n_ints: 768,
            saturation_count: [90013, 312209, 69934, 283166],
        };

        assert_eq!(expected_header, spectrum.header);

        let expected_data: Array<f32, Ix3> =
            ndarray_npy::read_npy(normalized_data_file).expect("unabe to read formatted data.");

        assert!(expected_data.abs_diff_eq(&spectrum.data, 1e-3))
    }

    #[test]
    fn multi_spec() {
        let data_file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("two_spectra");
        let mut file_handle = BufReader::new(
            fs::OpenOptions::new()
                .read(true)
                .open(&data_file)
                .unwrap_or_else(|_| panic!("Unable to open {}", data_file.display())),
        );

        let spectrum = DRSpectrum::from_bytes(&mut file_handle).expect("Unable to read test data");
        let spectrum2 = DRSpectrum::from_bytes(&mut file_handle).expect("Unable to read test data");

        assert_ne!(spectrum, spectrum2)
    }
}
