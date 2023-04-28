// If not stated otherwise in this file or this component's license file the
// following copyright and licenses apply:
//
// Copyright 2023 RDK Management
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

use ripple_sdk::{
    api::firebolt::fb_openrpc::FireboltVersionManifest,
    async_trait::async_trait,
    extn::{
        client::extn_sender::ExtnSender,
        extn_id::ExtnId,
        ffi::{ffi_channel::load_channel_builder, ffi_jsonrpsee::load_jsonrpsee_methods},
    },
    framework::bootstrap::Bootstep,
    log::{debug, error, info},
    utils::error::RippleError,
};

use crate::state::{
    bootstrap_state::{BootstrapState, ChannelsState},
    extn_state::PreLoadedExtnChannel,
};
use jsonrpsee::core::server::rpc_module::Methods;

/// Actual bootstep which loads the extensions into the ExtnState.
/// Currently this step loads
/// 1. Device Channel
/// 2. Device Extensions
pub struct LoadExtensionsStep;

#[async_trait]
impl Bootstep<BootstrapState> for LoadExtensionsStep {
    fn get_name(&self) -> String {
        "LoadExtensionsStep".into()
    }
    async fn setup(&self, state: BootstrapState) -> Result<(), RippleError> {
        let loaded_extensions = state.extn_state.loaded_libraries.read().unwrap();
        let mut deferred_channels: Vec<PreLoadedExtnChannel> = Vec::new();
        let mut device_channels: Vec<PreLoadedExtnChannel> = Vec::new();
        let mut jsonrpsee_extns: Methods = Methods::new();
        let mut open_rpcs: Vec<FireboltVersionManifest> = Vec::new();
        let main_sender = state.clone().extn_state.get_sender();
        for extn in loaded_extensions.iter() {
            unsafe {
                let path = extn.entry.clone().path;
                let library = &extn.library;
                info!(
                    "path {} with # of symbols {}",
                    path,
                    extn.metadata.symbols.len()
                );
                let channels = extn.get_channels();
                let extensions = extn.get_extns();
                for channel in channels {
                    debug!("loading channel builder for {}", channel.id);
                    if let Ok(extn_id) = ExtnId::try_from(channel.id.clone()) {
                        if let Ok(builder) = load_channel_builder(library) {
                            debug!("building channel {}", channel.id);
                            if let Ok(extn_channel) = (builder.build)(extn_id.to_string()) {
                                let preloaded_channel = PreLoadedExtnChannel {
                                    channel: extn_channel,
                                    extn_id: extn_id.clone(),
                                    symbol: channel.clone(),
                                };
                                if extn_id.is_device_channel() {
                                    device_channels.push(preloaded_channel);
                                } else {
                                    deferred_channels.push(preloaded_channel);
                                }
                            }
                        } else {
                            error!("invalid channel builder in {}", path);
                            return Err(RippleError::BootstrapError);
                        }
                    } else {
                        error!("invalid extn manifest entry for extn_id");
                        return Err(RippleError::BootstrapError);
                    }
                }
                for extension in extensions {
                    debug!("loading extension {}", extension.id);
                    if let Ok(extn_id) = ExtnId::try_from(extension.id.clone()) {
                        let builder = load_jsonrpsee_methods(library);
                        if let Some(builder) = builder {
                            let (_tx, tr) = ChannelsState::get_crossbeam_channel();
                            let extn_sender = ExtnSender::new(
                                main_sender.clone(),
                                extn_id,
                                extension.clone().uses,
                            );
                            if let Some(open_rpc) = (builder.get_extended_capabilities)() {
                                open_rpcs.push(open_rpc)
                            }

                            let _ = jsonrpsee_extns.merge((builder.build)(extn_sender, tr));
                        }
                    }
                }
                debug!("loading symbols from {}", extn.get_metadata().name);
            }
        }

        {
            let mut device_channel_state = state.extn_state.device_channels.write().unwrap();
            let _ = device_channel_state.extend(device_channels);
            info!("Device channels extension loaded");
        }

        {
            let mut deferred_channel_state = state.extn_state.deferred_channels.write().unwrap();
            let _ = deferred_channel_state.extend(deferred_channels);
            info!("Deferred channels extension loaded");
        }

        state.extn_state.extend_methods(jsonrpsee_extns);

        for open_rpc in open_rpcs {
            state.platform_state.open_rpc_state.add_open_rpc(open_rpc);
        }

        Ok(())
    }
}
