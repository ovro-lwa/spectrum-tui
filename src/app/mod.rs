use std::{
    io::{self, Write},
    pin::Pin,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use async_stream::stream;
use crossterm::{
    event::{DisableMouseCapture, Event, EventStream, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, LeaveAlternateScreen},
};
use futures::Stream;
use log::info;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Position},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use tokio::sync::mpsc::{Receiver, Sender};
use tokio_stream::{StreamExt, StreamMap};

use crate::{
    loader::{AutoSpectra, DiskLoader, EtcdLoader, SpectrumLoader},
    Action, TuiType,
};

pub(crate) mod ui;

const SELECTED_STYLE: Style = Style::new().bg(Color::Gray).add_modifier(Modifier::BOLD);

enum StreamReturn {
    Action(Result<Event, io::Error>),
    Data(AutoSpectra),
    Tick,
}

#[derive(Debug, PartialEq, Eq)]
enum InputMode {
    Normal,
    AntennaInput,
    RemoveAntenna,
}

#[derive(Debug)]
struct AntennaFilter {
    items: Vec<String>,
    state: ListState,
}

#[derive(Debug)]
pub(crate) struct App {
    /// Used to store/update which antennas are currently being plotted
    antenna_filter: AntennaFilter,

    /// Spectra to be plotted on the next draw
    spectra: Option<AutoSpectra>,
    /// The ambient refresh tick if nothing happens
    refresh_rate: Duration,

    /// Determines backend and how to load data
    data_backend: TuiType,

    /// Channel used to send new filters to the backend
    filter_sender: Sender<Vec<String>>,

    /// Filter receving channel to give to the SpectrumLoader backend
    filter_recv: Option<Receiver<Vec<String>>>,

    /// Current value of the input box
    input: String,
    /// Position of cursor in the editor area.
    character_index: usize,
    /// Tracks if we're adding to the Antenna filter or not
    input_mode: InputMode,
}
impl App {
    pub fn new(refresh_rate: Duration, data_backend: TuiType) -> Self {
        let (filter_sender, filter_recv) = tokio::sync::mpsc::channel(10);

        let antenna_filter = if let TuiType::Live { antenna, .. } = &data_backend {
            antenna.to_owned()
        } else {
            vec![]
        };

        Self {
            antenna_filter: AntennaFilter {
                items: antenna_filter,
                state: ListState::default(),
            },
            spectra: None,
            refresh_rate,
            data_backend,
            filter_sender,
            filter_recv: Some(filter_recv),
            input_mode: InputMode::Normal,
            input: String::new(),
            character_index: 0,
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

        frame.render_widget(ui::draw_charts(self.spectra.as_ref()), chunks[1]);

        let log_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(80), Constraint::Min(20)].as_ref())
            .split(chunks[2]);

        // Logs
        frame.render_widget(ui::draw_logs(), log_chunks[0]);
        // Body & Help
        frame.render_widget(ui::draw_help(), log_chunks[1]);

        match self.input_mode {
            InputMode::Normal => {}
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
                let area =
                    ui::center_popup(chunks[1], Constraint::Length(20), Constraint::Length(3));
                frame.render_widget(Clear, area); //this clears out the background
                frame.render_stateful_widget(list, area, &mut self.antenna_filter.state);
            }
        }
    }

    fn spawn_backend(
        backend: TuiType,
        mut filter_recv: Receiver<Vec<String>>,
    ) -> Receiver<AutoSpectra> {
        let (sender, recvr) = tokio::sync::mpsc::channel(30);

        tokio::spawn(async move {
            match backend {
                TuiType::File {
                    nspectra,
                    input_file,
                } => {
                    let mut data_loader = DiskLoader::new(nspectra, input_file);
                    if let Some(spec) = data_loader.get_data().await {
                        sender.send(spec).await?;
                    }
                }
                TuiType::Live { antenna, delay } => {
                    let mut data_loader = EtcdLoader::new("etcdv3service:2379").await?;
                    data_loader.filter_antenna(&antenna)?;
                    let mut interval = tokio::time::interval(Duration::from_secs(delay));

                    loop {
                        tokio::select! {
                            _ = interval.tick() => {
                                if let Some(spec) = data_loader.get_data().await {
                                    sender.send(spec).await?;
                                }
                            }
                            Some(filter) = filter_recv.recv() => {
                                data_loader.filter_antenna(&filter)?;
                                // force a tick now to update the data
                                interval.reset_immediately();
                            }
                        }
                    }
                }
            };
            Ok::<(), anyhow::Error>(())
        });
        recvr
    }

    fn init_streams(
        data_backend: TuiType,
        refresh_rate: Duration,
        filter_recv: Receiver<Vec<String>>,
    ) -> StreamMap<&'static str, Pin<Box<dyn Stream<Item = StreamReturn> + Send>>> {
        let mut data_recv = Self::spawn_backend(data_backend, filter_recv);

        let data_stream = Box::pin(
            stream! {
                while let Some(data) = data_recv.recv().await{
                    yield data
                }
            }
            .map(StreamReturn::Data),
        ) as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

        let tick_stream = {
            let mut tmp = tokio::time::interval(refresh_rate);

            tmp.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            Box::pin(tokio_stream::wrappers::IntervalStream::new(tmp).map(|_| StreamReturn::Tick))
        } as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

        let reader = EventStream::new().map(StreamReturn::Action);
        let reader = Box::pin(reader) as Pin<Box<dyn Stream<Item = StreamReturn> + Send>>;

        let mut stream = tokio_stream::StreamMap::new();

        stream.insert("input", reader);
        stream.insert("data", data_stream);
        stream.insert("tick", tick_stream);
        stream
    }

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

    pub async fn run<W: Write>(
        mut self,
        mut terminal: Terminal<CrosstermBackend<W>>,
    ) -> Result<()> {
        let mut stream = Self::init_streams(
            self.data_backend.clone(),
            self.refresh_rate,
            self.filter_recv.take().context("Antenna Filter missing.")?,
        );

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
                                        Action::NewAnt => self.input_mode = InputMode::AntennaInput,
                                        Action::DelAnt => {
                                            self.input_mode = InputMode::RemoveAntenna
                                        }
                                    }
                                }
                            }
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
                            // ignore other inputs in text mode
                            InputMode::AntennaInput => {}

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
                            // ignore other inputs in delete ant mode
                            InputMode::RemoveAntenna => {}
                        },
                        // we are not interested in Focuses and mouse movements
                        Ok(_) => {}
                    }
                }
                StreamReturn::Data(data) => {
                    info!("Received New autosprectra.");
                    self.spectra.replace(data);
                }
                StreamReturn::Tick => {}
            }

            terminal.draw(|frame| self.draw(frame))?;
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
}
