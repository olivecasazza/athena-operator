use iced::widget::text::Wrapping;
use iced::widget::{column, container, horizontal_rule, row, text};
use iced::{Border, Color, Element, Length, Padding};

#[derive(Debug, Clone, Copy)]
pub struct TablePalette {
    pub surface: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
}

#[derive(Debug, Clone, Copy)]
pub struct TableColumn {
    title: &'static str,
    width: f32,
}

impl TableColumn {
    pub fn portion(title: &'static str, portion: u16) -> Self {
        Self {
            title,
            width: f32::from(portion) * 170.0,
        }
    }
}

pub type TableRow<'a, Message> = Vec<Element<'a, Message>>;

pub fn data_table<'a, Message: 'a>(
    palette: TablePalette,
    columns: Vec<TableColumn>,
    rows: Vec<TableRow<'a, Message>>,
) -> Element<'a, Message> {
    let is_empty = rows.is_empty();
    let table_width = table_width(&columns);
    let header = columns.iter().fold(row!().spacing(16), |row, column| {
        row.push(table_cell(
            text(column.title)
                .size(15)
                .color(palette.text)
                .wrapping(Wrapping::None),
            column.width,
        ))
    });

    let body = rows.into_iter().fold(
        column![
            header.padding(Padding {
                top: 16.0,
                right: 0.0,
                bottom: 10.0,
                left: 0.0,
            }),
            horizontal_rule(1)
        ]
        .spacing(0)
        .width(Length::Fixed(table_width)),
        |column, cells| {
            column
                .push(table_row(&columns, cells))
                .push(horizontal_rule(1))
        },
    );

    let body = if is_empty {
        body.push(
            container(text("No resources").size(14).color(palette.muted))
                .padding([14, 0])
                .width(Length::Fill),
        )
    } else {
        body
    };

    container(body)
        .padding(10)
        .width(Length::Fill)
        .clip(true)
        .style(move |_| table_panel_style(palette))
        .into()
}

pub fn clipped_text<'a, Message: 'a>(
    value: &'a str,
    size: u16,
    color: Color,
) -> Element<'a, Message> {
    text(value)
        .size(size)
        .color(color)
        .wrapping(Wrapping::None)
        .width(Length::Fill)
        .into()
}

fn table_row<'a, Message: 'a>(
    columns: &[TableColumn],
    cells: TableRow<'a, Message>,
) -> Element<'a, Message> {
    let row = columns
        .iter()
        .zip(cells)
        .fold(row!().spacing(16), |row, (column, cell)| {
            row.push(table_cell(cell, column.width))
        });

    row.align_y(iced::Alignment::Center)
        .padding([12, 0])
        .width(Length::Fixed(table_width(columns)))
        .clip(true)
        .into()
}

fn table_cell<'a, Message: 'a>(
    content: impl Into<Element<'a, Message>>,
    width: f32,
) -> Element<'a, Message> {
    container(content)
        .width(Length::Fixed(width.max(1.0)))
        .height(Length::Shrink)
        .clip(true)
        .into()
}

fn table_width(columns: &[TableColumn]) -> f32 {
    columns.iter().map(|column| column.width).sum::<f32>()
        + columns.len().saturating_sub(1) as f32 * 16.0
}

fn table_panel_style(palette: TablePalette) -> container::Style {
    container::Style {
        background: Some(palette.surface.into()),
        text_color: Some(palette.text),
        border: Border {
            color: palette.border,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}
