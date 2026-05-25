use athena_api::benchmark_run::BenchmarkRun;
use athena_api::benchmark_suite::BenchmarkSuite;
use athena_api::experiment::Experiment;
use athena_api::experiment_template::ExperimentTemplate;
use athena_api::runtime_profile::RuntimeProfile;
use iced::clipboard;
use iced::widget::text::Wrapping;
use iced::widget::{
    Button, Space, button, column, container, horizontal_rule, pick_list, row, scrollable, text,
    text_editor,
};
use iced::{
    Background, Border, Color, Element, Font, Length, Padding, Shadow, Size, Task, Theme, window,
};
use kube::api::{Api, ListParams};
use kube::{Client, ResourceExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;

mod manifest_editor;
mod table;

use manifest_editor::{EditorDocument, EditorKind, EditorPalette, manifest_editor};
use table::{TableColumn, TablePalette, TableRow, clipped_text, data_table};

fn main() -> iced::Result {
    iced::application("Athena Console", AthenaConsole::update, AthenaConsole::view)
        .theme(|_| athena_theme())
        .window(window::Settings {
            size: Size::new(1280.0, 840.0),
            min_size: Some(Size::new(860.0, 520.0)),
            icon: athena_window_icon(),
            ..Default::default()
        })
        .default_font(Font::with_name("MonaspiceNe Nerd Font Mono"))
        .run_with(|| {
            (
                AthenaConsole {
                    active_view: View::Experiments,
                    phase_filter: PhaseFilter::All,
                    snapshot: ClusterSnapshot::default(),
                    template_editor: EditorDocument::new(
                        "Template YAML",
                        "No template loaded",
                        "# Select a template and load Kubernetes YAML.\n",
                    ),
                    manifest_editor: EditorDocument::new(
                        "Manifest IDE",
                        "No manifest loaded",
                        "# Select a Kubernetes resource and load its manifest.\n",
                    ),
                    selected_resource: None,
                    loading: true,
                    error: None,
                    action_status: None,
                },
                Task::perform(load_cluster_snapshot(), Message::Loaded),
            )
        })
}

fn athena_window_icon() -> Option<window::Icon> {
    const SIZE: u32 = 64;
    const PINK: [u8; 4] = [244, 114, 182, 255];
    const BLUE: [u8; 4] = [147, 197, 253, 255];
    const TEXT: [u8; 4] = [228, 228, 231, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    let mut rgba = vec![0; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let index = ((y * SIZE + x) * 4) as usize;
            let border = !(5..=58).contains(&x) || !(5..=58).contains(&y);
            let crossbar = (30..=34).contains(&y) && (20..=44).contains(&x);
            let left_leg =
                x > 10 && x < 32 && y > 9 && y < 55 && (x as i32 - y as i32 / 2 - 5).abs() < 3;
            let right_leg =
                x > 32 && x < 54 && y > 9 && y < 55 && (x as i32 + y as i32 / 2 - 58).abs() < 3;
            let blue_mark = ((12..=24).contains(&x) || (40..=52).contains(&x))
                && ((14..=18).contains(&y) || (46..=50).contains(&y));

            let pixel = if border {
                TEXT
            } else if blue_mark {
                BLUE
            } else if left_leg || right_leg || crossbar {
                PINK
            } else {
                BLACK
            };
            rgba[index..index + 4].copy_from_slice(&pixel);
        }
    }

    window::icon::from_rgba(rgba, SIZE, SIZE).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Experiments,
    Templates,
    Benchmarks,
    Runtime,
}

impl View {
    const ALL: [View; 4] = [
        View::Experiments,
        View::Templates,
        View::Benchmarks,
        View::Runtime,
    ];

    fn label(self) -> &'static str {
        match self {
            View::Experiments => "Experiments",
            View::Templates => "Templates",
            View::Benchmarks => "Benchmarks",
            View::Runtime => "Runtime",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PhaseFilter {
    All,
    Phase(String),
}

impl std::fmt::Display for PhaseFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PhaseFilter::All => write!(f, "All phases"),
            PhaseFilter::Phase(phase) => write!(f, "{phase}"),
        }
    }
}

#[derive(Debug, Clone)]
enum Message {
    Refresh,
    Loaded(Result<ClusterSnapshot, String>),
    ViewSelected(View),
    ResourceSelected(ResourceManifestRef),
    BackToResourceList,
    PhaseSelected(PhaseFilter),
    OpenGrafana,
    OpenUrl(String),
    OpenWorkspace(String),
    OpenResourceManifest(ResourceManifestRef),
    ResourceManifestLoaded(Result<(ResourceManifestRef, String), String>),
    ExternalActionFinished(Result<String, String>),
    LoadTemplateYaml { namespace: String, name: String },
    TemplateYamlLoaded(Result<(String, String, String), String>),
    EditDocument(EditorKind, text_editor::Action),
    CopyDocument(EditorKind),
    PasteDocument(EditorKind),
    ClipboardRead(EditorKind, Option<String>),
    SelectAllDocument(EditorKind),
    CopyText(String),
}

struct AthenaConsole {
    active_view: View,
    phase_filter: PhaseFilter,
    snapshot: ClusterSnapshot,
    template_editor: EditorDocument,
    manifest_editor: EditorDocument,
    selected_resource: Option<ResourceManifestRef>,
    loading: bool,
    error: Option<String>,
    action_status: Option<String>,
}

