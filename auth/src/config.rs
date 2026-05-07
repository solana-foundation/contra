use clap::Parser;

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
}
