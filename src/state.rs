use dashmap::{mapref::one::RefMut, DashMap};
use nanoid::nanoid;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::{
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
use tracing::{debug, info};

const CLIENT_JOIN_STALE: Duration = Duration::from_secs(10);
const GAME_STALE: Duration = Duration::from_secs(60);
const CLEANUP_INTERVAL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Default)]
pub struct MyState {
    games: DashMap<String, Game>,
    join_tokens: DashMap<String, String>,
    pub total_games_created: AtomicU64,
}

#[derive(Debug)]
pub struct Game {
    pub timestamp: u64,
    pub token: String,
    pub external_address: SocketAddr,
    pub local_address: SocketAddr,
    pub clients_to_join: Vec<(JoinClient, u64)>,
}

#[derive(Serialize, Debug, Clone)]
pub struct JoinClient {
    pub addr: SocketAddr,
    pub hard_nat: bool,
}

impl MyState {
    pub fn get_game_mut_by_join_token(&self, token: &str) -> Option<RefMut<String, Game>> {
        let now = unix_time_secs();
        let game_id = self.join_tokens.get(token)?;
        self.games
            .get_mut(&*game_id)
            .filter(|game| now - game.timestamp <= GAME_STALE.as_secs())
    }

    pub fn get_game_mut(&self, game_id: &str) -> Option<RefMut<String, Game>> {
        let now = unix_time_secs();
        self.games
            .get_mut(game_id)
            .filter(|game| now - game.timestamp <= GAME_STALE.as_secs())
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
            if !self.join_tokens.contains_key(&token) && !contains_bad_words(&token) {
                break token;
            }
        };
        debug!(
            "Created game {}, token: {}, addr: {}, local_addr: {}",
            game_id, token, external_address, local_address
        );
        let game = Game {
            timestamp: unix_time_secs(),
            token: token.clone(),
            external_address,
            local_address,
            clients_to_join: Vec::new(),
        };
        self.games.insert(game_id.clone(), game);
        self.join_tokens.insert(token.clone(), game_id.clone());
        self.total_games_created.fetch_add(1, Ordering::Relaxed);

        (game_id, token)
    }

    pub fn cleanup(&self) {
        let now = unix_time_secs();
        self.games.retain(|_, game| {
            let retain = now - game.timestamp <= GAME_STALE.as_secs();
            if !retain {
                self.join_tokens.remove(&game.token);
            }
            retain
        });
    }
}

impl Game {
    pub fn drain_joiners(&mut self) -> Vec<JoinClient> {
        let now = unix_time_secs();
        self.timestamp = now;
        self.clients_to_join
            .drain(..)
            .filter(|(_, timestamp)| now - *timestamp <= CLIENT_JOIN_STALE.as_secs())
            .map(|(c, _)| c)
            .collect()
    }

    pub fn add_joiner(&mut self, client: JoinClient) {
        self.clients_to_join.push((client, unix_time_secs()));
    }
}

fn unix_time_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
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

static BAD_WORDS: Lazy<Vec<&'static str>> =
    Lazy::new(|| include_str!("badwords.txt").lines().collect());

fn contains_bad_words(token: &str) -> bool {
    let token = token.to_ascii_lowercase();
    for bad_word in BAD_WORDS.iter() {
        if token.contains(bad_word) {
            return true;
        }
    }
    false
}

// Test bad words
#[test]
fn test_contains_bad_words() {
    assert!(contains_bad_words("ASDF-CuMJ_K"));
    assert!(!contains_bad_words("AdDF-aFcx"));
}

// Same as nanoid::alphabet::SAFE but dash, underscore and capital letters removed
pub const TOKEN_ALPHABET: [char; 36] = [
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i',
    'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z',
];
