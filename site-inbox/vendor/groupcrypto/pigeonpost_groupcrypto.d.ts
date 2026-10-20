/* tslint:disable */
/* eslint-disable */

/**
 * One enrolled device and all of its group state.
 */
export class GroupDevice {
    free(): void;
    [Symbol.dispose](): void;
    addMembers(group_id: string, key_packages: Array<any>): any;
    attestationPayload(): Uint8Array;
    /**
     * `remove` is an array of `{mailbox, deviceId}`.
     */
    commit(group_id: string, key_packages: Array<any>, remove: Array<any>): any;
    confirmCommit(group_id: string): any;
    createGroup(group_id: string): any;
    discardCommit(group_id: string): void;
    /**
     * `attachments` is an array of attachment references from `sealAttachment`.
     */
    encryptMessage(group_id: string, client_message_id: string, body: Uint8Array, attachments: Array<any>): any;
    epochAuthenticator(group_id: string): Uint8Array;
    epoch(group_id: string): number;
    exportState(key: Uint8Array): Uint8Array;
    forgetGroup(group_id: string): void;
    generateKeyPackages(count: number): Array<any>;
    generateLastResortKeyPackage(): Uint8Array;
    groups(): Array<any>;
    hasPendingCommit(group_id: string): boolean;
    /**
     * Restore from `exportState`.
     */
    static importState(blob: Uint8Array, key: Uint8Array): GroupDevice;
    installAttestation(mailbox_public_key: Uint8Array, signature: Uint8Array): void;
    joinFromWelcome(welcome: Uint8Array): any;
    members(group_id: string): any;
    /**
     * New device attested with the mailbox identity seed.
     */
    static newWithIdentitySeed(identity_seed: Uint8Array, device_id: string): GroupDevice;
    /**
     * New unattested device for `mailbox` (a `/k/` address).
     */
    constructor(mailbox: string, device_id: string);
    /**
     * `offer` is the `offer` object from a `historyOffer` result.
     */
    openHistory(offer: any, sealed_archive: Uint8Array): Uint8Array;
    /**
     * Returns `{type: "message"|"historyOffer"|"commit"|"ownCommitConfirmed"|"own", ...}`.
     */
    processIncoming(group_id: string, message: Uint8Array): any;
    removeMailbox(group_id: string, mailbox: string): any;
    safetyNumber(group_id: string): string;
    sealHistory(self_group_id: string, client_message_id: string, archive: Uint8Array): any;
    selfUpdate(group_id: string): any;
    readonly deviceId: string;
    readonly fingerprint: Uint8Array;
    readonly isAttested: boolean;
    readonly mailbox: string;
    readonly signaturePublicKey: Uint8Array;
}

/**
 * Protocol framing version string (`enc_scheme`).
 */
export function encScheme(): string;

/**
 * Six groups of five digits for a device fingerprint.
 */
export function fingerprintDigits(fingerprint: Uint8Array): string;

/**
 * 32 random bytes for `exportState`.
 */
export function generateStateKey(): Uint8Array;

/**
 * Parse the cleartext header of a group message.
 */
export function inspectEnvelope(message: Uint8Array): any;

/**
 * Validate and describe an uploaded key package.
 */
export function inspectKeyPackage(key_package: Uint8Array): any;

/**
 * `/k/` mailbox address for an identity seed.
 */
export function mailboxForSeed(identity_seed: Uint8Array): string;

/**
 * Open an attachment after checking its hash.
 */
export function openAttachment(sealed: Uint8Array, reference: any): Uint8Array;

/**
 * Seal an attachment: returns `{sealed, reference}`.
 */
export function sealAttachment(plaintext: Uint8Array): any;

/**
 * Sign a device attestation payload with a mailbox identity seed.
 */
export function signAttestationWithSeed(identity_seed: Uint8Array, payload: Uint8Array): Uint8Array;

/**
 * Verify a detached envelope signature; throws on failure.
 */
export function verifyEnvelopeSignature(device_signature_key: Uint8Array, message: Uint8Array, signature: Uint8Array): void;

/**
 * KeyPackageRefs a Welcome is addressed to.
 */
export function welcomeRecipients(welcome: Uint8Array): Array<any>;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_groupdevice_free: (a: number, b: number) => void;
    readonly encScheme: (a: number) => void;
    readonly fingerprintDigits: (a: number, b: number, c: number) => void;
    readonly generateStateKey: (a: number) => void;
    readonly groupdevice_addMembers: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly groupdevice_attestationPayload: (a: number, b: number) => void;
    readonly groupdevice_commit: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly groupdevice_confirmCommit: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_createGroup: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_deviceId: (a: number, b: number) => void;
    readonly groupdevice_discardCommit: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_encryptMessage: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => void;
    readonly groupdevice_epoch: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_epochAuthenticator: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_exportState: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_fingerprint: (a: number, b: number) => void;
    readonly groupdevice_forgetGroup: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_generateKeyPackages: (a: number, b: number, c: number) => void;
    readonly groupdevice_generateLastResortKeyPackage: (a: number, b: number) => void;
    readonly groupdevice_groups: (a: number) => number;
    readonly groupdevice_hasPendingCommit: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_importState: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly groupdevice_installAttestation: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly groupdevice_isAttested: (a: number) => number;
    readonly groupdevice_joinFromWelcome: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_mailbox: (a: number, b: number) => void;
    readonly groupdevice_members: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_new: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly groupdevice_newWithIdentitySeed: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly groupdevice_openHistory: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly groupdevice_processIncoming: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly groupdevice_removeMailbox: (a: number, b: number, c: number, d: number, e: number, f: number) => void;
    readonly groupdevice_safetyNumber: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_sealHistory: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
    readonly groupdevice_selfUpdate: (a: number, b: number, c: number, d: number) => void;
    readonly groupdevice_signaturePublicKey: (a: number, b: number) => void;
    readonly inspectEnvelope: (a: number, b: number, c: number) => void;
    readonly inspectKeyPackage: (a: number, b: number, c: number) => void;
    readonly mailboxForSeed: (a: number, b: number, c: number) => void;
    readonly openAttachment: (a: number, b: number, c: number, d: number) => void;
    readonly sealAttachment: (a: number, b: number, c: number) => void;
    readonly signAttestationWithSeed: (a: number, b: number, c: number, d: number, e: number) => void;
    readonly verifyEnvelopeSignature: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => void;
    readonly welcomeRecipients: (a: number, b: number, c: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
    readonly __wbindgen_export4: (a: number, b: number, c: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