impl AthenaConsole {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                self.loading = true;
                self.error = None;
                self.action_status = None;
                Task::perform(load_cluster_snapshot(), Message::Loaded)
            }
            Message::Loaded(Ok(snapshot)) => {
                self.snapshot = snapshot;
                self.loading = false;
                self.error = None;
                Task::none()
            }
            Message::Loaded(Err(error)) => {
                self.loading = false;
                self.error = Some(error);
                Task::none()
            }
            Message::ViewSelected(view) => {
                self.active_view = view;
                self.selected_resource = None;
                Task::none()
            }
            Message::ResourceSelected(resource) => {
                self.active_view = view_for_resource_kind(&resource.kind);
                self.selected_resource = Some(resource);
                Task::none()
            }
            Message::BackToResourceList => {
                self.selected_resource = None;
                Task::none()
            }
            Message::PhaseSelected(filter) => {
                self.phase_filter = filter;
                Task::none()
            }
            Message::OpenGrafana => Task::perform(open_grafana(), Message::ExternalActionFinished),
            Message::OpenUrl(url) => Task::perform(open_url(url), Message::ExternalActionFinished),
            Message::OpenWorkspace(path) => {
                Task::perform(open_workspace(path), Message::ExternalActionFinished)
            }
            Message::OpenResourceManifest(resource) => {
                self.active_view = view_for_resource_kind(&resource.kind);
                self.selected_resource = Some(resource.clone());
                Task::perform(
                    load_resource_manifest_yaml(resource),
                    Message::ResourceManifestLoaded,
                )
            }
            Message::ResourceManifestLoaded(Ok((resource, yaml))) => {
                self.manifest_editor.set_text(
                    format!("{} manifest", resource.kind),
                    resource.manifest_path(),
                    &yaml,
                );
                self.action_status = Some("Loaded manifest from Kubernetes".to_string());
                Task::none()
            }
            Message::ResourceManifestLoaded(Err(error)) => {
                self.action_status = Some(format!("Failed to load manifest: {error}"));
                Task::none()
            }
            Message::ExternalActionFinished(Ok(status)) => {
                self.action_status = Some(status);
                Task::none()
            }
            Message::ExternalActionFinished(Err(error)) => {
                self.action_status = Some(format!("Action failed: {error}"));
                Task::none()
            }
            Message::LoadTemplateYaml { namespace, name } => Task::perform(
                load_template_yaml(namespace, name),
                Message::TemplateYamlLoaded,
            ),
            Message::TemplateYamlLoaded(Ok((namespace, name, yaml))) => {
                self.template_editor.set_text(
                    "Template YAML",
                    format!("k8s://{namespace}/experimenttemplate/{name}.yaml"),
                    &yaml,
                );
                self.action_status = Some("Loaded template YAML from Kubernetes".to_string());
                Task::none()
            }
            Message::TemplateYamlLoaded(Err(error)) => {
                self.action_status = Some(format!("Failed to load template YAML: {error}"));
                Task::none()
            }
            Message::EditDocument(kind, action) => {
                self.editor_mut(kind).perform(action);
                Task::none()
            }
            Message::CopyDocument(kind) => {
                self.action_status = Some(format!("Copied {}", editor_label(kind)));
                clipboard::write(self.editor(kind).text())
            }
            Message::PasteDocument(kind) => {
                clipboard::read().map(move |contents| Message::ClipboardRead(kind, contents))
            }
            Message::ClipboardRead(kind, Some(contents)) => {
                self.editor_mut(kind)
                    .perform(text_editor::Action::Edit(text_editor::Edit::Paste(
                        Arc::new(contents),
                    )));
                self.action_status = Some(format!("Pasted clipboard into {}", editor_label(kind)));
                Task::none()
            }
            Message::ClipboardRead(_, None) => {
                self.action_status = Some("Clipboard is empty or unavailable".to_string());
                Task::none()
            }
            Message::SelectAllDocument(kind) => {
                self.editor_mut(kind).select_all();
                self.action_status = Some(format!("Selected {}", editor_label(kind)));
                Task::none()
            }
            Message::CopyText(value) => {
                self.action_status = Some("Copied text".to_string());
                clipboard::write(value)
            }
        }
    }

    fn editor(&self, kind: EditorKind) -> &EditorDocument {
        match kind {
            EditorKind::Template => &self.template_editor,
            EditorKind::Manifest => &self.manifest_editor,
        }
    }

    fn editor_mut(&mut self, kind: EditorKind) -> &mut EditorDocument {
        match kind {
            EditorKind::Template => &mut self.template_editor,
            EditorKind::Manifest => &mut self.manifest_editor,
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let header = container(
            column![
                text("Athena Console").size(42).color(PINK),
                text("Kubernetes Research Operator Dashboard")
                    .size(18)
                    .color(TEXT),
            ]
            .align_x(iced::Alignment::Center)
            .spacing(12),
        )
        .width(Length::Fill)
        .padding(Padding {
            top: 28.0,
            right: 0.0,
            bottom: 48.0,
            left: 0.0,
        });

        let nav = View::ALL.into_iter().fold(row!().spacing(28), |row, view| {
            let label = format!(
                "{}  [{}]",
                view.label().to_uppercase(),
                self.count_for(view)
            );
            let active = self.active_view == view;
            let button = button(
                text(label)
                    .size(14)
                    .color(if active { TEXT } else { MUTED }),
            )
            .style(move |theme, status| nav_button_style(theme, status, active))
            .padding([8, 10])
            .on_press(Message::ViewSelected(view));
            row.push(button)
        });

        let body = if self.loading {
            panel(text("Loading Athena resources from kubeconfig...").color(MUTED))
        } else if let Some(error) = &self.error {
            panel(text(format!("Failed to load cluster resources: {error}")).color(RED))
        } else {
            if let Some(resource) = &self.selected_resource {
                self.resource_view(resource)
            } else {
                match self.active_view {
                    View::Experiments => self.experiments_view(),
                    View::Templates => self.templates_view(),
                    View::Benchmarks => self.benchmarks_view(),
                    View::Runtime => self.runtime_view(),
                }
            }
        };

        let footer = container(row![
            text("athena console").size(13).color(TEXT),
            Space::with_width(Length::Fill),
            text(self.action_status.as_deref().unwrap_or(""))
                .size(12)
                .color(MUTED),
            action_button("refresh", 13, 88.0)
                .style(button_style)
                .padding([7, 10])
                .on_press(Message::Refresh),
        ])
        .padding([18, 18])
        .width(Length::Fill)
        .style(footer_style);

        container(column![
            header,
            container(nav).padding(Padding {
                top: 0.0,
                right: 26.0,
                bottom: 8.0,
                left: 26.0,
            }),
            horizontal_rule(1),
            scrollable(container(body).padding(Padding {
                top: 38.0,
                right: 26.0,
                bottom: 26.0,
                left: 26.0,
            })),
            footer,
        ])
        .width(Length::Fill)
        .height(Length::Fill)
        .style(page_style)
        .into()
    }

    fn count_for(&self, view: View) -> usize {
        match view {
            View::Experiments => self.snapshot.experiments.len(),
            View::Templates => self.snapshot.templates.len(),
            View::Benchmarks => {
                self.snapshot.benchmark_runs.len() + self.snapshot.benchmark_suites.len()
            }
            View::Runtime => self.snapshot.runtime_profiles.len(),
        }
    }

    fn experiments_view(&self) -> Element<'_, Message> {
        let options = self.phase_options();
        let heading = row![
            column![
                text("Experiments").size(28).color(TEXT),
                text("Kubernetes-native experiment resources, phases, workspace refs, and local IDE drafts.")
                    .size(15)
                    .color(TEXT),
            ]
            .spacing(8)
            .width(Length::Fill),
            pick_list(options, Some(self.phase_filter.clone()), Message::PhaseSelected)
                .style(pick_list_style),
            action_button("REFRESH", 14, 112.0)
                .style(primary_button_style)
                .padding([13, 18])
                .on_press(Message::Refresh),
        ]
        .align_y(iced::Alignment::Center)
        .spacing(14);

        let rows = self
            .snapshot
            .experiments
            .iter()
            .filter(|experiment| match &self.phase_filter {
                PhaseFilter::All => true,
                PhaseFilter::Phase(phase) => experiment.phase == *phase,
            })
            .map(experiment_row)
            .collect::<Vec<_>>();

        column![
            heading,
            self.metrics_debugging_panel(),
            data_table(
                table_palette(),
                vec![
                    TableColumn::portion("Experiment", 2),
                    TableColumn::portion("Phase", 1),
                    TableColumn::portion("Workspace", 4),
                    TableColumn::portion("Actions", 3),
                ],
                rows
            ),
        ]
        .spacing(30)
        .into()
    }

    fn metrics_debugging_panel(&self) -> Element<'_, Message> {
        let panels = self.metric_panels();
        let body: Element<'_, Message> = if panels.is_empty() {
            container(
                text("No metric panels reported in CR status yet.")
                    .size(14)
                    .color(MUTED),
            )
            .padding([8, 0])
            .into()
        } else {
            panels
                .into_iter()
                .take(6)
                .fold(column!().spacing(10), |column, metric_panel| {
                    column.push(metric_panel_card(metric_panel))
                })
                .into()
        };

        panel(
            column![
                row![
                    column![
                        text("Metrics debugging").size(23).color(TEXT),
                        text("Native metric panels from Experiment and BenchmarkRun status; Grafana remains the deep-link debug surface.")
                            .size(15)
                            .color(TEXT),
                    ]
                    .spacing(10)
                    .width(Length::Fill),
                    action_button("Open Grafana", 13, 152.0)
                        .style(button_style)
                        .padding([10, 14])
                        .on_press(Message::OpenGrafana),
                ]
                .align_y(iced::Alignment::Center)
                .spacing(16),
                body,
            ]
            .spacing(14),
        )
    }

    fn metric_panels(&self) -> Vec<&MetricPanelSummary> {
        self.snapshot
            .experiments
            .iter()
            .chain(self.snapshot.benchmark_runs.iter())
            .flat_map(|resource| resource.metric_panels.iter())
            .collect()
    }

    fn editor_panel(&self, kind: EditorKind) -> Element<'_, Message> {
        manifest_editor(
            kind,
            self.editor(kind),
            editor_palette(),
            move |action| Message::EditDocument(kind, action),
            Message::SelectAllDocument(kind),
            Message::CopyDocument(kind),
            Message::PasteDocument(kind),
        )
    }

    fn templates_view(&self) -> Element<'_, Message> {
        let rows = self
            .snapshot
            .templates
            .iter()
            .map(template_row)
            .collect::<Vec<_>>();

        column![
            section_heading(
                "Experiment templates",
                "Load Kubernetes-owned template YAML, edit parameters, and keep spawning grounded in CRDs."
            ),
            data_table(
                table_palette(),
                vec![
                    TableColumn::portion("Template", 2),
                    TableColumn::portion("Objective", 1),
                    TableColumn::portion("Source", 3),
                    TableColumn::portion("Actions", 1),
                ],
                rows
            ),
            self.editor_panel(EditorKind::Template),
        ]
        .spacing(24)
        .into()
    }

    fn benchmarks_view(&self) -> Element<'_, Message> {
        let suites = data_table(
            table_palette(),
            vec![
                TableColumn::portion("BenchmarkSuite", 2),
                TableColumn::portion("Ready", 1),
                TableColumn::portion("Tasks", 4),
            ],
            self.snapshot
                .benchmark_suites
                .iter()
                .map(resource_row)
                .collect::<Vec<_>>(),
        );
        let runs = data_table(
            table_palette(),
            vec![
                TableColumn::portion("BenchmarkRun", 2),
                TableColumn::portion("Phase", 1),
                TableColumn::portion("Suite", 4),
            ],
            self.snapshot
                .benchmark_runs
                .iter()
                .map(resource_row)
                .collect::<Vec<_>>(),
        );

        column![
            section_heading(
                "Benchmarks",
                "BenchmarkSuite and BenchmarkRun resources read directly from Kubernetes status."
            ),
            text("Suites").size(20).color(TEXT),
            suites,
            text("Runs").size(20).color(TEXT),
            runs,
        ]
        .spacing(26)
        .into()
    }

    fn runtime_view(&self) -> Element<'_, Message> {
        column![
            section_heading(
                "Runtime",
                "RuntimeProfile resources, execution mode, images, and workspace storage defaults."
            ),
            data_table(
                table_palette(),
                vec![
                    TableColumn::portion("RuntimeProfile", 2),
                    TableColumn::portion("Ready", 1),
                    TableColumn::portion("Runtime", 4),
                ],
                self.snapshot
                    .runtime_profiles
                    .iter()
                    .map(resource_row)
                    .collect::<Vec<_>>()
            )
        ]
        .spacing(24)
        .into()
    }

    fn resource_view(&self, resource_ref: &ResourceManifestRef) -> Element<'_, Message> {
        let Some(resource) = self.find_resource(resource_ref) else {
            return column![
                action_button("Back", 13, 72.0)
                    .style(button_style)
                    .padding([8, 10])
                    .on_press(Message::BackToResourceList),
                panel(
                    text(format!(
                        "{} {}/{} is no longer present in the loaded snapshot.",
                        resource_ref.kind, resource_ref.namespace, resource_ref.name
                    ))
                    .color(MUTED)
                ),
            ]
            .spacing(18)
            .into();
        };

        let mut actions = row![
            action_button("Back", 13, 72.0)
                .style(button_style)
                .padding([8, 10])
                .on_press(Message::BackToResourceList),
            action_button("Manifest IDE", 13, 112.0)
                .style(button_style)
                .padding([8, 10])
                .on_press(Message::OpenResourceManifest(resource.manifest_ref())),
        ]
        .spacing(8)
        .width(Length::Shrink)
        .clip(true);

        if let Some(path) = workspace_action_path(resource) {
            actions = actions
                .push(
                    action_button("Workspace", 13, 96.0)
                        .style(button_style)
                        .padding([8, 10])
                        .on_press(Message::OpenWorkspace(path.clone())),
                )
                .push(copy_button(path));
        }
        if let Some(url) = resource.logs_link.clone() {
            actions = actions.push(
                action_button("Logs", 13, 72.0)
                    .style(link_button_style)
                    .padding([8, 10])
                    .on_press(Message::OpenUrl(url)),
            );
        }
        if let Some(url) = resource.metrics_link.clone() {
            actions = actions.push(
                action_button("Metrics", 13, 96.0)
                    .style(link_button_style)
                    .padding([8, 10])
                    .on_press(Message::OpenUrl(url)),
            );
        }
        let manifest_path = resource.manifest_ref().manifest_path();

        column![
            row![
                column![
                    text(&resource.name).size(30).color(PINK),
                    text(format!("{} / {}", resource.namespace, resource.kind))
                        .size(14)
                        .color(MUTED),
                ]
                .spacing(7)
                .width(Length::Fill),
                container(actions).width(Length::Shrink),
            ]
            .align_y(iced::Alignment::Center)
            .spacing(18),
            panel(
                column![
                    detail_row("Phase", resource.phase.clone(), BLUE),
                    detail_row("Namespace", resource.namespace.clone(), TEXT),
                    detail_row("Kind", resource.kind.clone(), TEXT),
                    detail_row("Detail", resource.detail.clone(), TEXT),
                    detail_row(
                        "Workspace",
                        resource
                            .workspace_path
                            .clone()
                            .unwrap_or_else(|| "Not reported".to_string()),
                        TEXT
                    ),
                    detail_row("Manifest", manifest_path, TEXT),
                ]
                .spacing(12)
            ),
            self.editor_panel(EditorKind::Manifest),
            resource_metric_panels(resource),
        ]
        .spacing(24)
        .into()
    }

    fn find_resource(&self, resource_ref: &ResourceManifestRef) -> Option<&ResourceSummary> {
        self.snapshot
            .experiments
            .iter()
            .chain(self.snapshot.benchmark_suites.iter())
            .chain(self.snapshot.benchmark_runs.iter())
            .chain(self.snapshot.runtime_profiles.iter())
            .find(|resource| {
                resource.namespace == resource_ref.namespace
                    && resource.name == resource_ref.name
                    && resource.kind == resource_ref.kind
            })
    }

    fn phase_options(&self) -> Vec<PhaseFilter> {
        let mut phases = self
            .snapshot
            .experiments
            .iter()
            .map(|experiment| experiment.phase.clone())
            .collect::<Vec<_>>();
        phases.sort();
        phases.dedup();

        std::iter::once(PhaseFilter::All)
            .chain(phases.into_iter().map(PhaseFilter::Phase))
            .collect()
    }
}

