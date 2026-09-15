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
        config.baseBackgroundColor = .secondarySystemBackground
        config.baseForegroundColor = .label
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
        if let account, let inbox {
            cell.contentConfiguration = UIHostingConfiguration {
                VStack(spacing: 2) {
                    if showDay { HistoryDayBreak(label: Time.dayLabel(message.at)) }
                    MessageBubble(message: message)
                    #if DEBUG
                    if Fixtures.enabled && CommandLine.arguments.contains("-history-growth") && indexPath.row == 0 {
                        HistoryFixtureGrowth()
                    }
                    #endif
                }
                .padding(.horizontal, 12)
                .padding(.vertical, 1)
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
    var body: some View {
        Text(label)
            .font(.system(size: 11.5, weight: .medium))
            .foregroundStyle(Theme.muted)
            .padding(.horizontal, 10).padding(.vertical, 4)
            .background(.ultraThinMaterial, in: Capsule())
            .padding(.vertical, 8)
            .frame(maxWidth: .infinity)
    }
}
