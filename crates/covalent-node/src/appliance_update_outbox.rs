//! Persist an exact choice before its only write attempt. Recovery is read-only.
use crate::{appliance_updates::*, private_state_file::PrivateStateFile};
use covalent_core::{PublicIdentity, appliance_updates::*};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, sync::Mutex};
use uuid::Uuid;

const MAX_RECORDS: usize = 32;

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateAction {
    pub order: u64,
    pub proof: SignedAutomaticUpdateChange,
    #[serde(deserialize_with = "nullable")]
    pub receipt: Option<AutomaticUpdateReceipt>,
    /// Fresh authenticated state proves an old request cannot change current policy.
    /// It does not establish whether that request ran before the observation.
    #[serde(deserialize_with = "nullable")]
    pub observed_context: Option<AutomaticUpdateContext>,
}
pub(crate) fn obsolete(context: &AutomaticUpdateContext, choice: &AutomaticUpdateChange) -> bool {
    context.validate().is_ok()
        && context.bond.appliance == choice.bond.appliance
        && context.bond.controller == choice.bond.controller
        && (context.bond != choice.bond
            || context
                .revision
                .is_some_and(|r| r != choice.expected_revision))
}
impl UpdateAction {
    pub(crate) fn choice(&self) -> Result<AutomaticUpdateChange, ()> {
        if self.proof.payload.len() > MAX_AUTOMATIC_UPDATE_CHANGE_BYTES {
            return Err(());
        }
        let c: AutomaticUpdateChange = serde_json::from_str(&self.proof.payload).map_err(|_| ())?;
        verify_automatic_update_change(&c.bond, c.expected_revision, &self.proof).map_err(|_| ())
    }
    pub fn resolved(&self) -> bool {
        self.receipt
            .as_ref()
            .is_some_and(|r| r.state == UpdateOutcome::Applied)
            || self.observed_context.is_some()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    schema_version: u16,
    controller: PublicIdentity,
    order: u64,
    actions: Vec<UpdateAction>,
}
pub(crate) struct UpdateOutbox {
    file: PrivateStateFile,
    state: Mutex<Option<Saved>>,
}
impl UpdateOutbox {
    #[cfg(test)]
    pub(crate) fn fail_once_after_publish(&self) {
        self.file.fail_once_after_publish();
    }
    pub(crate) fn open(path: PathBuf, controller: PublicIdentity) -> Option<Self> {
        let (file, bytes) = PrivateStateFile::open(path).ok()?;
        let fresh = bytes.is_none();
        let saved = match bytes {
            None => Saved {
                schema_version: 1,
                controller: controller.clone(),
                order: 0,
                actions: Vec::new(),
            },
            Some(bytes) => serde_json::from_slice::<Saved>(&bytes).ok()?,
        };
        if saved.schema_version != 1
            || saved.controller != controller
            || saved.actions.len() > MAX_RECORDS
            || saved.order > i64::MAX as u64
        {
            return None;
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut orders = std::collections::BTreeSet::new();
        for a in &saved.actions {
            let c = a.choice().ok()?;
            if c.bond.controller != controller
                || !ids.insert(c.action_id)
                || a.order == 0
                || a.order > saved.order
                || !orders.insert(a.order)
                || a.receipt
                    .as_ref()
                    .is_some_and(|r| r.validate_for(&a.proof).is_err())
                || a.observed_context
                    .as_ref()
                    .is_some_and(|context| !obsolete(context, &c))
            {
                return None;
            }
        }
        let value = Self {
            file,
            state: Mutex::new(Some(saved.clone())),
        };
        if fresh {
            let mut state = value.state.lock().ok()?;
            value.persist(&mut state, saved).ok()?;
        }
        Some(value)
    }
    fn ready(&self, current: &mut Option<Saved>) -> Result<(), ()> {
        let checked = (|| {
            self.file
                .matches(&serde_json::to_vec(current.as_ref().ok_or(())?).map_err(|_| ())?)
        })();
        if checked.is_err() {
            *current = None;
        }
        checked
    }
    fn persist(&self, current: &mut Option<Saved>, next: Saved) -> Result<(), ()> {
        let bytes = serde_json::to_vec(&next).map_err(|_| ())?;
        if self.file.persist(&bytes).is_err() {
            *current = None;
            return Err(());
        }
        *current = Some(next);
        Ok(())
    }
    pub(crate) fn list(&self) -> Result<Vec<UpdateAction>, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        Ok(state.as_ref().ok_or(())?.actions.clone())
    }
    pub(crate) fn get(&self, id: Uuid) -> Result<UpdateAction, ()> {
        self.list()?
            .into_iter()
            .find(|a| a.choice().is_ok_and(|c| c.action_id == id))
            .ok_or(())
    }
    /// False means this exact request already exists and must never be written again.
    pub(crate) fn reserve(
        &self,
        proof: SignedAutomaticUpdateChange,
        context: &AutomaticUpdateContext,
    ) -> Result<bool, ()> {
        let pending = UpdateAction {
            order: 1,
            proof,
            receipt: None,
            observed_context: None,
        };
        let c = pending.choice()?;
        context.validate().map_err(|_| ())?;
        if !context.configured
            || context.busy
            || context.bond != c.bond
            || context.revision != Some(c.expected_revision)
        {
            return Err(());
        }
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        let current = state.as_ref().ok_or(())?;
        if let Some(old) = current
            .actions
            .iter()
            .find(|a| a.choice().is_ok_and(|old| old.action_id == c.action_id))
        {
            return if old.proof == pending.proof {
                Ok(false)
            } else {
                Err(())
            };
        }
        if c.bond.controller != current.controller {
            return Err(());
        }
        let mut next = current.clone();
        if next.actions.len() >= MAX_RECORDS {
            let index = next
                .actions
                .iter()
                .enumerate()
                .filter(|(_, a)| a.resolved())
                .min_by_key(|(_, a)| a.order)
                .map(|(i, _)| i)
                .ok_or(())?;
            next.actions.remove(index);
        }
        next.order = next
            .order
            .checked_add(1)
            .filter(|v| *v <= i64::MAX as u64)
            .ok_or(())?;
        next.actions.push(UpdateAction {
            order: next.order,
            ..pending
        });
        self.persist(&mut state, next)?;
        Ok(true)
    }
    pub(crate) fn observe(&self, context: &AutomaticUpdateContext) -> Result<(), ()> {
        context.validate().map_err(|_| ())?;
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        let mut next = state.as_ref().ok_or(())?.clone();
        if next.controller != context.bond.controller {
            return Err(());
        }
        let mut changed = false;
        for action in &mut next.actions {
            if !action.resolved() && obsolete(context, &action.choice()?) {
                action.observed_context = Some(context.clone());
                changed = true;
            }
        }
        if changed {
            self.persist(&mut state, next)?;
        }
        Ok(())
    }
    pub(crate) fn record(
        &self,
        id: Uuid,
        receipt: AutomaticUpdateReceipt,
    ) -> Result<UpdateAction, ()> {
        let mut state = self.state.lock().map_err(|_| ())?;
        self.ready(&mut state)?;
        let mut next = state.as_ref().ok_or(())?.clone();
        let action = next
            .actions
            .iter_mut()
            .find(|a| a.choice().is_ok_and(|c| c.action_id == id))
            .ok_or(())?;
        receipt.validate_for(&action.proof).map_err(|_| ())?;
        if action
            .receipt
            .as_ref()
            .is_some_and(|r| r.state == UpdateOutcome::Applied)
        {
            return Ok(action.clone());
        }
        action.receipt = Some(receipt);
        let result = action.clone();
        self.persist(&mut state, next)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covalent_core::{DeviceIdentity, appliance_import::ImportApprovalBond};
    use sha2::{Digest as _, Sha256};
    use std::os::unix::fs::PermissionsExt as _;

    fn directory() -> tempfile::TempDir {
        tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap()
    }
    fn fixture(phone: &DeviceIdentity) -> AutomaticUpdateContext {
        AutomaticUpdateContext {
            schema_version: 1,
            bond: ImportApprovalBond {
                appliance: DeviceIdentity::generate().public_identity(),
                controller: phone.public_identity(),
                root_generation: Uuid::new_v4(),
                native_generation: Uuid::new_v4(),
            },
            configured: true,
            enabled: false,
            revision: Some(Uuid::new_v4()),
            busy: false,
        }
    }
    fn proof(
        phone: &DeviceIdentity,
        context: &AutomaticUpdateContext,
        enabled: bool,
    ) -> SignedAutomaticUpdateChange {
        sign_automatic_update_change(
            phone,
            &AutomaticUpdateChange {
                schema_version: 1,
                bond: context.bond.clone(),
                action_id: Uuid::new_v4(),
                expected_revision: context.revision.unwrap(),
                enabled,
            },
        )
        .unwrap()
    }
    fn receipt(
        proof: &SignedAutomaticUpdateChange,
        state: UpdateOutcome,
    ) -> AutomaticUpdateReceipt {
        let c: AutomaticUpdateChange = serde_json::from_str(&proof.payload).unwrap();
        AutomaticUpdateReceipt {
            schema_version: 1,
            state,
            action_id: c.action_id,
            payload_digest: format!("{:x}", Sha256::digest(proof.payload.as_bytes())),
            revision: if state == UpdateOutcome::Applied {
                Uuid::new_v4()
            } else {
                c.expected_revision
            },
            enabled: c.enabled,
        }
    }
    fn id(p: &SignedAutomaticUpdateChange) -> Uuid {
        serde_json::from_str::<AutomaticUpdateChange>(&p.payload)
            .unwrap()
            .action_id
    }
    #[test]
    fn android_contract_fixture_contains_valid_node_signed_proof_and_exact_receipts() {
        let f: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/appliance-updates.json")).unwrap();
        let context: AutomaticUpdateContext =
            serde_json::from_value(f["initial"]["context"].clone()).unwrap();
        context.validate().unwrap();
        let pending: UpdateAction = serde_json::from_value(f["pending"].clone()).unwrap();
        let choice = pending.choice().unwrap();
        assert_eq!(choice.bond, context.bond);
        assert_eq!(Some(choice.expected_revision), context.revision);
        assert!(!pending.resolved());
        let applied: UpdateAction = serde_json::from_value(f["applied"].clone()).unwrap();
        assert_eq!(applied.proof, pending.proof);
        assert!(applied.resolved());
        applied
            .receipt
            .as_ref()
            .unwrap()
            .validate_for(&pending.proof)
            .unwrap();
        let current: AutomaticUpdateContext =
            serde_json::from_value(f["current"]["context"].clone()).unwrap();
        current.validate().unwrap();
        assert_eq!(current.revision, Some(applied.receipt.unwrap().revision));
        assert!(obsolete(&current, &choice));
    }
    #[test]
    fn exact_choice_survives_restart_and_never_gets_another_first_send() {
        let d = directory();
        let path = d.path().join("updates.json");
        let phone = DeviceIdentity::generate();
        let context = fixture(&phone);
        let p = proof(&phone, &context, true);
        {
            let journal = UpdateOutbox::open(path.clone(), phone.public_identity()).unwrap();
            assert!(journal.reserve(p.clone(), &context).unwrap());
            assert!(!journal.reserve(p.clone(), &context).unwrap());
            assert!(UpdateOutbox::open(path.clone(), phone.public_identity()).is_none());
        }
        let journal = UpdateOutbox::open(path.clone(), phone.public_identity()).unwrap();
        assert_eq!(journal.get(id(&p)).unwrap().proof, p);
        assert!(!journal.reserve(p.clone(), &context).unwrap());
        let unknown = receipt(&p, UpdateOutcome::NotObserved);
        assert!(!journal.record(id(&p), unknown.clone()).unwrap().resolved());
        let applied = receipt(&p, UpdateOutcome::Applied);
        assert!(journal.record(id(&p), applied.clone()).unwrap().resolved());
        assert_eq!(
            journal.record(id(&p), unknown).unwrap().receipt,
            Some(applied)
        );
        let mut altered = serde_json::from_str::<AutomaticUpdateChange>(&p.payload).unwrap();
        altered.enabled = false;
        assert!(
            journal
                .reserve(
                    sign_automatic_update_change(&phone, &altered).unwrap(),
                    &context
                )
                .is_err()
        );
        drop(journal);
        assert!(
            UpdateOutbox::open(path, phone.public_identity())
                .unwrap()
                .get(id(&p))
                .unwrap()
                .resolved()
        );
    }
    #[test]
    fn new_explicit_choice_is_possible_while_old_result_stays_unknown() {
        let d = directory();
        let phone = DeviceIdentity::generate();
        let mut context = fixture(&phone);
        let journal =
            UpdateOutbox::open(d.path().join("updates.json"), phone.public_identity()).unwrap();
        let old = proof(&phone, &context, true);
        assert!(journal.reserve(old.clone(), &context).unwrap());
        journal.observe(&context).unwrap();
        assert!(!journal.get(id(&old)).unwrap().resolved());
        let keep_paused = proof(&phone, &context, false);
        assert!(journal.reserve(keep_paused.clone(), &context).unwrap());
        assert!(!journal.get(id(&old)).unwrap().resolved());
        let applied = receipt(&keep_paused, UpdateOutcome::Applied);
        journal.record(id(&keep_paused), applied.clone()).unwrap();
        context.revision = Some(applied.revision);
        context.enabled = false;
        journal.observe(&context).unwrap();
        let observed = journal.get(id(&old)).unwrap();
        assert!(observed.resolved() && observed.receipt.is_none());
        assert_eq!(observed.observed_context, Some(context.clone()));
        assert!(
            verify_automatic_update_change(&context.bond, context.revision.unwrap(), &old).is_err()
        );
        let next = proof(&phone, &context, true);
        assert!(journal.reserve(next.clone(), &context).unwrap());
        context.bond.root_generation = Uuid::new_v4();
        journal.observe(&context).unwrap();
        assert!(journal.get(id(&next)).unwrap().resolved());
        assert!(
            verify_automatic_update_change(&context.bond, context.revision.unwrap(), &next)
                .is_err()
        );
        let different = fixture(&phone);
        assert!(!obsolete(&different, &observed.choice().unwrap()));
    }
    #[test]
    fn uncertain_publication_is_retained_and_reopened_without_resending() {
        let d = directory();
        let path = d.path().join("updates.json");
        let phone = DeviceIdentity::generate();
        let context = fixture(&phone);
        let p = proof(&phone, &context, true);
        let journal = UpdateOutbox::open(path.clone(), phone.public_identity()).unwrap();
        journal.fail_once_after_publish();
        assert!(journal.reserve(p.clone(), &context).is_err());
        assert!(journal.list().is_err());
        drop(journal);
        let journal = UpdateOutbox::open(path, phone.public_identity()).unwrap();
        assert_eq!(journal.get(id(&p)).unwrap().proof, p);
        assert!(!journal.reserve(p.clone(), &context).unwrap());
        assert!(!journal.get(id(&p)).unwrap().resolved());
    }
    #[test]
    fn unsafe_replaced_missing_or_modified_journals_never_reset_or_allow_writes() {
        for attack in [
            "contents", "mode", "hardlink", "symlink", "missing", "lock", "parent",
        ] {
            let d = directory();
            let path = d.path().join("updates.json");
            let phone = DeviceIdentity::generate();
            let context = fixture(&phone);
            let p = proof(&phone, &context, true);
            let journal = UpdateOutbox::open(path.clone(), phone.public_identity()).unwrap();
            journal.reserve(p.clone(), &context).unwrap();
            match attack {
                "contents" => std::fs::write(&path, b"broken").unwrap(),
                "mode" => {
                    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap()
                }
                "hardlink" => std::fs::hard_link(&path, d.path().join("copy")).unwrap(),
                "symlink" => {
                    std::fs::rename(&path, d.path().join("saved")).unwrap();
                    std::os::unix::fs::symlink(d.path().join("saved"), &path).unwrap();
                }
                "missing" => std::fs::remove_file(&path).unwrap(),
                "lock" => {
                    std::fs::remove_file(path.with_extension("lock")).unwrap();
                    std::fs::write(path.with_extension("lock"), b"").unwrap();
                    std::fs::set_permissions(
                        path.with_extension("lock"),
                        std::fs::Permissions::from_mode(0o600),
                    )
                    .unwrap();
                }
                "parent" => {
                    std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755))
                        .unwrap()
                }
                _ => unreachable!(),
            }
            assert!(journal.list().is_err(), "{attack}");
            assert!(
                journal
                    .reserve(proof(&phone, &context, false), &context)
                    .is_err()
            );
            drop(journal);
            if attack != "lock" {
                assert!(
                    UpdateOutbox::open(path.clone(), phone.public_identity()).is_none(),
                    "{attack}"
                );
            }
            if attack == "contents" {
                assert_eq!(std::fs::read(path).unwrap(), b"broken");
            }
        }
    }
    #[test]
    fn invalid_saved_proofs_receipts_and_contexts_are_preserved_and_refused() {
        for attack in [
            "duplicate",
            "unknown",
            "oversize",
            "wrong-phone",
            "bad-receipt",
            "same-revision",
            "bad-proof",
        ] {
            let d = directory();
            let path = d.path().join("updates.json");
            let phone = DeviceIdentity::generate();
            let context = fixture(&phone);
            let p = proof(&phone, &context, true);
            {
                let journal = UpdateOutbox::open(path.clone(), phone.public_identity()).unwrap();
                journal.reserve(p.clone(), &context).unwrap();
            }
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            match attack {
                "duplicate" => {
                    let a = value["actions"][0].clone();
                    value["actions"].as_array_mut().unwrap().push(a);
                }
                "unknown" => value["token"] = "forbidden".into(),
                "oversize" => {}
                "wrong-phone" => {
                    value["controller"] =
                        serde_json::to_value(DeviceIdentity::generate().public_identity()).unwrap()
                }
                "bad-receipt" => {
                    let mut r = receipt(&p, UpdateOutcome::Applied);
                    r.action_id = Uuid::new_v4();
                    value["actions"][0]["receipt"] = serde_json::to_value(r).unwrap();
                }
                "same-revision" => {
                    value["actions"][0]["observedContext"] = serde_json::to_value(&context).unwrap()
                }
                "bad-proof" => value["actions"][0]["proof"]["signature"] = "invalid".into(),
                _ => unreachable!(),
            }
            let bytes = if attack == "oversize" {
                vec![b' '; 131073]
            } else {
                serde_json::to_vec(&value).unwrap()
            };
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                UpdateOutbox::open(path.clone(), phone.public_identity()).is_none(),
                "{attack}"
            );
            assert_eq!(std::fs::read(path).unwrap(), bytes);
        }
    }
    #[test]
    fn history_bounds_preserve_unknown_commands_and_prune_only_resolved_records() {
        let d = directory();
        let phone = DeviceIdentity::generate();
        let context = fixture(&phone);
        let journal =
            UpdateOutbox::open(d.path().join("updates.json"), phone.public_identity()).unwrap();
        let mut proofs = Vec::new();
        for _ in 0..32 {
            let p = proof(&phone, &context, true);
            assert!(journal.reserve(p.clone(), &context).unwrap());
            proofs.push(p);
        }
        assert!(
            journal
                .reserve(proof(&phone, &context, true), &context)
                .is_err()
        );
        assert_eq!(journal.list().unwrap().len(), 32);
        journal
            .record(id(&proofs[5]), receipt(&proofs[5], UpdateOutcome::Applied))
            .unwrap();
        assert!(
            journal
                .reserve(proof(&phone, &context, true), &context)
                .unwrap()
        );
        assert!(journal.get(id(&proofs[5])).is_err());
        assert!(journal.get(id(&proofs[0])).is_ok());
    }
}
