use std::{io, pin::Pin, time::Duration};

use anyhow::Result;
use async_stream::stream;
use clap::Parser;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent,
        KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use loader::AutoSpectra;
use log::{error, info, trace, LevelFilter};
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::time::Instant;
use tokio_stream::Stream;
use tui_logger::{init_logger, set_default_level};

mod app;
use app::ui::draw;

mod loader;
use loader::SpectrumLoader;

use crate::loader::EtcdLoader;

enum Action {
    Break,
    NewAnt,
}
impl Action {
    pub fn from_event(event: Event) -> Option<Self> {
        trace!("Event::{:?}\r", event);

        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('n'),
                modifiers: KeyModifiers::NONE,
                kind: _,
                state: _,
            }) => Some(Self::NewAnt),
            Event::Key(KeyEvent {
                code: KeyCode::Esc,
                modifiers: KeyModifiers::NONE,
                kind: _,
                state: _,
            }) => Some(Self::Break),
            _ => None,
        }
    }
}

enum StreamReturn {
    Action(Result<Option<Action>, io::Error>),
    Data(AutoSpectra),
    Tick(Instant),
}

fn print_events(event: Result<Option<Action>, io::Error>) -> io::Result<Option<Action>> {
    match event {
        Ok(Some(action)) => match action {
            Action::NewAnt => Ok(None),
            Action::Break => Ok(Some(Action::Break)),
        },
        Err(err) => {
            error!("Error: {:?}\r", err);
            Ok(None)
        }
        _ => Ok(None),
    }
}

#[derive(Parser)]
#[command(author, version, about)]
struct Cli {
    #[clap(long, short)]
    /// The Antenna Name to grab autos
    /// This should be a string like LWA-250
    /// This antenna name is matched against the configuration name exactly.
    antenna: String,

    #[clap(long, short, default_value_t = 30)]
    /// The interval in seconds at which to poll for new autos
    delay: u64,
}

#[tokio::main]
async fn main() -> Result<(), io::Error> {
    init_logger(LevelFilter::Trace).unwrap();
    set_default_level(LevelFilter::Info);

    let cli = Cli::parse();

    // setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (sender, mut recvr) = tokio::sync::mpsc::channel(30);

    tokio::spawn(async move {
        let mut data_loader = EtcdLoader::new("etcdv3service:2379").await?;
        data_loader.filter_antenna(cli.antenna)?;

        let mut interval = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(
            Duration::from_secs(cli.delay),
        ));

        while let Some(_tick) = interval.next().await {
            if let Some(spec) = data_loader.get_data().await {
                sender.send(spec).await?;
            }
        }
        Ok::<(), anyhow::Error>(())
    });

    let data_stream = Box::pin(
        stream! {
            while let Some(data) = recvr.recv().await{
                yield data
            }
        }
        .map(StreamReturn::Data),
    ) as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

    let tick_stream = {
        let mut tmp = tokio::time::interval(Duration::from_millis(100));

        tmp.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        Box::pin(tokio_stream::wrappers::IntervalStream::new(tmp).map(StreamReturn::Tick))
    } as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

    let reader = EventStream::new()
        .map(|e| e.map(Action::from_event))
        .map(StreamReturn::Action);
    let reader = Box::pin(reader) as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

    let mut stream = tokio_stream::StreamMap::new();

    stream.insert("input", reader);
    stream.insert("data", data_stream);
    stream.insert("tick", tick_stream);

    let mut spectra: Option<AutoSpectra> = None;

    while let Some((_key, event)) = stream.next().await {
        match event {
            StreamReturn::Action(inner_event) => {
                if let Some(Action::Break) = print_events(inner_event)? {
                    break;
                }
            }
            StreamReturn::Data(data) => {
                info!("Received New autosprectra.");
                let _ = spectra.insert(data);
            }
            StreamReturn::Tick(_) => {}
        }

        terminal.draw(|frame| draw(frame, spectra.as_ref()))?;
    }

    // restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