fn experiment_row<'a>(experiment: &'a ResourceSummary) -> TableRow<'a, Message> {
    let mut actions = row![
        action_button("Manifest IDE", 13, 112.0)
            .style(button_style)
            .padding([8, 10])
            .on_press(Message::OpenResourceManifest(experiment.manifest_ref())),
    ]
    .spacing(8)
    .width(Length::Fill)
    .clip(true);
    if let Some(path) = workspace_action_path(experiment) {
        actions = actions
            .push(
                action_button("Workspace", 13, 96.0)
                    .style(button_style)
                    .padding([8, 10])
                    .on_press(Message::OpenWorkspace(path.clone())),
            )
            .push(copy_button(path));
    }
    if let Some(url) = experiment.logs_link.clone() {
        actions = actions.push(
            action_button("Logs", 13, 72.0)
                .style(link_button_style)
                .padding([8, 10])
                .on_press(Message::OpenUrl(url)),
        );
    }
    if let Some(url) = experiment.metrics_link.clone() {
        actions = actions.push(
            action_button("Metrics", 13, 96.0)
                .style(link_button_style)
                .padding([8, 10])
                .on_press(Message::OpenUrl(url)),
        );
    }

    vec![
        column![
            resource_name_button(experiment),
            value_text(&experiment.namespace, 14, TEXT),
        ]
        .spacing(5)
        .into(),
        clipped_text(&experiment.phase, 14, BLUE),
        column![
            value_text(
                experiment
                    .workspace_path
                    .as_deref()
                    .unwrap_or("Not reported"),
                14,
                TEXT,
            ),
            value_text(&experiment.detail, 12, MUTED),
        ]
        .spacing(4)
        .into(),
        actions.into(),
    ]
}

