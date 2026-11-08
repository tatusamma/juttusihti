//! The Help screen: how to use each part of the app, search syntax and keyboard shortcuts.

use iced::widget::{column, container, row, scrollable, text};
use iced::{Element, Fill};

use crate::app::Message;
use crate::style::{self, SIZE_BODY, SPACE};

/// Keyboard shortcuts in the reader, as (keys, action).
const SHORTCUTS: [(&str, &str); 11] = [
    ("j / ↓", "Next article"),
    ("k / ↑", "Previous article"),
    ("o / Enter", "Open the article in the browser"),
    ("a", "Read the full article in the reader"),
    ("s", "Save or unsave the article"),
    ("m", "Mark the article read or unread"),
    ("n / p", "Next or previous page of the list"),
    ("/ or Ctrl+F", "Search"),
    ("Ctrl+R", "Refresh all feeds"),
    ("Esc", "Close menus and panels"),
    ("F1", "This help"),
];

/// Search syntax, as (example, meaning).
const SEARCH: [(&str, &str); 7] = [
    ("tram strike", "Both words (AND is implicit)"),
    ("\"city council\"", "The exact phrase"),
    ("tram OR bus", "Either word"),
    ("-sports, NOT sports", "Leave out articles with the word"),
    ("työttömyysturva*", "Prefix: also matches inflected forms such as työttömyysturvaan"),
    ("title:sää", "Only in the title (also author:, summary:, content:)"),
    ("NEAR(rail budget, 10)", "Words within 10 words of each other; ( ) groups terms"),
];

