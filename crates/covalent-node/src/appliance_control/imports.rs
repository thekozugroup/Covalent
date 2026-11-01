use super::*;
use crate::appliance_import_outbox::ImportAction;
use crate::appliance_imports::*;
use crate::sync_control::{
    FolderControlError, FolderControlOperation, FolderControlPayload, send_folder_control,
};
use covalent_core::appliance_import::{
    ImportApproval, SignedImportApproval, verify_import_approval, verify_import_withdrawal,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportStatus {
    pub schema_version: u16,
    pub appliance_id: DeviceId,
    pub controller_id: DeviceId,
    pub bond_generation: Uuid,
    pub imports_supported: bool,
    pub imports_available: bool,
    #[serde(deserialize_with = "super::required_nullable")]
    pub context: Option<ImportContext>,
}
impl ImportStatus {
    fn valid(&self, pi: &PublicIdentity, phone: &PublicIdentity) -> bool {
        self.schema_version == 1
            && self.appliance_id == pi.device_id
            && self.controller_id == phone.device_id
            && !self.bond_generation.is_nil()
            && (self.imports_supported || !self.imports_available)
            && self.imports_available == self.context.is_some()
            && self.context.as_ref().is_none_or(|c| {
                c.validate().is_ok()
                    && c.bond.appliance == *pi
                    && c.bond.controller == *phone
                    && c.bond.native_generation == self.bond_generation
            })
    }
}
impl ControllerBondStore {
    pub(crate) async fn read_imports(
        &self,
        engine: &Engine,
        requester: DeviceId,
        context_version: Option<u16>,
    ) -> Result<ImportStatus, FolderControlError> {
        let version = context_version.unwrap_or(1);
        if !matches!(version, 1 | 2) {
            return Err(FolderControlError::Rejected);
        }
        let before = self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?;
        let _permit = self
            .host_ops
            .clone()
            .try_acquire_owned()
            .map_err(|_| FolderControlError::Busy)?;
        let context = match self.imports.get() {
            None => None,
            Some(host) => tokio::time::timeout(std::time::Duration::from_secs(8), host.context())
                .await
                .ok()
                .and_then(Result::ok)
                .and_then(|c| c.for_version(version).ok())
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
        Ok(ImportStatus {
            schema_version: 1,
            appliance_id: engine.device_id(),
            controller_id: requester,
            bond_generation: before.generation,
            imports_supported: self.imports.get().is_some(),
            imports_available: context.is_some(),
            context,
        })
    }
    pub(crate) async fn import_action(
        &self,
        engine: &Engine,
        requester: DeviceId,
        operation: ImportOperation,
        proof: &SignedImportApproval,
        withdrawal_signature: Option<&str>,
    ) -> Result<ImportReceipt, FolderControlError> {
        let before = self
            .authorized(engine, requester)
            .map_err(|_| FolderControlError::Rejected)?;
        let choice: ImportApproval =
            serde_json::from_str(&proof.payload).map_err(|_| FolderControlError::Rejected)?;
        if choice.bond.appliance != before.appliance
            || Some(&choice.bond.controller) != before.controller.as_ref()
            || choice.bond.native_generation != before.generation
            || verify_import_approval(&choice.bond, proof).is_err()
        {
            return Err(FolderControlError::Rejected);
        }
        if operation == ImportOperation::Withdraw {
            if withdrawal_signature
                .is_none_or(|s| verify_import_withdrawal(&choice.bond, proof, s).is_err())
            {
                return Err(FolderControlError::Rejected);
            }
        } else if withdrawal_signature.is_some() {
            return Err(FolderControlError::Rejected);
        }
        let _permit = self
            .host_ops
            .clone()
            .try_acquire_owned()
            .map_err(|_| FolderControlError::Busy)?;
        let host = self.imports.get().ok_or(FolderControlError::Unavailable)?;
        let receipt = tokio::time::timeout(
            std::time::Duration::from_secs(8),
            host.action(operation, proof, withdrawal_signature),
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
fn outbox(state: &AppState) -> Result<&crate::appliance_import_outbox::ImportOutbox, ApiError> {
    state
        .import_outbox
        .as_deref()
        .ok_or_else(|| error(BondError::Unavailable))
}
async fn context(state: &AppState, peer: DeviceId) -> Result<ImportStatus, ApiError> {
    let pi = crate::sync_control::current_identity(&state.engine, peer)
        .map_err(|_| error(BondError::Rejected))?;
    let binding = target(state, peer)?;
    let read = |context_version| {
        send_folder_control(
            Arc::clone(&state.engine),
            binding.clone(),
            FolderControlOperation::ReadApplianceImports {
                requester_id: state.engine.device_id(),
                target_id: peer,
                context_version,
            },
        )
    };
    // Old decoders close an unknown read frame without a reply. Only that transport
    // outcome permits one legacy read; refusals, invalid replies and writes never retry.
    let value = tokio::time::timeout(crate::sync_control::EXCHANGE_TIMEOUT, async {
        match read(Some(2)).await {
            Err(FolderControlError::NoResponse) => read(None).await,
            result => result,
        }
    })
    .await
    .map_err(|_| error(BondError::Unavailable))?
    .map_err(|_| error(BondError::Unavailable))?;
    match value {
        FolderControlPayload::ApplianceImports(value)
            if value.valid(&pi, &state.engine.public_identity()) =>
        {
            Ok(value)
        }
        FolderControlPayload::Rejected => Err(error(BondError::Rejected)),
        _ => Err(error(BondError::Unavailable)),
    }
}
pub(crate) async fn import_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<RemoteStatusRequest>,
) -> Result<axum::Json<ImportStatus>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(context(&state, request.peer_id).await?))
}
pub(crate) async fn import_actions(
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
async fn action(
    state: &AppState,
    id: Uuid,
    operation: ImportOperation,
) -> Result<ImportAction, ApiError> {
    let saved = outbox(state)?
        .get(id)
        .map_err(|_| error(BondError::Unavailable))?;
    let choice = saved.choice().map_err(|_| error(BondError::Unavailable))?;
    let pi = crate::sync_control::current_identity(&state.engine, choice.bond.appliance.device_id)
        .map_err(|_| error(BondError::Rejected))?;
    if pi != choice.bond.appliance || choice.bond.controller != state.engine.public_identity() {
        return Err(error(BondError::Rejected));
    }
    let withdrawal_signature = if operation == ImportOperation::Withdraw {
        saved.withdrawal_signature.clone()
    } else {
        None
    };
    let result = send_folder_control(
        Arc::clone(&state.engine),
        target(state, pi.device_id)?,
        FolderControlOperation::ApplianceImport {
            requester_id: state.engine.device_id(),
            target_id: pi.device_id,
            operation,
            proof: saved.proof.clone(),
            withdrawal_signature,
        },
    )
    .await;
    match result {
        Ok(FolderControlPayload::ApplianceImportReceipt(receipt))
            if receipt.validate_for(&saved.proof).is_ok() =>
        {
            outbox(state)?
                .record(id, receipt)
                .map_err(|_| error(BondError::Unavailable))
        }
        // A missing/refused acknowledgement is not permission to repeat the mutation.
        _ => outbox(state)?
            .get(id)
            .map_err(|_| error(BondError::Unavailable)),
    }
}
pub(crate) async fn confirm_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(choice): ContractJson<ImportApproval>,
) -> Result<axum::Json<ImportAction>, ApiError> {
    authorize(&state, &headers)?;
    if choice.bond.controller != state.engine.public_identity() {
        return Err(error(BondError::Rejected));
    }
    let bytes = choice
        .canonical_bytes()
        .map_err(|_| error(BondError::Rejected))?;
    let proof = SignedImportApproval {
        payload: String::from_utf8(bytes.clone()).map_err(|_| error(BondError::Rejected))?,
        signature: state
            .engine
            .sign_transport_transcript_with_domain(choice.approval_domain(), &bytes),
    };
    if let Some(old) = outbox(&state)?
        .list()
        .map_err(|_| error(BondError::Unavailable))?
        .into_iter()
        .find(|a| a.choice().is_ok_and(|c| c.action_id == choice.action_id))
    {
        if old.proof != proof {
            return Err(error(BondError::Conflict));
        }
        return Ok(axum::Json(old));
    }
    let fresh = context(&state, choice.bond.appliance.device_id)
        .await?
        .context
        .ok_or_else(|| error(BondError::Unavailable))?;
    if !fresh.admits(&choice) {
        return Err(error(BondError::Conflict));
    }
    let source = fresh
        .sources
        .into_iter()
        .find(|s| s.source_id == choice.source_id)
        .ok_or_else(|| error(BondError::Conflict))?;
    let first = outbox(&state)?
        .reserve(proof, source)
        .map_err(|_| error(BondError::Conflict))?;
    Ok(axum::Json(if first {
        action(&state, choice.action_id, ImportOperation::Approve).await?
    } else {
        outbox(&state)?
            .get(choice.action_id)
            .map_err(|_| error(BondError::Unavailable))?
    }))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ActionRequest {
    action_id: Uuid,
}
pub(crate) async fn check_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<ActionRequest>,
) -> Result<axum::Json<ImportAction>, ApiError> {
    authorize(&state, &headers)?;
    Ok(axum::Json(
        action(&state, request.action_id, ImportOperation::Lookup).await?,
    ))
}
pub(crate) async fn withdraw_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    ContractJson(request): ContractJson<ActionRequest>,
) -> Result<axum::Json<ImportAction>, ApiError> {
    authorize(&state, &headers)?;
    let saved = outbox(&state)?
        .get(request.action_id)
        .map_err(|_| error(BondError::Unavailable))?;
    let choice = saved.choice().map_err(|_| error(BondError::Unavailable))?;
    let signature = state.engine.sign_transport_transcript_with_domain(
        choice.withdrawal_domain(),
        saved.proof.payload.as_bytes(),
    );
    let first = outbox(&state)?
        .withdraw(request.action_id, signature)
        .map_err(|_| error(BondError::Unavailable))?;
    Ok(axum::Json(
        action(
            &state,
            request.action_id,
            if first {
                ImportOperation::Withdraw
            } else {
                ImportOperation::Lookup
            },
        )
        .await?,
    ))
}