fn section_heading<'a>(title: &'a str, subtitle: &'a str) -> Element<'a, Message> {
    column![
        text(title).size(28).color(TEXT),
        text(subtitle).size(15).color(TEXT),
    ]
    .spacing(8)
    .into()
}

fn template_row<'a>(template: &'a TemplateSummary) -> TableRow<'a, Message> {
    vec![
        column![
            value_text(&template.name, 15, PINK),
            value_text(&template.namespace, 14, TEXT),
        ]
        .spacing(5)
        .into(),
        clipped_text(&template.objective, 14, BLUE),
        value_text(&template.detail, 14, TEXT),
        action_button("Load YAML", 13, 120.0)
            .style(button_style)
            .padding([8, 12])
            .on_press(Message::LoadTemplateYaml {
                namespace: template.namespace.clone(),
                name: template.name.clone(),
            })
            .into(),
    ]
}

fn resource_row<'a>(resource: &'a ResourceSummary) -> TableRow<'a, Message> {
    let mut detail = row![value_text(&resource.detail, 14, TEXT)]
        .spacing(8)
        .width(Length::Fill)
        .clip(true);

    if let Some(url) = resource
        .metrics_link
        .clone()
        .or_else(|| resource.logs_link.clone())
    {
        detail = detail.push(
            action_button("Open", 13, 72.0)
                .style(link_button_style)
                .padding([8, 10])
                .on_press(Message::OpenUrl(url)),
        );
    }
    vec![
        column![
            resource_name_button(resource),
            value_text(&resource.namespace, 14, TEXT),
        ]
        .spacing(5)
        .into(),
        clipped_text(&resource.phase, 14, BLUE),
        detail.into(),
    ]
}

