//! Port forwards and live transports (`server/src/routes/forwards.ts`) and saved tunnels
//! (`routes/tunnels.ts`).

use muxus_core::api::{
    ConnectionsResponse, ForwardInfo, ForwardRequest, TunnelInput, TunnelRecord, TunnelsResponse,
};
use muxus_core::js::utf16_len;
use muxus_core::validate::Validate;

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};

/// The zod checks of `forwardRequestSchema`.
fn check_forward(request: &ForwardRequest) -> Result<(), String> {
    let too_small_string = "Too small: expected string to have >=1 characters".to_string();
    let too_small_number = "Too small: expected number to be >=1".to_string();
    if request.conn_id.is_empty() {
        return Err(too_small_string);
    }
    // 0 asks for any free port, for a local or dynamic forward only.
    if request.bind_port == 0 && request.kind == muxus_core::api::ForwardType::Remote {
        return Err("bindPort must be 1–65535".to_string());
    }
    if request
        .target_host
        .as_deref()
        .is_some_and(|host| utf16_len(host) < 1)
    {
        return Err(too_small_string);
    }
    if request.target_port == Some(0) {
        return Err(too_small_number);
    }
    if request.tunnel_id.as_deref().is_some_and(str::is_empty) {
        return Err(too_small_string);
    }
    Ok(())
}

impl Backend {
    /// `GET /api/forwards`: every forward, or those on one connection.
    pub async fn forwards(&self, conn_id: Option<&str>) -> ApiResult<Vec<ForwardInfo>> {
        let conn_id = conn_id.map(str::to_string);
        self.run(move |shared| async move { Ok(shared.forwards.list(conn_id.as_deref())) })
            .await
    }

    /// `POST /api/forwards`.
    pub async fn start_forward(&self, request: ForwardRequest) -> ApiResult<ForwardInfo> {
        check_forward(&request).map_err(ApiError::bad_request)?;
        self.run(move |shared| async move { Ok(shared.forwards.start(request).await?) })
            .await
    }

    /// `DELETE /api/forwards/:id`.
    pub async fn stop_forward(&self, id: &str) -> ApiResult<()> {
        let id = id.to_string();
        self.run(move |shared| async move {
            shared.forwards.stop(&id);
            Ok(())
        })
        .await
    }

    /// `PATCH /api/forwards/:id`: adopt a running forward into a saved tunnel ("save as tunnel"
    /// without restarting the listener).
    pub async fn assign_forward_tunnel(&self, id: &str, tunnel_id: &str) -> ApiResult<ForwardInfo> {
        if tunnel_id.is_empty() {
            return Err(ApiError::bad_request("tunnelId is required"));
        }
        let (id, tunnel_id) = (id.to_string(), tunnel_id.to_string());
        self.run(move |shared| async move {
            shared
                .forwards
                .assign_tunnel(&id, &tunnel_id)
                .ok_or_else(|| ApiError::not_found("forward not found"))
        })
        .await
    }

    /// `GET /api/connections`: live SSH transports, so the forwarding panel can reuse one for a
    /// tunnel instead of dialing a second connection.
    pub async fn connections_list(&self) -> ApiResult<ConnectionsResponse> {
        self.run(|shared| async move {
            Ok(ConnectionsResponse {
                connections: shared.connections.list(),
            })
        })
        .await
    }

    /// `GET /api/tunnels`.
    pub async fn tunnels(&self) -> ApiResult<TunnelsResponse> {
        self.blocking(|shared| {
            Ok(TunnelsResponse {
                tunnels: shared.database.list_tunnels()?,
            })
        })
        .await
    }

    /// `PUT /api/tunnels`: saving ends a redial that should no longer happen.
    pub async fn save_tunnel(&self, tunnel: TunnelInput) -> ApiResult<TunnelRecord> {
        let mut tunnel = tunnel;
        tunnel.validate().map_err(ApiError::invalid)?;
        let saved = self
            .blocking(move |shared| Ok(shared.database.save_tunnel(&tunnel)?))
            .await?;
        let id = saved.id.clone();
        self.run(move |shared| async move {
            shared.forwards.tunnel_changed(&id);
            Ok(())
        })
        .await?;
        Ok(saved)
    }

    /// `DELETE /api/tunnels/:id`.
    pub async fn delete_tunnel(&self, id: &str) -> ApiResult<bool> {
        let id = id.to_string();
        let lookup = id.clone();
        let deleted = self
            .blocking(move |shared| Ok(shared.database.delete_tunnel(&lookup)?))
            .await?;
        self.run(move |shared| async move {
            shared.forwards.tunnel_changed(&id);
            Ok(())
        })
        .await?;
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use muxus_core::api::{ForwardRequest, ForwardType};

    use super::check_forward;

    fn request(kind: ForwardType, bind_port: u16) -> ForwardRequest {
        ForwardRequest {
            conn_id: "c1".into(),
            kind,
            bind_port,
            target_host: None,
            target_port: None,
            tunnel_id: None,
            independent: None,
        }
    }

    #[test]
    fn lets_local_and_dynamic_forwards_ask_for_any_free_port() {
        assert!(check_forward(&request(ForwardType::Local, 0)).is_ok());
        assert!(check_forward(&request(ForwardType::Dynamic, 0)).is_ok());
        assert_eq!(
            check_forward(&request(ForwardType::Remote, 0)),
            Err("bindPort must be 1–65535".to_string())
        );
        assert!(check_forward(&request(ForwardType::Remote, 9000)).is_ok());
    }
}
