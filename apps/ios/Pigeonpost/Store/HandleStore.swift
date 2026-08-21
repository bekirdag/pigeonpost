//  Buying a handle.
//
//  StoreKit 2 does the payment; it does not do the entitlement. What the app learns from a purchase
//  is a transaction id, which it hands to the postbox — and the postbox asks Apple what that id
//  means. Nothing here decides that a name was bought, which is why a jailbroken phone, a patched
//  binary or a replayed receipt buys nothing: the only party whose word counts never runs on this
//  device.
//
//  The unfinished-transaction listener is the other half. A purchase can complete while the app is
//  closed, on another device, or after a renewal nobody was watching, and StoreKit will hand it over
//  the next time the app runs. Finishing a transaction before the postbox has recorded it would
//  throw away the only pointer we had.

import Foundation
import StoreKit

private struct TimedOut: Error {}

@MainActor
@Observable
final class HandleStore {
    enum Phase: Equatable {
        case idle
        case loading
        /// This deployment does not sell handles at all — the postbox has no App Store key and
        /// answers 404. The only case that should render nothing, because there is nothing to say.
        case unavailable
        /// The postbox sells handles, but the App Store will not hand this build a product to buy:
        /// awaiting review, not sold in this storefront, or the store was unreachable.
        ///
        /// Its own case, because folding it into `unavailable` meant the section vanished and left
        /// nobody — including me — able to tell "not for sale here" from "something went wrong".
        /// Silence is the one answer that cannot be acted on.
        case notOnSaleYet(String)
        case forSale(displayPrice: String)
        case buying
        case owned(namespace: String, renews: Date?)
        case failed(String)
    }

    private(set) var phase: Phase = .idle
    /// The name being bought, as typed. Held here so a failed purchase does not lose it.
    var wantedName = ""

    private var product: Product?
    /// Written once in `init` and read once in `deinit`, both on the main actor's own terms and
    /// never concurrently — which `deinit` cannot prove, since it is nonisolated. Leaving the task
    /// uncancelled instead would keep one `Transaction.updates` loop alive per Settings visit.
    private nonisolated(unsafe) var listener: Task<Void, Never>?
    private weak var account: Account?

    init(account: Account?) {
        self.account = account
        listener = Task { [weak self] in
            // Every transaction StoreKit has not been told is finished, including ones that
            // completed while the app was not running.
            for await update in Transaction.updates {
                await self?.settle(update)
            }
        }
    }

    deinit { listener?.cancel() }

    /// What this account owns, and what it would cost if it owns nothing.
    func refresh() async {
        // Never in fixture mode. There is no token there, and asking for one walks
        // `Session.token()` → `renew()` → `forget()`, which signs the app out — so simply opening
        // Settings threw you back to the sign-in screen. The same path is reachable for real: a
        // session that has just expired would then be ended by a screen somebody opened to read a
        // setting, rather than by the request they were actually making.
        guard !Fixtures.enabled else {
            phase = .unavailable
            return
        }
        guard let client = account?.client else { return }
        phase = .loading
        do {
            // A ceiling on the whole lookup. `.loading` renders as "Checking your handle…", and a
            // request that never returns leaves that on screen for ever — which is exactly what a
            // phone showed. The postbox call carries the long-poll timeout of 90 seconds, and
            // StoreKit's has no stated bound at all, so neither can be relied on to end this.
            let offer = try await withTimeout(seconds: 12) { try await client.handleOffer() }
            if let namespace = offer.namespace {
                phase = .owned(namespace: namespace, renews: offer.renewsOn)
                // A name with no mailbox under it is not an address. The postbox mints one when the
                // purchase lands; this is for the namespaces bought before it did — including the
                // one whose owner reported that the handle they had just paid for appeared in no
                // list anywhere.
                await account?.ensureMailbox(inNamespace: namespace)
                return
            }
            guard let productId = offer.productId else {
                phase = .unavailable
                return
            }
            let products = try await withTimeout(seconds: 12) {
                try await Product.products(for: [productId])
            }
            guard let product = products.first else {
                // An empty list, not an error: a product still awaiting review, or not sold in this
                // storefront, comes back this way.
                phase = .notOnSaleYet(
                    "Handles are not on sale from this build yet. The subscription is still going through App Store review."
                )
                return
            }
            self.product = product
            phase = .forSale(displayPrice: product.displayPrice)
        } catch let failure as APIError where failure.status == 404 {
            // The postbox has no App Store key. Selling is simply not a thing this deployment does.
            phase = .unavailable
        } catch let failure as APIError {
            phase = .notOnSaleYet(failure.errorDescription ?? "The postbox could not be asked about handles.")
        } catch is TimedOut {
            phase = .notOnSaleYet("Checking your handle took too long. Tap to try again.")
        } catch {
            phase = .notOnSaleYet("Could not reach the App Store.")
        }
    }

