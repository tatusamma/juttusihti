//! AI setup screen: the defaults used by the quick AI panel (provider,
//! fast or quality model, limits), Sign in with ChatGPT, API keys in the OS
//! secret store, how to get a key from each provider, and a local or
//! self-hosted OpenAI-compatible server (address, model, optional key).

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
    SetClickbait(bool),
    ClickbaitThreshold(String),
    DefaultsSaved(Result<(), String>),
    LocalUrl(String),
    LocalModel(String),
    CheckLocal,
    LocalChecked(Result<Vec<String>, String>),
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
    /// Clickbait threshold in percent, as typed.
    threshold: String,
    status: Vec<(Provider, Result<bool, String>)>,
    inputs: [String; Provider::ALL.len()],
    message: Option<Result<String, String>>,
    chatgpt: ChatGpt,
    /// Local server address and model, as typed.
    local_url: String,
    local_model: String,
    /// Models the local server offered on the last check, or why it failed.
    local_check: Option<Result<Vec<String>, String>>,
    checking_local: bool,
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
            threshold: String::new(),
            status: Vec::new(),
            inputs: Default::default(),
            message: None,
            chatgpt: ChatGpt::Unknown,
            local_url: String::new(),
            local_model: String::new(),
            local_check: None,
            checking_local: false,
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
                self.threshold = d.clickbait.threshold_percent.to_string();
                self.local_url = d.local.base_url.clone();
                self.local_model = d.local.model.clone();
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
            Msg::SetClickbait(on) => return self.change(|d| d.clickbait.enabled = on),
            Msg::ClickbaitThreshold(v) => {
                self.threshold = v;
                if let Ok(n) = self.threshold.trim().trim_end_matches('%').trim().parse::<u8>()
                    && (1..=100).contains(&n)
                {
                    return self.change(|d| d.clickbait.threshold_percent = n);
                }
            }
            Msg::LocalUrl(v) => {
                self.local_url = v;
                self.local_check = None;
                let t = self.local_url.trim().to_string();
                if t.is_empty() || juttusihti_ai::normalize_local_base(&t).is_ok() {
                    return self.change(|d| d.local.base_url = t);
                }
            }
            Msg::LocalModel(v) => {
                self.local_model = v;
                let t = self.local_model.trim().to_string();
                if t.is_empty() || juttusihti_ai::valid_local_model(&t) {
                    return self.change(|d| {
                        d.local.model = t;
                        if d.provider == Provider::Local {
                            d.model = None;
                        }
                    });
                }
            }
            Msg::CheckLocal => {
                let local = juttusihti_core::ai_defaults::LocalServer {
                    base_url: self.local_url.trim().to_string(),
                    model: self.local_model.trim().to_string(),
                };
                self.checking_local = true;
                self.local_check = None;
                return Cmd::perform(
                    async move {
                        let endpoint = ai_setup::local_endpoint(&local).await?;
                        let client = juttusihti_ai::Client::new(endpoint, juttusihti_ai::RetryPolicy::default())
                            .map_err(|e| e.to_string())?;
                        client.list_models().await.map_err(|e| e.to_string())
                    },
                    Msg::LocalChecked,
                );
            }
            Msg::LocalChecked(r) => {
                self.checking_local = false;
                let first = r.as_ref().ok().and_then(|m| m.first().cloned());
                self.local_check = Some(r);
                // Fill in the model when the server has one and none is set.
                if self.local_model.trim().is_empty()
                    && let Some(m) = first
                {
                    return self.update(Msg::LocalModel(m));
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
        if d.provider == Provider::Local {
            let model = d.model();
            col = col.push(
                row![
                    label("Model"),
                    style::note(if model.is_empty() {
                        "Set the server and its model under “Local or self-hosted server” below.".to_string()
                    } else {
                        format!("{model} on your own server: no API cost.")
                    }),
                ]
                .spacing(SPACE[1])
                .align_y(Center),
            );
        }
        let mut tiers = column![].spacing(SPACE[0]);
        for t in Tier::ALL {
            if let Some(r) = recommended(d.provider, t) {
                tiers = tiers.push(style::choice(r.note, t, Some(d.tier), Msg::SetTier));
            }
        }
        if d.provider != Provider::Local {
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
        }
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
        // The Decisions API needs an OpenAI API key; other runs rewrite in one stage.
        if d.provider == Provider::Openai && d.billing() == Billing::ApiKey {
            let c = &d.clickbait;
            let mut check = row![
                label("Headlines"),
                style::check(c.enabled, "Check for clickbait first").on_toggle(Msg::SetClickbait),
                style::help(
                    "Before rewriting, a fast, cheap classifier (OpenAI Decisions API) rates how likely each \
                        headline is clickbait. Only headlines at or above the threshold are sent for a rewrite; \
                        the rest stay as they are. Ratings are remembered, so changing the threshold is free."
                ),
            ]
            .spacing(SPACE[1])
            .align_y(Center);
            if c.enabled {
                check = check.push(text("rewrite from").size(SIZE_BODY));
                check = check.push(small_input("70", &self.threshold, Msg::ClickbaitThreshold));
                check = check.push(text("% likely").size(SIZE_BODY));
            }
            col = col.push(check);
        }
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
        col = col.push(rule::horizontal(1)).push(self.view_local());
        scrollable(col).into()
    }

    fn view_local(&self) -> Element<'_, Msg> {
        let label = |s: &'static str| text(s).size(SIZE_BODY).width(120);
        let url_error = {
            let t = self.local_url.trim();
            (!t.is_empty()).then(|| juttusihti_ai::normalize_local_base(t).err()).flatten()
        };
        let model_error = {
            let t = self.local_model.trim();
            (!t.is_empty() && !juttusihti_ai::valid_local_model(t))
                .then_some("Model names cannot contain spaces or unusual characters.")
        };
        let mut col = column![
            style::title("Local or self-hosted server"),
            style::intro(
                "Run analyses on your own model with any OpenAI-compatible server. No API cost.",
                "Any server that speaks the OpenAI chat-completions API works: llama.cpp's llama-server, Ollama, \
                 vLLM or LM Studio, on this computer or another one on your network. Runs cost nothing in API \
                 fees; the estimate still shows tokens, and you still approve every run before articles are sent. \
                 If the server cannot force JSON-schema output, Juttusihti asks for JSON in the prompt and checks \
                 every reply.",
            ),
            row![
                label("Server address"),
                text_input("http://192.168.1.20:8080/v1", &self.local_url)
                    .style(style::text_input)
                    .on_input(Msg::LocalUrl)
                    .size(SIZE_BODY)
                    .padding(SPACE[1]),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        ]
        .spacing(SPACE[2]);
        let can_check = !self.checking_local && !self.local_url.trim().is_empty() && url_error.is_none();
        if let Some(e) = url_error {
            col = col.push(style::note(e));
        }
        let offered = match &self.local_check {
            Some(Ok(models)) if !models.is_empty() => Some(models.clone()),
            _ => None,
        };
        let mut model_row = row![
            label("Model"),
            text_input("e.g. qwen3:8b or Qwen/Qwen3-8B", &self.local_model)
                .style(style::text_input)
                .on_input(Msg::LocalModel)
                .size(SIZE_BODY)
                .padding(SPACE[1]),
        ]
        .spacing(SPACE[1])
        .align_y(Center);
        if let Some(models) = offered {
            let selected = models.iter().find(|m| **m == self.local_model.trim()).cloned();
            model_row = model_row.push(
                pick_list(models, selected, Msg::LocalModel)
                    .style(style::pick_list)
                    .menu_style(style::menu)
                    .placeholder("Server's models")
                    .text_size(SIZE_BODY),
            );
        }
        col = col.push(model_row);
        if let Some(e) = model_error {
            col = col.push(style::note(e));
        }
        col = col.push(
            row![
                style::quiet_button(None, "Check server", style::quiet, can_check.then_some(Msg::CheckLocal)),
                style::note(match &self.local_check {
                    _ if self.checking_local => "Asking the server for its models…".to_string(),
                    None => "Asks the server which models it has. No articles are sent.".to_string(),
                    Some(Ok(m)) if m.is_empty() => "The server answered but listed no models.".to_string(),
                    Some(Ok(m)) =>
                        format!("The server answered with {} model{}.", m.len(), if m.len() == 1 { "" } else { "s" }),
                    Some(Err(e)) => format!("Could not reach the server: {e}"),
                }),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        );
        col = col.push(style::heading("API key (optional)"));
        col = col.push(style::note(
            "Only if the server was started with a key (llama-server --api-key, vLLM --api-key, a proxy). \
             Leave empty for Ollama and LM Studio.",
        ));
        col = col.push(self.key_row(Provider::Local, "Paste the server's API key"));
        col = col.push(
            container(
                text(
                    "Typical addresses — llama-server: http://HOST:8080/v1 · Ollama: http://HOST:11434/v1 · \
                     vLLM: http://HOST:8000/v1 · LM Studio: http://HOST:1234/v1. Articles go to this address \
                     when you approve a run; use https or a trusted network for a server on another computer.",
                )
                .size(SIZE_SMALL)
                .style(style::body),
            )
            .padding(SPACE[2])
            .style(style::card),
        );
        col.into()
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
