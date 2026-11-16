use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct Photo {
    pub id: i64, pub hash: String, pub rel_path: String,
    pub width: u32, pub height: u32,
    pub taken_at: Option<String>, pub taken_src: String,
    pub title: String, pub note: String,
    pub event_id: Option<i64>, pub rating: i32, pub hidden: bool,
}

#[derive(Clone, Debug)] pub struct Tag { pub id: i64, pub name: String, pub kind: String }
#[derive(Clone, Debug)] pub struct Event { pub id: i64, pub name: String }

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Debug)]
pub enum Order { DateDesc, DateAsc, ImportDesc }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub tags: Vec<i64>,
    pub tags_all: bool,               // true = И, false = ИЛИ
    pub from: Option<String>,         // YYYY-MM-DD
    pub to: Option<String>,
    pub event_id: Option<i64>,
    pub include_hidden: bool,
    pub order: Order,
}
impl Default for Query {
    fn default() -> Self {
        Self { text: String::new(), tags: vec![], tags_all: true, from: None, to: None,
               event_id: None, include_hidden: false, order: Order::DateDesc }
    }
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Debug)]
pub enum Transition { None, Fade, KenBurns }
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Debug)]
pub enum Fit { Contain, Cover }
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize, Debug)]
pub enum PlayOrder { Chrono, Shuffle }

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Overlay { pub date: bool, pub event: bool, pub title: bool, pub tags: bool }

#[derive(Clone, Serialize, Deserialize, Debug)]
pub enum Source { All, Query(Query) }

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct SlideSettings {
    pub source: Source,
    pub seconds: f32,
    pub transition: Transition,
    pub transition_ms: u32,
    pub order: PlayOrder,
    pub fit: Fit,
    pub overlay: Overlay,
    pub screen_side: u32,
    pub loop_playback: bool,
}
impl Default for SlideSettings {
    fn default() -> Self {
        Self {
            source: Source::All, seconds: 8.0, transition: Transition::Fade,
            transition_ms: 700, order: PlayOrder::Shuffle, fit: Fit::Contain,
            overlay: Overlay { date: true, event: true, title: true, tags: false },
            screen_side: 2560, loop_playback: true,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Slide {
    pub id: i64, pub hash: String,
    pub date: Option<String>, pub event: String, pub title: String, pub tags: Vec<String>,
}
