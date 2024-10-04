use dashmap::{mapref::one::RefMut, DashMap};
use nanoid::nanoid;
use once_cell::sync::Lazy;
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};
use tracing::{debug, info};

const CLIENT_JOIN_STALE: Duration = Duration::from_secs(10);
const GAME_STALE: Duration = Duration::from_secs(60);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Default)]
pub struct MyState {
    games: DashMap<String, Game>,
    token_to_game: DashMap<String, String>,
    pub total_games_created: AtomicU64,
}

#[derive(Debug)]
pub struct Game {
    pub updated: Instant,
    pub join_token: String,
    pub external_address: SocketAddr,
    pub local_address: SocketAddr,
    pub clients_to_join: Vec<(JoinClient, Instant)>,
}

#[derive(Serialize, Debug, Clone)]
pub struct JoinClient {
    pub addr: SocketAddr,
    pub hard_nat: bool,
}

impl MyState {
    pub fn get_game_mut_by_join_token(&self, token: &str) -> Option<RefMut<String, Game>> {
        let game_id = self.token_to_game.get(token)?;
        self.get_game_mut(&game_id)
    }

    pub fn get_game_mut(&self, game_id: &str) -> Option<RefMut<String, Game>> {
        self.games
            .get_mut(game_id)
            .filter(|game| game.updated.elapsed() <= GAME_STALE)
    }

    pub fn create_game(
        &self,
        external_address: SocketAddr,
        local_address: SocketAddr,
    ) -> (String, String) {
        let game_id = loop {
            let game_id = nanoid!(20, &TOKEN_ALPHABET);
            if !self.games.contains_key(&game_id) {
                break game_id;
            }
        };
        let token = loop {
            // https://zelark.github.io/nano-id-cc/
            let token = nanoid!(10, &TOKEN_ALPHABET);
            if !self.token_to_game.contains_key(&token) && !contains_bad_words(&token) {
                break token;
            }
        };
        debug!(
            "Created game {}, token: {}, addr: {}, local_addr: {}",
            game_id, token, external_address, local_address
        );
        let game = Game {
            updated: Instant::now(),
            join_token: token.clone(),
            external_address,
            local_address,
            clients_to_join: Vec::new(),
        };
        self.games.insert(game_id.clone(), game);
        self.token_to_game.insert(token.clone(), game_id.clone());
        self.total_games_created.fetch_add(1, Ordering::Relaxed);

        (game_id, token)
    }

    pub fn cleanup(&self) {
        self.games.retain(|_, game| {
            let retain = game.updated.elapsed() <= GAME_STALE;
            if !retain {
                self.token_to_game.remove(&game.join_token);
            }
            retain
        });
    }
}

impl Game {
    pub fn drain_joiners(&mut self) -> Vec<JoinClient> {
        self.updated = Instant::now();
        self.clients_to_join
            .drain(..)
            .filter(|(_, created)| created.elapsed() <= CLIENT_JOIN_STALE)
            .map(|(c, _)| c)
            .collect()
    }

    pub fn add_joiner(&mut self, client: JoinClient) {
        self.clients_to_join.push((client, Instant::now()));
    }
}

pub async fn state_cleanup(state: &'static MyState) {
    let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
    let mut last_total_games_created = 0;
    loop {
        interval.tick().await;
        state.cleanup();
        let diff_games_created =
            state.total_games_created.load(Ordering::Relaxed) - last_total_games_created;
        if diff_games_created > 0 {
            last_total_games_created += diff_games_created;
            info!(
                "Total games created: {}, in the last 5 mins: {}",
                last_total_games_created, diff_games_created
            );
        }
    }
}

static BAD_WORDS_REGEX: Lazy<Regex> = Lazy::new(|| {
    let words = include_str!("badwords.txt")
        .lines()
        .collect::<Vec<_>>()
        .join("|");
    RegexBuilder::new(&words)
        .case_insensitive(true)
        .unicode(false)
        .build()
        .unwrap()
});

fn contains_bad_words(token: &str) -> bool {
    BAD_WORDS_REGEX.is_match(token)
}

// Test bad words
#[test]
fn test_contains_bad_words() {
    assert!(contains_bad_words("ASDF-CuMJ_K"));
    assert!(!contains_bad_words("AdDF-aFcx"));
}

#[test]
fn scuffed_bench() {
    let mut total = 0;
    for _ in 0..1000000 {
        if contains_bad_words("as3dfbi74") {
            total += 1;
        }
    }

    println!("{}", total)
}

// Same as nanoid::alphabet::SAFE but dash, underscore and capital letters removed
pub const TOKEN_ALPHABET: [char; 36] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i',
    'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
];
