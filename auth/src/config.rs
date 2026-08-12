use clap::Parser;
use std::num::NonZeroU32;

#[derive(Parser, Debug, Clone)]
#[command(name = "private-channel-auth")]
#[command(about = "PrivateChannel authentication service")]
pub struct Config {
    #[arg(long, env = "AUTH_PORT", default_value = "8903")]
    pub port: u16,

    #[arg(long, env = "AUTH_DATABASE_URL")]
    pub database_url: String,

    #[arg(long, env = "JWT_SECRET")]
    pub jwt_secret: String,

    /// Value for the Access-Control-Allow-Origin header.
    /// Set to the frontend origin in production (e.g. "https://app.private_channel.xyz").
    /// Defaults to "*" so local dev works without extra config, but should be
    /// restricted in any environment that handles real credentials.
    #[arg(long, env = "CORS_ALLOWED_ORIGIN", default_value = "*")]
    pub cors_allowed_origin: String,

    /// Maximum number of connections in the database pool.
    #[arg(long, env = "AUTH_DATABASE_MAX_CONNECTIONS", default_value = "10")]
    pub database_max_connections: u32,

    /// Maximum number of Argon2 hashes running at once. Hashing is CPU-bound,
    /// so raising this past the core count costs memory without adding throughput.
    #[arg(long, env = "AUTH_ARGON2_MAX_CONCURRENCY", default_value = "4")]
    pub argon2_max_concurrency: usize,

    /// Sustained per-IP request rate for /auth/register and /auth/login.
    #[arg(long, env = "AUTH_RATE_LIMIT_PER_SECOND", default_value = "5")]
    pub auth_rate_limit_per_second: NonZeroU32,

    /// Burst allowance above the sustained per-IP rate.
    #[arg(long, env = "AUTH_RATE_LIMIT_BURST", default_value = "10")]
    pub auth_rate_limit_burst: NonZeroU32,

    /// Credential attempts allowed per minute against a single username,
    /// regardless of which IPs they come from.
    #[arg(long, env = "AUTH_USERNAME_ATTEMPTS_PER_MINUTE", default_value = "5")]
    pub auth_username_attempts_per_minute: NonZeroU32,
}
