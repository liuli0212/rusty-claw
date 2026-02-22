use std::sync::Arc;
use serde_json::Value;
use async_trait::async_trait;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ToolError {
    #[error(Execution failed: {})]
    ExecutionFailed(String),
    #[error(Invalid arguments: {})]
    InvalidArguments(String),
    #[error(Timeout)]
    Timeout,
    #[error(IO error: {})]
    IoError(#[from] std::io::Error),
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn parameters_schema(&self) -> serde_json::Value;
    async fn execute(&self, args: Value) -> Result<String, ToolError>;
}
