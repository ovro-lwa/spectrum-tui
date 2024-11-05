#![allow(dead_code)]

use std::{
    fs,
    io::{BufRead, BufReader, ErrorKind, Read, Seek, SeekFrom},
    net::TcpStream,
    path::{Path, PathBuf},
    time::Duration,
};

// adapted from https://github.com/lwa-project/lsl/blob/main/lsl/reader/drspec.cpp
use anyhow::{anyhow, bail, ensure, Context, Result};
use async_trait::async_trait;
use byteorder::{LittleEndian, ReadBytesExt};
use hifitime::Epoch;
use ndarray::{Array, Axis, Ix1, Ix2, Ix3};
use ssh2::{ErrorCode, Session, Sftp};

use crate::loader::{AutoSpectra, SpectrumLoader};

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
            0x09 => Some(Self::LinearRealHalf),
            0x0a => Some(Self::LinearOtherHalf),
            0x0f => Some(Self::LinearFull),
            0x10 => Some(Self::StokesI),
            0x20 => Some(Self::StokesQ),
            0x40 => Some(Self::StokesU),
            0x80 => Some(Self::StokesV),
            0x90 => Some(Self::StokesRealHalf),
            0xa0 => Some(Self::StokesOtherHalf),
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

    pub fn desription(&self) -> Vec<String> {
        match self {
            Self::LinearXX => vec!["XX".into()],
            Self::LinearXYReRe => vec!["Re(XY)".into()],
            Self::LinearXYIm => vec!["Im(XY)".into()],
            Self::LinearYY => vec!["YY".into()],
            Self::LinearRealHalf => vec!["XX".into(), "YY".into()],
            Self::LinearOtherHalf => vec!["Re(XY)".into(), "Im(XY)".into()],
            Self::LinearFull => vec!["XX".into(), "Re(XY)".into(), "Im(XY)".into(), "YY".into()],
            Self::StokesI => vec!["I".into()],
            Self::StokesQ => vec!["Q".into()],
            Self::StokesU => vec!["U".into()],
            Self::StokesV => vec!["V".into()],
            Self::StokesRealHalf => vec!["I".into(), "V".into()],
            Self::StokesOtherHalf => vec!["Q".into(), "U".into()],
            Self::StokesFull => vec!["I".into(), "Q".into(), "U".into(), "V".into()],
        }
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
    const LEN: usize = 76;

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
            Self::LEN,
            fs::OpenOptions::new()
                .read(true)
                .open(path)
                .with_context(|| format!("Unable to open {}", path.display()))?,
        );

        Self::from_bytes(&mut buffer)
    }

    /// Calculate the % of integrations that are saturated per pol per tuning
    pub fn calc_saturation(&self) -> Vec<f64> {
        let tmp_sats = self
            .saturation_count
            .map(|x| x as f64 / (self.n_ints as f64 * self.n_freqs as f64));
        match self.stokes_format {
            PolarizationType::LinearXX => vec![tmp_sats[0], tmp_sats[2]],
            PolarizationType::LinearXYReRe | PolarizationType::LinearXYIm => {
                vec![tmp_sats[0].max(tmp_sats[1]), tmp_sats[2].max(tmp_sats[3])]
            }
            PolarizationType::LinearYY => vec![tmp_sats[1], tmp_sats[3]],
            PolarizationType::LinearRealHalf => tmp_sats.to_vec(),
            PolarizationType::LinearOtherHalf => {
                let sat1 = tmp_sats[0].max(tmp_sats[1]);
                let sat2 = tmp_sats[2].max(tmp_sats[3]);
                vec![sat1, sat1, sat2, sat2]
            }
            PolarizationType::LinearFull => {
                let sat1 = tmp_sats[0].max(tmp_sats[1]);
                let sat2 = tmp_sats[2].max(tmp_sats[3]);
                vec![
                    tmp_sats[0],
                    sat1,
                    sat1,
                    tmp_sats[1],
                    tmp_sats[2],
                    sat2,
                    sat2,
                    tmp_sats[3],
                ]
            }
            PolarizationType::StokesI
            | PolarizationType::StokesQ
            | PolarizationType::StokesU
            | PolarizationType::StokesV => {
                vec![tmp_sats[0].max(tmp_sats[1]), tmp_sats[2].max(tmp_sats[3])]
            }
            PolarizationType::StokesRealHalf | PolarizationType::StokesOtherHalf => {
                let sat1 = tmp_sats[0].max(tmp_sats[1]);
                let sat2 = tmp_sats[2].max(tmp_sats[3]);
                vec![sat1, sat1, sat2, sat2]
            }
            PolarizationType::StokesFull => {
                let sat1 = tmp_sats[0].max(tmp_sats[1]);
                let sat2 = tmp_sats[2].max(tmp_sats[3]);
                vec![sat1, sat1, sat1, sat1, sat2, sat2, sat2, sat2]
            }
        }
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

    fn len_bytes(&self) -> usize {
        2 * 4 * self.n_freqs as usize * self.stokes_format.pol_count() as usize
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

    pub data: Array<f64, Ix3>,
}
impl DRSpectrum {
    /// Locates the next spectrum in the file and sets the cursor position
    pub fn find_next_spectra<R: Read + Seek>(buffer: &mut BufReader<R>) -> Result<()> {
        loop {
            let bytes = buffer.fill_buf()?;

            if bytes.is_empty() {
                bail!("No additional data in file");
            }

            let pos = bytes
                .as_ref()
                .windows(DRHeader::SYNC_HEADER.to_le_bytes().len())
                .position(|window| window == DRHeader::SYNC_HEADER.to_le_bytes());

            match pos {
                Some(index) => {
                    buffer.consume(index);
                    return Ok(());
                }
                None => {
                    let len = bytes.len();
                    buffer.consume(len);
                }
            }
        }
    }

    pub fn read_last_spectrum<R: Read + Seek>(buffer: &mut BufReader<R>) -> Result<Self> {
        DRSpectrum::find_next_spectra(buffer)?;

        let header = DRHeader::from_bytes(buffer)?;
        // advance past this spectrum
        // we have 2 tunings * n_freqs * npols * 4 (byte depth) bytes
        let spectra_len = header.len_bytes();

        let total_offset = spectra_len as i64 + DRHeader::LEN as i64;
        buffer.seek(SeekFrom::End(-total_offset))?;

        DRSpectrum::from_bytes(buffer)
    }

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
                ) as f64
            }))
            .to_shape(data_shape)
            .with_context(|| format!("Unable to coerce data vec into shape: {data_shape:?}"))?
            .to_owned()
        };

        // an (n_tunings, 1, npols)  conversion factor
        let data_norms = {
            let tmp_norms = header
                .fills
                .iter()
                .map(|f| *f as f64 * header.n_freqs as f64)
                .collect::<Vec<f64>>();

            let pre_array = match header.stokes_format {
                PolarizationType::LinearXX => vec![tmp_norms[0], tmp_norms[2]],
                PolarizationType::LinearYY => vec![tmp_norms[1], tmp_norms[3]],
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

    pub fn into_autospectra(self) -> AutoSpectra {
        // package the data up
        // transform to MHz
        let Self { header, data } = self;
        let descriptions: Vec<String> = {
            header
                .stokes_format
                .desription()
                .iter()
                .zip(header.calc_saturation().iter())
                .map(|(desc, sat)| format!("{desc:<6 } {:.2}", sat * 100.0))
                .collect()
        };
        let freqs = header.get_freqs().map(|x| x / 1e6);

        let mut data_out =
            Array::<f64, Ix2>::zeros((descriptions.len(), 2 * header.n_freqs as usize));

        for (mut inner_data_out, polarization_data) in
            data_out.outer_iter_mut().zip(data.axis_iter(Axis(2)))
        {
            inner_data_out.assign(&polarization_data.flatten());
        }

        let flat_freqs = freqs.flatten().to_owned();

        AutoSpectra::new(descriptions, flat_freqs, data_out, false)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DiskLoader {
    /// File to read spectra from
    file: PathBuf,
}
impl DiskLoader {
    pub fn new(input_file: PathBuf) -> Self {
        Self { file: input_file }
    }
}
#[async_trait]
impl SpectrumLoader for DiskLoader {
    async fn get_data(&mut self) -> Option<AutoSpectra> {
        let mut file_handle = BufReader::new(
            fs::OpenOptions::new()
                .read(true)
                .open(&self.file)
                .with_context(|| format!("Unable to open {}", self.file.display()))
                .ok()?,
        );

        Some(
            DRSpectrum::from_bytes(&mut file_handle)
                .ok()?
                .into_autospectra(),
        )
    }

    /// Filters the antennas to be plotted based on their string names.
    fn filter_antenna(&mut self, _antenna_number: &[String]) -> Result<()> {
        Ok(())
    }
}

/// A Spectrum loader for the LWA North Arm
/// connects to the datarecorder and reads from the spectrum
/// file on disk
pub struct DRLoader {
    /// The DataRecorder this loader listens to
    pub data_recorder: String,

    /// DataRecorder spectrum file
    pub filename: Option<PathBuf>,

    /// the basename of the file we are reading
    pub file_tag: Option<String>,

    /// SFTP session use to query for new files and read data
    sftp: Sftp,

    /// the last timestamp data was gathered for
    last_timestamp: Epoch,
}
impl std::fmt::Debug for DRLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DRLoader")
            .field("data_recorder", &self.data_recorder)
            .field("filename", &self.filename)
            .finish()
    }
}
impl DRLoader {
    pub fn new<P: AsRef<str>, R: AsRef<Path>>(data_recorder: P, identity_file: R) -> Result<Self> {
        let data_recorder = data_recorder.as_ref();
        // Connect to the local SSH server
        let tcp = TcpStream::connect(format!("{}:22", data_recorder))
            .context("Error initializing TCP connection")?;

        let mut sess = Session::new().context("Unable to initialize SSH Session")?;
        sess.set_tcp_stream(tcp);
        sess.handshake().context("SSH Handshake error")?;

        // Try to authenticate with the first identity in the agent.
        sess.userauth_pubkey_file("mcsdr", None, identity_file.as_ref(), None)
            .context("Error authenticating as mcsdr")?;
        // Make sure we succeeded
        ensure!(
            sess.authenticated(),
            "SSH Session could not be authenticated"
        );

        let mut me = Self {
            data_recorder: data_recorder.to_owned(),
            filename: None,
            file_tag: None,
            sftp: sess.sftp().context("Error initializing sftp server")?,
            last_timestamp: Epoch::from_unix_seconds(0.0),
        };

        me.find_latest_file()?;

        Ok(me)
    }

    fn get_file<P: AsRef<Path>>(&mut self, pathname: P) -> Result<Option<PathBuf>, ssh2::Error> {
        Ok(self
            .sftp
            .readdir(pathname.as_ref())?
            .into_iter()
            .filter_map(|(path, stat)| if stat.is_dir() { Some(path) } else { None })
            .map(|path| self.sftp.readdir(&path.join("DROS/Spec/")))
            .filter_map(Result::ok)
            .flatten()
            .filter(|(path, stat)| {
                stat.is_file()
                    && path
                        .file_stem()
                        .and_then(|name| name.to_str())
                        .map_or(false, |name| name.starts_with("0"))
            })
            .max_by_key(|(_path1, stat1)| stat1.mtime.unwrap_or(0))
            .map(|(path, _stat)| path))
    }

    fn find_latest_file(&mut self) -> Result<()> {
        self.filename = 'file_block: {
            let paths_to_check = [
                "/LWA_STORAGE/Internal/",
                // Paht may have an extra DR# in the name since
                // multiple data recorders can run on the same machine.
                &format!(
                    "/LWA_STORAGE/{}/Internal/",
                    self.data_recorder.to_uppercase()
                ),
            ];
            for path in paths_to_check {
                match self.get_file(path) {
                    Ok(Some(remote_path)) => {
                        break 'file_block Some(remote_path);
                    }
                    Ok(None) => {}
                    // error code 2 is a No Such file. This is the most likely
                    // case for one of the two paths not existing.
                    Err(err) if err.code() == ErrorCode::SFTP(2) => {}
                    // any other kind of error we propagate
                    Err(err) => return Err(err.into()),
                }
            }
            None
        };

        if let Some(path) = &self.filename {
            self.file_tag = path
                .file_name()
                .and_then(|name| name.to_str().map(|x| x.to_owned()));

            if let Some(name) = &self.file_tag {
                log::info!("Reading spectra from {name} on {}", self.data_recorder);
            }
        }

        Ok(())
    }

    fn get_latest_spectra(&mut self) -> Result<Option<DRSpectrum>> {
        if let Some(filename) = &self.filename {
            let file_handle = self
                .sftp
                .open(filename)
                .with_context(|| format!("Error opening remote file: {}", filename.display()))?;
            let mut reader = BufReader::new(file_handle);

            let res = DRSpectrum::read_last_spectrum(&mut reader).map(Some);
            if let Err(ref err) = res {
                log::error!("Error reading specutrm file: {err}");
            }
            res
        } else {
            Ok(None)
        }
    }
}

