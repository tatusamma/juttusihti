//! "Find categories with AI", the ✨ on the sidebar's Uncategorized row: a
//! small consent panel in the sidebar, then one run for the next uncategorized
//! articles whose feed gave no publisher categories. What the AI finds is
//! saved directly (it may create new, ungrouped categories).

use chrono::Local;
use iced::widget::{column, row, text};
use iced::{Center, Element, Fill};
use juttusihti_ai::categorize_articles::{self, MAX_ARTICLES};
use juttusihti_ai::{Client, RetryPolicy};
use juttusihti_core::ai_defaults::AiDefaults;
use juttusihti_core::budget::Limits;
use juttusihti_core::categories::{ApplyStats, ArticleForCategorizing};
use juttusihti_core::pricing::{Billing, Catalog, Micros, Price, Provider, Usage};
use juttusihti_core::store::ListFilter;

use crate::category_sidebar::Msg;
use crate::db::Db;
use crate::organise_draft::{limit_line, over_limit, run_limits};
use crate::research::{cost_text, thousands};
use crate::style::{self, SIZE_SMALL, SPACE};
use crate::{ai_setup, research};

/// What a run would send and cost, worked out when the panel opens.
#[derive(Debug, Clone)]
pub struct Plan {
    pub provider: Provider,
    pub billing: Billing,
    pub model: String,
    /// Exactly the articles that are sent.
    pub articles: Vec<ArticleForCategorizing>,
    /// All category names, sent as the vocabulary.
    pub vocabulary: Vec<String>,
    /// Articles that could be sent, including these.
    pub eligible: usize,
    /// Worst-case tokens (input and output) and their cost.
    pub tokens: u64,
    pub cost: Option<Micros>,
    pub price: Option<Price>,
    pub limits: Limits,
    pub ready: Result<(), String>,
}

impl Plan {
    pub fn build(
        d: &AiDefaults,
        articles: Vec<ArticleForCategorizing>,
        eligible: usize,
        vocabulary: Vec<String>,
        price: Option<Price>,
        ready: Result<(), String>,
    ) -> Self {
        let (worst_in, worst_out) = categorize_articles::estimate_tokens(&articles, &vocabulary);
        let worst = Usage { input: worst_in, output: worst_out, ..Default::default() };
        Self {
            provider: d.provider,
            billing: d.billing(),
            model: d.model(),
            tokens: worst.total(),
            cost: price.as_ref().and_then(|p| p.cost(&worst, false)),
            price,
            articles,
            vocabulary,
            eligible,
            limits: run_limits(d),
            ready,
        }
    }

    /// Why Send is disabled, if it is.
    pub fn blocker(&self) -> Option<String> {
        if self.articles.is_empty() {
            return Some("No uncategorized articles without publisher categories.".into());
        }
        if let Err(e) = &self.ready {
            return Some(e.clone());
        }
        over_limit(&self.limits, self.tokens, self.cost, &self.model)
    }

    pub fn heading(&self) -> String {
        match self.articles.len() {
            1 => "Find categories for the next article".into(),
            n => format!("Find categories for the next {n} articles"),
        }
    }

    pub fn sends_line(&self) -> String {
        format!("Sends their headlines and descriptions and your category names to {}.", self.provider)
    }

    pub fn estimate_line(&self) -> String {
        format!("At most {} tokens, {}.", thousands(self.tokens), cost_text(Some(self.billing), self.cost))
    }

    pub fn limit_line(&self) -> String {
        limit_line(self.billing, &self.limits)
    }
}

/// The panel's state.
#[derive(Debug)]
pub enum Panel {
    Planning,
    Ready(Box<Plan>),
    /// Sent; waiting for the answer.
    Running(Box<Plan>),
    /// The summary of the finished run.
    Done(String),
    Failed(String),
}

/// Works out what a run for `filter` (All/Unread and the feed picker) would
/// send and cost. Sends nothing.
pub async fn plan(db: Db, catalog: Catalog, filter: ListFilter) -> Result<Box<Plan>, String> {
    let mut d = ai_setup::defaults(&db).await?;
    // The clickbait check plays no part here, so its key is not needed.
    d.clickbait.enabled = false;
    let ready = ai_setup::readiness(&d).await;
    let (articles, eligible, vocabulary) = db
        .call(move |s| -> juttusihti_core::Result<_> {
            let (articles, eligible) = s.articles_to_categorize(filter, MAX_ARTICLES)?;
            Ok((articles, eligible, s.categories()?.into_iter().map(|c| c.name).collect()))
        })
        .await?;
    let price = catalog.price_for(d.provider, &d.model(), d.billing(), Local::now().date_naive());
    Ok(Box::new(Plan::build(&d, articles, eligible, vocabulary, price, ready)))
}

