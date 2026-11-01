use super::*;
use crate::appliance_updates::*;
use crate::sync_control::FolderControlError;
use covalent_core::appliance_updates::{
    AutomaticUpdateChange, SignedAutomaticUpdateChange, verify_automatic_update_change,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateStatus {
    pub schema_version: u16,
    pub appliance_id: DeviceId,
    pub controller_id: DeviceId,
    pub bond_generation: Uuid,
    pub updates_supported: bool,
    pub updates_available: bool,
    #[serde(deserialize_with = "super::required_nullable")]
    pub context: Option<AutomaticUpdateContext>,
}
impl UpdateStatus {
    fn valid(&self, pi: &PublicIdentity, phone: &PublicIdentity) -> bool {
        self.schema_version == 1
            && self.appliance_id == pi.device_id
            && self.controller_id == phone.device_id
            && !self.bond_generation.is_nil()
            && (self.updates_supported || !self.updates_available)
            && self.updates_available == self.context.is_some()
            && self.context.as_ref().is_none_or(|c| {
                c.validate().is_ok()
                    && c.bond.appliance == *pi
                    && c.bond.controller == *phone
                    && c.bond.native_generation == self.bond_generation
            })
    }
}
impl ControllerBondStore {
    pub(crate) async fn read_updates(
        &self,
        engine: &Engine,
        requester: DeviceId,
    ) -> Result<UpdateStatus, FolderControlError> {
        let before = self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?;
        let _permit = self
            .host_ops
            .clone()
            .try_acquire_owned()
            .map_err(|_| FolderControlError::Busy)?;
        let context = match self.updates.get() {
            None => None,
            Some(host) => tokio::time::timeout(std::time::Duration::from_secs(8), host.context())
                .await
                .ok()
                .and_then(Result::ok)
                .filter(|c| {
                    c.validate().is_ok()
                        && c.bond.appliance == before.appliance
                        && Some(&c.bond.controller) == before.controller.as_ref()
                        && c.bond.native_generation == before.generation
                }),
        };
        if self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?
            != before
        {
            return Err(FolderControlError::Rejected);
        }
        Ok(UpdateStatus {
            schema_version: 1,
            appliance_id: engine.device_id(),
            controller_id: requester,
            bond_generation: before.generation,
            updates_supported: self.updates.get().is_some(),
            updates_available: context.is_some(),
            context,
        })
    }
    pub(crate) async fn update_action(
        &self,
        engine: &Engine,
        requester: DeviceId,
        operation: UpdateOperation,
        proof: &SignedAutomaticUpdateChange,
    ) -> Result<AutomaticUpdateReceipt, FolderControlError> {
        let before = self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?;
        if proof.payload.len() > covalent_core::appliance_updates::MAX_AUTOMATIC_UPDATE_CHANGE_BYTES
        {
            return Err(FolderControlError::Rejected);
        }
        let choice: AutomaticUpdateChange =
            serde_json::from_str(&proof.payload).map_err(|_| FolderControlError::Rejected)?;
        // This checks the approved peer's intent, not the trusted root revision.
        // The privileged host must independently verify its root bond and state.
        if choice.bond.appliance != before.appliance
            || Some(&choice.bond.controller) != before.controller.as_ref()
            || choice.bond.native_generation != before.generation
            || verify_automatic_update_change(&choice.bond, choice.expected_revision, proof)
                .is_err()
        {
            return Err(FolderControlError::Rejected);
        }
        let _permit = self
            .host_ops
            .clone()
            .try_acquire_owned()
            .map_err(|_| FolderControlError::Busy)?;
        let host = self.updates.get().ok_or(FolderControlError::Unavailable)?;
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            host.action(operation, proof),
        )
        .await
        .map_err(|_| FolderControlError::TimedOut)?
        .map_err(|_| FolderControlError::Unavailable)?;
        if receipt.validate_for(proof).is_err()
            || self
                .authorized(engine, requester)
                .map_err(|_| FolderControlError::Rejected)?
                != before
        {
            return Err(FolderControlError::Rejected);
        }
        Ok(receipt)
    }
}

