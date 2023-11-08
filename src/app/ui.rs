use ndarray::Array;
use ratatui::{
    backend::Backend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    symbols,
    text::Span,
    widgets::{
        Axis, Block, BorderType, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table,
    },
    Frame,
};
use tui_logger::TuiLoggerWidget;

use crate::loader::AutoSpectra;

fn draw_title<'a>() -> Paragraph<'a> {
    Paragraph::new("Spectrum Tui!!")
        .style(Style::default().fg(Color::LightCyan))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .style(Style::default().fg(Color::White))
                .border_type(BorderType::Plain),
        )
}

// pub fn draw<B>(rect: &mut Frame<B>, data: &[Vec<(f64, f64)>], xmin: f64, xmax: f64)
pub fn draw<B>(rect: &mut Frame<B>, data: Option<&AutoSpectra>)
where
    B: Backend,
{
    let size = rect.size();

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
    let title = draw_title();
    rect.render_widget(title, chunks[0]);

    let charts = draw_charts(data);
    rect.render_widget(charts, chunks[1]);

    // Logs
    let logs = draw_logs();
    let help = draw_help();
    // Body & Help
    let log_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(80), Constraint::Min(20)].as_ref())
        .split(chunks[2]);

    rect.render_widget(logs, log_chunks[0]);
    rect.render_widget(help, log_chunks[1]);
}

fn draw_logs<'a>() -> TuiLoggerWidget<'a> {
    TuiLoggerWidget::default()
        .style_error(Style::default().fg(Color::Red))
        .style_debug(Style::default().fg(Color::Green))
        .style_warn(Style::default().fg(Color::Yellow))
        .style_trace(Style::default().fg(Color::Gray))
        .style_info(Style::default().fg(Color::Blue))
        .block(
            Block::default()
                .title("Logs")
                .border_style(Style::default().fg(Color::White).bg(Color::Black))
                .borders(Borders::ALL),
        )
        .style(Style::default().fg(Color::White).bg(Color::Black))
}

fn draw_help<'a>() -> Table<'a> {
    let key_style = Style::default().fg(Color::LightCyan);
    let help_style = Style::default().fg(Color::Gray);

    let rows = vec![
        Row::new(vec![
            Cell::from(Span::styled("<Esc>", key_style)),
            Cell::from(Span::styled("Quit", help_style)),
        ]),
        Row::new(vec![
            Cell::from(Span::styled("<c>", key_style)),
            Cell::from(Span::styled("Find Cursor", help_style)),
        ]),
        Row::new(vec![
            Cell::from(Span::styled("<Cntrl+c>", key_style)),
            Cell::from(Span::styled("Spicy Find Cursor", help_style)),
        ]),
    ];

    Table::new(rows)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .title("Help"),
        )
        .widths(&[Constraint::Length(11), Constraint::Min(20)])
        .column_spacing(1)
}

// fn draw_charts(data: &[Vec<(f64, f64)>], xmin: f64, xmax: f64) -> Chart {
fn draw_charts(data: Option<&AutoSpectra>) -> Chart {
    let datasets = data.map_or(vec![], |specs| {
        let n_spectra = specs.spectra.len();
        specs
            .spectra
            .iter()
            .zip(specs.ant_names.iter())
            .enumerate()
            .map(|(cnt, (x, name))| {
                Dataset::default()
                    .name(name)
                    .marker(symbols::Marker::Braille)
                    .style(Style::default().fg(Color::Indexed(
                        ((cnt as f32 / n_spectra as f32) * (cnt + 1) as f32) as u8 % 255_u8,
                    )))
                    .graph_type(GraphType::Line)
                    .data(x.as_slice())
            })
            .collect::<Vec<_>>()
    });

    let xmin = data.map_or(0.0, |x| x.freq_min);
    let xmax = data.map_or(10.0, |x| x.freq_max);
    let labels = Array::linspace(xmin, xmax, 11)
        .iter()
        .map(|x| Span::raw(format!("{:.1}", x)))
        .collect::<Vec<_>>();

    Chart::new(datasets)
        .block(
            Block::default()
                .title(Span::styled(
                    "AutoSpectra",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
                .borders(Borders::ALL)
                .style(Style::default()),
        )
        .x_axis(
            Axis::default()
                .title("Freq [MHz]")
                .style(Style::default().fg(Color::Gray))
                .bounds([xmin, xmax])
                .labels(labels),
        )
        .y_axis(
            Axis::default()
                .title("Power [dB]")
                .style(Style::default().fg(Color::Gray))
                .bounds([-120.0, -20.0])
                .labels(
                    Array::linspace(-120.0, -20.0, 11)
                        .iter()
                        .map(|x| Span::raw(format!("{:.1}", x)))
                        .collect::<Vec<_>>(),
                ),
        )
}