/// Sends the consented request, saves what came back and returns the summary line.
pub async fn run(db: Db, plan: Plan) -> Result<String, String> {
    let endpoint = research::endpoint(plan.provider, plan.billing).await?;
    let client = Client::new(endpoint, RetryPolicy::default()).map_err(|e| e.to_string())?;
    let found = categorize_articles::categorize(
        &client,
        &plan.model,
        &plan.articles,
        &plan.vocabulary,
        plan.price.as_ref(),
        plan.limits,
        db.shared(),
    )
    .await
    .map_err(|e| e.to_string())?;
    let sent: Vec<_> = plan.articles.iter().map(|a| a.id).collect();
    let n = sent.len();
    // One category per article: the first name the AI gave.
    let found: Vec<_> = found.into_iter().map(|(id, names)| (id, names.into_iter().next())).collect();
    let stats = db.call(move |s| s.apply_article_categories(&found, &sent)).await?;
    Ok(summary(stats, n, plan.eligible.saturating_sub(n)))
}

/// "Categorized 82 of 100 articles; 3 new categories. 240 left."
pub fn summary(stats: ApplyStats, sent: usize, left: usize) -> String {
    let articles = if sent == 1 { "article" } else { "articles" };
    let mut line = format!("Categorized {} of {sent} {articles}", stats.categorized);
    match stats.new_categories {
        0 => {}
        1 => line.push_str("; 1 new category"),
        n => line.push_str(&format!("; {n} new categories")),
    }
    line.push('.');
    if left > 0 {
        line.push_str(&format!(" {left} left."));
    }
    line
}

/// The panel at the top of the sidebar: estimating, consent, progress, the
/// summary or the error.
pub fn view(panel: &Panel) -> Element<'_, Msg> {
    let small = |line: String| text(line).size(SIZE_SMALL).style(style::body);
    let close = |label| style::quiet_button(None, label, style::quiet, Some(Msg::CloseAi));
    let content: Element<'_, Msg> = match panel {
        Panel::Planning => style::note("Estimating…").into(),
        Panel::Running(_) => style::note("Finding categories…").into(),
        Panel::Done(line) => column![small(line.clone()), close("Close")].spacing(SPACE[1]).into(),
        Panel::Failed(e) => {
            column![text(e).size(SIZE_SMALL).style(style::danger), close("Close")].spacing(SPACE[1]).into()
        }
        Panel::Ready(p) => {
            let mut col = column![
                text(p.heading()).size(SIZE_SMALL).font(style::SEMIBOLD),
                small(p.sends_line()),
                small(p.estimate_line()),
                style::note(p.limit_line()),
            ]
            .spacing(SPACE[1]);
            let blocker = p.blocker();
            if let Some(why) = &blocker {
                col = col.push(text(why.clone()).size(SIZE_SMALL).style(style::danger));
            }
            col.push(
                row![style::main_button("Send", blocker.is_none().then_some(Msg::SendAi)), close("Cancel")]
                    .spacing(SPACE[1])
                    .align_y(Center),
            )
            .into()
        }
    };
    iced::widget::container(content).padding(SPACE[2]).width(Fill).style(style::card).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_counts_what_was_saved() {
        let s = |categorized, new_categories| ApplyStats { categorized, new_categories };
        assert_eq!(summary(s(82, 3), 100, 240), "Categorized 82 of 100 articles; 3 new categories. 240 left.");
        assert_eq!(summary(s(1, 1), 1, 0), "Categorized 1 of 1 article; 1 new category.");
        assert_eq!(summary(s(0, 0), 5, 0), "Categorized 0 of 5 articles.");
    }

    #[test]
    fn the_plan_is_bounded_and_mirrors_the_limits() {
        let d = AiDefaults { billing: Billing::ChatGptPlan, max_tokens: Some(10), ..Default::default() };
        let one = vec![ArticleForCategorizing { id: 1, title: "T".into(), text: "D".into() }];
        let p = Plan::build(&d, one, 7, vec![], Some(Price::chatgpt_plan(&d.model())), Ok(()));
        assert_eq!(p.heading(), "Find categories for the next article");
        assert!(p.blocker().unwrap().contains("more than the per-run limit"));
        let p = Plan { limits: Limits { max_tokens: None, ..p.limits }, ..p };
        assert_eq!(p.blocker(), None);
        let p = Plan { ready: Err("Sign in to ChatGPT first.".into()), ..p };
        assert_eq!(p.blocker().as_deref(), Some("Sign in to ChatGPT first."));
    }
}
