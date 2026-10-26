//! AI Providers screen: Sign in with ChatGPT, API keys in the OS secret
//! store, and how to get a key from each provider.

use iced::widget::{Space, button, column, container, row, rule, scrollable, text, text_input};
use iced::{Element, Font, Task as Cmd, font};
use juttusihti_core::pricing::Provider;

use crate::{plan, secrets};

const CLAUDE_CONSOLE: &str = "https://platform.claude.com/";
const CLAUDE_KEYS: &str = "https://platform.claude.com/settings/keys";
const GEMINI_KEYS: &str = "https://aistudio.google.com/apikey";
const GEMINI_TERMS: &str = "https://ai.google.dev/gemini-api/terms";
const OPENAI_KEYS: &str = "https://platform.openai.com/api-keys";
const CHATGPT_HELP: &str =
    "https://help.openai.com/en/articles/20001542-using-your-chatgpt-plan-in-other-apps-and-sites";

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Vec<(Provider, Result<bool, String>)>),
    Input(Provider, String),
    Save(Provider),
    Remove(Provider),
    Done(Result<String, String>),
    Open(&'static str),
    ChatGptStatus(Result<Option<String>, String>),
    SignIn,
    SignInStarted(Result<(String, plan::Pending), String>),
    SignInFinished(Result<String, String>),
    CancelSignIn,
    CopyLink,
    SignOut,
}

enum ChatGpt {
    Unknown,
    SignedOut,
    SignedIn(String),
    Starting,
    /// Waiting for the browser; the link is shown in case it did not open.
    Waiting {
        url: String,
        abort: iced::task::Handle,
    },
}

pub struct State {
    status: Vec<(Provider, Result<bool, String>)>,
    inputs: [String; 3],
    message: Option<Result<String, String>>,
    chatgpt: ChatGpt,
}

fn idx(p: Provider) -> usize {
    Provider::ALL.iter().position(|x| *x == p).unwrap_or(0)
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("secret store task panicked")
}

impl State {
    pub fn new() -> Self {
        Self { status: Vec::new(), inputs: Default::default(), message: None, chatgpt: ChatGpt::Unknown }
    }

