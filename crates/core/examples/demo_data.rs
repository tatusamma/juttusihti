//! Writes a database of made-up news for screenshots and demos.
//!
//! Every feed, town, person and story here is invented; the addresses are on
//! the reserved `.example` domain. Run:
//!
//! ```sh
//! cargo run -p juttusihti-core --example demo_data -- <empty data dir>
//! JUTTUSIHTI_DATA_DIR=<that dir> cargo run --release -p juttusihti
//! ```

use std::collections::HashMap;
use std::path::PathBuf;

use juttusihti_core::categories::Favourite;
use juttusihti_core::store::Store;
use juttusihti_core::{NewArticle, NewFeed, NewTerm, Origin, now};

/// A feed: title, host, folder, and its stories.
struct Feed {
    title: &'static str,
    host: &'static str,
    folder: &'static str,
    stories: &'static [Story],
}

/// A story: headline, publisher categories, hours ago, read, and its text.
struct Story(&'static str, &'static [&'static str], i64, bool, &'static str);

const FEEDS: &[Feed] = &[
    Feed {
        title: "Harbour Herald",
        host: "harbourherald.example",
        folder: "Daily",
        stories: &[
            Story(
                "Council approves the new tram line to Saltmarsh",
                &["Politics", "Transport"],
                1,
                false,
                "<p>After a four-hour meeting, Port Linden's council voted 31 to 20 to build a tram line from the \
                 central market to the Saltmarsh district. Work is due to start next spring and to take three years.</p>\
                 <p>The line will have eleven stops and replace two bus routes. Councillor Mira Hollis, who led the \
                 planning committee, said the decision ends a debate that began more than a decade ago. \"People in \
                 Saltmarsh have waited long enough for a fast way into town,\" she said.</p>\
                 <p>Opponents argued the cost, estimated at 214 million, is too high for a city of this size, and \
                 asked for an independent review of the passenger forecasts. The council promised to publish the \
                 forecasts in full before the first contracts are signed.</p>\
                 <p>Shop owners along Quay Street will be offered rent support while the street is closed to cars.                  The harbour authority, which owns the land at the Saltmarsh end, has agreed to give up a strip                  of the old rail yard for the final stop and a small depot.</p>                 <p>The first trams are expected to carry passengers in the spring of 2030.</p>",
            ),
            Story(
                "Old ferry terminal to become a public library",
                &["Culture", "City"],
                3,
                false,
                "<p>The empty ferry terminal on the east pier will reopen as a library and reading hall in two \
                 years. The design keeps the long waiting hall and its glass roof.</p>",
            ),
            Story(
                "Harbour ice came three weeks late this winter",
                &["Weather"],
                6,
                true,
                "<p>The harbour master's log shows the latest freeze since records began in 1911. Fishing boats \
                 kept working into the new year.</p>",
            ),
            Story(
                "School meals budget rises for the first time in six years",
                &["Politics", "Education"],
                9,
                false,
                "<p>The education board added 4 percent to the school meals budget. Kitchens say the money goes \
                 to vegetables and to repairing old ovens.</p>",
            ),
            Story(
                "Night buses return to the northern suburbs",
                &["Transport"],
                20,
                true,
                "<p>Two night routes cut in the last timetable are back from Friday, after a petition signed by \
                 6,000 residents.</p>",
            ),
            Story(
                "Linden Theatre sells out its winter season",
                &["Culture"],
                27,
                false,
                "<p>Every performance of the winter programme is sold out, a first for the theatre. Extra \
                 matinees are being planned.</p>",
            ),
        ],
    },
    Feed {
        title: "The Northern Ledger",
        host: "northernledger.example",
        folder: "Daily",
        stories: &[
            Story(
                "Shipyard wins order for two hybrid ferries",
                &["Economy", "Industry"],
                2,
                false,
                "<p>Grey Bay Shipyard signed a contract for two hybrid ferries, securing work for about 400 \
                 people until the end of the decade.</p>",
            ),
            Story(
                "Rents in Port Linden rose more slowly last year",
                &["Economy", "Housing"],
                5,
                false,
                "<p>Average rents rose 1.8 percent, the smallest increase in five years, according to the \
                 regional statistics office.</p>",
            ),
            Story(
                "Small bakeries ask for relief on energy bills",
                &["Economy"],
                11,
                true,
                "<p>An association of 60 bakeries says oven costs have doubled in three winters and asks the \
                 region for a temporary discount.</p>",
            ),
            Story(
                "Regional bank opens branch in a mobile bus",
                &["Economy", "Services"],
                26,
                false,
                "<p>The bus will visit eight villages each week, after the last village branch closed in \
                 autumn.</p>",
            ),
            Story(
                "Wind farm off Grey Bay reaches full power",
                &["Energy", "Industry"],
                31,
                false,
                "<p>All 24 turbines are now connected. The farm is expected to cover the yearly use of 90,000 \
                 homes.</p>",
            ),
        ],
    },
    Feed {
        title: "Pitchside",
        host: "pitchside.example",
        folder: "Weekend",
        stories: &[
            Story(
                "Linden United hold the league leaders to a draw",
                &[],
                2,
                false,
                "<p>A late equaliser from Tove Brandt gave United a 2-2 draw at home in front of 8,400 \
                 spectators.</p>",
            ),
            Story(
                "Brandt signs a three-year extension",
                &[],
                8,
                false,
                "<p>The club's top scorer stays until 2029. \"This is home,\" she said after training.</p>",
            ),
            Story(
                "Youth academy opens a second training pitch",
                &[],
                24,
                true,
                "<p>The new pitch doubles the academy's evening capacity and is open to local schools in the \
                 mornings.</p>",
            ),
            Story(
                "Cup draw sends United to the island",
                &[],
                30,
                false,
                "<p>United travel to Skerry Rovers in the quarter-final, a ground reachable only by ferry.</p>",
            ),
        ],
    },
    Feed {
        title: "Bit & Byte Weekly",
        host: "bitandbyte.example",
        folder: "Weekend",
        stories: &[
            Story(
                "A plain guide to what a language model can and cannot do",
                &["AI", "Explainers"],
                4,
                false,
                "<p>Language models predict text. That makes them good at drafting and summarising, and \
                 unreliable as a source of facts. This guide shows how to tell the difference.</p>",
            ),
            Story(
                "City archive digitises a century of council minutes",
                &["AI", "Open data"],
                10,
                false,
                "<p>Volunteers and a text-recognition tool turned 120,000 pages into searchable text, free \
                 for anyone to download.</p>",
            ),
            Story(
                "Check your router: a flaw in older models needs an update",
                &["Security"],
                13,
                false,
                "<p>Routers sold before 2023 by two makers can be reset from outside. Both have published \
                 fixes; here is how to install them.</p>",
            ),
            Story(
                "Repairable phone lasts a week on one charge",
                &["Gadgets"],
                28,
                true,
                "<p>Our month with a phone whose battery and screen come off with one screwdriver.</p>",
            ),
            Story(
                "Why your password manager asks for a passkey",
                &["Security", "Explainers"],
                33,
                false,
                "<p>Passkeys replace passwords with a key stored on your device. What changes, and what does \
                 not.</p>",
            ),
        ],
    },
    Feed {
        title: "Science Lantern",
        host: "sciencelantern.example",
        folder: "Weekend",
        stories: &[
            Story(
                "Seagrass meadows return to the bay after ten years",
                &[],
                7,
                false,
                "<p>Divers counted meadows on 40 hectares of sea floor that was bare a decade ago. Cleaner \
                 water gets the credit.</p>",
            ),
            Story(
                "How migrating cranes pick their rest stops",
                &[],
                15,
                false,
                "<p>Tracking data from 80 birds shows they return to the same fields year after year, even \
                 when better ones are nearby.</p>",
            ),
            Story(
                "A school class measures the town's night sky",
                &[],
                29,
                true,
                "<p>Pupils counted stars from 30 backyards to map light pollution. The darkest spot is the \
                 old cemetery.</p>",
            ),
        ],
    },
    Feed {
        title: "Kitchen Table",
        host: "kitchentable.example",
        folder: "Weekend",
        stories: &[
            Story(
                "Rye bread without a starter: a weekend recipe",
                &["Food", "Recipes"],
                12,
                false,
                "<p>A slow overnight dough gives the sour taste without keeping a starter alive all week.</p>",
            ),
            Story(
                "Five soups from one bag of root vegetables",
                &["Food"],
                25,
                false,
                "<p>Carrot, parsnip, beetroot and two you may not expect.</p>",
            ),
            Story(
                "The market hall's smoked fish stall turns ninety",
                &["Food", "City"],
                34,
                true,
                "<p>Three generations, one recipe, and a queue every Saturday.</p>",
            ),
        ],
    },
];

/// Groups with their categories and the publisher terms filed under each.
/// A category and the publisher terms filed under it.
type Member = (&'static str, &'static [&'static str]);

const GROUPS: &[(&str, &[Member])] = &[
    (
        "News",
        &[
            ("Politics", &["Politics", "Education"]),
            ("Economy", &["Economy", "Industry", "Housing", "Services", "Energy"]),
            ("City", &["City", "Transport"]),
        ],
    ),
    ("Sport", &[("Football", &[])]),
    ("Technology", &[("AI", &["AI", "Open data"]), ("Security", &["Security"]), ("Gadgets", &["Gadgets"])]),
    ("Life", &[("Culture", &["Culture"]), ("Food", &["Food", "Recipes"]), ("Science", &[])]),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(std::env::args().nth(1).ok_or("usage: demo_data <empty data dir>")?);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("juttusihti.db");
    if path.exists() {
        return Err(format!("{} already exists; give an empty directory", path.display()).into());
    }
    let mut s = Store::open(&path)?;
    let t = now();

    let mut feed_ids = HashMap::new();
    let mut read = Vec::new();
    for f in FEEDS {
        let folder = s.create_folder(f.folder)?;
        let feed = s.add_feed(&NewFeed {
            url: format!("https://{}/feed.xml", f.host),
            title: f.title.into(),
            site_url: Some(format!("https://{}/", f.host)),
            folder_id: Some(folder),
        })?;
        feed_ids.insert(f.title, feed);
        let entries: Vec<NewArticle> = f
            .stories
            .iter()
            .enumerate()
            .map(|(i, Story(title, terms, hours, is_read, text))| {
                let guid = format!("{}-{i}", f.host);
                if *is_read {
                    read.push(guid.clone());
                }
                NewArticle {
                    url: Some(format!("https://{}/stories/{i}", f.host)),
                    guid,
                    title: (*title).into(),
                    summary: Some((*text).into()),
                    content: Some((*text).into()),
                    author: None,
                    published_at: Some(t - hours * 3600 - 600),
                    terms: terms.iter().map(|n| NewTerm { name: (*n).into(), scheme: String::new() }).collect(),
                }
            })
            .collect();
        s.ingest(feed, &entries)?;
    }
    for guid in read {
        let id = s.conn().query_row("SELECT id FROM articles WHERE guid = ?1", [guid], |r| r.get(0))?;
        s.set_read(id, true)?;
    }

    // Groups, categories and term mappings. "Weather", "Explainers" stay unsorted.
    let terms = s.unsorted_terms()?;
    let mut categories = HashMap::new();
    for (group_name, members) in GROUPS {
        let group = s.create_group(group_name)?;
        for (index, (name, mapped)) in members.iter().enumerate() {
            let category = s.create_category(name)?;
            s.move_category(category, Some(group), index)?;
            categories.insert(*name, category);
            for term in terms.iter().filter(|t| mapped.contains(&t.name.as_str())) {
                s.map_term(term.id, Some(category))?;
            }
        }
        if *group_name == "Sport" {
            // A feed with no categories of its own: a default, and a place in the group.
            s.set_feed_default_category(feed_ids["Pitchside"], Some(categories["Football"]))?;
            s.set_feed_group(feed_ids["Pitchside"], Some(group))?;
        }
        if *group_name == "Technology" {
            s.add_favourite(Favourite::Group(group))?;
        }
    }
    s.set_feed_default_category(feed_ids["Science Lantern"], Some(categories["Science"]))?;
    s.add_favourite(Favourite::Category(categories["Politics"]))?;

    // Two labels on the first story.
    let tram: i64 =
        s.conn().query_row("SELECT id FROM articles WHERE guid = 'harbourherald.example-0'", [], |r| r.get(0))?;
    for name in ["Tram line", "City council"] {
        let label = s.create_label(name)?;
        s.tag_article(tram, label, Origin::User, None)?;
    }

    println!("{}", path.display());
    Ok(())
}
