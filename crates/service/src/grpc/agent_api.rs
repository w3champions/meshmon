//! Tonic service implementation for the five agent RPCs.

use crate::state::AppState;
use meshmon_protocol::{
    AgentApi, ConfigResponse, GetConfigRequest, GetTargetsRequest, MetricsBatch,
    PushMetricsResponse, PushRouteSnapshotResponse, RegisterRequest, RegisterResponse,
    RouteSnapshotRequest, TargetsResponse,
};
use tonic::{Request, Response, Status};

/// Concrete implementation of the `AgentApi` tonic service.
///
/// Each RPC body is a stub returning [`Status::unimplemented`]; the real
/// implementations arrive in Tasks 10–12, 15–16.
pub struct AgentApiImpl {
    state: AppState,
}

impl AgentApiImpl {
    /// Construct a new impl from the shared application state.
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// Borrow the shared application state.
    pub fn state(&self) -> &AppState {
        &self.state
    }
}

#[tonic::async_trait]
impl AgentApi for AgentApiImpl {
    async fn register(
        &self,
        _request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        Err(Status::unimplemented("register"))
    }

    async fn push_metrics(
        &self,
        _request: Request<MetricsBatch>,
    ) -> Result<Response<PushMetricsResponse>, Status> {
        Err(Status::unimplemented("push_metrics"))
    }

    async fn push_route_snapshot(
        &self,
        _request: Request<RouteSnapshotRequest>,
    ) -> Result<Response<PushRouteSnapshotResponse>, Status> {
        Err(Status::unimplemented("push_route_snapshot"))
    }

    async fn get_config(
        &self,
        _request: Request<GetConfigRequest>,
    ) -> Result<Response<ConfigResponse>, Status> {
        Err(Status::unimplemented("get_config"))
    }

    async fn get_targets(
        &self,
        _request: Request<GetTargetsRequest>,
    ) -> Result<Response<TargetsResponse>, Status> {
        Err(Status::unimplemented("get_targets"))
    }
}
