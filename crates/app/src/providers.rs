//! AI setup screen: the defaults used by the quick AI panel (provider,
//! fast or quality model, limits), Sign in with ChatGPT, API keys in the OS
//! secret store, and how to get a key from each provider.

use iced::widget::{column, container, pick_list, row, rule, scrollable, text, text_input};
use iced::{Center, Element, Task as Cmd};
use juttusihti_core::ai_defaults::{AiDefaults, PayWith, Tier, recommended};
use juttusihti_core::pricing::{Micros, Provider, format_usd, usd_to_micros};

use crate::db::Db;
use crate::icons::Icon;
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};
use crate::{ai_setup, plan, secrets};

const CLAUDE_CONSOLE: &str = "https://platform.claude.com/";
const CLAUDE_KEYS: &str = "https://platform.claude.com/settings/keys";
const GEMINI_KEYS: &str = "https://aistudio.google.com/apikey";
const GEMINI_TERMS: &str = "https://ai.google.dev/gemini-api/terms";
const OPENAI_KEYS: &str = "https://platform.openai.com/api-keys";
const GOOGLE_FACTCHECK_API: &str = "https://console.cloud.google.com/apis/library/factchecktools.googleapis.com";
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
    SetPayWith(PayWith),
    SetTier(Tier),
    MaxArticles(String),
    MaxCost(String),
    SetClickbait(bool),
    ClickbaitThreshold(String),
    DefaultsSaved(Result<(), String>),
    FactLimitLoaded(Result<Micros, String>),
    FactLimit(String),
    FactLimitSaved(Result<(), String>),
    GoogleLoaded(Result<bool, String>),
    GoogleInput(String),
    SaveGoogle,
    RemoveGoogle,
    GoogleDone(Result<String, String>),
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
    inputs: [String; 3],
    message: Option<Result<String, String>>,
    chatgpt: ChatGpt,
    /// Fact check limit in USD, as typed.
    fact_limit: String,
    google: Option<Result<bool, String>>,
    google_input: String,
}

/// Dollars typed by the user (`$` and decimal comma allowed) as micro-USD;
/// `None` unless above zero.
fn parse_limit(input: &str) -> Option<Micros> {
    let t = input.trim().trim_start_matches('$').replace(',', ".");
    t.parse::<f64>().ok().and_then(|x| usd_to_micros(x).ok()).filter(|m| *m > 0)
}

async fn load_limit(db: &Db) -> Result<Micros, String> {
    db.call(|s| s.fact_check_limit()).await
}

