use std::{io, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use log::{trace, LevelFilter};
use ratatui::{
    backend::CrosstermBackend,
    style::Style,
    text::Span,
    widgets::{Cell, Row},
    Terminal,
};
use tui_logger::{init_logger, set_default_level};

#[cfg(any(feature = "ovro", feature = "lwa-na"))]
use std::path::PathBuf;

mod app;
use app::App;

mod loader;

enum Action {
    Break,
    #[cfg(feature = "ovro")]
    NewAnt,
    #[cfg(feature = "ovro")]
    DelAnt,
    ToggleLog,
    #[cfg(feature = "lwa-na")]
    ToggleStats,
    ChangeYLims,
}
impl Action {
    pub fn from_event(event: KeyEvent) -> Option<Self> {
        trace!("Event::{:?}\r", event);

        match event {
            #[cfg(feature = "ovro")]
            KeyEvent {
                code: KeyCode::Char('a'),
                modifiers: KeyModifiers::NONE,
                kind: _,
                state: _,
            } => Some(Self::NewAnt),
            #[cfg(feature = "ovro")]
            KeyEvent {
                code: KeyCode::Char('d'),
                modifiers: KeyModifiers::NONE,
                kind: _,
                state: _,
            } => Some(Self::DelAnt),
            KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                kind: _,
                state: _,
            }
            | KeyEvent {
                code: KeyCode::Char('q'),
                modifiers: _,
                kind: _,
                state: _,
            } => Some(Self::Break),
            KeyEvent {
                code: KeyCode::Char('l'),
                ..
            } => Some(Self::ToggleLog),
            KeyEvent {
                code: KeyCode::Char('y'),
                ..
            } => Some(Self::ChangeYLims),
            #[cfg(feature = "lwa-na")]
            KeyEvent {
                code: KeyCode::Char('s'),
                ..
            } => Some(Self::ToggleStats),
            _ => None,
        }
    }

    pub fn gen_help<'a>(key_style: Style, help_style: Style) -> Vec<Row<'a>> {
        vec![
            Row::new(vec![
                Cell::from(Span::styled("<Esc>/q", key_style)),
                Cell::from(Span::styled("Quit", help_style)),
            ]),
            #[cfg(feature = "ovro")]
            Row::new(vec![
                Cell::from(Span::styled("a", key_style)),
                Cell::from(Span::styled("Add New Antenna", help_style)),
            ]),
            #[cfg(feature = "ovro")]
            Row::new(vec![
                Cell::from(Span::styled("d", key_style)),
                Cell::from(Span::styled("Remove Antenna", help_style)),
            ]),
            Row::new(vec![
                Cell::from(Span::styled("l", key_style)),
                Cell::from(Span::styled("Toggle dB", help_style)),
            ]),
            Row::new(vec![
                Cell::from(Span::styled("y", key_style)),
                Cell::from(Span::styled("Change Y-lims", help_style)),
            ]),
            #[cfg(feature = "lwa-na")]
            Row::new(vec![
                Cell::from(Span::styled("s", key_style)),
                Cell::from(Span::styled("Toggle Saturation Stats", help_style)),
            ]),
        ]
    }
}

#[derive(Debug, Subcommand, Clone)]
enum TuiType {
    #[cfg(not(any(feature = "ovro", feature = "lwa-na")))]
    #[clap(name = "no-op")]
    Noop,
    #[cfg(any(feature = "ovro", feature = "lwa-na"))]
    #[clap(arg_required_else_help = true)]
    /// Plot spectra from an RFIMonitorTool output npy file
    File {
        #[cfg(feature = "ovro")]
        #[clap(short = 'n', required = true)]
        /// The number of antenna spectra to load
        nspectra: usize,
        #[clap()]
        /// Numpy save file from the RFIMonitor
        input_file: PathBuf,
    },
    #[clap(arg_required_else_help = true)]
    /// Watch live autospectra from the correlator
    #[cfg(any(feature = "ovro", feature = "lwa-na"))]
    Live {
        #[cfg(feature = "ovro")]
        #[clap( num_args = 1.., value_delimiter = ' ')]
        /// The Antenna Name(s) to grab autos
        ///
        /// This should be a string like LWA-250.
        ///
        /// This antenna name is matched against the configuration name exactly.
        ///
        /// This can also be a space separated list of antennas: LWA-124 LWA-250 ...etc
        antenna: Vec<String>,

        #[cfg(feature = "lwa-na")]
        #[clap()]
        /// The hostname of the data recorder from which spectra will be loaded.
        data_recorder: String,

        #[cfg(feature = "lwa-na")]
        #[clap(
            long="identity-file",
            short='i',
            required=false,
            default_value = "~/.ssh/id_rsa",
            value_parser = |path: &str| expanduser::expanduser(path)
        )]
        /// SSH identity file used to connect to the data recorder.
        identity_file: PathBuf,

        #[clap(long, short, default_value_t = 30.0)]
        /// The interval in seconds at which to poll for new autos
        delay: f64,
    },
}
#[cfg(feature = "lwa-na")]
impl TuiType {
    /// returns the refresh rate in seconds
    pub(crate) fn data_rate(&self) -> f64 {
        match self {
            TuiType::File { .. } => 1.0,
            TuiType::Live { delay, .. } => *delay,
        }
    }
}

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[clap(subcommand)]
    tv_type: TuiType,
}

fn get_log_level() -> LevelFilter {
    std::env::var("LOG")
        .or(std::env::var("RUST_LOG"))
        .ok()
        .and_then(|level| <LevelFilter as std::str::FromStr>::from_str(&level).ok())
        .unwrap_or(LevelFilter::Info)
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logger(LevelFilter::Trace).unwrap();
    set_default_level(get_log_level());

    let cli = Cli::parse();

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let app = App::new(Duration::from_millis(100), cli.tv_type);
    let result = app.run(&mut terminal).await;

    // we always want to restore the terminal
    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}
