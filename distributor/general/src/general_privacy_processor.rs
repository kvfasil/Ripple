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

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use ripple_sdk::{
    api::distributor::distributor_privacy::{
        GetPropertyParams, PrivacyRequest, PrivacySetting, PrivacySettings, SetPropertyParams,
    },
    async_trait::async_trait,
    extn::{
        client::{
            extn_client::ExtnClient,
            extn_processor::{
                DefaultExtnStreamer, ExtnRequestProcessor, ExtnStreamProcessor, ExtnStreamer,
            },
        },
        extn_client_message::{ExtnPayloadProvider, ExtnResponse},
    },
    framework::file_store::FileStore,
};
use serde::{Deserialize, Serialize};

pub struct DistributorPrivacyProcessor {
    state: PrivacyState,
    streamer: DefaultExtnStreamer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrivacyData {
    settings: PrivacySettings,
    data_collection: HashMap<String, bool>,
    entitlement: HashMap<String, bool>,
}

#[derive(Debug, Clone)]
pub struct PrivacyState {
    client: ExtnClient,
    privacy_data: Arc<RwLock<FileStore<PrivacyData>>>,
}

impl PrivacyState {
    fn new(client: ExtnClient, path: String) -> Self {
        let path = get_privacy_path(path);
        let store = if let Ok(v) = FileStore::load(path.clone()) {
            v
        } else {
            FileStore::new(path.clone(), PrivacyData::new())
        };

        Self {
            client,
            privacy_data: Arc::new(RwLock::new(store)),
        }
    }

    fn get_property(&self, params: GetPropertyParams) -> bool {
        let data = self.privacy_data.read().unwrap();
        match params.setting {
            PrivacySetting::AppDataCollection(a) => return data.value.get_data_collections(a),
            PrivacySetting::AppEntitlementCollection(e) => {
                return data.value.get_ent_collections(e)
            }
            PrivacySetting::ContinueWatching => {
                return data.value.settings.allow_resume_points.clone()
            }
            PrivacySetting::UnentitledContinueWatching => {
                return data.value.settings.allow_unentitled_resume_points.clone()
            }
            PrivacySetting::WatchHistory => return data.value.settings.allow_watch_history.clone(),
            PrivacySetting::ProductAnalytics => {
                return data.value.settings.allow_product_analytics.clone()
            }
            PrivacySetting::Personalization => {
                return data.value.settings.allow_personalization.clone()
            }
            PrivacySetting::UnentitledPersonalization => {
                return data.value.settings.allow_unentitled_personalization.clone()
            }
            PrivacySetting::RemoteDiagnostics => {
                return data.value.settings.allow_remote_diagnostics.clone()
            }
            PrivacySetting::PrimaryContentAdTargeting => {
                return data
                    .value
                    .settings
                    .allow_primary_content_ad_targeting
                    .clone()
            }
            PrivacySetting::PrimaryBrowseAdTargeting => {
                return data
                    .value
                    .settings
                    .allow_primary_browse_ad_targeting
                    .clone()
            }
            PrivacySetting::AppContentAdTargeting => {
                return data.value.settings.allow_app_content_ad_targeting.clone()
            }
            PrivacySetting::Acr => return data.value.settings.allow_acr_collection.clone(),
            PrivacySetting::CameraAnalytics => {
                return data.value.settings.allow_camera_analytics.clone()
            }
        }
    }

    fn set_property(&self, params: SetPropertyParams) -> bool {
        let mut data = self.privacy_data.write().unwrap();
        match params.setting.clone() {
            PrivacySetting::AppDataCollection(a) => {
                data.value.set_data_collections(a, params.value)
            }
            PrivacySetting::AppEntitlementCollection(e) => {
                data.value.set_ent_collections(e, params.value)
            }
            _ => data.value.set_setting(params.setting, params.value),
        }
        false
    }

