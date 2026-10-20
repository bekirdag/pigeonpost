//! wasm-bindgen surface of `pigeonpost-groupcrypto` for the web inbox.
//!
//! Byte fields are `Uint8Array`s, counters are numbers, records are plain objects with
//! camelCase keys. Failures throw an `Error` whose `name` is `GroupCryptoError` and whose
//! `code` is the stable machine code shared with every other binding.

use js_sys::{Array, Object, Reflect, Uint8Array};
use pigeonpost_groupcrypto as core;
use wasm_bindgen::prelude::*;

type Result<T> = std::result::Result<T, JsValue>;

fn err(e: core::Error) -> JsValue {
    let error = js_sys::Error::new(&e.to_string());
    error.set_name("GroupCryptoError");
    let _ = Reflect::set(&error, &"code".into(), &e.code().into());
    error.into()
}

fn invalid(message: &str) -> JsValue {
    err(core::Error::InvalidInput(message.to_owned()))
}

fn bytes(b: &[u8]) -> JsValue {
    Uint8Array::from(b).into()
}

struct Obj(Object);

impl Obj {
    fn new() -> Self {
        Obj(Object::new())
    }
    fn set(self, key: &str, value: impl Into<JsValue>) -> Self {
        let _ = Reflect::set(&self.0, &key.into(), &value.into());
        self
    }
    fn done(self) -> JsValue {
        self.0.into()
    }
}

fn get(obj: &JsValue, key: &str) -> Result<JsValue> {
    Reflect::get(obj, &key.into())
}

fn get_string(obj: &JsValue, key: &str) -> Result<String> {
    get(obj, key)?
        .as_string()
        .ok_or_else(|| invalid(&format!("{key} must be a string")))
}

fn get_bytes(obj: &JsValue, key: &str) -> Result<Vec<u8>> {
    to_bytes(&get(obj, key)?, key)
}

fn to_bytes(value: &JsValue, what: &str) -> Result<Vec<u8>> {
    value
        .dyn_ref::<Uint8Array>()
        .map(Uint8Array::to_vec)
        .ok_or_else(|| invalid(&format!("{what} must be a Uint8Array")))
}

fn byte_list(array: &Array, what: &str) -> Result<Vec<Vec<u8>>> {
    array.iter().map(|v| to_bytes(&v, what)).collect()
}

fn arr<const N: usize>(b: &[u8], what: &str) -> Result<[u8; N]> {
    b.try_into()
        .map_err(|_| invalid(&format!("{what} must be {N} bytes")))
}

fn member(m: &core::MemberInfo) -> JsValue {
    Obj::new()
        .set("mailbox", m.mailbox.as_str())
        .set("deviceId", m.device_id.as_str())
        .set("leafIndex", m.leaf_index)
        .set("signatureKey", bytes(&m.signature_key))
        .set("fingerprint", bytes(&m.fingerprint))
        .done()
}

fn members(v: &[core::MemberInfo]) -> JsValue {
    v.iter().map(member).collect::<Array>().into()
}

fn attachment(a: &core::AttachmentRef) -> JsValue {
    Obj::new()
        .set("key", bytes(&a.key))
        .set("sealedSha256", bytes(&a.sealed_sha256))
        .set("plaintextLen", a.plaintext_len as f64)
        .done()
}

fn attachment_in(v: &JsValue) -> Result<core::AttachmentRef> {
    let len = get(v, "plaintextLen")?
        .as_f64()
        .ok_or_else(|| invalid("plaintextLen must be a number"))?;
    if !(0.0..=9_007_199_254_740_991.0).contains(&len) || len.fract() != 0.0 {
        return Err(invalid("plaintextLen out of range"));
    }
    Ok(core::AttachmentRef {
        key: arr(&get_bytes(v, "key")?, "key")?,
        sealed_sha256: arr(&get_bytes(v, "sealedSha256")?, "sealedSha256")?,
        plaintext_len: len as u64,
    })
}

fn joined(j: &core::Joined) -> JsValue {
    Obj::new()
        .set("groupId", j.group_id.as_str())
        .set("epoch", j.epoch as f64)
        .set("members", members(&j.members))
        .done()
}

