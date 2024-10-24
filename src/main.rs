use std::{io, path::PathBuf, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{EnableMouseCapture, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{enable_raw_mode, EnterAlternateScreen},
};
use log::{trace, LevelFilter};
use ratatui::{backend::CrosstermBackend, Terminal};
use tui_logger::{init_logger, set_default_level};

mod app;
use app::App;

mod loader;

enum Action {
    Break,
    NewAnt,
}
impl Action {
    pub fn from_event(event: KeyEvent) -> Option<Self> {
        trace!("Event::{:?}\r", event);

        match event {
            KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::NONE,
                kind: _,
                state: _,
            } => Some(Self::NewAnt),
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
            _ => None,
        }
    }
}

#[derive(Debug, Subcommand, Clone)]
enum TuiType {
    #[clap(arg_required_else_help = true)]
    /// Plot spectra from an RFIMonitorTool output npy file
    File {
        #[clap(short = 'n', required = true)]
        /// The number of antenna spectra to load
        nspectra: usize,
        #[clap()]
        /// Numpy save file from the RFIMonitor
        input_file: PathBuf,
    },
    #[clap(arg_required_else_help = true)]
    /// Watch live autospectra from the correlator
    Live {
        #[clap( num_args = 1.., value_delimiter = ' ')]
        /// The Antenna Name(s) to grab autos
        ///
        /// This should be a string like LWA-250.
        ///
        /// This antenna name is matched against the configuration name exactly.
        ///
        /// This can also be a space separated list of antennas: LWA-124 LWA-250 ...etc
        antenna: Vec<String>,

        #[clap(long, short, default_value_t = 30)]
        /// The interval in seconds at which to poll for new autos
        delay: u64,
    },
}

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[clap(subcommand)]
    tv_type: TuiType,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logger(LevelFilter::Trace).unwrap();
    set_default_level(LevelFilter::Info);

    let cli = Cli::parse();

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;

    let app = App::new(Duration::from_millis(100), cli.tv_type);
    app.run(terminal).await?;

    Ok(())
}
