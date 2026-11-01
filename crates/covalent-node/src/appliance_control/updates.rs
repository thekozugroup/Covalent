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