    fn get_settings(&self) -> PrivacySettings {
        let data = self.privacy_data.read().unwrap();
        data.value.settings.clone()
    }
}

impl PrivacyData {
    fn new() -> Self {
        Self {
            settings: PrivacySettings::new(),
            data_collection: HashMap::new(),
            entitlement: HashMap::new(),
        }
    }
    fn get_data_collections(&self, id: String) -> bool {
        self.data_collection.get(&id).cloned().unwrap_or(false)
    }

    fn get_ent_collections(&self, id: String) -> bool {
        self.entitlement.get(&id).cloned().unwrap_or(false)
    }

    fn set_data_collections(&mut self, id: String, data: bool) {
        self.data_collection.insert(id, data);
    }

    fn set_ent_collections(&mut self, id: String, data: bool) {
        self.entitlement.insert(id, data);
    }

    fn set_setting(&mut self, setting: PrivacySetting, data: bool) {
        match setting {
            PrivacySetting::ContinueWatching => self.settings.allow_resume_points = data,
            PrivacySetting::UnentitledContinueWatching => {
                self.settings.allow_unentitled_resume_points = data
            }
            PrivacySetting::WatchHistory => self.settings.allow_watch_history = data,
            PrivacySetting::ProductAnalytics => self.settings.allow_product_analytics = data,
            PrivacySetting::Personalization => self.settings.allow_personalization = data,
            PrivacySetting::UnentitledPersonalization => {
                self.settings.allow_unentitled_personalization = data
            }
            PrivacySetting::RemoteDiagnostics => self.settings.allow_remote_diagnostics = data,
            PrivacySetting::PrimaryContentAdTargeting => {
                self.settings.allow_primary_content_ad_targeting = data
            }
            PrivacySetting::PrimaryBrowseAdTargeting => {
                self.settings.allow_primary_browse_ad_targeting = data
            }
            PrivacySetting::AppContentAdTargeting => {
                self.settings.allow_app_content_ad_targeting = data
            }
            PrivacySetting::Acr => self.settings.allow_acr_collection = data,
            PrivacySetting::CameraAnalytics => self.settings.allow_camera_analytics = data,
            _ => {}
        }
    }
}

fn get_privacy_path(saved_dir: String) -> String {
    format!("{}/{}", saved_dir, "privacy_settings")
}

impl DistributorPrivacyProcessor {
    pub fn new(client: ExtnClient, path: String) -> DistributorPrivacyProcessor {
        DistributorPrivacyProcessor {
            state: PrivacyState::new(client, path),
            streamer: DefaultExtnStreamer::new(),
        }
    }
}

impl ExtnStreamProcessor for DistributorPrivacyProcessor {
    type STATE = PrivacyState;
    type VALUE = PrivacyRequest;

    fn get_state(&self) -> Self::STATE {
        self.state.clone()
    }

    fn receiver(
        &mut self,
    ) -> ripple_sdk::tokio::sync::mpsc::Receiver<ripple_sdk::extn::extn_client_message::ExtnMessage>
    {
        self.streamer.receiver()
    }

    fn sender(
        &self,
    ) -> ripple_sdk::tokio::sync::mpsc::Sender<ripple_sdk::extn::extn_client_message::ExtnMessage>
    {
        self.streamer.sender()
    }
}

#[async_trait]
impl ExtnRequestProcessor for DistributorPrivacyProcessor {
    fn get_client(&self) -> ExtnClient {
        self.state.client.clone()
    }
    async fn process_request(
        state: Self::STATE,
        msg: ripple_sdk::extn::extn_client_message::ExtnMessage,
        extracted_message: Self::VALUE,
    ) -> bool {
        match extracted_message {
            PrivacyRequest::GetProperty(p) => {
                let resp = state.get_property(p);
                Self::respond(state.client.clone(), msg, ExtnResponse::Boolean(resp))
                    .await
                    .is_ok()
            }
            PrivacyRequest::SetProperty(p) => {
                state.set_property(p);
                Self::ack(state.client.clone(), msg).await.is_ok()
            }
            PrivacyRequest::GetProperties(_) => {
                let v = state.get_settings();
                Self::respond(
                    state.client.clone(),
                    msg,
                    v.get_extn_payload().as_response().unwrap(),
                )
                .await
                .is_ok()
            }
        }
    }
}