use std::{
    io::{self, Write},
    pin::Pin,
    time::Duration,
};

#[cfg(not(any(feature = "ovro", feature = "lwa-na")))]
use ndarray::{arr2, Array};

use anyhow::{bail, Context, Error, Result};
use crossterm::event::{Event, EventStream};
use futures::Stream;
use log::info;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Frame, Terminal,
};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_stream::{wrappers::ReceiverStream, StreamExt, StreamMap};

#[cfg(feature = "lwa-na")]
use crate::loader::north_arm::{DRLoader, DiskLoader as NADiskLoader, SaturationStats};

#[cfg(feature = "ovro")]
use {
    crate::loader::ovro::{DiskLoader as OvroDiskLoader, EtcdLoader},
    crossterm::event::{KeyCode, KeyEventKind},
    ratatui::{
        layout::Position,
        style::{Color, Modifier, Style},
        widgets::{Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph},
    },
};

// otherwise clippy complains about the Trait import
#[allow(unused_imports)]
use crate::{
    loader::{AutoSpectra, SpectrumLoader},
    Action, TuiType,
};

pub(crate) mod ui;

#[cfg(feature = "ovro")]
const SELECTED_STYLE: Style = Style::new().bg(Color::Gray).add_modifier(Modifier::BOLD);

enum StreamReturn {
    Action(Result<Event, io::Error>),
    #[cfg(feature = "lwa-na")]
    Data((AutoSpectra, Option<SaturationStats>)),
    #[cfg(not(feature = "lwa-na"))]
    Data(AutoSpectra),
    Tick,
}

#[derive(Debug, PartialEq, Eq)]
enum InputMode {
    Normal,
    #[cfg(feature = "ovro")]
    AntennaInput,
    #[cfg(feature = "ovro")]
    RemoveAntenna,
}

#[cfg(feature = "ovro")]
#[derive(Debug)]
struct AntennaFilter {
    items: Vec<String>,
    state: ListState,
}

#[derive(Debug)]
pub(crate) struct App {
    #[cfg(feature = "ovro")]
    /// Used to store/update which antennas are currently being plotted
    antenna_filter: AntennaFilter,

    /// Spectra to be plotted on the next draw
    ///
    spectra: Option<AutoSpectra>,
    /// The ambient refresh tick if nothing happens
    refresh_rate: Duration,

    /// Determines backend and how to load data
    data_backend: TuiType,

    #[allow(dead_code)]
    // we use this channel to indicate to the loader when to
    // halt even if there is no filtering functionality
    /// Channel used to send new filters to the backend
    filter_sender: Sender<Vec<String>>,

    /// Filter receving channel to give to the SpectrumLoader backend
    filter_recv: Option<Receiver<Vec<String>>>,

    #[cfg(feature = "ovro")]
    /// Current value of the input box
    input: String,

    #[cfg(feature = "ovro")]
    /// Position of cursor in the editor area.
    character_index: usize,
    /// Tracks if we're adding to the Antenna filter or not
    input_mode: InputMode,

    log_plot: Option<bool>,

    #[cfg(feature = "lwa-na")]
    /// some saturation statistics to print
    saturations: Option<SaturationStats>,

    #[cfg(feature = "lwa-na")]
    show_stats: bool,
}
#[cfg(feature = "ovro")]
impl App {
    // BEGIN: function pulled from the ratatui user input example
    fn clamp_cursor(&self, new_cursor_pos: usize) -> usize {
        new_cursor_pos.clamp(0, self.input.chars().count())
    }

    fn move_cursor_left(&mut self) {
        let cursor_moved_left = self.character_index.saturating_sub(1);
        self.character_index = self.clamp_cursor(cursor_moved_left);
    }

    fn move_cursor_right(&mut self) {
        let cursor_moved_right = self.character_index.saturating_add(1);
        self.character_index = self.clamp_cursor(cursor_moved_right);
    }

    /// Returns the byte index based on the character position.
    ///
    /// Since each character in a string can be contain multiple bytes, it's necessary to calculate
    /// the byte index based on the index of the character.
    fn byte_index(&self) -> usize {
        self.input
            .char_indices()
            .map(|(i, _)| i)
            .nth(self.character_index)
            .unwrap_or(self.input.len())
    }