fn resource_name_button<'a>(resource: &'a ResourceSummary) -> Element<'a, Message> {
    button(
        text(&resource.name)
            .size(15)
            .color(PINK)
            .wrapping(Wrapping::None)
            .width(Length::Fill),
    )
    .width(Length::Fill)
    .padding(0)
    .style(resource_name_button_style)
    .on_press(Message::ResourceSelected(resource.manifest_ref()))
    .into()
}

fn resource_metric_panels(resource: &ResourceSummary) -> Element<'_, Message> {
    if resource.metric_panels.is_empty() {
        return panel(
            text("No metric panels reported for this resource.")
                .size(14)
                .color(MUTED),
        );
    }

    panel(
        column![
            text("Metric panels").size(21).color(TEXT),
            resource
                .metric_panels
                .iter()
                .fold(column!().spacing(10), |column, metric_panel| {
                    column.push(metric_panel_card(metric_panel))
                }),
        ]
        .spacing(12),
    )
}

fn metric_panel_card(metric_panel: &MetricPanelSummary) -> Element<'_, Message> {
    let mut header = row![
        column![
            text(&metric_panel.title).size(16).color(PINK),
            text(&metric_panel.source).size(12).color(MUTED),
        ]
        .spacing(4)
        .width(Length::Fill),
        text(&metric_panel.status).size(13).color(BLUE),
    ]
    .align_y(iced::Alignment::Center)
    .spacing(12);

    if let Some(url) = metric_panel.metrics_link.clone() {
        header = header.push(
            action_button("Grafana", 13, 88.0)
                .style(link_button_style)
                .padding([7, 10])
                .on_press(Message::OpenUrl(url)),
        );
    }

    container(
        column![
            header,
            row![
                metric_value("Objective", &metric_panel.objective),
                metric_value("Latest", &metric_panel.latest),
                metric_value("Best", &metric_panel.best),
                metric_value("Series", &metric_panel.series),
            ]
            .spacing(10),
        ]
        .spacing(10),
    )
    .padding(10)
    .width(Length::Fill)
    .style(panel_style)
    .into()
}

fn metric_value<'a>(label: &'static str, value: &'a str) -> Element<'a, Message> {
    container(
        column![
            text(label).size(12).color(MUTED),
            text(value)
                .size(14)
                .color(TEXT)
                .wrapping(Wrapping::None)
                .width(Length::Fill),
        ]
        .spacing(3),
    )
    .width(Length::Fill)
    .clip(true)
    .into()
}

fn detail_row(label: &'static str, value: String, color: Color) -> Element<'static, Message> {
    row![
        text(label)
            .size(13)
            .color(MUTED)
            .width(Length::Fixed(120.0)),
        text(value.clone())
            .size(14)
            .color(color)
            .wrapping(Wrapping::None)
            .width(Length::Fill),
        copy_button(value),
    ]
    .spacing(14)
    .align_y(iced::Alignment::Center)
    .into()
}

fn workspace_action_path(resource: &ResourceSummary) -> Option<String> {
    resource.workspace_path.clone()
}

fn value_text<'a>(value: &'a str, size: u16, color: Color) -> Element<'a, Message> {
    clipped_text(value, size, color)
}

fn copy_button(value: String) -> Button<'static, Message> {
    action_button("Copy", 13, 72.0)
        .style(link_button_style)
        .padding([8, 10])
        .on_press(Message::CopyText(value))
}

fn action_button<'a>(label: &'a str, size: u16, width: f32) -> Button<'a, Message> {
    button(
        text(label)
            .size(size)
            .color(TEXT)
            .wrapping(Wrapping::None)
            .width(Length::Fixed(width))
            .align_x(iced::alignment::Horizontal::Center),
    )
    .width(Length::Fixed(width + 18.0))
    .height(Length::Fixed(size as f32 + 24.0))
    .clip(true)
}

const PAGE: Color = Color::BLACK;
const SURFACE: Color = Color {
    r: 0.035,
    g: 0.035,
    b: 0.043,
    a: 1.0,
};
const BORDER: Color = Color {
    r: 0.247,
    g: 0.247,
    b: 0.275,
    a: 1.0,
};
const TEXT: Color = Color {
    r: 0.894,
    g: 0.894,
    b: 0.906,
    a: 1.0,
};
const MUTED: Color = Color {
    r: 0.443,
    g: 0.443,
    b: 0.478,
    a: 1.0,
};
const PINK: Color = Color {
    r: 0.957,
    g: 0.447,
    b: 0.714,
    a: 1.0,
};
const PINK_DARK: Color = Color {
    r: 0.514,
    g: 0.094,
    b: 0.267,
    a: 1.0,
};
const BLUE: Color = Color {
    r: 0.576,
    g: 0.773,
    b: 0.992,
    a: 1.0,
};
const BLUE_DARK: Color = Color {
    r: 0.118,
    g: 0.227,
    b: 0.541,
    a: 1.0,
};
const RED: Color = Color {
    r: 0.973,
    g: 0.443,
    b: 0.443,
    a: 1.0,
};

fn athena_theme() -> Theme {
    Theme::custom(
        "Athena High Contrast".to_string(),
        iced::theme::Palette {
            background: PAGE,
            text: TEXT,
            primary: PINK,
            success: BLUE,
            danger: RED,
        },
    )
}

fn page_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PAGE)),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

fn footer_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(PAGE)),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn panel<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .padding(10)
        .width(Length::Fill)
        .style(panel_style)
        .into()
}

fn panel_style(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn table_palette() -> TablePalette {
    TablePalette {
        surface: SURFACE,
        border: BORDER,
        text: TEXT,
        muted: MUTED,
    }
}

fn editor_palette() -> EditorPalette {
    EditorPalette {
        page: PAGE,
        surface: SURFACE,
        border: BORDER,
        text: TEXT,
        muted: MUTED,
        accent: PINK,
        selection: BLUE_DARK,
    }
}

fn editor_label(kind: EditorKind) -> &'static str {
    match kind {
        EditorKind::Template => "template YAML",
        EditorKind::Manifest => "manifest draft",
    }
}

