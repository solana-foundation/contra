
use sqlx::PgPool;
use std::sync::Arc;
use crate::jwt::JwtConfig;

mod error;
mod db;
mod routes;
mod jwt;
mod models;
mod config;

#[derive(Clone)]                                                                        
pub struct AppState {
    pub pool: PgPool,                                                                   
    pub jwt: Arc<JwtConfig>,
}

fn main() {

}
