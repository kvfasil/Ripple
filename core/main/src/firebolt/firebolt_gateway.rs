// Copyright 2023 Comcast Cable Communications Management, LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// SPDX-License-Identifier: Apache-2.0
//

use jsonrpsee::{core::server::rpc_module::Methods, types::TwoPointZero};
use ripple_sdk::{
    api::{
        apps::EffectiveTransport,
        firebolt::{
            fb_capabilities::JSON_RPC_STANDARD_ERROR_INVALID_PARAMS,
            fb_openrpc::FireboltOpenRpcMethod,
        },
        gateway::{
            rpc_error::RpcError,
            rpc_gateway_api::{ApiMessage, ApiProtocol, RpcRequest},
        },
    },
    chrono::Utc,
    extn::extn_client_message::ExtnMessage,
    log::{debug, error, info, warn},
    serde_json::{self, Value},
    tokio,
};
use serde::Serialize;

use crate::{
    firebolt::firebolt_gatekeeper::FireboltGatekeeper,
    service::{apps::app_events::AppEvents, telemetry_builder::TelemetryBuilder},
    state::{
        bootstrap_state::BootstrapState, openrpc_state::OpenRpcState,
        platform_state::PlatformState, session_state::Session,
    },
};

use super::rpc_router::RpcRouter;
pub struct FireboltGateway {
    state: BootstrapState,
}

#[derive(Serialize)]
pub struct JsonRpcMessage {
    pub jsonrpc: TwoPointZero,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Debug, Clone)]
pub enum FireboltGatewayCommand {
    RegisterSession {
        session_id: String,
        session: Session,
    },
    UnregisterSession {
        session_id: String,
        cid: String,
    },
    HandleRpc {
        request: RpcRequest,
    },
    HandleRpcForExtn {
        msg: ExtnMessage,
    },
}

impl FireboltGateway {
    pub fn new(state: BootstrapState, methods: Methods) -> FireboltGateway {
        for method in methods.method_names() {
            info!("Adding RPC method {}", method);
        }
        state.platform_state.router_state.update_methods(methods);
        FireboltGateway { state }
    }

    pub async fn start(&self) {
        info!("Starting Gateway Listener");
        let mut firebolt_gateway_rx = self
            .state
            .channels_state
            .get_gateway_receiver()
            .expect("Gateway receiver to be available");
        while let Some(cmd) = firebolt_gateway_rx.recv().await {
            use FireboltGatewayCommand::*;

            match cmd {
                RegisterSession {
                    session_id,
                    session,
                } => {
                    self.state
                        .platform_state
                        .session_state
                        .add_session(session_id, session);
                }
                UnregisterSession { session_id, cid } => {
                    AppEvents::remove_session(&self.state.platform_state, session_id.clone());
                    self.state.platform_state.session_state.clear_session(&cid);
                }
                HandleRpc { request } => self.handle(request, None).await,
                HandleRpcForExtn { msg } => {
                    if let Some(request) = msg.payload.clone().extract() {
                        self.handle(request, Some(msg)).await
                    } else {
                        error!("Not a valid RPC Request {:?}", msg);
                    }
                }
            }
        }
    }

