//! AI Providers screen: store or remove API keys in the OS secret store.

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Font, Task as Cmd, font};
use juttusihti_core::pricing::Provider;

use crate::secrets;

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Vec<(Provider, Result<bool, String>)>),
    Input(Provider, String),
    Save(Provider),
    Remove(Provider),
    Done(Result<String, String>),
}

pub struct State {
    status: Vec<(Provider, Result<bool, String>)>,
    inputs: [String; 3],
    message: Option<Result<String, String>>,
}

fn idx(p: Provider) -> usize {
    Provider::ALL.iter().position(|x| *x == p).unwrap_or(0)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("secret store task panicked")
}

impl State {
    pub fn new() -> Self {
        Self { status: Vec::new(), inputs: Default::default(), message: None }
    }

    pub fn load(&self) -> Cmd<Msg> {
        Cmd::perform(blocking(secrets::status), Msg::Loaded)
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Loaded(s) => self.status = s,
            Msg::Input(p, s) => self.inputs[idx(p)] = s,
            Msg::Save(p) => {
                let key = std::mem::take(&mut self.inputs[idx(p)]);
                return Cmd::perform(
                    blocking(move || secrets::set(p, &key).map(|_| format!("Saved the {p} key."))),
                    Msg::Done,
                );
            }
            Msg::Remove(p) => {
                return Cmd::perform(
                    blocking(move || secrets::delete(p).map(|_| format!("Removed the {p} key."))),
                    Msg::Done,
                );
            }
            Msg::Done(r) => {
                self.message = Some(r);
                return self.load();
            }
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let mut col = column![
            text("AI Providers").size(24).font(bold),
            text(
                "API keys are kept in your operating system's secret store, not in Juttusihti's database. \
                 Nothing is sent to a provider until you start an analysis and agree to send its articles."
            )
            .size(13),
        ]
        .spacing(16)
        .padding(24)
        .max_width(760);
        for p in Provider::ALL {
            let state = match self.status.iter().find(|(x, _)| *x == p).map(|(_, s)| s) {
                Some(Ok(true)) => "Key stored".to_string(),
                Some(Ok(false)) => "No key".to_string(),
                Some(Err(e)) => e.clone(),
                None => "…".to_string(),
            };
            let has_key = matches!(self.status.iter().find(|(x, _)| *x == p), Some((_, Ok(true))));
            col = col.push(
                column![
                    row![text(p.label()).size(16).font(bold), text(state).size(13)].spacing(12).align_y(iced::Center),
                    row![
                        text_input("Paste API key", &self.inputs[idx(p)])
                            .secure(true)
                            .on_input(move |s| Msg::Input(p, s))
                            .on_submit(Msg::Save(p)),
                        button(text("Save"))
                            .on_press_maybe((!self.inputs[idx(p)].trim().is_empty()).then_some(Msg::Save(p))),
                        button(text("Remove")).style(button::danger).on_press_maybe(has_key.then_some(Msg::Remove(p))),
                    ]
                    .spacing(8),
                ]
                .spacing(6),
            );
        }
        match &self.message {
            Some(Ok(m)) => col = col.push(container(text(m).size(13)).padding(8).style(container::success)),
            Some(Err(m)) => col = col.push(container(text(m).size(13)).padding(8).style(container::danger)),
            None => {}
        }
        scrollable(col).into()
    }
}
