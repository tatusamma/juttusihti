//! AI setup screen: the defaults used by the quick AI panel (provider,
//! fast or quality model, limits), Sign in with ChatGPT, API keys in the OS
//! secret store, and how to get a key from each provider.

use iced::widget::{column, container, pick_list, row, rule, scrollable, text, text_input};
use iced::{Center, Element, Task as Cmd};
use juttusihti_core::ai_defaults::{AiDefaults, Tier, recommended};
use juttusihti_core::pricing::{Billing, Provider, format_usd, usd_to_micros};

use crate::db::Db;
use crate::icons::Icon;
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};
use crate::{ai_setup, plan, secrets};

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
    DefaultsLoaded(Result<AiDefaults, String>),
    SetProvider(Provider),
    SetBilling(Billing),
    SetTier(Tier),
    MaxArticles(String),
    MaxCost(String),
    DefaultsSaved(Result<(), String>),
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
    db: Db,
    defaults: Option<AiDefaults>,
    max_articles: String,
    max_cost: String,
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
    pub fn new(db: Db) -> Self {
        Self {
            db,
            defaults: None,
            max_articles: String::new(),
            max_cost: String::new(),
            status: Vec::new(),
            inputs: Default::default(),
            message: None,
            chatgpt: ChatGpt::Unknown,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::batch([
            Cmd::perform(async move { ai_setup::defaults(&db).await }, Msg::DefaultsLoaded),
            Cmd::perform(blocking(secrets::status), Msg::Loaded),
            Cmd::perform(plan::status(), Msg::ChatGptStatus),
        ])
    }

    /// Changes the defaults and saves them.
    fn change(&mut self, f: impl FnOnce(&mut AiDefaults)) -> Cmd<Msg> {
        let Some(d) = self.defaults.as_mut() else { return Cmd::none() };
        f(d);
        let (db, d) = (self.db.clone(), d.clone());
        Cmd::perform(async move { ai_setup::save(&db, d).await }, Msg::DefaultsSaved)
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::DefaultsLoaded(Ok(d)) => {
                self.max_articles = d.max_articles.to_string();
                self.max_cost = d.max_cost.map(format_usd).unwrap_or_default().trim_start_matches('$').to_string();
                self.defaults = Some(d);
            }
            Msg::DefaultsLoaded(Err(e)) | Msg::DefaultsSaved(Err(e)) => self.message = Some(Err(e)),
            Msg::DefaultsSaved(Ok(())) => {}
            Msg::SetProvider(p) => {
                return self.change(|d| {
                    d.provider = p;
                    d.model = None;
                });
            }
            Msg::SetBilling(b) => return self.change(|d| d.billing = b),
            Msg::SetTier(t) => {
                return self.change(|d| {
                    d.tier = t;
                    d.model = None;
                });
            }
            Msg::MaxArticles(v) => {
                self.max_articles = v;
                if let Ok(n) = self.max_articles.trim().parse::<u32>()
                    && (1..=1_000).contains(&n)
                {
                    return self.change(|d| d.max_articles = n);
                }
            }
            Msg::MaxCost(v) => {
                self.max_cost = v;
                let t = self.max_cost.trim().trim_start_matches('$').replace(',', ".");
                let parsed = if t.is_empty() {
                    Some(None)
                } else {
                    t.parse::<f64>().ok().and_then(|x| usd_to_micros(x).ok()).map(Some)
                };
                if let Some(c) = parsed {
                    return self.change(|d| d.max_cost = c);
                }
            }
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
            style::note(state),
            row![
                text_input(placeholder, &self.inputs[idx(p)])
                    .style(style::text_input)
                    .secure(true)
                    .on_input(move |s| Msg::Input(p, s))
                    .on_submit(Msg::Save(p))
                    .size(SIZE_BODY)
                    .padding(SPACE[1]),
                style::quiet_button(
                    None,
                    "Save",
                    style::quiet,
                    (!self.inputs[idx(p)].trim().is_empty()).then_some(Msg::Save(p))
                ),
                style::quiet_button(None, "Remove", style::quiet_danger, has_key.then_some(Msg::Remove(p))),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        ]
        .spacing(SPACE[1])
        .into()
    }

    fn view_defaults(&self) -> Element<'_, Msg> {
        let Some(d) = &self.defaults else { return style::note("…").into() };
        let label = |s: &'static str| text(s).size(SIZE_BODY).width(120);
        let mut col = column![
            style::heading("AI defaults"),
            style::note("Used by the ✨ AI button above every article list. Set once; change any time."),
            row![
                label("Provider"),
                pick_list(&Provider::ALL[..], Some(d.provider), Msg::SetProvider)
                    .style(style::pick_list)
                    .menu_style(style::menu)
                    .text_size(SIZE_BODY),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        ]
        .spacing(SPACE[1]);
        if d.provider == Provider::Openai {
            col = col.push(
                row![
                    label("Pay with"),
                    style::choice("My ChatGPT plan", Billing::ChatGptPlan, Some(d.billing), Msg::SetBilling),
                    style::choice("API key", Billing::ApiKey, Some(d.billing), Msg::SetBilling),
                ]
                .spacing(SPACE[3])
                .align_y(Center),
            );
        }
        let mut tiers = column![].spacing(SPACE[0]);
        for t in Tier::ALL {
            tiers = tiers.push(style::choice(recommended(d.provider, t).note, t, Some(d.tier), Msg::SetTier));
        }
        col = col.push(
            row![
                row![
                    text("Model").size(SIZE_BODY),
                    style::help(
                        "Fast models handle headlines, labels, people & places and related stories well and use \
                         the fewest tokens. Pick Quality when you mostly want research summaries."
                    ),
                ]
                .spacing(SPACE[0])
                .width(120),
                tiers
            ]
            .spacing(SPACE[1]),
        );
        if d.model.is_some() {
            col = col.push(style::note(format!("Using model {}", d.model())));
        }
        col = col.push(
            row![
                label("Per quick run"),
                small_input("50", &self.max_articles, Msg::MaxArticles),
                text("newest articles, at most").size(SIZE_BODY),
                small_input("0.25", &self.max_cost, Msg::MaxCost),
                text("USD").size(SIZE_BODY),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        );
        col.into()
    }

    fn view_chatgpt(&self) -> Element<'_, Msg> {
        let mut col = column![style::intro(
            "With a ChatGPT Plus or Pro plan you can sign in instead of using an API key.",
            "With a ChatGPT Plus or Pro plan you can sign in instead of using an API key. Analysis runs then \
             count against your plan's usage, not API credit. You can set a weekly limit for Juttusihti in \
             ChatGPT's settings.",
        )]
        .spacing(SPACE[1]);
        col = col.push(match &self.chatgpt {
            ChatGpt::Unknown => Element::from(style::note("…")),
            ChatGpt::SignedOut => {
                row![style::main_button("Sign in with ChatGPT", Some(Msg::SignIn)), style::note("Not signed in"),]
                    .spacing(SPACE[2])
                    .align_y(Center)
                    .into()
            }
            ChatGpt::Starting => style::note("Starting sign-in…").into(),
            ChatGpt::Waiting { url, .. } => column![
                text("Finish signing in in your browser. Juttusihti is waiting for it to come back.")
                    .size(SIZE_SMALL)
                    .style(style::body),
                row![
                    style::quiet_button(None, "Copy sign-in link", style::quiet, Some(Msg::CopyLink)),
                    style::quiet_button(None, "Cancel", style::quiet, Some(Msg::CancelSignIn)),
                ]
                .spacing(SPACE[1]),
                style::note(format!(
                    "If no browser opened, paste the link into one: {}…",
                    url.chars().take(60).collect::<String>()
                )),
            ]
            .spacing(SPACE[1])
            .into(),
            ChatGpt::SignedIn(email) => row![
                text(format!("Signed in as {email}")).size(SIZE_SMALL).style(style::body),
                style::quiet_button(None, "Sign out", style::quiet, Some(Msg::SignOut)),
            ]
            .spacing(SPACE[2])
            .align_y(Center)
            .into(),
        });
        col.push(link("How ChatGPT plan usage in other apps works", CHATGPT_HELP)).into()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let mut col = column![
            column![
                style::title("AI setup"),
                style::intro(
                    "Keys stay in your system's secret store; nothing is sent until you start a run.",
                    "Credentials are kept in your operating system's secret store, not in Juttusihti's database. \
                     Nothing is sent to a provider until you start an analysis and agree to send its articles.",
                ),
            ]
            .spacing(SPACE[1]),
        ]
        .spacing(SPACE[4])
        .padding(SPACE[4])
        .max_width(760);
        if let Some(m) = &self.message {
            col = col.push(style::message(m));
        }

        col = col.push(self.view_defaults());
        col = col.push(rule::horizontal(1)).push(
            column![
                style::title("OpenAI"),
                style::heading("Sign in with ChatGPT"),
                self.view_chatgpt(),
                style::heading("Or an OpenAI API key (billed per token)"),
                self.key_row(Provider::Openai, "Paste API key (sk-…)"),
                link("Create an OpenAI API key", OPENAI_KEYS),
            ]
            .spacing(SPACE[2]),
        );

        col = col.push(rule::horizontal(1)).push(
            column![
                style::title("Anthropic Claude"),
                steps(&[
                    "Open the Claude Console and sign in, or create an account. A Claude Pro or Max subscription \
                     does not include API use; the API is billed separately, so add credits under Billing first.",
                    "Open the API keys page and press “Create key”. Name it “Juttusihti”, choose a workspace \
                     (keys scoped to a workspace work without extra settings) and an expiry date.",
                    "Copy the key. It starts with “sk-ant-” and is shown only once.",
                    "Paste it below and press Save.",
                ]),
                row![link("Open Claude Console", CLAUDE_CONSOLE), link("Open API keys page", CLAUDE_KEYS)]
                    .spacing(SPACE[1]),
                style::note("Tip: you can also set a monthly spend limit for your organization in the Console."),
                self.key_row(Provider::Anthropic, "Paste API key (sk-ant-…)"),
            ]
            .spacing(SPACE[2]),
        );

        col = col.push(rule::horizontal(1)).push(
            column![
                style::title("Google Gemini"),
                steps(&[
                    "Open Google AI Studio and sign in with your Google account.",
                    "Press “Create API key”. Pick an existing Google Cloud project or let AI Studio create one.",
                    "Copy the key, paste it below and press Save.",
                ]),
                container(
                    text(
                        "Privacy: on Gemini's free tier, Google may use what you send to improve its products, and \
                         human reviewers may read it. With billing set up for the key's project (paid tier), it \
                         does not. Set up billing in AI Studio before analyzing material you do not want reviewed.",
                    )
                    .size(SIZE_SMALL)
                    .style(style::body),
                )
                .padding(SPACE[2])
                .style(style::card),
                row![link("Open Google AI Studio", GEMINI_KEYS), link("Gemini API terms", GEMINI_TERMS)]
                    .spacing(SPACE[1]),
                self.key_row(Provider::Gemini, "Paste API key"),
            ]
            .spacing(SPACE[2]),
        );
        scrollable(col).into()
    }
}

fn steps<'a>(items: &[&'static str]) -> Element<'a, Msg> {
    let mut col = column![].spacing(SPACE[0]);
    for (i, s) in items.iter().enumerate() {
        col = col.push(
            row![
                text(format!("{}.", i + 1)).size(SIZE_SMALL).style(style::muted).width(20),
                text(*s).size(SIZE_SMALL).style(style::body)
            ]
            .spacing(SPACE[0]),
        );
    }
    col.into()
}

/// A quiet button that opens `url` in the browser.
fn link<'a>(label: &'static str, url: &'static str) -> Element<'a, Msg> {
    style::quiet_button(Some(Icon::External), label, style::quiet, Some(Msg::Open(url)))
}

/// A short number field.
fn small_input<'a>(placeholder: &'a str, value: &'a str, on: fn(String) -> Msg) -> Element<'a, Msg> {
    text_input(placeholder, value)
        .style(style::text_input)
        .on_input(on)
        .size(SIZE_BODY)
        .padding(SPACE[1])
        .width(70)
        .into()
}