    pub async fn handle(&self, request: RpcRequest, extn_msg: Option<ExtnMessage>) {
        info!(
            "firebolt_gateway Received Firebolt request {} {} {}",
            request.ctx.request_id, request.method, request.params_json
        );
        // First check sender if no sender no need to process
        let callback_c = extn_msg.clone();
        match request.ctx.protocol {
            ApiProtocol::Extn => {
                if callback_c.is_none() || callback_c.unwrap().callback.is_none() {
                    error!("No callback for request {:?} ", request);
                    return;
                }
            }
            _ => {
                if !self
                    .state
                    .platform_state
                    .session_state
                    .has_session(&request.ctx)
                {
                    error!("No sender for request {:?} ", request);
                    return;
                }
            }
        }
        let platform_state = self.state.platform_state.clone();
        /*
         * The reason for spawning a new thread is that when request-1 comes, and it waits for
         * user grant. The response from user grant, (eg ChallengeResponse) comes as rpc which
         * in-turn goes through the permission check and sends a gate request. But the single
         * thread which was listening on the channel will be waiting for the user response. This
         * leads to a stall.
         */
        let mut request_c = request.clone();
        request_c.method = FireboltOpenRpcMethod::name_with_lowercase_module(&request.method);

        let metrics_timer = TelemetryBuilder::start_firebolt_metrics_timer(
            &platform_state.get_client().get_extn_client(),
            request_c.method.clone(),
            request_c.ctx.app_id.clone(),
        );

        let open_rpc_state = self.state.platform_state.open_rpc_state.clone();

        tokio::spawn(async move {
            let start = Utc::now().timestamp_millis();

            // Validate incoming request parameters.
            if let Err(error_string) = validate_request(open_rpc_state, &request_c) {
                let now = Utc::now().timestamp_millis();

                RpcRouter::log_rdk_telemetry_message(
                    &request.ctx.app_id,
                    &request.method,
                    JSON_RPC_STANDARD_ERROR_INVALID_PARAMS,
                    now - start,
                );

                TelemetryBuilder::stop_and_send_firebolt_metrics_timer(
                    &platform_state.clone(),
                    metrics_timer,
                    format!("{}", JSON_RPC_STANDARD_ERROR_INVALID_PARAMS),
                )
                .await;

                let json_rpc_error = JsonRpcError {
                    code: JSON_RPC_STANDARD_ERROR_INVALID_PARAMS,
                    message: error_string,
                    data: None,
                };

                send_json_rpc_error(&platform_state, &request, json_rpc_error).await;
                return;
            }

            let result = FireboltGatekeeper::gate(platform_state.clone(), request_c.clone()).await;

            match result {
                Ok(_) => {
                    if !platform_state
                        .endpoint_state
                        .handle_brokerage(request_c.clone(), extn_msg.clone())
                    {
                        // Route
                        match request.clone().ctx.protocol {
                            ApiProtocol::Extn => {
                                if let Some(extn_msg) = extn_msg {
                                    RpcRouter::route_extn_protocol(
                                        &platform_state,
                                        request.clone(),
                                        extn_msg,
                                    )
                                    .await
                                } else {
                                    error!("missing invalid message not forwarding");
                                }
                            }
                            _ => {
                                if let Some(session) = platform_state
                                    .clone()
                                    .session_state
                                    .get_session(&request_c.ctx)
                                {
                                    // if the websocket disconnects before the session is recieved this leads to an error
                                    RpcRouter::route(
                                        platform_state.clone(),
                                        request_c,
                                        session,
                                        metrics_timer.clone(),
                                    )
                                    .await;
                                } else {
                                    error!("session is missing request is not forwarded");
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let deny_reason = e.reason;
                    // log firebolt response message in RDKTelemetry 1.0 friendly format
                    let now = Utc::now().timestamp_millis();

                    RpcRouter::log_rdk_telemetry_message(
                        &request.ctx.app_id,
                        &request.method,
                        deny_reason.get_rpc_error_code(),
                        now - start,
                    );

                    TelemetryBuilder::stop_and_send_firebolt_metrics_timer(
                        &platform_state.clone(),
                        metrics_timer,
                        format!("{}", deny_reason.get_observability_error_code()),
                    )
                    .await;

                    error!(
                        "Failed gateway present error {:?} {:?}",
                        request, deny_reason
                    );

                    let caps = e.caps.iter().map(|x| x.as_str()).collect();
                    let json_rpc_error = JsonRpcError {
                        code: deny_reason.get_rpc_error_code(),
                        message: deny_reason.get_rpc_error_message(caps),
                        data: None,
                    };

                    send_json_rpc_error(&platform_state, &request, json_rpc_error).await;
                }
            }
        });
    }
}

fn validate_request(open_rpc_state: OpenRpcState, request: &RpcRequest) -> Result<(), String> {
    let major_version = open_rpc_state.get_version().major.to_string();
    let openrpc_validator = open_rpc_state.get_openrpc_validator();

    if let Some(rpc_method) = openrpc_validator.get_method_by_name(&request.method) {
        let validator = openrpc_validator
            .params_validator(major_version, &rpc_method.name)
            .unwrap();

        if let Ok(params) = serde_json::from_str::<Vec<serde_json::Value>>(&request.params_json) {
            if params.len() > 1 {
                if let Err(errors) = validator.validate(&params[1]) {
                    let mut error_string = String::new();
                    for error in errors {
                        error_string.push_str(&format!("{} ", error));
                    }
                    return Err(error_string);
                }
            }
        }
    } else {
        // TODO: Currently LifecycleManagement and other APIs are not in the schema. Let these pass through to their
        // respective handlers for now.
        debug!(
            "validate_request: Method not found in schema: {}",
            request.method
        );
    }

    Ok(())
}

async fn send_json_rpc_error(
    platform_state: &PlatformState,
    request: &RpcRequest,
    json_rpc_error: JsonRpcError,
) {
    if let Some(session) = platform_state
        .clone()
        .session_state
        .get_session(&request.ctx)
    {
        let error_message = JsonRpcMessage {
            jsonrpc: TwoPointZero {},
            id: request.ctx.call_id,
            error: Some(json_rpc_error),
        };

        if let Ok(error_message) = serde_json::to_string(&error_message) {
            let api_message = ApiMessage::new(
                request.clone().ctx.protocol,
                error_message,
                request.clone().ctx.request_id,
            );

            match session.get_transport() {
                EffectiveTransport::Websocket => {
                    if let Err(e) = session.send_json_rpc(api_message).await {
                        error!(
                            "send_json_rpc_error: Error sending websocket message: e={:?}",
                            e
                        )
                    }
                }
                EffectiveTransport::Bridge(id) => {
                    if let Err(e) = platform_state.send_to_bridge(id, api_message).await {
                        error!(
                            "send_json_rpc_error: Error sending bridge message: e={:?}",
                            e
                        )
                    }
                }
            }
        } else {
            error!("send_json_rpc_error: Could not serialize error message");
        }
    } else {
        warn!(
            "send_json_rpc_error: Session no found: method={}",
            request.method
        );
    }
}
