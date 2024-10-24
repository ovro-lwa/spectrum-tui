use ndarray::Array;
use ratatui::{
    layout::{Alignment, Constraint, Flex, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::Span,
    widgets::{Axis, Block, BorderType, Borders, Chart, Dataset, GraphType, Paragraph, Table},
};
use tui_logger::TuiLoggerWidget;

use crate::{loader::AutoSpectra, Action};

pub(crate) fn draw_title<'a>() -> Paragraph<'a> {
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

pub(crate) fn draw_logs<'a>() -> TuiLoggerWidget<'a> {
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

pub(crate) fn draw_help<'a>() -> Table<'a> {
    let key_style = Style::default().fg(Color::LightCyan);
    let help_style = Style::default().fg(Color::Gray);

    let rows = Action::gen_help(key_style, help_style);

    Table::new(rows, &[Constraint::Length(11), Constraint::Min(20)])
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .title("Help"),
        )
        // .widths(&[Constraint::Length(11), Constraint::Min(20)])
        .column_spacing(1)
}

pub(crate) fn draw_charts(data: Option<&AutoSpectra>) -> Chart {
    let datasets = data.map_or(vec![], |specs| {
        let n_spectra = specs.spectra.len();
        specs
            .spectra
            .iter()
            .zip(specs.ant_names.iter())
            .enumerate()
            .map(|(cnt, (x, name))| {
                let fraction = ((cnt + 1) as f32 / n_spectra as f32) * 255.0;

                Dataset::default()
                    .name(name.clone())
                    .marker(symbols::Marker::Braille)
                    .style(Style::default().fg(Color::Indexed(fraction as u8)))
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

/// helper function to create a centered rect using up certain percentage of the available rect `r`
pub(crate) fn center_popup(area: Rect, horizontal: Constraint, vertical: Constraint) -> Rect {
    let [area] = Layout::horizontal([horizontal])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([vertical]).flex(Flex::Center).areas(area);
    area
}
