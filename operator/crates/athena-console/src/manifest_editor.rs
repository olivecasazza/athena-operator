use iced::widget::{button, column, container, row, text, text_editor};
use iced::{Background, Border, Color, Element, Length, Shadow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorKind {
    Template,
    Manifest,
}

pub struct EditorDocument {
    title: String,
    source: String,
    content: text_editor::Content,
}

impl EditorDocument {
    pub fn new(title: impl Into<String>, source: impl Into<String>, text: &str) -> Self {
        Self {
            title: title.into(),
            source: source.into(),
            content: text_editor::Content::with_text(text),
        }
    }

    pub fn set_text(&mut self, title: impl Into<String>, source: impl Into<String>, text: &str) {
        self.title = title.into();
        self.source = source.into();
        self.content = text_editor::Content::with_text(text);
    }

    pub fn perform(&mut self, action: text_editor::Action) {
        self.content.perform(action);
    }

    pub fn select_all(&mut self) {
        self.content.perform(text_editor::Action::SelectAll);
    }

    pub fn text(&self) -> String {
        self.content.text()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EditorPalette {
    pub page: Color,
    pub surface: Color,
    pub border: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub selection: Color,
}

pub fn manifest_editor<'a, Message: Clone + 'a>(
    kind: EditorKind,
    document: &'a EditorDocument,
    palette: EditorPalette,
    on_action: impl Fn(text_editor::Action) -> Message + 'a,
    on_select_all: Message,
    on_copy: Message,
    on_paste: Message,
) -> Element<'a, Message> {
    container(
        column![
            row![
                column![
                    text(&document.title).size(23).color(palette.text),
                    text(&document.source).size(13).color(palette.muted),
                ]
                .spacing(4)
                .width(Length::Fill),
                editor_button("Select all", 112.0, palette).on_press(on_select_all),
                editor_button("Copy", 72.0, palette).on_press(on_copy),
                editor_button("Paste", 80.0, palette).on_press(on_paste),
                text(match kind {
                    EditorKind::Template => "template draft",
                    EditorKind::Manifest => "manifest draft",
                })
                .size(13)
                .color(palette.muted),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(10),
            text_editor(&document.content)
                .height(Length::Fixed(340.0))
                .padding(10)
                .style(move |_, status| editor_style(status, palette))
                .on_action(on_action),
        ]
        .spacing(12),
    )
    .padding(10)
    .width(Length::Fill)
    .style(move |_| panel_style(palette))
    .into()
}

fn editor_button<'a, Message: Clone + 'a>(
    label: &'a str,
    width: f32,
    palette: EditorPalette,
) -> button::Button<'a, Message> {
    button(
        text(label)
            .size(13)
            .color(palette.text)
            .wrapping(iced::widget::text::Wrapping::None)
            .width(Length::Fixed(width))
            .align_x(iced::alignment::Horizontal::Center),
    )
    .width(Length::Fixed(width + 18.0))
    .height(Length::Fixed(37.0))
    .padding([7, 10])
    .clip(true)
    .style(move |_, status| button_style(status, palette))
}

fn panel_style(palette: EditorPalette) -> container::Style {
    container::Style {
        background: Some(Background::Color(palette.surface)),
        text_color: Some(palette.text),
        border: Border {
            color: palette.border,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn button_style(status: button::Status, palette: EditorPalette) -> button::Style {
    let border_color = match status {
        button::Status::Hovered | button::Status::Pressed => palette.accent,
        button::Status::Disabled | button::Status::Active => palette.border,
    };
    let text_color = match status {
        button::Status::Hovered | button::Status::Pressed => palette.text,
        button::Status::Disabled | button::Status::Active => palette.muted,
    };
    let background = match status {
        button::Status::Pressed => palette.surface,
        _ => palette.page,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color,
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 0.0.into(),
        },
        shadow: Shadow::default(),
    }
}

fn editor_style(status: text_editor::Status, palette: EditorPalette) -> text_editor::Style {
    let border_color = match status {
        text_editor::Status::Active => palette.border,
        text_editor::Status::Hovered | text_editor::Status::Focused => palette.accent,
        text_editor::Status::Disabled => palette.muted,
    };

    text_editor::Style {
        background: Background::Color(palette.page),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 0.0.into(),
        },
        icon: palette.accent,
        placeholder: palette.muted,
        value: palette.text,
        selection: palette.selection,
    }
}