fn nav_button_style(_: &Theme, status: button::Status, active: bool) -> button::Style {
    let hot = active || matches!(status, button::Status::Hovered | button::Status::Pressed);

    button::Style {
        background: Some(Background::Color(PAGE)),
        text_color: if hot { TEXT } else { MUTED },
        border: Border {
            color: if hot { PINK } else { PAGE },
            width: 1.0,
            radius: 0.0.into(),
        },
        shadow: Shadow::default(),
    }
}

fn primary_button_style(_: &Theme, status: button::Status) -> button::Style {
    let background = match status {
        button::Status::Hovered | button::Status::Pressed => BLUE_DARK,
        button::Status::Disabled | button::Status::Active => PINK_DARK,
    };

    button::Style {
        background: Some(Background::Color(background)),
        text_color: TEXT,
        border: Border {
            color: background,
            width: 1.0,
            radius: 0.0.into(),
        },
        shadow: Shadow::default(),
    }
}

fn button_style(_: &Theme, status: button::Status) -> button::Style {
    let border_color = match status {
        button::Status::Hovered | button::Status::Pressed => PINK,
        button::Status::Disabled | button::Status::Active => BORDER,
    };
    let text_color = match status {
        button::Status::Hovered | button::Status::Pressed => TEXT,
        button::Status::Disabled => MUTED,
        button::Status::Active => MUTED,
    };
    let background = match status {
        button::Status::Pressed => SURFACE,
        _ => PAGE,
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

fn link_button_style(_: &Theme, status: button::Status) -> button::Style {
    let text_color = match status {
        button::Status::Hovered | button::Status::Pressed => TEXT,
        button::Status::Disabled => MUTED,
        button::Status::Active => BLUE,
    };

    button::Style {
        background: Some(Background::Color(PAGE)),
        text_color,
        border: Border {
            color: match status {
                button::Status::Hovered | button::Status::Pressed => BLUE,
                button::Status::Disabled | button::Status::Active => BORDER,
            },
            width: 1.0,
            radius: 0.0.into(),
        },
        shadow: Shadow::default(),
    }
}

fn resource_name_button_style(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: match status {
            button::Status::Hovered | button::Status::Pressed => TEXT,
            button::Status::Disabled | button::Status::Active => PINK,
        },
        border: Border {
            color: match status {
                button::Status::Hovered | button::Status::Pressed => PINK,
                button::Status::Disabled | button::Status::Active => SURFACE,
            },
            width: 1.0,
            radius: 0.0.into(),
        },
        shadow: Shadow::default(),
    }
}

fn view_for_resource_kind(kind: &str) -> View {
    match kind {
        "experiment" => View::Experiments,
        "benchmarksuite" | "benchmarkrun" => View::Benchmarks,
        "runtimeprofile" => View::Runtime,
        _ => View::Experiments,
    }
}

fn pick_list_style(_: &Theme, status: pick_list::Status) -> pick_list::Style {
    let border_color = match status {
        pick_list::Status::Hovered | pick_list::Status::Opened => PINK,
        pick_list::Status::Active => BORDER,
    };

    pick_list::Style {
        text_color: TEXT,
        placeholder_color: MUTED,
        handle_color: PINK,
        background: Background::Color(PAGE),
        border: Border {
            color: border_color,
            width: 1.0,
            radius: 0.0.into(),
        },
    }
}

#[derive(Debug, Clone, Default)]
struct ClusterSnapshot {
    experiments: Vec<ResourceSummary>,
    templates: Vec<TemplateSummary>,
    benchmark_suites: Vec<ResourceSummary>,
    benchmark_runs: Vec<ResourceSummary>,
    runtime_profiles: Vec<ResourceSummary>,
}

#[derive(Debug, Clone)]
struct ResourceSummary {
    namespace: String,
    name: String,
    kind: String,
    phase: String,
    detail: String,
    workspace_path: Option<String>,
    logs_link: Option<String>,
    metrics_link: Option<String>,
    metric_panels: Vec<MetricPanelSummary>,
}

#[derive(Debug, Clone)]
struct MetricPanelSummary {
    title: String,
    source: String,
    objective: String,
    latest: String,
    best: String,
    series: String,
    status: String,
    metrics_link: Option<String>,
}

impl ResourceSummary {
    fn manifest_ref(&self) -> ResourceManifestRef {
        ResourceManifestRef {
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            kind: self.kind.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResourceManifestRef {
    namespace: String,
    name: String,
    kind: String,
}

impl ResourceManifestRef {
    fn manifest_path(&self) -> String {
        format!("k8s://{}/{}/{}.yaml", self.namespace, self.kind, self.name)
    }
}

#[derive(Debug, Clone)]
struct TemplateSummary {
    namespace: String,
    name: String,
    objective: String,
    detail: String,
}

async fn load_cluster_snapshot() -> Result<ClusterSnapshot, String> {
    load_cluster_snapshot_inner()
        .await
        .map_err(|error| error.to_string())
}

async fn open_grafana() -> Result<String, String> {
    let url = std::env::var("ATHENA_GRAFANA_URL").unwrap_or_else(|_| {
        "https://grafana.casazza.io/d/athena-athena-experiment-debugging/athena-experiment-debugging"
            .to_string()
    });
    open_url(url).await
}

async fn open_url(url: String) -> Result<String, String> {
    if url.trim().is_empty() {
        return Err("URL is empty".to_string());
    }

    run_command("xdg-open", &[&url]).map(|_| format!("Opened link: {url}"))
}

async fn open_workspace(path: String) -> Result<String, String> {
    if path.trim().is_empty() {
        return Err("workspace path is empty".to_string());
    }

    let local_path = Path::new(&path);
    if !local_path.exists() {
        return Ok(format!(
            "Workspace path is cluster-only, not mounted locally: {path}"
        ));
    }

    let editor = configured_editor();

    run_command(&editor, &[&path])
        .or_else(|_| run_command("xdg-open", &[&path]))
        .map(|_| format!("Opened workspace: {path}"))
}

async fn load_resource_manifest_yaml(
    resource: ResourceManifestRef,
) -> Result<(ResourceManifestRef, String), String> {
    let output = Command::new("kubectl")
        .args([
            "-n",
            &resource.namespace,
            "get",
            &resource.kind,
            &resource.name,
            "-o",
            "yaml",
        ])
        .output()
        .map_err(|error| format!("failed to run kubectl: {error}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    String::from_utf8(output.stdout)
        .map(|yaml| (resource, yaml))
        .map_err(|error| format!("kubectl returned invalid UTF-8: {error}"))
}

fn configured_editor() -> String {
    std::env::var("ATHENA_EDITOR")
        .or_else(|_| std::env::var("VISUAL"))
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "lapce".to_string())
}

async fn load_template_yaml(
    namespace: String,
    name: String,
) -> Result<(String, String, String), String> {
    let output = Command::new("kubectl")
        .args([
            "-n",
            &namespace,
            "get",
            "experimenttemplate",
            &name,
            "-o",
            "yaml",
        ])
        .output()
        .map_err(|error| format!("failed to run kubectl: {error}"))?;

    if output.status.success() {
        String::from_utf8(output.stdout)
            .map(|yaml| (namespace, name, yaml))
            .map_err(|error| format!("kubectl returned invalid UTF-8: {error}"))
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn run_command(command: &str, args: &[&str]) -> Result<(), String> {
    Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("{command}: {error}"))
}

fn experiment_metric_panels(
    namespace: &str,
    name: &str,
    status: &athena_api::experiment::ExperimentStatus,
) -> Vec<MetricPanelSummary> {
    if let Some(metrics) = &status.metrics_detail {
        let series_count = metrics.series.len();
        let point_count = metrics
            .series
            .iter()
            .map(|series| series.points.len())
            .sum::<usize>();
        return vec![MetricPanelSummary {
            title: metrics
                .objective_name
                .clone()
                .unwrap_or_else(|| "Objective metrics".to_string()),
            source: format!("{namespace}/experiment/{name}"),
            objective: format!(
                "{} {}",
                metrics.objective_name.as_deref().unwrap_or("objective"),
                metrics.objective_goal.as_deref().unwrap_or("track")
            ),
            latest: metrics
                .latest
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "Not reported".to_string()),
            best: metrics
                .best
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "Not reported".to_string()),
            series: format!("{series_count} series / {point_count} points"),
            status: format!("{:?}", status.phase),
            metrics_link: status.metrics_link.clone(),
        }];
    }

    if status.metrics.is_empty() {
        return Vec::new();
    }

    vec![MetricPanelSummary {
        title: "Reported metrics".to_string(),
        source: format!("{namespace}/experiment/{name}"),
        objective: "status.metrics".to_string(),
        latest: status
            .metrics
            .iter()
            .take(3)
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(", "),
        best: "Not reported".to_string(),
        series: "summary only".to_string(),
        status: format!("{:?}", status.phase),
        metrics_link: status.metrics_link.clone(),
    }]
}

fn benchmark_metric_panels(
    namespace: &str,
    name: &str,
    status: &athena_api::benchmark_run::BenchmarkRunStatus,
) -> Vec<MetricPanelSummary> {
    let series_count = status.metric_series.len();
    let point_count = status
        .metric_series
        .iter()
        .map(|series| series.points.len())
        .sum::<usize>();

    if status.aggregate_metrics.is_empty() && series_count == 0 {
        return Vec::new();
    }

    let latest = if status.aggregate_metrics.is_empty() {
        "Not reported".to_string()
    } else {
        status
            .aggregate_metrics
            .iter()
            .take(3)
            .map(|(key, aggregate)| {
                format!(
                    "{key}=mean:{} count:{}",
                    format_optional(aggregate.mean),
                    aggregate
                        .count
                        .map(|count| count.to_string())
                        .unwrap_or_else(|| "n/a".to_string())
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };

    vec![MetricPanelSummary {
        title: "Benchmark aggregates".to_string(),
        source: format!("{namespace}/benchmarkrun/{name}"),
        objective: "aggregateMetrics".to_string(),
        latest,
        best: status
            .aggregate_metrics
            .iter()
            .next()
            .and_then(|(_, aggregate)| aggregate.max.or(aggregate.min).or(aggregate.mean))
            .map(|value| format!("{value:.4}"))
            .unwrap_or_else(|| "Not reported".to_string()),
        series: format!("{series_count} series / {point_count} points"),
        status: format!("{:?}", status.phase),
        metrics_link: status.metrics_link.clone(),
    }]
}

fn format_optional(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "n/a".to_string())
}

async fn load_cluster_snapshot_inner() -> anyhow::Result<ClusterSnapshot> {
    let client = Client::try_default().await?;
    let list_params = ListParams::default().limit(500);

    let experiments_api = Api::<Experiment>::all(client.clone());
    let templates_api = Api::<ExperimentTemplate>::all(client.clone());
    let benchmark_suites_api = Api::<BenchmarkSuite>::all(client.clone());
    let benchmark_runs_api = Api::<BenchmarkRun>::all(client.clone());
    let runtime_profiles_api = Api::<RuntimeProfile>::all(client);

    let (
        experiment_list,
        template_list,
        benchmark_suite_list,
        benchmark_run_list,
        runtime_profile_list,
    ) = tokio::try_join!(
        experiments_api.list(&list_params),
        templates_api.list(&list_params),
        benchmark_suites_api.list(&list_params),
        benchmark_runs_api.list(&list_params),
        runtime_profiles_api.list(&list_params),
    )?;

    let experiments = experiment_list
        .items
        .into_iter()
        .map(|experiment| {
            let status = experiment.status.as_ref();
            let namespace = experiment
                .namespace()
                .unwrap_or_else(|| "default".to_string());
            let name = experiment.name_any();
            let metric_panels = status
                .map(|status| experiment_metric_panels(&namespace, &name, status))
                .unwrap_or_default();
            ResourceSummary {
                namespace,
                name,
                kind: "experiment".to_string(),
                phase: status
                    .map(|status| format!("{:?}", status.phase))
                    .unwrap_or_else(|| "Pending".to_string()),
                detail: status
                    .and_then(|status| status.message.clone())
                    .unwrap_or_else(|| experiment.spec.hypothesis.clone()),
                workspace_path: status.and_then(|status| status.workspace_path.clone()),
                logs_link: status.and_then(|status| status.logs_link.clone()),
                metrics_link: status.and_then(|status| status.metrics_link.clone()),
                metric_panels,
            }
        })
        .collect();

    let templates = template_list
        .items
        .into_iter()
        .map(|template| TemplateSummary {
            namespace: template
                .namespace()
                .unwrap_or_else(|| "default".to_string()),
            name: template.name_any(),
            objective: format!(
                "{} / {:?}",
                template.spec.objective.metric, template.spec.objective.goal
            ),
            detail: format!(
                "runtime={} source={}",
                template.spec.runtime_profile_ref, template.spec.source.git.url
            ),
        })
        .collect();

    let benchmark_suites = benchmark_suite_list
        .items
        .into_iter()
        .map(|suite| ResourceSummary {
            namespace: suite.namespace().unwrap_or_else(|| "default".to_string()),
            name: suite.name_any(),
            kind: "benchmarksuite".to_string(),
            phase: suite
                .status
                .as_ref()
                .map(|status| {
                    if status.ready {
                        "Ready".to_string()
                    } else {
                        "NotReady".to_string()
                    }
                })
                .unwrap_or_else(|| "No status".to_string()),
            detail: format!("{:?} tasks={}", suite.spec.taxonomy, suite.spec.tasks.len()),
            workspace_path: None,
            logs_link: None,
            metrics_link: None,
            metric_panels: Vec::new(),
        })
        .collect();

    let benchmark_runs = benchmark_run_list
        .items
        .into_iter()
        .map(|run| {
            let status = run.status.as_ref();
            let namespace = run.namespace().unwrap_or_else(|| "default".to_string());
            let name = run.name_any();
            let metric_panels = status
                .map(|status| benchmark_metric_panels(&namespace, &name, status))
                .unwrap_or_default();
            ResourceSummary {
                namespace,
                name,
                kind: "benchmarkrun".to_string(),
                phase: status
                    .map(|status| format!("{:?}", status.phase))
                    .unwrap_or_else(|| "Pending".to_string()),
                detail: run
                    .spec
                    .output
                    .as_ref()
                    .and_then(|output| output.workspace_path.clone())
                    .unwrap_or_else(|| format!("suite={}", run.spec.suite_ref.name)),
                workspace_path: run
                    .spec
                    .output
                    .as_ref()
                    .and_then(|output| output.workspace_path.clone()),
                logs_link: status.and_then(|status| status.logs_link.clone()),
                metrics_link: status.and_then(|status| status.metrics_link.clone()),
                metric_panels,
            }
        })
        .collect();

    let runtime_profiles = runtime_profile_list
        .items
        .into_iter()
        .map(|profile| ResourceSummary {
            namespace: profile.namespace().unwrap_or_else(|| "default".to_string()),
            name: profile.name_any(),
            kind: "runtimeprofile".to_string(),
            phase: profile
                .status
                .as_ref()
                .map(|status| {
                    if status.ready {
                        "Ready".to_string()
                    } else {
                        "NotReady".to_string()
                    }
                })
                .unwrap_or_else(|| "No status".to_string()),
            detail: format!(
                "{:?} {:?} image={}",
                profile.spec.runtime.runtime_type, profile.spec.runtime.mode, profile.spec.image
            ),
            workspace_path: None,
            logs_link: None,
            metrics_link: None,
            metric_panels: Vec::new(),
        })
        .collect();

    Ok(ClusterSnapshot {
        experiments,
        templates,
        benchmark_suites,
        benchmark_runs,
        runtime_profiles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn experiment_summary(
        name: &str,
        workspace_path: Option<&str>,
        detail: &str,
    ) -> ResourceSummary {
        ResourceSummary {
            namespace: "athena".to_string(),
            name: name.to_string(),
            kind: "experiment".to_string(),
            phase: "Running".to_string(),
            detail: detail.to_string(),
            workspace_path: workspace_path.map(ToString::to_string),
            logs_link: Some("https://logs.example.test".to_string()),
            metrics_link: Some("https://metrics.example.test".to_string()),
            metric_panels: vec![MetricPanelSummary {
                title: "Objective".to_string(),
                source: format!("athena/experiment/{name}"),
                objective: "reward maximize".to_string(),
                latest: "0.42".to_string(),
                best: "0.42".to_string(),
                series: "1 series / 1 points".to_string(),
                status: "Running".to_string(),
                metrics_link: Some("https://metrics.example.test".to_string()),
            }],
        }
    }

    fn console_with_snapshot(snapshot: ClusterSnapshot) -> AthenaConsole {
        AthenaConsole {
            active_view: View::Experiments,
            phase_filter: PhaseFilter::All,
            snapshot,
            template_editor: EditorDocument::new("Template YAML", "test", ""),
            manifest_editor: EditorDocument::new("Manifest IDE", "test", ""),
            selected_resource: None,
            loading: false,
            error: None,
            action_status: None,
        }
    }

    #[test]
    fn experiment_ide_manifest_path_formats_kind_namespace_and_name() {
        let resource = experiment_summary("train-canary", None, "canary");

        assert_eq!(
            resource.manifest_ref().manifest_path(),
            "k8s://athena/experiment/train-canary.yaml"
        );
    }

    #[test]
    fn console_e2e_selecting_experiment_opens_resource_detail_view() {
        let resource = experiment_summary("canary-run-03", Some("/workspace/canary-run-03"), "ok");
        let resource_ref = resource.manifest_ref();
        let snapshot = ClusterSnapshot {
            experiments: vec![resource],
            ..Default::default()
        };
        let mut console = console_with_snapshot(snapshot);

        let _ = console.update(Message::ResourceSelected(resource_ref.clone()));
        let _ = console.view();

        assert_eq!(console.active_view, View::Experiments);
        assert_eq!(console.selected_resource, Some(resource_ref));
        assert!(
            console
                .find_resource(console.selected_resource.as_ref().unwrap())
                .is_some()
        );
    }

    #[test]
    fn console_e2e_status_detail_is_not_treated_as_workspace_action() {
        let resource = experiment_summary("canary-run-03", None, "PVC not found");
        let resource_ref = resource.manifest_ref();
        let snapshot = ClusterSnapshot {
            experiments: vec![resource],
            ..Default::default()
        };
        let mut console = console_with_snapshot(snapshot);

        let _ = console.update(Message::ResourceSelected(resource_ref.clone()));
        let selected = console.find_resource(&resource_ref).unwrap();

        assert_eq!(selected.detail, "PVC not found");
        assert_eq!(workspace_action_path(selected), None);
    }
}
