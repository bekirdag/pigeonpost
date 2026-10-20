import SwiftUI
import UIKit

/// The newest row is the table's origin. Its bottom is therefore exact even before any older
/// self-sizing rows have been measured. Reversing the table and each cell keeps the content upright
/// and makes older pages append away from the reader, without height estimates or scroll retries.
struct ConversationHistoryView: UIViewControllerRepresentable {
    let messages: [ThreadMessage]
    let latestRequest: Int
    let account: Account
    let inbox: Inbox

    func makeUIViewController(context: Context) -> HistoryController { HistoryController() }

    func updateUIViewController(_ controller: HistoryController, context: Context) {
        controller.update(messages, latestRequest: latestRequest, account: account, inbox: inbox)
    }
}

@MainActor
final class HistoryController: UIViewController, UITableViewDataSource, UITableViewDelegate {
    private let table = BottomOriginTable(frame: .zero, style: .plain)
    private let latestButton = UIButton(type: .system)
    private var window = HistoryWindow()
    private var source: [ThreadMessage] = []
    private var rows: [ThreadMessage] = []
    private var sourceIDs: [String] = []
    private var dayBreaks: Set<String> = []
    private var request: Int?
    private var account: Account?
    private var inbox: Inbox?
    private var loadingOlder = false
    private var dragStartOffset: CGFloat = 0
    private var userScrolling = false
    /// Each card's frame in its row, upright (`history-row`), for the long-press preview.
    private var cardFrames: [String: CGRect] = [:]

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .clear
        table.backgroundColor = .clear
        table.transform = CGAffineTransform(scaleX: 1, y: -1)
        table.separatorStyle = .none
        table.allowsSelection = false
        table.rowHeight = UITableView.automaticDimension
        table.estimatedRowHeight = 160
        table.selfSizingInvalidation = .enabledIncludingConstraints
        table.contentInsetAdjustmentBehavior = .never
        table.contentInset = UIEdgeInsets(top: 10, left: 0, bottom: 10, right: 0)
        table.keyboardDismissMode = .interactive
        table.alwaysBounceVertical = true
        let dismissKeyboard = UITapGestureRecognizer(target: self, action: #selector(endEditing))
        dismissKeyboard.cancelsTouchesInView = false
        table.addGestureRecognizer(dismissKeyboard)
        table.dataSource = self
        table.delegate = self
        table.register(UITableViewCell.self, forCellReuseIdentifier: "message")
        table.accessibilityIdentifier = "conversation-history"
        table.onLayout = { [weak self] in self?.reportPosition() }
        table.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(table)
        NSLayoutConstraint.activate([
            table.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            table.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            table.topAnchor.constraint(equalTo: view.topAnchor),
            table.bottomAnchor.constraint(equalTo: view.bottomAnchor)
        ])

        var config = UIButton.Configuration.filled()
        config.image = UIImage(systemName: "arrow.down")
        config.cornerStyle = .capsule
        config.baseBackgroundColor = UIColor(Theme.ground)
        config.baseForegroundColor = UIColor(Theme.ink)
        config.background.strokeColor = UIColor(Theme.strokeStrong)
        config.background.strokeWidth = 1
        config.contentInsets = NSDirectionalEdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14)
        latestButton.configuration = config
        latestButton.accessibilityLabel = "Latest messages"
        latestButton.accessibilityIdentifier = "history-latest"
        latestButton.addTarget(self, action: #selector(showLatest), for: .touchUpInside)
        latestButton.isHidden = true
        latestButton.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(latestButton)
        NSLayoutConstraint.activate([
            latestButton.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor, constant: -16),
            latestButton.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -12)
        ])
    }

    func update(_ messages: [ThreadMessage], latestRequest: Int, account: Account, inbox: Inbox) {
        loadViewIfNeeded()
        self.account = account
        self.inbox = inbox
        let explicitlyRequested = request != nil && request != latestRequest
        request = latestRequest
        guard messages != source || explicitlyRequested else { return }
        source = messages
        sourceIDs = messages.map(\.id)
        dayBreaks = Set(messages.enumerated().compactMap { index, message in
            index == 0 || !Time.sameDay(messages[index - 1].at, message.at) ? message.id : nil
        })
        if explicitlyRequested { table.followingLatest = true }
        let anchor = readingAnchor()
        window.update(sourceIDs, followingLatest: table.followingLatest)
        let previousRows = rows
        rebuildRows()
        // Unseen incoming mail has no effect on the rendered history or its measured geometry.
        guard rows != previousRows || explicitlyRequested else { updateLatestButton(); return }
        table.reloadData()
        table.layoutIfNeeded()
        restore(anchor)
        updateLatestButton()
    }

    private func rebuildRows() {
        let byID = Dictionary(source.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
        rows = window.ids.compactMap { byID[$0] }
    }

    func tableView(_ tableView: UITableView, numberOfRowsInSection section: Int) -> Int { rows.count }

    func tableView(_ tableView: UITableView, cellForRowAt indexPath: IndexPath) -> UITableViewCell {
        let cell = tableView.dequeueReusableCell(withIdentifier: "message", for: indexPath)
        cell.backgroundColor = .clear
        let message = rows[indexPath.row]
        cell.accessibilityIdentifier = "message:" + message.id
        let showDay = dayBreaks.contains(message.id)
        // The desktop layout an iPad uses gives the conversation the conversation design's margins
        // and its centred column; a phone keeps every point of its width.
        let wide = traitCollection.horizontalSizeClass == .regular
        if let account, let inbox {
            cell.contentConfiguration = UIHostingConfiguration {
                VStack(spacing: 2) {
                    if showDay { HistoryDayBreak(label: Time.dayLabel(message.at)) }
                    MessageBubble(message: message, showsMenu: false) { [weak self] frame in
                        self?.cardFrames[message.id] = frame
                    }
                    #if DEBUG
                    if Fixtures.enabled && CommandLine.arguments.contains("-history-growth") && indexPath.row == 0 {
                        HistoryFixtureGrowth()
                    }
                    #endif
                }
                .frame(maxWidth: 860)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, wide ? 24 : 12)
                .padding(.vertical, wide ? 8 : 5)
                .coordinateSpace(name: "history-row")
                .environment(account)
                .environment(inbox)
                .id(message.id)
                // Keep the inverse transform in the hosted view. UIKit can replace a hosting
                // configuration's contentView when it resizes, discarding a UIKit-side transform.
                .scaleEffect(x: 1, y: -1)
            }.margins(.all, 0)
        }
        return cell
    }

    // ---- the long-press menu ------------------------------------------------------------------
    //
    // Presented by the table rather than by SwiftUI's `.contextMenu` on the bubble. The table is
    // turned upside down and each row turned back, and the system's preview of a row is a snapshot
    // taken in the table's space: the message lifted mirrored, or collapsed to a hairline, and came
    // back flipped (screen recording, 2026-10-20). Here the preview is the row's own snapshot turned
    // back upright, cut to the card, and placed where the card is on screen.

    func tableView(_ tableView: UITableView, contextMenuConfigurationForRowAt indexPath: IndexPath,
                   point: CGPoint) -> UIContextMenuConfiguration? {
        guard rows.indices.contains(indexPath.row) else { return nil }
        let message = rows[indexPath.row]
        return UIContextMenuConfiguration(identifier: message.id as NSString, previewProvider: nil) { [weak self] _ in
            self?.menu(for: message)
        }
    }

    func tableView(_ tableView: UITableView,
                   previewForHighlightingContextMenuWithConfiguration configuration: UIContextMenuConfiguration) -> UITargetedPreview? {
        preview(for: configuration)
    }

    func tableView(_ tableView: UITableView,
                   previewForDismissingContextMenuWithConfiguration configuration: UIContextMenuConfiguration) -> UITargetedPreview? {
        preview(for: configuration)
    }

    private func preview(for configuration: UIContextMenuConfiguration) -> UITargetedPreview? {
        guard let id = configuration.identifier as? String,
              let row = rows.firstIndex(where: { $0.id == id }),
              let cell = table.cellForRow(at: IndexPath(row: row, section: 0)),
              let snapshot = cell.contentView.snapshotView(afterScreenUpdates: false) else { return nil }
        let bounds = cell.contentView.bounds
        let upright = UIView(frame: bounds)
        snapshot.transform = CGAffineTransform(scaleX: 1, y: -1)
        snapshot.frame = bounds
        upright.addSubview(snapshot)
        let parameters = UIPreviewParameters()
        parameters.backgroundColor = .clear
        let card = cardFrames[id].map { $0.intersection(bounds) } ?? bounds
        if !card.isNull, card.width > 1, card.height > 1 {
            parameters.visiblePath = UIBezierPath(roundedRect: card, cornerRadius: 14)
        }
        let center = table.convert(cell.center, to: view)
        return UITargetedPreview(view: upright, parameters: parameters, target: UIPreviewTarget(container: view, center: center))
    }

    private func menu(for message: ThreadMessage) -> UIMenu {
        let mine = message.kind == .outgoing
        var actions: [UIMenuElement] = [
            UIAction(title: "Copy", image: UIImage(systemName: "doc.on.doc")) { _ in Clipboard.copy(message.body) },
        ]
        // Only inbound. Reporting your own message would report yourself.
        if !mine {
            actions.append(UIAction(title: "Report spam", image: UIImage(systemName: "exclamationmark.bubble"),
                                    attributes: .destructive) { [weak self] _ in self?.confirmReport(message) })
        }
        // Both directions: a sent copy takes up the same room in the mailbox as a received one.
        actions.append(UIAction(title: "Delete", image: UIImage(systemName: "trash"),
                                attributes: .destructive) { [weak self] _ in self?.confirmDelete(message) })
        return UIMenu(children: actions)
    }

    private func confirmReport(_ message: ThreadMessage) {
        confirm(title: "Report this message as spam?",
                detail: "The postbox is told which message and who sent it. It counts against that sender's standing. Nothing is deleted, and they are not told.",
                action: "Report spam", message: message) { [weak self] in
            Task { await self?.inbox?.reportSpam(messageId: message.id) }
        }
    }

    private func confirmDelete(_ message: ThreadMessage) {
        confirm(title: "Delete this message?",
                detail: "Deleted from this mailbox and gone for good — this is not archiving, and there is no undo. The other side keeps their copy and is not told. The room it took is freed.",
                action: "Delete", message: message) { [weak self] in
            Task { await self?.inbox?.deleteMessage(id: message.id) }
        }
    }

    private func confirm(title: String, detail: String, action: String, message: ThreadMessage, run: @escaping () -> Void) {
        let alert = UIAlertController(title: title, message: detail, preferredStyle: .actionSheet)
        alert.addAction(UIAlertAction(title: action, style: .destructive) { _ in run() })
        alert.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        // An iPad shows an action sheet as a popover, which needs somewhere to point.
        if let popover = alert.popoverPresentationController {
            if let row = rows.firstIndex(where: { $0.id == message.id }),
               let cell = table.cellForRow(at: IndexPath(row: row, section: 0)) {
                popover.sourceView = view
                popover.sourceRect = table.convert(cell.frame, to: view)
            } else {
                popover.sourceView = view
                popover.sourceRect = CGRect(x: view.bounds.midX, y: view.bounds.midY, width: 1, height: 1)
            }
        }
        present(alert, animated: true)
    }

    func scrollViewWillBeginDragging(_ scrollView: UIScrollView) {
        userScrolling = true
        dragStartOffset = scrollView.contentOffset.y
        table.followingLatest = false
    }

    func scrollViewDidScroll(_ scrollView: UIScrollView) {
        guard userScrolling else { return }
        updateLatestButton()
        // Only the person's upward history gesture requests a page; layout and short initial
        // content cannot silently expand the whole snapshot. Appending leaves existing rows put.
        guard !loadingOlder, scrollView.contentOffset.y > dragStartOffset + 12,
              let oldestVisible = table.indexPathsForVisibleRows?.map(\.row).max(),
              oldestVisible >= rows.count - 2, window.hasOlder(in: sourceIDs) else { return }
        loadingOlder = true
        let oldCount = rows.count
        let added = window.loadOlder(sourceIDs)
        rebuildRows()
        table.performBatchUpdates {
            table.insertRows(at: (oldCount..<(oldCount + added)).map { IndexPath(row: $0, section: 0) }, with: .none)
        } completion: { [weak self] _ in self?.loadingOlder = false }
    }

    func scrollViewDidEndDragging(_ scrollView: UIScrollView, willDecelerate decelerate: Bool) {
        if !decelerate { finishScrolling() }
    }

    func scrollViewDidEndDecelerating(_ scrollView: UIScrollView) { finishScrolling() }

    private func finishScrolling() {
        userScrolling = false
        if table.contentOffset.y <= table.latestOffset + 2 {
            showLatest()
        } else {
            updateLatestButton()
        }
    }

    @objc private func showLatest() {
        table.followingLatest = true
        window.update(sourceIDs, followingLatest: true)
        rebuildRows()
        table.reloadData()
        table.setNeedsLayout()
        table.layoutIfNeeded()
        updateLatestButton()
    }

    @objc private func endEditing() { view.window?.endEditing(true) }

    private func updateLatestButton() {
        latestButton.isHidden = table.followingLatest ||
            (table.contentOffset.y <= table.latestOffset + 30 && rows.first?.id == source.last?.id)
    }

    private struct Anchor { let id: String; let offset: CGFloat }

    private func readingAnchor() -> Anchor? {
        guard !table.followingLatest, let path = table.indexPathsForVisibleRows?.min(), path.row < rows.count else { return nil }
        return Anchor(id: rows[path.row].id, offset: table.rectForRow(at: path).minY - table.contentOffset.y)
    }

    private func restore(_ anchor: Anchor?) {
        guard !table.followingLatest, let anchor, let row = rows.firstIndex(where: { $0.id == anchor.id }) else { return }
        let y = table.rectForRow(at: IndexPath(row: row, section: 0)).minY - anchor.offset
        table.setContentOffset(CGPoint(x: 0, y: y), animated: false)
    }

    private func reportPosition() {
        guard LandingReport.enabled, !rows.isEmpty else { return }
        let rect = table.convert(table.rectForRow(at: IndexPath(row: 0, section: 0)), to: nil)
        LandingReport.floor(rect.maxY)
        #if DEBUG
        table.accessibilityValue = "loaded=\(rows.count);latest=\(table.followingLatest);first=\(rows.first?.id ?? "")"
        #endif
    }
}

private final class BottomOriginTable: UITableView {
    var followingLatest = true
    var onLayout: (() -> Void)?
    var latestOffset: CGFloat { -adjustedContentInset.top }

    override func layoutSubviews() {
        super.layoutSubviews()
        // Row zero's bottom needs no total-content-height measurement. Keyboard, Dynamic Type
        // and asynchronous attachment sizing all converge on this same origin in the layout pass.
        if followingLatest && !isDragging && !isDecelerating && abs(contentOffset.y - latestOffset) > 0.5 {
            contentOffset = CGPoint(x: 0, y: latestOffset)
        }
        onLayout?()
    }
}

private struct HistoryDayBreak: View {
    let label: String
    var body: some View { DayDivider(label: label) }
}
