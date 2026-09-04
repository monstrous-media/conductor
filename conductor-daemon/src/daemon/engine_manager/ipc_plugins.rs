// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

use super::*;

impl EngineManager {
    pub(crate) async fn handle_list_plugins(&mut self, id: String) -> IpcResponse {
        let executor = self.action_executor.lock().await;
        let pm = executor.plugin_manager();
        let available = match pm.list_available() {
            Ok(list) => list,
            Err(e) => {
                return IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::InternalError.as_u16(),
                        message: format!("Failed to list plugins: {}", e),
                        details: None,
                    }),
                };
            }
        };
        let loaded = match pm.list_loaded() {
            Ok(list) => list,
            Err(e) => {
                return IpcResponse {
                    id,
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::InternalError.as_u16(),
                        message: format!("Failed to list loaded plugins: {}", e),
                        details: None,
                    }),
                };
            }
        };
        create_success_response(
            &id,
            Some(json!({
                "available": available,
                "loaded": loaded,
            })),
        )
    }

    pub(crate) async fn handle_get_plugin_info(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let name = match extract_validated_plugin_name(request, &id) {
            Ok(n) => n,
            Err(resp) => return resp,
        };
        let executor = self.action_executor.lock().await;
        match executor.plugin_manager().get_metadata(&name) {
            Ok(meta) => match serde_json::to_value(meta) {
                Ok(val) => create_success_response(&id, Some(val)),
                Err(e) => IpcResponse {
                    id: id.clone(),
                    status: ResponseStatus::Error,
                    data: None,
                    error: Some(ErrorDetails {
                        code: IpcErrorCode::InternalError.as_u16(),
                        message: format!("Failed to serialize plugin metadata: {}", e),
                        details: None,
                    }),
                },
            },
            Err(e) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InvalidRequest.as_u16(),
                    message: format!("Plugin not found: {}", e),
                    details: None,
                }),
            },
        }
    }

    pub(crate) async fn handle_enable_plugin(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let name = match extract_validated_plugin_name(request, &id) {
            Ok(n) => n,
            Err(resp) => return resp,
        };
        let mut executor = self.action_executor.lock().await;
        match executor.plugin_manager_mut().enable_plugin(&name) {
            Ok(()) => create_success_response(
                &id,
                Some(json!({ "message": format!("Plugin '{}' enabled", name) })),
            ),
            Err(e) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: format!("Failed to enable plugin: {}", e),
                    details: None,
                }),
            },
        }
    }

    pub(crate) async fn handle_disable_plugin(
        &mut self,
        request: &crate::daemon::types::IpcRequest,
        id: String,
    ) -> IpcResponse {
        let name = match extract_validated_plugin_name(request, &id) {
            Ok(n) => n,
            Err(resp) => return resp,
        };
        let mut executor = self.action_executor.lock().await;
        match executor.plugin_manager_mut().disable_plugin(&name) {
            Ok(()) => create_success_response(
                &id,
                Some(json!({ "message": format!("Plugin '{}' disabled", name) })),
            ),
            Err(e) => IpcResponse {
                id,
                status: ResponseStatus::Error,
                data: None,
                error: Some(ErrorDetails {
                    code: IpcErrorCode::InternalError.as_u16(),
                    message: format!("Failed to disable plugin: {}", e),
                    details: None,
                }),
            },
        }
    }
}
