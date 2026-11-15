//! The Help screen: how to use each part of the app, search syntax and keyboard shortcuts.

use iced::widget::{column, container, row, scrollable, text};
use iced::{Element, Fill};

use crate::app::Message;
use crate::style::{self, SIZE_BODY, SPACE};

/// Keyboard shortcuts in the reader, as (keys, action).
const SHORTCUTS: [(&str, &str); 12] = [
    ("j / ↓", "Next article"),
    ("k / ↑", "Previous article"),
    ("o / Enter", "Open the article in the browser"),
    ("a", "Read the full article in the reader"),
    ("s", "Save or unsave the article"),
    ("m", "Mark the article read or unread"),
    ("n / p", "Next or previous page of the list"),
    ("/ or Ctrl+F", "Search"),
    ("c", "Show or hide the categories sidebar"),
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
const SECTIONS: [(&str, &[&str]); 12] = [
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
            "The ⋯ menu marks the whole list read, hides articles you have dismissed as irrelevant, switches between AI and original headlines and, for one feed, edits (rename, move to a folder, set its default category and category group) or unsubscribes it.",
            "Labels can be added to an article from its preview. The Labels screen renames, merges and deletes them. The article's Category row shows its category and lets you change it; see Categories below.",
            "When a feed only gives a teaser, \"Read full article\" (or press a) loads the whole article; see Full articles below.",
            "The label menu next to the search box shows only articles with that label (labels you added or accepted, not pending AI suggestions). It combines with the search, the feed menu and All / Unread; Clear removes both the search and the label.",
            "The categories sidebar between the rail and the list picks a group or a category: All articles, Uncategorized, your ★ Favourites, then the groups (click the arrow to fold one) and Ungrouped. Feeds you put in a group are listed under it after its categories, with a feed icon; click one to see that feed's articles. Uncategorized lists the articles in no category (an article that is in a group only through its feed is still uncategorized); its ✨ has AI find categories for the next 100 uncategorized articles whose feed gave no categories and has no default category (articles with unsorted publisher categories wait until you map them; feeds that only give ignored ones count as giving none). You see what is sent and an estimate first; the AI gives each article one main category, may create new ones, filed under one of your groups (Ungrouped when none fits; it never creates groups), and saves the result directly, without Review; articles it has checked are not sent again. Each press handles the next articles. The numbers count articles and follow All / Unread and the feed menu. \"Find category…\" finds a category, group or feed by name; Esc or ✕ shows the whole list again. Right-click a row, or press the star that appears on hover, to add it to or remove it from Favourites; drag rows to reorder them. « hides the sidebar and » (or c) shows it again; the pick stays applied. Organise…, at the top of the sidebar, opens the Categories screen. Clear removes the search but keeps the category.",
        ],
    ),
    (
        "Categories",
        &[
            "Categories are a short list for quickly filtering the news; they are separate from labels, which serve search and analytics. Groups gather categories (for example Politics and Sport under News). The Categories screen in the left rail (also opened by Organise…) lists them. + New group and + New category add them; drag a category onto a group, or use Move to…, to file it, and drag to reorder. Rename, merge and delete are on the same screen. Deleting a category returns its terms to Unsorted.",
            "Publisher categories are the section names a feed gives its articles. Juttusihti collects them as terms and files them under the category with the same name: at once when you have one, otherwise in a new category in Ungrouped once the name is on two articles, so \"Kotimaa\" from two feeds lands in one category and a one-off name (a player, a place) waits in Unsorted terms. The screen shows each term under its category. Move a term into another category with Move to… and it files that publisher's articles there; move it to Unsorted or Ignore it (under Ignored, so it is skipped) and it stays that way at later refreshes. Publisher categories are collected from now on; articles fetched earlier get them only if they are still in the feed at the next refresh.",
            "✨ Suggest categories asks your AI to sort your ungrouped categories into groups and to file the publisher terms still in Unsorted; a new category that fits no group goes to Ungrouped. Before sending, tick the ungrouped categories you want put into groups (All and None help with a long list); the others stay in Ungrouped. Before anything is sent you see what will be sent (the publisher terms with their feed titles, plus your group and category names; no article text) with an estimate, and you press the button to consent. The result is an editable draft: rename, move or drop items as you like. Nothing is saved until you press Approve. The ✨ suggestions keep your groups and file the categories that clearly belong there, and propose new groups for the other sections of a newspaper (Sport, Culture and so on), so no group takes most of the categories; the ✨ on Uncategorized files a new category under one of your groups and never creates groups. Terms and articles in any language go to the matching category.",
            "A feed can have a default category, for feeds that send no publisher categories but stay on one theme. Set it in the feed's edit form (Edit feed, in the feed menu or the ⋯ menu): it files the feed's articles whose publisher categories give no category, old articles too, and those articles are never sent to the ✨. Its publisher categories are not filed automatically while it has a default; clearing the default lets them be filed at the next refresh. Clearing the default brings back any category AI had given them. A feed can also sit in one group, so all of its articles show under that group whatever their categories. Pick the group in the feed's edit form, or type a name in \"or new group\" to create the group and file the feed in one step; on the Categories screen use Add feed… in a group, drag a feed row to another group or to Ungrouped, or use Remove from group. A category shows which feeds it is the default for (Default for: …).",
            "In the article view, the Category row shows the article's category and where it came from: from the feed, the default of its feed, from AI, or set by you. Pick another category to change it; the article then has exactly that one category. Choose Automatic to clear your choice and return to the feed's or the AI's.",
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
            "The result appears above the article as a score, such as \"Fact check: 2 of 8 supported · 1 contradicted · 5 unverified\". Click it to see each claim with its verdict, reason and sources and the guideline checks. Under the score is a reminder that the check was made with AI and is a helpful guide rather than a guarantee, so look at the sources before relying on it. \"Reduced check: web search was not available.\" means the model or plan could not search the web, so the verdicts rest on the article, Wikipedia and published fact checks only. A red ! in the article list marks articles with contradicted or misleading claims.",
            "AI setup → Fact check sets the limit per check ($1.00 by default; each claim sets aside the most it could cost before it starts, and if a check reaches the limit the remaining claims are marked Not checked, so raise the limit to check more; on a ChatGPT plan the plan's own usage limits apply instead) and the optional Google Fact Check Tools API key (free from Google Cloud; if Google rejects it, the result card says so), kept in the system's secret store. Web searches can run slightly over the limit with OpenAI and Gemini. Articles in any language work; explanations are in English. Only the latest check per article is kept. If credit or sign-in runs out, the check stops and an earlier result is kept.",
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
            "Everything AI produces starts as a suggestion. On Review you confirm or reject entities, relationships, relevance and summaries, one by one or in bulk. AI never overrides a choice you have made. The one exception is the ✨ on Uncategorized: it saves the categories it finds directly, and never changes a category you chose.",
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
            "All data is in one SQLite database on this computer; its path is shown at the bottom of Settings. Set JUTTUSIHTI_DATA_DIR to keep it elsewhere. Before an update changes the database, a copy of it is saved next to it (juttusihti-before-schema-N.db); an older version of Juttusihti can open that copy, but not the updated database.",
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
