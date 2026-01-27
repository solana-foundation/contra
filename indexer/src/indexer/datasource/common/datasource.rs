use crate::error::DataSourceError;

pub use super::types::*;
use async_trait::async_trait;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait DataSource: Send + Sync {
    async fn start(
        &mut self,
        tx: InstructionSender,
        cancellation_token: CancellationToken,
    ) -> Result<JoinHandle<()>, DataSourceError>;

    /// Request graceful shutdown of the datasource
    /// This should stop accepting new data and clean up connections
    async fn shutdown(&mut self) -> Result<(), DataSourceError>;
}