/// Sections of the guide, as (heading, paragraphs).
const SECTIONS: [(&str, &[&str]); 11] = [
    (
        "Getting started",
        &[
            "Open News in the left rail, then the feed menu at the top of the article list. Paste a site address or a feed URL into \"Add a site or feed\" and press Add: Juttusihti finds the site's feeds for you.",
            "Already use another reader? Export OPML there and use Import OPML in the feed menu or in Settings.",
            "Feeds refresh on a schedule (Settings → Feeds) and with the refresh button in the top bar. Everything you fetch stays on this computer and can be read offline.",
        ],
    ),
    (
        "Reading",
        &[
            "The feed menu chooses what the list shows: all news, a folder or one feed. All / Unread filters the list. Click an article to read it on the right; the star saves it to Saved in the rail.",
            "The ⋯ menu marks the whole list read, hides articles you have dismissed as irrelevant, switches between AI and original headlines and, for one feed, edits (rename, move to a folder) or unsubscribes it.",
            "Labels can be added to an article from its preview. The Labels screen renames, merges and deletes them.",
            "When a feed only gives a teaser, \"Read full article\" (or press a) loads the whole article; see Full articles below.",
            "The label menu next to the search box shows only articles with that label (labels you added or accepted, not pending AI suggestions). It combines with the search, the feed menu and All / Unread; Clear removes both the search and the label.",
        ],
    ),
    (
        "Full articles",
        &[
            "Many feeds give only a headline and a teaser. Open the article and press \"Read full article\" next to \"Open in browser\", or press a. The button shows \"Fetching…\" for about 5 to 10 seconds, then the whole text replaces the teaser and the button says \"Fetched\". The main picture is shown faded behind the headline. A fetched article opens instantly and offline after that.",
            "Nothing is fetched until you press the button. A hidden browser starts with a brand-new, empty profile, so the site sees a stranger with no earlier cookies or logins. Known trackers and ads are blocked (EasyList and EasyPrivacy). Cookie banners are accepted for you, including the free choice of \"consent or pay\" walls, so you never have to click one. When the text and picture are saved, the whole profile, cookies included, is deleted, so the next visit cannot be linked to this one.",
            "Subscriber-only articles keep only the part the site shows everyone, with the note \"The rest of this article is for subscribers.\" Juttusihti never gets around a paywall.",
            "If it does not work, the reader says why: \"Couldn't find the article text on the page.\" (also what a site that refuses the visit looks like) or \"The site took too long to answer.\" The teaser and \"Open in browser\" stay as they were.",
            "Fetched text is searchable and is used by Custom analysis when you choose \"Archived article text\". It is never sent to an AI provider unless you include the article in an AI run.",
            "Settings → Full articles: \"Download pictures\" (turn it off for text only; the picture is then never requested), the cache size limit (500 MB by default; the oldest fetched articles fall back to their teaser first, and Saved articles are never removed to make room) and \"Clear cache now\".",
        ],
    ),
    (
        "AI for a list (✨)",
        &[
            "Nothing is sent to an AI provider until you ask. Connect a provider first on AI setup: sign in with ChatGPT, or add an OpenAI, Anthropic (Claude) or Gemini API key. Keys are kept in the system's secret store.",
            "The ✨ button above the article list works on what the list shows. Pick tasks (clickbait headlines, labels, related stories, people and places, summary), check the local estimate of requests, tokens and cost, then press \"Send N articles\". That button is your consent for that run; the Setup button opens AI setup.",
            "Results arrive while the run goes on. Rewritten headlines show an AI tag with the original underneath. Runs can be paused, resumed and cancelled, and the spending limits from AI setup are never exceeded.",
            "With an OpenAI API key, clickbait headlines are checked first: a fast, cheap classifier (OpenAI's Decisions API) rates how likely each headline is clickbait, and only those at or above the threshold (70% by default) are sent for a rewrite. Ratings are remembered, so a new threshold costs nothing. Turn the check off or change the threshold on AI setup. The check needs API credit, so on a ChatGPT plan you can choose: \"Pay with\" on AI setup offers the plan alone (headlines are rewritten in one step without the check), or the plan with API credit for the check only, which needs both the sign-in and a key. With Claude or Gemini, headlines are rewritten in one step without the check.",
            "An API key is billed to the provider account behind it, which needs credit; a ChatGPT plan is not. If the account runs out of credit, the run pauses and says so. Add credit on the provider's billing page, then press Resume under Custom analysis: finished work is kept and never paid for twice.",
        ],
    ),
    (
        "Fact check",
        &[
            "Press \"Fact check\" in the reader, after \"Read full article\", to have an AI check the factual claims of an article. It needs the full text: until the article is fetched (or the feed gave at least 300 characters of full content) the button is disabled, and its tooltip says \"Load the full article first\". Once a result exists the button says \"Check again\".",
            "The button only opens a panel. It shows the provider and its Quality model, what will be sent where, an estimate and the limit for one check. Nothing is sent until you press \"Check with <provider>\". The article goes to your AI provider; the claims go to Google Fact Check only if you added a key, and searches go to Wikipedia.",
            "The AI picks up to 8 checkable factual claims (opinions are skipped), searches the web with the provider's own search (up to 3 searches per claim), looks up Wikipedia and, with a key, published fact checks. Each claim gets a verdict: Supported, Contradicted, Misleading, Unverified or Not checked (limit reached). Sources from the article's own site or copies of the same story do not count, Wikipedia is only background, and a verdict needs a real source found during the check, otherwise it becomes Unverified. Published fact checks show the publisher and the rating as published.",
            "It also checks the article against the Finnish journalists' guidelines (Journalistin ohjeet, https://jsn.fi/journalistin-ohjeet/): JSN 15 (the headline is backed by the text), 12 (source criticism), 21 (right to reply) and 16 (hidden advertising).",
            "The result appears above the article as a score, such as \"Fact check: 2 of 8 supported · 1 contradicted · 5 unverified\". Click it to see each claim with its verdict, reason and sources, the guideline checks and the note \"AI-assisted check. Verify the sources before relying on it.\" \"Reduced check: web search was not available.\" means the model or plan could not search the web, so the verdicts rest on the article, Wikipedia and published fact checks only. A red ! in the article list marks articles with contradicted or misleading claims.",
            "AI setup → Fact check sets the limit per check ($1.00 by default) and the optional Google Fact Check Tools API key (free from Google Cloud), kept in the system's secret store. Web searches can run slightly over the limit with OpenAI and Gemini. Articles in any language work; explanations are in English. Only the latest check per article is kept. If credit or sign-in runs out, the check stops and an earlier result is kept.",
        ],
    ),
    (
        "Custom analysis",
        &[
            "For a larger study, Custom analysis sets the date range, feeds, folders, labels or a search, the model, content depth and limits. The preview is computed locally; the analysis is saved and waits for your approval before anything is sent.",
            "Recent runs are listed here, including ✨ runs. A paused or stopped run shows why it stopped and has a Resume button.",
        ],
    ),
    (
        "Review",
        &[
            "Everything AI produces starts as a suggestion. On Review you confirm or reject entities, relationships, relevance and summaries, one by one or in bulk. AI never overrides a choice you have made.",
        ],
    ),
    (
        "Reports",
        &[
            "Charts of what you read: volume per source, topics, labels, entities that appear together, relevance and clickbait trends, and AI usage and cost. Choose 7, 30, 90 or 365 days and day, week or month buckets, and export any report as CSV.",
        ],
    ),
    (
        "Timeline, map and relationship map",
        &[
            "Timeline shows articles per source, story threads and dated events. Drag to pan and scroll to zoom.",
            "Map shows places from articles. Map tiles stay off until you turn them on, because the viewed area is then sent to the tile server; without them a coordinate grid is shown. Places named by AI have no coordinates: check the evidence and use \"Place on map\".",
            "Relationship map draws people, organisations and places and how they connect: solid lines are confirmed, dashed lines are AI suggestions.",
        ],
    ),
    (
        "Settings",
        &[
            "How long to keep articles (saved and researched articles are always kept), the refresh interval, dark or light theme, the CPU renderer for lower memory use, full-article pictures and cache size, and the map tile server and its cache.",
        ],
    ),
    (
        "Your data",
        &[
            "All data is in one SQLite database on this computer; its path is shown at the bottom of Settings. Set JUTTUSIHTI_DATA_DIR to keep it elsewhere.",
        ],
    ),
];

/// A two-column table of (left, right) rows; `left` is drawn semibold.
fn table<'a>(rows: &[(&'a str, &'a str)]) -> Element<'a, Message> {
    column(rows.iter().map(|&(l, r)| {
        row![text(l).size(SIZE_BODY).font(style::SEMIBOLD).width(220), text(r).size(SIZE_BODY).width(Fill)]
            .spacing(SPACE[2])
            .into()
    }))
    .spacing(SPACE[1])
    .into()
}

pub fn view<'a>() -> Element<'a, Message> {
    let mut col = column![
        style::title("Help"),
        style::note("Juttusihti is a news reader that keeps everything on this computer, with optional AI research."),
    ]
    .spacing(SPACE[4])
    .width(760);
    for (heading, paragraphs) in SECTIONS {
        let section = column![style::heading(heading)]
            .extend(paragraphs.iter().map(|p| text(*p).size(SIZE_BODY).style(style::body).into()))
            .spacing(SPACE[1]);
        col = col.push(section);
    }
    col = col
        .push(column![style::heading("Searching"), table(&SEARCH)].spacing(SPACE[1]))
        .push(column![style::heading("Keyboard shortcuts"), table(&SHORTCUTS)].spacing(SPACE[1]));
    scrollable(container(col).padding(SPACE[4])).width(Fill).into()
}