    fn enter_char(&mut self, new_char: char) {
        let index = self.byte_index();
        self.input.insert(index, new_char);
        self.move_cursor_right();
    }

    fn delete_char(&mut self) {
        let is_not_cursor_leftmost = self.character_index != 0;
        if is_not_cursor_leftmost {
            // Method "remove" is not used on the saved text for deleting the selected char.
            // Reason: Using remove on String works on bytes instead of the chars.
            // Using remove would require special care because of char boundaries.

            let current_index = self.character_index;
            let from_left_to_current_index = current_index - 1;

            // Getting all characters before the selected character.
            let before_char_to_delete = self.input.chars().take(from_left_to_current_index);
            // Getting all characters after selected character.
            let after_char_to_delete = self.input.chars().skip(current_index);

            // Put all characters together except the selected one.
            // By leaving the selected one out, it is forgotten and therefore deleted.
            self.input = before_char_to_delete.chain(after_char_to_delete).collect();
            self.move_cursor_left();
        }
    }

    fn reset_cursor(&mut self) {
        self.character_index = 0;
    }

    // Submit the antenna to the backend but also reset to plotter mode
    async fn submit_antenna_filter(&mut self) -> Result<()> {
        let new_ant = self.input.trim().to_uppercase().to_owned();
        if new_ant.is_empty() {
            info!("Invalid antenna name...Skipping");
            return Ok(());
        }
        info!("Adding Antenna {new_ant:?}");
        self.antenna_filter.items.push(new_ant);

        self.filter_sender
            .send(self.antenna_filter.items.clone())
            .await?;

        self.input.clear();
        self.reset_cursor();
        self.input_mode = InputMode::Normal;

        Ok(())
    }

    // END ratatui example functions

    // BEGIN functions pulled from list examples edited for need
    fn select_next(&mut self) {
        self.antenna_filter.state.select_next();
    }

    fn select_previous(&mut self) {
        self.antenna_filter.state.select_previous();
    }

    async fn remove_antenna(&mut self) -> Result<()> {
        if let Some(i) = self.antenna_filter.state.selected() {
            let removed = self.antenna_filter.items.remove(i);
            info!("Removing: {removed}");
            self.filter_sender
                .send(self.antenna_filter.items.clone())
                .await?;
        }

        // reset the list state and the input mode
        self.input_mode = InputMode::Normal;
        self.antenna_filter.state = ListState::default();

        Ok(())
    }
    // END list examples
}

#[cfg(feature = "lwa-na")]
type BackendReturn = Result<Receiver<(AutoSpectra, Option<SaturationStats>)>>;
#[cfg(not(feature = "lwa-na"))]
type BackendReturn = Result<Receiver<AutoSpectra>>;
impl App {
    pub fn new(refresh_rate: Duration, data_backend: TuiType) -> Self {
        let (filter_sender, filter_recv) = tokio::sync::mpsc::channel(10);

        #[cfg(feature = "ovro")]
        let antenna_filter = match &data_backend {
            TuiType::File { nspectra, .. } => {
                (0..*nspectra).map(|s| s.to_string()).collect::<Vec<_>>()
            }
            TuiType::Live { antenna, .. } => antenna.clone(),
        };

        Self {
            #[cfg(feature = "ovro")]
            antenna_filter: AntennaFilter {
                items: antenna_filter,
                state: ListState::default(),
            },
            spectra: None,
            refresh_rate,
            data_backend,
            input_mode: InputMode::Normal,
            filter_sender,
            filter_recv: Some(filter_recv),
            #[cfg(feature = "ovro")]
            input: String::new(),
            #[cfg(feature = "ovro")]
            character_index: 0,
            log_plot: None,
            #[cfg(feature = "lwa-na")]
            saturations: None,
            #[cfg(feature = "lwa-na")]
            show_stats: false,
        }
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let size = frame.area();

        // Vertical layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(
                [
                    Constraint::Min(3),
                    Constraint::Percentage(80),
                    Constraint::Percentage(20),
                ]
                .as_ref(),
            )
            .split(size);

        // Title
        frame.render_widget(ui::draw_title(), chunks[0]);