fn outgoing(m: &core::OutgoingMessage) -> JsValue {
    Obj::new()
        .set("groupId", m.group_id.as_str())
        .set("epoch", m.epoch as f64)
        .set("clientMessageId", m.client_message_id.as_str())
        .set("ciphertext", bytes(&m.ciphertext))
        .set("envelopeSignature", bytes(&m.envelope_signature))
        .done()
}

fn commit_bundle(b: &core::CommitBundle) -> JsValue {
    Obj::new()
        .set("groupId", b.group_id.as_str())
        .set("epoch", b.epoch as f64)
        .set("commitId", b.commit_id.as_str())
        .set("commit", bytes(&b.commit))
        .set(
            "welcome",
            b.welcome.as_deref().map(bytes).unwrap_or(JsValue::NULL),
        )
        .set(
            "welcomeRecipients",
            b.welcome_recipients
                .iter()
                .map(|r| bytes(r))
                .collect::<Array>(),
        )
        .set("envelopeSignature", bytes(&b.envelope_signature))
        .set("added", members(&b.added))
        .set("removed", members(&b.removed))
        .done()
}

fn outcome(o: &core::CommitOutcome) -> JsValue {
    Obj::new()
        .set("groupId", o.group_id.as_str())
        .set("committer", member(&o.committer))
        .set("commitId", o.commit_id.as_str())
        .set("epoch", o.epoch as f64)
        .set("added", members(&o.added))
        .set("removed", members(&o.removed))
        .set("updated", members(&o.updated))
        .set("selfRemoved", o.self_removed)
        .done()
}

fn history_offer(o: &core::HistoryOffer) -> JsValue {
    Obj::new()
        .set("groupId", o.group_id.as_str())
        .set("sender", member(&o.sender))
        .set("clientMessageId", o.client_message_id.as_str())
        .set("archive", attachment(&o.archive))
        .done()
}

/// One enrolled device and all of its group state.
#[wasm_bindgen]
pub struct GroupDevice {
    inner: core::Device,
}

#[wasm_bindgen]
impl GroupDevice {
    /// New unattested device for `mailbox` (a `/k/` address).
    #[wasm_bindgen(constructor)]
    pub fn new(mailbox: &str, device_id: &str) -> Result<GroupDevice> {
        Ok(Self {
            inner: core::Device::new(mailbox, device_id).map_err(err)?,
        })
    }

    /// New device attested with the mailbox identity seed.
    #[wasm_bindgen(js_name = newWithIdentitySeed)]
    pub fn new_with_identity_seed(identity_seed: &[u8], device_id: &str) -> Result<GroupDevice> {
        let seed = arr(identity_seed, "identity seed")?;
        Ok(Self {
            inner: core::Device::new_with_identity_seed(&seed, device_id).map_err(err)?,
        })
    }

    /// Restore from `exportState`.
    #[wasm_bindgen(js_name = importState)]
    pub fn import_state(blob: &[u8], key: &[u8]) -> Result<GroupDevice> {
        Ok(Self {
            inner: core::Device::import_state(blob, key).map_err(err)?,
        })
    }

    #[wasm_bindgen(js_name = exportState)]
    pub fn export_state(&self, key: &[u8]) -> Result<Vec<u8>> {
        self.inner.export_state(key).map_err(err)
    }

    #[wasm_bindgen(js_name = attestationPayload)]
    pub fn attestation_payload(&self) -> Result<Vec<u8>> {
        self.inner.attestation_payload().map_err(err)
    }

    #[wasm_bindgen(js_name = installAttestation)]
    pub fn install_attestation(
        &mut self,
        mailbox_public_key: &[u8],
        signature: &[u8],
    ) -> Result<()> {
        self.inner
            .install_attestation(mailbox_public_key, signature)
            .map_err(err)
    }

    #[wasm_bindgen(getter)]
    pub fn mailbox(&self) -> String {
        self.inner.mailbox().to_owned()
    }

    #[wasm_bindgen(getter, js_name = deviceId)]
    pub fn device_id(&self) -> String {
        self.inner.device_id().to_owned()
    }

    #[wasm_bindgen(getter, js_name = signaturePublicKey)]
    pub fn signature_public_key(&self) -> Vec<u8> {
        self.inner.signature_public_key()
    }

