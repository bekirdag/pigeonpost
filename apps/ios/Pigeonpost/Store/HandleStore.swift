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

@MainActor
@Observable
final class HandleStore {
    enum Phase: Equatable {
        case idle
        case loading
        /// Nothing to sell here: the postbox has no App Store key, or the product is not on sale in
        /// this storefront. Shown as absence rather than as an error.
        case unavailable
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
        guard let client = account?.client else { return }
        phase = .loading
        do {
            let offer = try await client.handleOffer()
            if let namespace = offer.namespace {
                phase = .owned(namespace: namespace, renews: offer.renewsOn)
                return
            }
            guard let productId = offer.productId else {
                phase = .unavailable
                return
            }
            let products = try await Product.products(for: [productId])
            guard let product = products.first else {
                // A real state, not a bug: a product still in review, or not sold in this
                // storefront, returns an empty list rather than an error.
                phase = .unavailable
                return
            }
            self.product = product
            phase = .forSale(displayPrice: product.displayPrice)
        } catch let failure as APIError where failure.status == 404 {
            // The postbox has no App Store key. Selling is simply not a thing this deployment does.
            phase = .unavailable
        } catch {
            phase = .unavailable
        }
    }

    /// Buy, then claim. The name is sent with the claim rather than with the purchase because Apple
    /// has no field for it — which means the two can disagree, and the postbox is what reconciles
    /// them.
    func buy() async {
        guard let product else { return }
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
        default:
            wantedName = "alex"
            phase = .forSale(displayPrice: "$8.00")
        }
    }
    #endif

    /// What the postbox will canonicalise anyway, done here so the field shows it. Trimming a
    /// leading slash matters: people type the address they have seen, not the name.
    static func tidy(_ raw: String) -> String {
        raw.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
            .lowercased()
    }
}