async fn save_limit(db: &Db, m: Micros) -> Result<(), String> {
    db.call(move |s| s.set_fact_check_limit(m)).await
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
            fact_limit: String::new(),
            google: None,
            google_input: String::new(),
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        let limit_db = self.db.clone();
        Cmd::batch([
            Cmd::perform(async move { load_limit(&limit_db).await }, Msg::FactLimitLoaded),
            Cmd::perform(blocking(|| secrets::google_factcheck_key().map(|k| k.is_some())), Msg::GoogleLoaded),
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
                self.defaults = Some(d);
            }
            Msg::DefaultsLoaded(Err(e)) | Msg::DefaultsSaved(Err(e)) => self.message = Some(Err(e)),
            Msg::DefaultsSaved(Ok(())) => {}
            Msg::FactLimitLoaded(Ok(m)) => self.fact_limit = format!("{:.2}", m as f64 / 1_000_000.0),
            Msg::FactLimitLoaded(Err(e)) | Msg::FactLimitSaved(Err(e)) => self.message = Some(Err(e)),
            Msg::FactLimitSaved(Ok(())) => {}
            Msg::FactLimit(v) => {
                self.fact_limit = v;
                if let Some(m) = parse_limit(&self.fact_limit) {
                    let db = self.db.clone();
                    return Cmd::perform(async move { save_limit(&db, m).await }, Msg::FactLimitSaved);
                }
            }
            Msg::GoogleLoaded(r) => self.google = Some(r),
            Msg::GoogleInput(s) => self.google_input = s,
            Msg::SaveGoogle => {
                let key = std::mem::take(&mut self.google_input);
                return Cmd::perform(
                    blocking(move || {
                        secrets::set_google_factcheck_key(&key).map(|_| "Saved the Google Fact Check key.".to_string())
                    }),
                    Msg::GoogleDone,
                );
            }
            Msg::RemoveGoogle => {
                return Cmd::perform(
                    blocking(|| {
                        secrets::delete_google_factcheck_key().map(|_| "Removed the Google Fact Check key.".to_string())
                    }),
                    Msg::GoogleDone,
                );
            }
            Msg::GoogleDone(r) => {
                self.message = Some(r);
                return Cmd::perform(
                    blocking(|| secrets::google_factcheck_key().map(|k| k.is_some())),
                    Msg::GoogleLoaded,
                );
            }
            Msg::SetProvider(p) => {
                return self.change(|d| {
                    d.provider = p;
                    d.model = None;
                });
            }
            Msg::SetPayWith(p) => return self.change(|d| p.apply(&mut d.billing, &mut d.clickbait)),
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

    fn view_fact_check(&self) -> Element<'_, Msg> {
        let has_key = matches!(self.google, Some(Ok(true)));
        let state = match &self.google {
            Some(Ok(true)) => "Key stored".to_string(),
            Some(Ok(false)) => "No key".to_string(),
            Some(Err(e)) => e.clone(),
            None => "…".to_string(),
        };
        let mut col = column![
            style::title("Fact check"),
            style::note("Fact checks use your provider's quality model and its web search."),
            row![
                text("Limit per fact check (USD)").size(SIZE_BODY),
                small_input("1.00", &self.fact_limit, Msg::FactLimit),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        ]
        .spacing(SPACE[2]);
        if parse_limit(&self.fact_limit).is_none() && !self.fact_limit.is_empty() {
            col = col.push(style::note("Enter an amount above 0."));
        }
        col = col.push(style::note(FACT_LIMIT_NOTE));
        col.push(style::heading("Google Fact Check Tools API key (optional)"))
            .push(style::note(
                "Free from Google Cloud. Adds published fact checks (Faktabaari, AFP and others) and makes checks \
                 more accurate.",
            ))
            .push(link("Open Google Cloud", GOOGLE_FACTCHECK_API))
            .push(style::note(state))
            .push(
                row![
                    text_input("Paste API key", &self.google_input)
                        .style(style::text_input)
                        .secure(true)
                        .on_input(Msg::GoogleInput)
                        .on_submit(Msg::SaveGoogle)
                        .size(SIZE_BODY)
                        .padding(SPACE[1]),
                    style::quiet_button(
                        None,
                        "Save",
                        style::quiet,
                        (!self.google_input.trim().is_empty()).then_some(Msg::SaveGoogle)
                    ),
                    style::quiet_button(None, "Remove", style::quiet_danger, has_key.then_some(Msg::RemoveGoogle)),
                ]
                .spacing(SPACE[1])
                .align_y(Center),
            )
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
            let pay_with = Some(d.pay_with());
            col = col.push(
                row![
                    label("Pay with"),
                    column![
                        style::choice("My ChatGPT plan", PayWith::ChatGptPlan, pay_with, Msg::SetPayWith),
                        style::choice("API key", PayWith::ApiKey, pay_with, Msg::SetPayWith),
                        style::choice(
                            "ChatGPT plan, with API credit for the clickbait check",
                            PayWith::Split,
                            pay_with,
                            Msg::SetPayWith
                        ),
                    ]
                    .spacing(SPACE[0]),
                ]
                .spacing(SPACE[1]),
            );
            if d.pay_with() == PayWith::Split {
                col = col.push(style::note(
                    "Needs both the ChatGPT sign-in and an OpenAI API key with credit, set up below. If the credit \
                     runs out, headlines are rewritten on the plan without the check.",
                ));
            }
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
        // The Decisions API needs an OpenAI API key; other runs rewrite in one stage.
        if d.provider == Provider::Openai && d.pay_with() != PayWith::ChatGptPlan {
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
        col = col.push(rule::horizontal(1)).push(self.view_fact_check());
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

/// Under the fact check limit: what the limit does when a check reaches it.
const FACT_LIMIT_NOTE: &str = concat!(
    "Each claim sets aside the most it could cost before it starts. If a check reaches the limit, ",
    "the remaining claims are marked Not checked; raise the limit to check more. ",
    "On a ChatGPT plan the plan's own usage limits apply instead.",
);

#[cfg(test)]
mod tests {
    use super::*;
    use juttusihti_core::store::Store;

    #[test]
    fn limit_note_explains_what_happens_at_the_limit() {
        assert!(FACT_LIMIT_NOTE.starts_with(
            "Each claim sets aside the most it could cost before it starts. If a check reaches the limit, the remaining"
        ));
        assert!(
            FACT_LIMIT_NOTE.ends_with(
                "raise the limit to check more. On a ChatGPT plan the plan's own usage limits apply instead."
            )
        );
    }

    #[test]
    fn limit_input_parses_dollars() {
        assert_eq!(parse_limit("1.00"), Some(1_000_000));
        assert_eq!(parse_limit(" $0,50 "), Some(500_000));
        assert_eq!(parse_limit("0"), None);
        assert_eq!(parse_limit("abc"), None);
        assert_eq!(parse_limit(""), None);
        assert_eq!(parse_limit("-1"), None);
    }

    #[tokio::test]
    async fn saved_limit_updates_the_store() {
        let db = Db::new(Store::open_in_memory().unwrap());
        assert_eq!(load_limit(&db).await.unwrap(), 1_000_000);
        save_limit(&db, parse_limit("2.50").unwrap()).await.unwrap();
        assert_eq!(load_limit(&db).await.unwrap(), 2_500_000);
    }
}