fn outbox(state: &AppState) -> Result<&crate::appliance_update_outbox::UpdateOutbox, ApiError> {
    state
        .update_outbox
        .as_deref()
        .ok_or_else(|| error(BondError::Unavailable))
}
async fn context(state: &AppState, peer: DeviceId) -> Result<UpdateStatus, ApiError> {
    use crate::sync_control::{
        FolderControlOperation as Op, FolderControlPayload as Payload, send_folder_control,
    };
    // Damaged/lost durable state must fail before any recovery or mutation.
    outbox(state)?
        .list()
        .map_err(|_| error(BondError::Unavailable))?;
    let pi = crate::sync_control::current_identity(&state.engine, peer)
        .map_err(|_| error(BondError::Rejected))?;
    let value = send_folder_control(
        Arc::clone(&state.engine),
        target(state, peer)?,
        Op::ReadApplianceUpdates {
            requester_id: state.engine.device_id(),
            target_id: peer,
        },
    )
    .await
    .map_err(|_| error(BondError::Unavailable))?;
    match value {
        Payload::ApplianceUpdates(status) if status.valid(&pi, &state.engine.public_identity()) => {
            if let Some(c) = &status.context {
                outbox(state)?
                    .observe(c)
                    .map_err(|_| error(BondError::Unavailable))?;
            }
            Ok(status)
        }
        Payload::Rejected => Err(error(BondError::Rejected)),
        _ => Err(error(BondError::Unavailable)),
    }
}
pub(crate) async fn update_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RemoteStatusRequest>,
) -> Result<axum::Json<UpdateStatus>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(context(&state, request.peer_id).await?))
}
pub(crate) async fn update_actions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<axum::Json<serde_json::Value>, ApiError> {
    authorize(&state, &headers)?;
    let actions = outbox(&state)?
        .list()
        .map_err(|_| error(BondError::Unavailable))?;
    Ok(axum::Json(
        serde_json::json!({"schemaVersion":1,"actions":actions}),
    ))
}
async fn dispatch(
    state: &AppState,
    id: Uuid,
    operation: UpdateOperation,
) -> Result<crate::appliance_update_outbox::UpdateAction, ApiError> {
    use crate::sync_control::{
        FolderControlOperation as Op, FolderControlPayload as Payload, send_folder_control,
    };
    let saved = outbox(state)?
        .get(id)
        .map_err(|_| error(BondError::Unavailable))?;
    if saved.resolved() {
        return Ok(saved);
    }
    let c = saved.choice().map_err(|_| error(BondError::Unavailable))?;
    let pi = crate::sync_control::current_identity(&state.engine, c.bond.appliance.device_id)
        .map_err(|_| error(BondError::Rejected))?;
    if pi != c.bond.appliance || c.bond.controller != state.engine.public_identity() {
        return Err(error(BondError::Rejected));
    }
    let reply = send_folder_control(
        Arc::clone(&state.engine),
        target(state, pi.device_id)?,
        Op::ApplianceUpdate {
            requester_id: state.engine.device_id(),
            target_id: pi.device_id,
            operation,
            proof: saved.proof.clone(),
        },
    )
    .await;
    match reply {
        Ok(Payload::ApplianceUpdateReceipt(receipt))
            if receipt.validate_for(&saved.proof).is_ok() =>
        {
            outbox(state)?
                .record(id, receipt)
                .map_err(|_| error(BondError::Unavailable))
        }
        // A timeout/refusal/unknown reply does not authorize a second write.
        _ => outbox(state)?
            .get(id)
            .map_err(|_| error(BondError::Unavailable)),
    }
}
pub(crate) async fn confirm_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(choice): ContractJson<AutomaticUpdateChange>,
) -> Result<axum::Json<crate::appliance_update_outbox::UpdateAction>, ApiError> {
    authorize(&state, &headers)?;
    if choice.bond.controller != state.engine.public_identity() {
        return Err(error(BondError::Rejected));
    }
    let bytes = choice
        .canonical_bytes()
        .map_err(|_| error(BondError::Rejected))?;
    let proof = SignedAutomaticUpdateChange {
        signature: state.engine.sign_transport_transcript_with_domain(
            covalent_core::appliance_updates::AUTOMATIC_UPDATES_DOMAIN,
            &bytes,
        ),
        payload: String::from_utf8(bytes).map_err(|_| error(BondError::Rejected))?,
    };
    if let Some(saved) = outbox(&state)?
        .list()
        .map_err(|_| error(BondError::Unavailable))?
        .into_iter()
        .find(|a| a.choice().is_ok_and(|c| c.action_id == choice.action_id))
    {
        if saved.proof != proof {
            return Err(error(BondError::Conflict));
        }
        return Ok(axum::Json(saved));
    }
    let current = context(&state, choice.bond.appliance.device_id)
        .await?
        .context
        .ok_or_else(|| error(BondError::Unavailable))?;
    if current.busy
        || !current.configured
        || current.bond != choice.bond
        || current.revision != Some(choice.expected_revision)
    {
        return Err(error(BondError::Conflict));
    }
    let first = outbox(&state)?
        .reserve(proof, &current)
        .map_err(|_| error(BondError::Conflict))?;
    Ok(axum::Json(if first {
        dispatch(&state, choice.action_id, UpdateOperation::Apply).await?
    } else {
        outbox(&state)?
            .get(choice.action_id)
            .map_err(|_| error(BondError::Unavailable))?
    }))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UpdateActionRequest {
    action_id: Uuid,
}
pub(crate) async fn check_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<UpdateActionRequest>,
) -> Result<axum::Json<crate::appliance_update_outbox::UpdateAction>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(
        dispatch(&state, request.action_id, UpdateOperation::Lookup).await?,
    ))
}