#[async_trait]
impl SpectrumLoader for DRLoader {
    /// Loads autospectrum data from the underlying source and sends
    /// correlations (freq, val) pairs over the channel to the main process.
    async fn get_data(&mut self) -> Option<AutoSpectra> {
        let spectra = match self.get_latest_spectra() {
            Ok(val) => Ok(val),
            Err(err) => match err.downcast::<std::io::Error>() {
                Ok(error) if error.kind() == ErrorKind::UnexpectedEof => {
                    // in this case we're reading data but it is not all written yet
                    // wait a little bit and try again
                    std::thread::sleep(Duration::from_micros(50));
                    self.get_latest_spectra()
                }
                Ok(error) => Err(error.into()),
                Err(error) => Err(error),
            },
        }
        .ok()
        .flatten()?;

        if self.last_timestamp == spectra.header.timestamp {
            log::info!("Timestamp unchanged, attempting to find new file.");
            // no new data has been written, close this file and look for a new one.
            self.find_latest_file().ok()?;
            self.get_latest_spectra()
                .ok()
                .flatten()
                .map(|spec| spec.into_autospectra())
        } else {
            self.last_timestamp = spectra.header.timestamp;

            Some(spectra.into_autospectra())
        }
    }

    /// Filters the antennas to be plotted based on their string names.
    fn filter_antenna(&mut self, _antenna_number: &[String]) -> Result<()> {
        // not sure if we can even do anything with this
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::io::Seek;

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

        let expected_data = expected_data.mapv(|x| x as f64);

        assert!(expected_data.abs_diff_eq(&spectrum.data, 1e-5))
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

    #[test]
    fn find_next_spectra() {
        let data_file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("two_spectra");
        let mut file_handle = BufReader::new(
            fs::OpenOptions::new()
                .read(true)
                .open(&data_file)
                .unwrap_or_else(|_| panic!("Unable to open {}", data_file.display())),
        );

        let mut cnt = 0;

        while let Ok(()) = DRSpectrum::find_next_spectra(&mut file_handle) {
            assert_eq!(cnt, file_handle.stream_position().unwrap());
            cnt += 32844;
            let len = file_handle.buffer().len();
            file_handle.consume(len);
        }
    }

    #[test]
    fn read_last() {
        let data_file = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("data")
            .join("two_spectra");
        let mut file_handle = BufReader::new(
            fs::OpenOptions::new()
                .read(true)
                .open(&data_file)
                .unwrap_or_else(|_| panic!("Unable to open {}", data_file.display())),
        );

        let _ = DRSpectrum::from_bytes(&mut file_handle).expect("unable to read test data.");
        let expected_spectra =
            DRSpectrum::from_bytes(&mut file_handle).expect("unable to read test data.");

        // rewind the file
        file_handle.rewind().expect("unable to rewind test file.");

        let spectrum = DRSpectrum::read_last_spectrum(&mut file_handle)
            .expect("Unable to read last spectrum.");

        assert_eq!(expected_spectra, spectrum)
    }
}