    /// Buy, then claim. The name is sent with the claim rather than with the purchase because Apple
    /// has no field for it — which means the two can disagree, and the postbox is what reconciles
    /// them.
    func buy() async {
        guard !Fixtures.enabled, let product else { return }
        let name = Self.tidy(wantedName)
        guard !name.isEmpty else {
            phase = .failed("Choose a name first.")
            return
        }
        phase = .buying
        do {
            switch try await product.purchase() {
            case let .success(verification):
                await settle(verification)
            case .userCancelled:
                await refresh()
            case .pending:
                // Ask to Buy, or a payment the bank is still thinking about. The transaction will
                // arrive through `Transaction.updates` if it ever completes.
                phase = .failed("That purchase is waiting for approval. It will appear here once it goes through.")
            @unknown default:
                await refresh()
            }
        } catch {
            phase = .failed("The purchase did not complete.")
        }
    }

    /// Purchases made on another device, or on this one before a reinstall.
    func restore() async {
        guard !Fixtures.enabled else { return }
        phase = .buying
        do {
            try await AppStore.sync()
        } catch {
            // `sync` throws when the person dismisses the sign-in sheet, which is not a failure
            // worth reporting as one.
        }
        for await entitlement in Transaction.currentEntitlements {
            await settle(entitlement, finishing: false)
        }
        await refresh()
    }

    /// Take one transaction as far as it goes: verify locally, tell the postbox, and only then let
    /// StoreKit forget it.
    private func settle(_ result: VerificationResult<Transaction>, finishing: Bool = true) async {
        guard case let .verified(transaction) = result else {
            // StoreKit could not verify its own signature. Nothing to send: the postbox would ask
            // Apple and be told the same thing, one round trip later.
            phase = .failed("That purchase could not be verified.")
            return
        }
        guard let client = account?.client else { return }
        let name = Self.tidy(wantedName)
        do {
            let offer = try await client.claimHandle(
                transactionId: String(transaction.id),
                // A renewal arriving unattended has no typed name; the postbox already knows which
                // one this subscription bought and ignores what we send.
                namespace: name.isEmpty ? "renewal" : name
            )
            if finishing { await transaction.finish() }
            if let namespace = offer.namespace {
                phase = .owned(namespace: namespace, renews: offer.renewsOn)
                wantedName = ""
                // The postbox mints `<namespace>/main` as part of the claim, so this is mostly a
                // reload — but it is what puts the new mailbox in the Mailboxes list without
                // waiting for the next launch, and it still covers a mint the postbox could not do.
                await account?.ensureMailbox(inNamespace: namespace)
            } else {
                await refresh()
            }
        } catch let failure as APIError {
            // Left unfinished on purpose. The money is Apple's problem and it has been taken; the
            // namespace is ours and has not been granted. Finishing here would discard the id that
            // is the only way to try again.
            phase = .failed(Self.explain(failure))
        } catch {
            phase = .failed("Could not reach the postbox. Your purchase is safe — reopen Settings to finish.")
        }
    }

    /// The postbox's codes, said the way a person would say them.
    private static func explain(_ failure: APIError) -> String {
        switch failure.code {
        case "namespace_taken": return "Someone already has that name. Try another."
        case "name_reserved": return "That name is reserved. Try another."
        case "purchase_already_named":
            return failure.detail ?? "This subscription already bought a different name."
        case "purchase_already_used":
            return "That subscription belongs to another Pigeonpost account."
        case "purchase_expired": return "That subscription has lapsed."
        case "purchase_refunded": return "That purchase was refunded."
        case "invalid_namespace": return "That name cannot be a handle."
        default: return failure.errorDescription ?? "Could not claim that name."
        }
    }

    #if DEBUG
    /// Put the section into a fixed state for a screenshot. See `Fixtures.handleState`.
    func stage(_ state: String) {
        switch state {
        case "owned":
            phase = .owned(namespace: "/alex", renews: Date(timeIntervalSince1970: 1_818_800_000))
        case "soon":
            phase = .notOnSaleYet("Handles are not on sale from this build yet. The subscription is still going through App Store review.")
        default:
            wantedName = "alex"
            phase = .forSale(displayPrice: "$8.00")
        }
    }
    #endif

    /// Run `work`, or give up.
    ///
    /// There is no shared deadline between a URLSession call and a StoreKit one, and a view state
    /// that only ever ends when a network call chooses to is not a state — it is a hang with a
    /// label on it.
    private func withTimeout<T: Sendable>(
        seconds: UInt64,
        _ work: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await work() }
            group.addTask {
                try await Task.sleep(nanoseconds: seconds * 1_000_000_000)
                throw TimedOut()
            }
            guard let first = try await group.next() else { throw TimedOut() }
            group.cancelAll()
            return first
        }
    }

    /// What the postbox will canonicalise anyway, done here so the field shows it. Trimming a
    /// leading slash matters: people type the address they have seen, not the name.
    static func tidy(_ raw: String) -> String {
        raw.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            .lowercased()
    }
}
