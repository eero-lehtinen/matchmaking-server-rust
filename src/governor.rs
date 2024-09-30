use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use axum::extract::Request;
use governor::middleware::NoOpMiddleware;
use serde::{Deserialize, Serialize};
use tower_governor::{
    governor::GovernorConfigBuilder, key_extractor::KeyExtractor, GovernorError, GovernorLayer,
};

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq)]
pub struct CfCompatibleIp;

impl KeyExtractor for CfCompatibleIp {
    type Key = IpAddr;

    fn extract<B>(&self, req: &Request<B>) -> Result<Self::Key, GovernorError> {
        if let Some(cf_ip) = req.headers().get("Cf-Connecting-IP") {
            cf_ip.to_str().ok().and_then(|s| s.parse::<IpAddr>().ok())
        } else {
            req.extensions()
                .get::<axum::extract::ConnectInfo<SocketAddr>>()
                .map(|addr| addr.ip())
        }
        .ok_or(GovernorError::UnableToExtractKey)
    }
}

pub fn make_governor_layer() -> GovernorLayer<CfCompatibleIp, NoOpMiddleware> {
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(CfCompatibleIp)
            .finish()
            .unwrap(),
    );
    let limiter = governor_conf.limiter().clone();
    tokio::task::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            limiter.retain_recent();
        }
    });

    GovernorLayer {
        config: governor_conf,
    }
}