    #[wasm_bindgen(getter)]
    pub fn fingerprint(&self) -> Vec<u8> {
        self.inner.fingerprint().to_vec()
    }

    #[wasm_bindgen(getter, js_name = isAttested)]
    pub fn is_attested(&self) -> bool {
        self.inner.is_attested()
    }

    pub fn groups(&self) -> Array {
        self.inner.groups().into_iter().map(JsValue::from).collect()
    }

    #[wasm_bindgen(js_name = generateKeyPackages)]
    pub fn generate_key_packages(&mut self, count: u32) -> Result<Array> {
        Ok(self
            .inner
            .generate_key_packages(count)
            .map_err(err)?
            .iter()
            .map(|k| bytes(k))
            .collect())
    }

    #[wasm_bindgen(js_name = generateLastResortKeyPackage)]
    pub fn generate_last_resort_key_package(&mut self) -> Result<Vec<u8>> {
        self.inner.generate_last_resort_key_package().map_err(err)
    }

    #[wasm_bindgen(js_name = createGroup)]
    pub fn create_group(&mut self, group_id: &str) -> Result<JsValue> {
        Ok(joined(&self.inner.create_group(group_id).map_err(err)?))
    }

    #[wasm_bindgen(js_name = joinFromWelcome)]
    pub fn join_from_welcome(&mut self, welcome: &[u8]) -> Result<JsValue> {
        Ok(joined(&self.inner.join_from_welcome(welcome).map_err(err)?))
    }

    pub fn members(&self, group_id: &str) -> Result<JsValue> {
        Ok(members(&self.inner.members(group_id).map_err(err)?))
    }

    pub fn epoch(&self, group_id: &str) -> Result<f64> {
        Ok(self.inner.epoch(group_id).map_err(err)? as f64)
    }

    #[wasm_bindgen(js_name = hasPendingCommit)]
    pub fn has_pending_commit(&self, group_id: &str) -> Result<bool> {
        self.inner.has_pending_commit(group_id).map_err(err)
    }

    #[wasm_bindgen(js_name = safetyNumber)]
    pub fn safety_number(&self, group_id: &str) -> Result<String> {
        self.inner.safety_number(group_id).map_err(err)
    }

    #[wasm_bindgen(js_name = epochAuthenticator)]
    pub fn epoch_authenticator(&self, group_id: &str) -> Result<Vec<u8>> {
        self.inner.epoch_authenticator(group_id).map_err(err)
    }