        if let Some(log) = self.log_plot {
            if let Some(spec) = self.spectra.as_mut() {
                spec.plot_log = log;
            }
        }

        frame.render_widget(ui::draw_charts(self.spectra.as_ref()), chunks[1]);

        cfg_if::cfg_if! {
            if #[cfg(feature="lwa-na")]{
                match self.show_stats{
                    true =>{
                        let log_chunks=   Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(60), Constraint::Min(20), Constraint::Min(20)].as_ref())
                        .split(chunks[2]);

                        // Logs
                        frame.render_widget(ui::draw_logs(), log_chunks[0]);
                        // stats
                        frame.render_widget(self.saturations.as_ref().map(|x| x.as_table()).unwrap_or_default(), log_chunks[1]);
                        // Body & Help
                        frame.render_widget(ui::draw_help(), log_chunks[2]);
                    },
                    false =>{
                        let log_chunks=   Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([Constraint::Percentage(80), Constraint::Min(20)].as_ref())
                        .split(chunks[2]);

                        // Logs
                        frame.render_widget(ui::draw_logs(), log_chunks[0]);
                        // Body & Help
                        frame.render_widget(ui::draw_help(), log_chunks[1]);

                    }
                }
            } else{

                let log_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(80), Constraint::Min(20)].as_ref())
                    .split(chunks[2]);

                // Logs
                frame.render_widget(ui::draw_logs(), log_chunks[0]);
                // Body & Help
                frame.render_widget(ui::draw_help(), log_chunks[1]);
            }
        }

        match self.input_mode {
            InputMode::Normal => {}
            #[cfg(feature = "ovro")]
            InputMode::AntennaInput => {
                let input = Paragraph::new(self.input.as_str())
                    .style(Style::default())
                    .block(
                        Block::default()
                            .title("Enter Antenna Name")
                            .borders(Borders::ALL),
                    );

                let area =
                    ui::center_popup(chunks[1], Constraint::Length(20), Constraint::Length(3));
                frame.render_widget(Clear, area); //this clears out the background
                frame.render_widget(input, area);

                frame.set_cursor_position(Position::new(
                    // Draw the cursor at the current position in the input field.
                    // This position is can be controlled via the left and right arrow key
                    area.x + self.character_index as u16 + 1,
                    // Move one line down, from the border to the input line
                    area.y + 1,
                ));
            }
            #[cfg(feature = "ovro")]
            InputMode::RemoveAntenna => {
                let items: Vec<ListItem> = self
                    .antenna_filter
                    .items
                    .iter()
                    .map(|todo_item| ListItem::from(todo_item.clone()))
                    .collect();
                // render the List in the middle of the screen
                let list = List::new(items)
                    .highlight_style(SELECTED_STYLE)
                    .highlight_symbol(">")
                    .highlight_spacing(HighlightSpacing::Always)
                    .block(
                        Block::default()
                            .title("Select Antenna")
                            .borders(Borders::ALL),
                    );
                let area = ui::center_popup(chunks[1], Constraint::Length(20), Constraint::Max(20));
                frame.render_widget(Clear, area); //this clears out the background
                frame.render_stateful_widget(list, area, &mut self.antenna_filter.state);
            }
        }
    }

    async fn spawn_backend(
        backend: TuiType,
        // make some lint exceptions to allow the no-feature
        // test compilation to work
        #[allow(unused_mut)]
        #[allow(unused_variables)]
        mut filter_recv: Receiver<Vec<String>>,
    ) -> BackendReturn {
        let (sender, recvr) = tokio::sync::mpsc::channel(30);

        match backend {
            #[cfg(not(any(feature = "ovro", feature = "lwa-na")))]
            TuiType::Noop => {
                tokio::spawn(async move {
                    sender
                        .send(AutoSpectra::new(
                            vec!["Test".to_owned()],
                            Array::linspace(0.0, 200.0, 5),
                            arr2(&[[5.0, 3.0, 1.0, 4.0, 0.33]]),
                            false,
                        ))
                        .await?;
                    Ok::<(), Error>(())
                });
            }
            #[cfg(any(feature = "ovro", feature = "lwa-na"))]
            TuiType::File {
                #[cfg(feature = "ovro")]
                nspectra,
                input_file,
            } => {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "ovro")]{
                        let mut data_loader = OvroDiskLoader::new(input_file);
                        data_loader.filter_antenna(
                            (0..nspectra)
                                .map(|s| format!("{s}"))
                                .collect::<Vec<_>>()
                                .as_slice(),
                        )?;

                    } else if #[cfg(feature = "lwa-na")] {
                        let mut data_loader = NADiskLoader::new(input_file);

                    }
                }
                tokio::spawn(async move {
                    if let Some(spec) = data_loader.get_data().await {
                        cfg_if::cfg_if! {
                            if #[cfg(feature="lwa-na")]{
                                    sender.send((spec, data_loader.get_stats())).await?;
                            } else {
                                sender.send(spec).await?;
                            }
                        }
                    }

                    #[cfg(feature = "ovro")]
                    while let Some(filter) = filter_recv.recv().await {
                        data_loader.filter_antenna(&filter)?;
                        if let Some(spec) = data_loader.get_data().await {
                            sender.send(spec).await?;
                        }
                    }
                    Ok::<(), Error>(())
                });
            }
            #[cfg(any(feature = "ovro", feature = "lwa-na"))]
            TuiType::Live {
                #[cfg(feature = "ovro")]
                antenna,
                #[cfg(feature = "lwa-na")]
                data_recorder,
                #[cfg(feature = "lwa-na")]
                identity_file,
                delay,
            } => {
                cfg_if::cfg_if! {
                    if #[cfg(feature = "ovro")]{
                        let mut data_loader = EtcdLoader::new("etcdv3service:2379").await?;
                        data_loader.filter_antenna(&antenna)?;

                    } else if #[cfg(feature = "lwa-na")] {
                        let mut data_loader = DRLoader::new(&data_recorder, identity_file).with_context(|| {
                            format!("Error Connecting to data recorder {data_recorder}")
                        })?;

                    }
                }
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(delay));

                    cfg_if::cfg_if! {
                        if #[cfg(feature = "ovro")]{

                            loop {
                                tokio::select! {
                                    _ = interval.tick() => {
                                        if let Some(spec) = data_loader.get_data().await {
                                            sender.send(spec).await?;
                                        }
                                    },
                                    Some(filter) = filter_recv.recv() => {
                                        data_loader.filter_antenna(&filter)?;
                                        // force a tick now to update the data
                                        interval.reset_immediately();
                                    }
                                    else => break,
                                }
                            }
                        } else  if #[cfg(feature="lwa-na")]{
                            loop {
                                tokio::select! {
                                    _ = interval.tick() => {
                                        if let Some(spec) = data_loader.get_data().await {
                                            sender.send((spec, data_loader.get_stats())).await?;
                                        }
                                    },
                                    Some(filter) = filter_recv.recv() => {
                                        data_loader.filter_antenna(&filter)?;
                                        // force a tick now to update the data
                                        interval.reset_immediately();
                                    }
                                    else => break,
                                }
                            }
                        } else {
                            loop {
                                tokio::select! {
                                    _ = interval.tick() => {
                                        if let Some(spec) = data_loader.get_data().await {
                                            sender.send(spec).await?;
                                        }
                                    },
                                    Some(filter) = filter_recv.recv() => {
                                        data_loader.filter_antenna(&filter)?;
                                        // force a tick now to update the data
                                        interval.reset_immediately();
                                    }
                                    else => break,
                                }
                            }
                        }
                    }
                    Ok::<(), Error>(())
                });
            }
        }
        Ok(recvr)
    }

    async fn init_streams(
        data_backend: TuiType,
        refresh_rate: Duration,
        filter_recv: Receiver<Vec<String>>,
    ) -> Result<StreamMap<&'static str, Pin<Box<dyn Stream<Item = StreamReturn> + Send>>>> {
        let mut stream = tokio_stream::StreamMap::new();

        let data_recv = Self::spawn_backend(data_backend, filter_recv).await?;

        let data_stream = Box::pin(ReceiverStream::new(data_recv).map(StreamReturn::Data));

        let tick_stream = {
            let mut tmp = tokio::time::interval(refresh_rate);

            tmp.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            Box::pin(tokio_stream::wrappers::IntervalStream::new(tmp).map(|_| StreamReturn::Tick))
        } as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

        let reader = EventStream::new().map(StreamReturn::Action);
        let reader = Box::pin(reader) as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

        stream.insert("input", reader);
        stream.insert("data", data_stream);
        stream.insert("tick", tick_stream);
        Ok(stream)
    }

    pub async fn run<W: Write>(
        mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
    ) -> Result<()> {
        let mut stream = Self::init_streams(
            self.data_backend.clone(),
            self.refresh_rate,
            self.filter_recv.take().context("Antenna Filter missing.")?,
        )
        .await?;

        'plotting_loop: while let Some((_key, event)) = stream.next().await {
            match event {
                StreamReturn::Action(maybe_event) => {
                    match maybe_event {
                        Err(err) => {
                            bail!("Error getting keyboard event: {err}");
                        }
                        Ok(Event::Key(event)) => match self.input_mode {
                            InputMode::Normal => {
                                if let Some(action) = Action::from_event(event) {
                                    match action {
                                        Action::Break => break 'plotting_loop,
                                        #[cfg(feature = "ovro")]
                                        Action::NewAnt => self.input_mode = InputMode::AntennaInput,
                                        #[cfg(feature = "ovro")]
                                        Action::DelAnt => {
                                            self.input_mode = InputMode::RemoveAntenna
                                        }
                                        Action::ToggleLog => {
                                            // toggle the switch
                                            if let Some(log) = self.log_plot.as_mut() {
                                                *log = !*log;
                                            }
                                        }
                                        #[cfg(feature = "lwa-na")]
                                        Action::ToggleStats => self.show_stats = !self.show_stats,
                                    }
                                }
                            }
                            #[cfg(feature = "ovro")]
                            InputMode::AntennaInput if event.kind == KeyEventKind::Press => {
                                match event.code {
                                    KeyCode::Enter => self.submit_antenna_filter().await?,
                                    KeyCode::Char(to_insert) => self.enter_char(to_insert),
                                    KeyCode::Backspace => self.delete_char(),
                                    KeyCode::Left => self.move_cursor_left(),
                                    KeyCode::Right => self.move_cursor_right(),
                                    KeyCode::Esc => self.input_mode = InputMode::Normal,
                                    _ => {}
                                }
                            }
                            #[cfg(feature = "ovro")]
                            // ignore other inputs in text mode
                            InputMode::AntennaInput => {}

                            #[cfg(feature = "ovro")]
                            // Remove an antenna from the filter
                            InputMode::RemoveAntenna if event.kind == KeyEventKind::Press => {
                                match event.code {
                                    KeyCode::Esc => self.input_mode = InputMode::Normal,
                                    KeyCode::Char('j') | KeyCode::Down => self.select_next(),
                                    KeyCode::Char('k') | KeyCode::Up => self.select_previous(),
                                    KeyCode::Enter => {
                                        self.remove_antenna().await?;
                                    }
                                    _ => {}
                                }
                            }
                            #[cfg(feature = "ovro")]
                            // ignore other inputs in delete ant mode
                            InputMode::RemoveAntenna => {}
                        },
                        // we are not interested in Focuses and mouse movements
                        Ok(_) => {}
                    }
                }
                #[cfg(feature = "lwa-na")]
                StreamReturn::Data((data, new_stats)) => {
                    info!("Received New autosprectra.");
                    if self.log_plot.is_none() {
                        self.log_plot = Some(data.plot_log);
                    }
                    self.spectra.replace(data);

                    if let Some(new_stats) = new_stats {
                        match self.saturations.as_mut() {
                            Some(stats) => stats.update(new_stats, self.data_backend.data_rate()),
                            None => {
                                self.saturations.replace(new_stats);
                            }
                        }
                    }
                }
                #[cfg(not(feature = "lwa-na"))]
                StreamReturn::Data(data) => {
                    info!("Received New autosprectra.");
                    if self.log_plot.is_none() {
                        self.log_plot = Some(data.plot_log);
                    }
                    self.spectra.replace(data);
                }
                StreamReturn::Tick => {}
            }

            terminal.draw(|frame| self.draw(frame))?;
        }

        Ok(())
    }
}
