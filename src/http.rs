//! The one HTTP client for what zitch fetches itself: itch.io's page data
//! and API, GitHub releases, and covers. butler makes its own calls.

use std::sync::OnceLock;
use std::time::Duration;

const USER_AGENT: &str = concat!("zitch/", env!("ZITCH_VERSION"));

/// How long to wait for a host to answer at all. The body has no limit of
/// its own: a cover or an update archive takes as long as the link
/// allows, and a request that wants a deadline on the whole exchange sets
/// one through its own config.
const CONNECT: Duration = Duration::from_secs(10);
const FIRST_BYTE: Duration = Duration::from_secs(20);

/// Names zitch in every request and gives up on a host that does not
/// answer.
pub fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_connect(Some(CONNECT))
            .timeout_recv_response(Some(FIRST_BYTE))
            .build();
        ureq::Agent::new_with_config(config)
    })
}