    /// `remove` is an array of `{mailbox, deviceId}`.
    pub fn commit(
        &mut self,
        group_id: &str,
        key_packages: Array,
        remove: Array,
    ) -> Result<JsValue> {
        let kps = byte_list(&key_packages, "key package")?;
        let remove = remove
            .iter()
            .map(|r| {
                Ok(core::DeviceRef {
                    mailbox: get_string(&r, "mailbox")?,
                    device_id: get_string(&r, "deviceId")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(commit_bundle(
            &self.inner.commit(group_id, &kps, &remove).map_err(err)?,
        ))
    }

    #[wasm_bindgen(js_name = addMembers)]
    pub fn add_members(&mut self, group_id: &str, key_packages: Array) -> Result<JsValue> {
        let kps = byte_list(&key_packages, "key package")?;
        Ok(commit_bundle(
            &self.inner.add_members(group_id, &kps).map_err(err)?,
        ))
    }

    #[wasm_bindgen(js_name = removeMailbox)]
    pub fn remove_mailbox(&mut self, group_id: &str, mailbox: &str) -> Result<JsValue> {
        Ok(commit_bundle(
            &self.inner.remove_mailbox(group_id, mailbox).map_err(err)?,
        ))
    }

    #[wasm_bindgen(js_name = selfUpdate)]
    pub fn self_update(&mut self, group_id: &str) -> Result<JsValue> {
        Ok(commit_bundle(
            &self.inner.self_update(group_id).map_err(err)?,
        ))
    }

    #[wasm_bindgen(js_name = confirmCommit)]
    pub fn confirm_commit(&mut self, group_id: &str) -> Result<JsValue> {
        Ok(outcome(&self.inner.confirm_commit(group_id).map_err(err)?))
    }

    #[wasm_bindgen(js_name = discardCommit)]
    pub fn discard_commit(&mut self, group_id: &str) -> Result<()> {
        self.inner.discard_commit(group_id).map_err(err)
    }

    /// `attachments` is an array of attachment references from `sealAttachment`.
    #[wasm_bindgen(js_name = encryptMessage)]
    pub fn encrypt_message(
        &mut self,
        group_id: &str,
        client_message_id: &str,
        body: &[u8],
        attachments: Array,
    ) -> Result<JsValue> {
        let atts = attachments
            .iter()
            .map(|a| attachment_in(&a))
            .collect::<Result<Vec<_>>>()?;
        Ok(outgoing(
            &self
                .inner
                .encrypt_message(group_id, client_message_id, body, &atts)
                .map_err(err)?,
        ))
    }

    /// Returns `{type: "message"|"historyOffer"|"commit"|"ownCommitConfirmed"|"own", ...}`.
    #[wasm_bindgen(js_name = processIncoming)]
    pub fn process_incoming(&mut self, group_id: &str, message: &[u8]) -> Result<JsValue> {
        Ok(
            match self
                .inner
                .process_incoming(group_id, message)
                .map_err(err)?
            {
                core::Incoming::Message(m) => Obj::new()
                    .set("type", "message")
                    .set("groupId", m.group_id.as_str())
                    .set("sender", member(&m.sender))
                    .set("epoch", m.epoch as f64)
                    .set("clientMessageId", m.client_message_id.as_str())
                    .set("body", bytes(&m.body))
                    .set(
                        "attachments",
                        m.attachments.iter().map(attachment).collect::<Array>(),
                    )
                    .done(),
                core::Incoming::HistoryOffer(o) => Obj::new()
                    .set("type", "historyOffer")
                    .set("offer", history_offer(&o))
                    .done(),
                core::Incoming::Commit(o) => Obj::new()
                    .set("type", "commit")
                    .set("outcome", outcome(&o))
                    .done(),
                core::Incoming::OwnCommitConfirmed(o) => Obj::new()
                    .set("type", "ownCommitConfirmed")
                    .set("outcome", outcome(&o))
                    .done(),
                core::Incoming::Own => Obj::new().set("type", "own").done(),
            },
        )
    }

    #[wasm_bindgen(js_name = sealHistory)]
    pub fn seal_history(
        &mut self,
        self_group_id: &str,
        client_message_id: &str,
        archive: &[u8],
    ) -> Result<JsValue> {
        let t = self
            .inner
            .seal_history(self_group_id, client_message_id, archive)
            .map_err(err)?;
        Ok(Obj::new()
            .set("sealedArchive", bytes(&t.sealed_archive))
            .set("message", outgoing(&t.message))
            .done())
    }

    /// `offer` is the `offer` object from a `historyOffer` result.
    #[wasm_bindgen(js_name = openHistory)]
    pub fn open_history(&self, offer: JsValue, sealed_archive: &[u8]) -> Result<Vec<u8>> {
        let sender = get(&offer, "sender")?;
        let leaf = get(&sender, "leafIndex")?.as_f64().unwrap_or(0.0);
        let core_offer = core::HistoryOffer {
            group_id: get_string(&offer, "groupId")?,
            sender: core::MemberInfo {
                mailbox: get_string(&sender, "mailbox")?,
                device_id: get_string(&sender, "deviceId")?,
                leaf_index: leaf as u32,
                signature_key: get_bytes(&sender, "signatureKey")?,
                fingerprint: arr(&get_bytes(&sender, "fingerprint")?, "fingerprint")?,
            },
            client_message_id: get_string(&offer, "clientMessageId")?,
            archive: attachment_in(&get(&offer, "archive")?)?,
        };
        self.inner
            .open_history(&core_offer, sealed_archive)
            .map_err(err)
    }

    #[wasm_bindgen(js_name = forgetGroup)]
    pub fn forget_group(&mut self, group_id: &str) -> Result<()> {
        self.inner.forget_group(group_id).map_err(err)
    }
}

/// Validate and describe an uploaded key package.
#[wasm_bindgen(js_name = inspectKeyPackage)]
pub fn inspect_key_package(key_package: &[u8]) -> Result<JsValue> {
    let i = core::inspect_key_package(key_package).map_err(err)?;
    Ok(Obj::new()
        .set("keyPackageRef", bytes(&i.key_package_ref))
        .set("mailbox", i.mailbox.as_str())
        .set("deviceId", i.device_id.as_str())
        .set("signatureKey", bytes(&i.signature_key))
        .set("fingerprint", bytes(&i.fingerprint))
        .set("lastResort", i.last_resort)
        .set("notAfter", i.not_after as f64)
        .done())
}

/// Parse the cleartext header of a group message.
#[wasm_bindgen(js_name = inspectEnvelope)]
pub fn inspect_envelope(message: &[u8]) -> Result<JsValue> {
    let i = core::inspect_envelope(message).map_err(err)?;
    Ok(Obj::new()
        .set("groupId", i.group_id.as_str())
        .set("epoch", i.epoch as f64)
        .set(
            "kind",
            match i.kind {
                core::MessageKind::Application => "application",
                core::MessageKind::Commit => "commit",
            },
        )
        .set("senderMailbox", i.sender_mailbox.as_str())
        .set("senderDevice", i.sender_device.as_str())
        .set("clientMessageId", i.client_message_id.as_str())
        .done())
}

/// Verify a detached envelope signature; throws on failure.
#[wasm_bindgen(js_name = verifyEnvelopeSignature)]
pub fn verify_envelope_signature(
    device_signature_key: &[u8],
    message: &[u8],
    signature: &[u8],
) -> Result<()> {
    core::verify_envelope_signature(device_signature_key, message, signature).map_err(err)
}

/// KeyPackageRefs a Welcome is addressed to.
#[wasm_bindgen(js_name = welcomeRecipients)]
pub fn welcome_recipients(welcome: &[u8]) -> Result<Array> {
    Ok(core::welcome_recipients(welcome)
        .map_err(err)?
        .iter()
        .map(|r| bytes(r))
        .collect())
}

/// Seal an attachment: returns `{sealed, reference}`.
#[wasm_bindgen(js_name = sealAttachment)]
pub fn seal_attachment(plaintext: &[u8]) -> Result<JsValue> {
    let s = core::seal_attachment(plaintext).map_err(err)?;
    Ok(Obj::new()
        .set("sealed", bytes(&s.sealed))
        .set("reference", attachment(&s.reference))
        .done())
}

/// Open an attachment after checking its hash.
#[wasm_bindgen(js_name = openAttachment)]
pub fn open_attachment(sealed: &[u8], reference: JsValue) -> Result<Vec<u8>> {
    core::open_attachment(sealed, &attachment_in(&reference)?).map_err(err)
}

/// 32 random bytes for `exportState`.
#[wasm_bindgen(js_name = generateStateKey)]
pub fn generate_state_key() -> Result<Vec<u8>> {
    core::generate_state_key().map_err(err)
}

/// Six groups of five digits for a device fingerprint.
#[wasm_bindgen(js_name = fingerprintDigits)]
pub fn fingerprint_digits(fingerprint: &[u8]) -> Result<String> {
    Ok(core::fingerprint_digits(&arr(fingerprint, "fingerprint")?))
}

/// `/k/` mailbox address for an identity seed.
#[wasm_bindgen(js_name = mailboxForSeed)]
pub fn mailbox_for_seed(identity_seed: &[u8]) -> Result<String> {
    Ok(core::mailbox_for_seed(&arr(
        identity_seed,
        "identity seed",
    )?))
}

/// Sign a device attestation payload with a mailbox identity seed.
#[wasm_bindgen(js_name = signAttestationWithSeed)]
pub fn sign_attestation_with_seed(identity_seed: &[u8], payload: &[u8]) -> Result<Vec<u8>> {
    Ok(
        core::sign_attestation_with_seed(&arr(identity_seed, "identity seed")?, payload)
            .map_err(err)?
            .to_vec(),
    )
}

/// Protocol framing version string (`enc_scheme`).
#[wasm_bindgen(js_name = encScheme)]
pub fn enc_scheme() -> String {
    core::ENC_SCHEME.to_owned()
}
