use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(name = "contra-auth")]
#[command(about = "Contra authentication service")]
pub struct Config {
    #[arg(long, env = "AUTH_PORT", default_value = "8903")]
    pub port: u16,

    #[arg(long, env = "AUTH_DATABASE_URL")]
    pub database_url: String,

    #[arg(long, env = "JWT_SECRET")]
    pub jwt_secret: String,
}