    pub fn load(&self) -> Cmd<Msg> {
        Cmd::batch([
            Cmd::perform(blocking(secrets::status), Msg::Loaded),
            Cmd::perform(plan::status(), Msg::ChatGptStatus),
        ])
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
            Msg::Open(url) => {
                if let Err(e) = open::that_detached(url) {
                    self.message = Some(Err(format!("Could not open the browser: {e}. The address is {url}")));
                }
            }
            Msg::ChatGptStatus(r) => {
                if !matches!(self.chatgpt, ChatGpt::Starting | ChatGpt::Waiting { .. }) {
                    self.chatgpt = match r {
                        Ok(Some(email)) => ChatGpt::SignedIn(email),
                        Ok(None) => ChatGpt::SignedOut,
                        Err(e) => {
                            self.message = Some(Err(e));
                            ChatGpt::SignedOut
                        }
                    };
                }
            }
            Msg::SignIn => {
                self.message = None;
                self.chatgpt = ChatGpt::Starting;
                return Cmd::perform(plan::start_sign_in(), Msg::SignInStarted);
            }
            Msg::SignInStarted(Ok((url, pending))) => {
                if let Err(e) = open::that_detached(&url) {
                    self.message = Some(Err(format!("Could not open the browser ({e}). Copy the link below instead.")));
                }
                let (task, abort) = Cmd::perform(plan::finish_sign_in(pending), Msg::SignInFinished).abortable();
                self.chatgpt = ChatGpt::Waiting { url, abort };
                return task;
            }
            Msg::SignInStarted(Err(e)) | Msg::SignInFinished(Err(e)) => {
                self.chatgpt = ChatGpt::SignedOut;
                self.message = Some(Err(format!("ChatGPT sign-in failed: {e}")));
                return Cmd::perform(plan::status(), Msg::ChatGptStatus);
            }
            Msg::SignInFinished(Ok(email)) => {
                self.message = Some(Ok(format!("Signed in with ChatGPT as {email}.")));
                self.chatgpt = ChatGpt::SignedIn(email);
            }
            Msg::CancelSignIn => {
                if let ChatGpt::Waiting { abort, .. } = &self.chatgpt {
                    abort.abort();
                }
                self.chatgpt = ChatGpt::Unknown;
                return Cmd::perform(plan::status(), Msg::ChatGptStatus);
            }
            Msg::CopyLink => {
                if let ChatGpt::Waiting { url, .. } = &self.chatgpt {
                    return iced::clipboard::write(url.clone());
                }
            }
            Msg::SignOut => {
                return Cmd::perform(
                    async { plan::sign_out().await.map(|_| "Signed out of ChatGPT.".to_string()) },
                    Msg::Done,
                );
            }
        }
        Cmd::none()
    }

    fn key_row(&self, p: Provider, placeholder: &'static str) -> Element<'_, Msg> {
        let has_key = matches!(self.status.iter().find(|(x, _)| *x == p), Some((_, Ok(true))));
        let state = match self.status.iter().find(|(x, _)| *x == p).map(|(_, s)| s) {
            Some(Ok(true)) => "Key stored".to_string(),
            Some(Ok(false)) => "No key".to_string(),
            Some(Err(e)) => e.clone(),
            None => "…".to_string(),
        };
        column![
            text(state).size(13),
            row![
                text_input(placeholder, &self.inputs[idx(p)])
                    .secure(true)
                    .on_input(move |s| Msg::Input(p, s))
                    .on_submit(Msg::Save(p)),
                button(text("Save")).on_press_maybe((!self.inputs[idx(p)].trim().is_empty()).then_some(Msg::Save(p))),
                button(text("Remove")).style(button::danger).on_press_maybe(has_key.then_some(Msg::Remove(p))),
            ]
            .spacing(8),
        ]
        .spacing(6)
        .into()
    }

    fn view_chatgpt(&self) -> Element<'_, Msg> {
        let mut col = column![
            text(
                "With a ChatGPT Plus or Pro plan you can sign in instead of using an API key. Analysis runs \
                 then count against your plan's usage, not API credit. You can set a weekly limit for \
                 Juttusihti in ChatGPT's settings."
            )
            .size(13),
        ]
        .spacing(8);
        col = col.push(match &self.chatgpt {
            ChatGpt::Unknown => Element::from(text("…").size(13)),
            ChatGpt::SignedOut => {
                row![button(text("Sign in with ChatGPT")).on_press(Msg::SignIn), text("Not signed in").size(13),]
                    .spacing(12)
                    .align_y(iced::Center)
                    .into()
            }
            ChatGpt::Starting => text("Starting sign-in…").size(13).into(),
            ChatGpt::Waiting { url, .. } => column![
                text("Finish signing in in your browser. Juttusihti is waiting for it to come back.").size(13),
                row![
                    button(text("Copy sign-in link")).style(button::secondary).on_press(Msg::CopyLink),
                    button(text("Cancel")).style(button::secondary).on_press(Msg::CancelSignIn),
                ]
                .spacing(8),
                text(format!(
                    "If no browser opened, paste the link into one: {}…",
                    url.chars().take(60).collect::<String>()
                ))
                .size(11),
            ]
            .spacing(6)
            .into(),
            ChatGpt::SignedIn(email) => row![
                text(format!("Signed in as {email}")).size(13),
                button(text("Sign out")).style(button::secondary).on_press(Msg::SignOut),
            ]
            .spacing(12)
            .align_y(iced::Center)
            .into(),
        });
        col.push(link("How ChatGPT plan usage in other apps works", CHATGPT_HELP)).into()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let heading = |s: &'static str| text(s).size(18).font(bold);
        let sub = |s: &'static str| text(s).size(14).font(bold);
        let mut col = column![
            text("AI Providers").size(24).font(bold),
            text(
                "Credentials are kept in your operating system's secret store, not in Juttusihti's database. \
                 Nothing is sent to a provider until you start an analysis and agree to send its articles."
            )
            .size(13),
        ]
        .spacing(16)
        .padding(24)
        .max_width(760);
        match &self.message {
            Some(Ok(m)) => col = col.push(container(text(m).size(13)).padding(8).style(container::success)),
            Some(Err(m)) => col = col.push(container(text(m).size(13)).padding(8).style(container::danger)),
            None => {}
        }

        col = col
            .push(rule::horizontal(1))
            .push(heading("OpenAI"))
            .push(sub("Sign in with ChatGPT"))
            .push(self.view_chatgpt());
        col = col
            .push(sub("Or an OpenAI API key (billed per token)"))
            .push(self.key_row(Provider::Openai, "Paste API key (sk-…)"));
        col = col.push(link("Create an OpenAI API key", OPENAI_KEYS));

        col = col.push(rule::horizontal(1)).push(heading("Anthropic Claude")).push(steps(&[
            "Open the Claude Console and sign in, or create an account. A Claude Pro or Max subscription \
             does not include API use; the API is billed separately, so add credits under Billing first.",
            "Open the API keys page and press “Create key”. Name it “Juttusihti”, choose a workspace \
             (keys scoped to a workspace work without extra settings) and an expiry date.",
            "Copy the key. It starts with “sk-ant-” and is shown only once.",
            "Paste it below and press Save.",
        ]));
        col = col.push(
            row![
                button(text("Open Claude Console")).style(button::secondary).on_press(Msg::Open(CLAUDE_CONSOLE)),
                button(text("Open API keys page")).style(button::secondary).on_press(Msg::Open(CLAUDE_KEYS)),
            ]
            .spacing(8),
        );
        col = col
            .push(text("Tip: you can also set a monthly spend limit for your organization in the Console.").size(12));
        col = col.push(self.key_row(Provider::Anthropic, "Paste API key (sk-ant-…)"));

        col = col.push(rule::horizontal(1)).push(heading("Google Gemini")).push(steps(&[
            "Open Google AI Studio and sign in with your Google account.",
            "Press “Create API key”. Pick an existing Google Cloud project or let AI Studio create one.",
            "Copy the key, paste it below and press Save.",
        ]));
        col = col.push(
            container(
                text(
                    "Privacy: on Gemini's free tier, Google may use what you send to improve its products, and \
                     human reviewers may read it. With billing set up for the key's project (paid tier), it \
                     does not. Set up billing in AI Studio before analyzing material you do not want reviewed.",
                )
                .size(12),
            )
            .padding(8)
            .style(container::warning),
        );
        col = col.push(
            row![
                button(text("Open Google AI Studio")).style(button::secondary).on_press(Msg::Open(GEMINI_KEYS)),
                button(text("Gemini API terms")).style(button::secondary).on_press(Msg::Open(GEMINI_TERMS)),
            ]
            .spacing(8),
        );
        col = col.push(self.key_row(Provider::Gemini, "Paste API key"));
        col = col.push(Space::new().height(24));
        scrollable(col).into()
    }
}

fn steps<'a>(items: &[&'static str]) -> Element<'a, Msg> {
    let mut col = column![].spacing(4);
    for (i, s) in items.iter().enumerate() {
        col = col.push(row![text(format!("{}.", i + 1)).size(13).width(20), text(*s).size(13)].spacing(4));
    }
    col.into()
}

fn link<'a>(label: &'static str, url: &'static str) -> Element<'a, Msg> {
    button(text(label).size(12)).style(button::text).padding(0).on_press(Msg::Open(url)).into()
}
