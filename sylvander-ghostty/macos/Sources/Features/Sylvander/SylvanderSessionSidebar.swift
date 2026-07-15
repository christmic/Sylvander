#if os(macOS)
import AppKit
import SwiftUI

struct SylvanderSessionSidebar: View {
    @ObservedObject var store: SylvanderSessionStore
    @State private var showingComposer = false
    @State private var renamingSession: SylvanderSession?
    @State private var renameDraft = ""
    @State private var destructiveAction: DestructiveAction?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            search
            status

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    sessionSection("CONTINUE", sessions: continuingSessions)
                    sessionSection("RECENT", sessions: recentSessions)
                }
                .padding(.top, 10)
            }
        }
        .frame(width: 276)
        .background(
            LinearGradient(
                colors: [SylvanderWorkspacePalette.canvas, SylvanderWorkspacePalette.panel],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        )
        .foregroundStyle(SylvanderWorkspacePalette.text)
        .sheet(isPresented: $showingComposer) {
            SylvanderSessionComposer(store: store, isPresented: $showingComposer)
        }
        .sheet(item: $renamingSession) { session in
            renameSheet(session)
        }
        .alert(item: $destructiveAction) { action in
            destructiveAlert(action)
        }
    }

    private var header: some View {
        HStack {
            Text("SESSIONS")
                .font(.system(size: 10, weight: .bold, design: .monospaced))
                .tracking(1.2)
                .foregroundStyle(SylvanderWorkspacePalette.dim)
            Spacer()
            Button {
                showingComposer = true
            } label: {
                Image(systemName: "plus")
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(SylvanderWorkspacePalette.active)
            }
            .buttonStyle(.plain)
            .help("Start a new session")
            Button(action: store.refresh) {
                Image(systemName: "arrow.clockwise")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(SylvanderWorkspacePalette.dim)
            }
            .buttonStyle(.plain)
            .help("Refresh sessions")
        }
        .padding(.horizontal, 18)
        .padding(.top, 12)
        .frame(height: 54)
        .overlay(alignment: .bottom) { Rectangle().fill(SylvanderWorkspacePalette.rule).frame(height: 1) }
    }

    private var search: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(SylvanderWorkspacePalette.muted)
            TextField("FILTER SESSIONS", text: $store.query)
                .textFieldStyle(.plain)
                .font(.system(size: 10, weight: .medium, design: .monospaced))
                .foregroundStyle(SylvanderWorkspacePalette.text)
        }
        .padding(.horizontal, 18)
        .frame(height: 52)
        .overlay(alignment: .bottom) {
            Rectangle().fill(SylvanderWorkspacePalette.rule).frame(height: 1).padding(.horizontal, 18)
        }
    }

    @ViewBuilder
    private var status: some View {
        switch store.connectionState {
        case .connecting:
            statusLine("CONNECTING", color: SylvanderWorkspacePalette.idle)
        case .online where store.sessions.isEmpty:
            statusLine("NO SESSIONS", color: SylvanderWorkspacePalette.muted)
        case .online:
            EmptyView()
        case .recovering(_, let attempt):
            statusLine("RECONNECTING · \(attempt)", color: SylvanderWorkspacePalette.idle)
        }
    }

    private func statusLine(_ text: String, color: Color) -> some View {
        HStack(spacing: 8) {
            Circle().fill(color).frame(width: 5, height: 5)
            Text(text)
            Spacer()
        }
        .font(.system(size: 9, weight: .semibold, design: .monospaced))
        .tracking(0.8)
        .foregroundStyle(color)
        .padding(.horizontal, 18)
        .frame(height: 34)
    }

    private func sessionRow(_ session: SylvanderSession) -> some View {
        let selected = store.selectedSessionID == session.id
        let activity = store.activity(for: session.id)
        let unread = store.unreadSessionIDs.contains(session.id)
        return Button {
            withAnimation(.easeOut(duration: 0.16)) {
                store.selectedSessionID = session.id
            }
        } label: {
            HStack(alignment: .top, spacing: 10) {
                Circle()
                    .fill(activityColor(activity))
                    .frame(width: unread ? 8 : 6, height: unread ? 8 : 6)
                    .padding(.top, 5)
                    .shadow(color: unread ? activityColor(activity).opacity(0.7) : .clear, radius: 4)

                VStack(alignment: .leading, spacing: 5) {
                    Text(session.label)
                        .font(.system(size: 12, weight: selected ? .semibold : .regular, design: .monospaced))
                        .foregroundStyle(selected ? SylvanderWorkspacePalette.text : SylvanderWorkspacePalette.dim)
                        .lineLimit(1)
                    HStack(spacing: 6) {
                        Text(session.workspaceName)
                            .lineLimit(1)
                        Text("·")
                        Text(relativeAge(session.lastSeenSeconds))
                    }
                    .font(.system(size: 9, weight: .regular, design: .monospaced))
                    .foregroundStyle(SylvanderWorkspacePalette.muted)
                }

                Spacer(minLength: 4)

                Text(activity.rawValue)
                    .font(.system(size: 8, weight: .semibold, design: .monospaced))
                    .foregroundStyle(activityColor(activity))
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 13)
            .contentShape(Rectangle())
            .background(selected ? SylvanderWorkspacePalette.raised : SylvanderWorkspacePalette.panel)
            .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .stroke(selected ? SylvanderWorkspacePalette.active.opacity(0.85) : SylvanderWorkspacePalette.rule.opacity(0.65), lineWidth: 1)
            }
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(session.label), \(activity.rawValue)\(unread ? ", unread" : "")")
        .contextMenu {
            Button("Rename…") {
                renameDraft = session.label
                renamingSession = session
            }
            Divider()
            Button("Archive…") {
                destructiveAction = .archive(session)
            }
            Button("Delete Permanently…", role: .destructive) {
                destructiveAction = .delete(session)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
    }

    @ViewBuilder
    private func sessionSection(_ title: String, sessions: [SylvanderSession]) -> some View {
        if !sessions.isEmpty {
            Text(title)
                .font(.system(size: 9, weight: .bold, design: .monospaced))
                .tracking(1)
                .foregroundStyle(SylvanderWorkspacePalette.muted)
                .padding(.horizontal, 18)
                .padding(.top, title == "CONTINUE" ? 6 : 20)
                .padding(.bottom, 6)

            ForEach(sessions) { session in
                sessionRow(session)
            }
        }
    }

    private var continuingSessions: [SylvanderSession] {
        store.filteredSessions.filter {
            [.running, .waiting].contains(store.activity(for: $0.id)) || $0.presence == .active
        }
    }

    private var recentSessions: [SylvanderSession] {
        let continuingIDs = Set(continuingSessions.map(\.id))
        return store.filteredSessions.filter { !continuingIDs.contains($0.id) }
    }

    private func activityColor(_ activity: SylvanderSessionActivity) -> Color {
        switch activity {
        case .idle: SylvanderWorkspacePalette.muted
        case .running: SylvanderWorkspacePalette.active
        case .waiting: SylvanderWorkspacePalette.idle
        case .complete: SylvanderWorkspacePalette.complete
        case .failed: SylvanderWorkspacePalette.signal
        }
    }

    private func relativeAge(_ seconds: UInt64) -> String {
        switch seconds {
        case 0..<60: "NOW"
        case 60..<3_600: "\(seconds / 60)M"
        case 3_600..<86_400: "\(seconds / 3_600)H"
        default: "\(seconds / 86_400)D"
        }
    }

    private func renameSheet(_ session: SylvanderSession) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Rename session")
                .font(.system(size: 17, weight: .semibold, design: .rounded))
            TextField("Session name", text: $renameDraft)
                .textFieldStyle(.roundedBorder)
            HStack {
                Spacer()
                Button("Cancel") { renamingSession = nil }
                Button("Rename") {
                    Task {
                        if await store.renameSession(id: session.id, label: renameDraft) {
                            renamingSession = nil
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(renameDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(24)
        .frame(width: 380)
    }

    private func destructiveAlert(_ action: DestructiveAction) -> Alert {
        switch action {
        case .archive(let session):
            Alert(
                title: Text("Archive “\(session.label)”?"),
                message: Text("The session leaves this workspace but remains recoverable from other Sylvander clients."),
                primaryButton: .destructive(Text("Archive")) {
                    Task { await store.archiveSession(id: session.id) }
                },
                secondaryButton: .cancel()
            )
        case .delete(let session):
            Alert(
                title: Text("Delete “\(session.label)” permanently?"),
                message: Text("Conversation history and session metadata will be removed. This cannot be undone."),
                primaryButton: .destructive(Text("Delete Permanently")) {
                    Task { await store.deleteSession(id: session.id) }
                },
                secondaryButton: .cancel()
            )
        }
    }

    private enum DestructiveAction: Identifiable {
        case archive(SylvanderSession)
        case delete(SylvanderSession)

        var id: String {
            switch self {
            case .archive(let session): "archive-\(session.id)"
            case .delete(let session): "delete-\(session.id)"
            }
        }
    }
}

private struct SylvanderSessionComposer: View {
    @ObservedObject var store: SylvanderSessionStore
    @Binding var isPresented: Bool
    @State private var agents: [SylvanderAgent] = []
    @State private var selectedAgentID = ""
    @State private var label = ""
    @State private var workspace: String?
    @State private var loadingAgents = true

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Start a focused session")
                    .font(.system(size: 20, weight: .semibold, design: .rounded))
                Text("Name the work. Sylvander keeps its terminal and context ready while you switch away.")
                    .font(.system(size: 12, design: .rounded))
                    .foregroundStyle(SylvanderWorkspacePalette.dim)
            }

            VStack(alignment: .leading, spacing: 8) {
                Text("SESSION NAME").composerLabel()
                TextField("e.g. Review authentication flow", text: $label)
                    .textFieldStyle(.roundedBorder)
            }

            if agents.count > 1 {
                VStack(alignment: .leading, spacing: 8) {
                    Text("AGENT").composerLabel()
                    Picker("", selection: $selectedAgentID) {
                        ForEach(agents) { agent in Text(agent.name).tag(agent.id) }
                    }
                    .labelsHidden()
                }
            }

            VStack(alignment: .leading, spacing: 8) {
                Text("WORKSPACE · OPTIONAL").composerLabel()
                HStack {
                    Text(workspace ?? inheritedWorkspaceText)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(workspace == nil ? SylvanderWorkspacePalette.muted : SylvanderWorkspacePalette.text)
                        .lineLimit(1)
                    Spacer()
                    Button("Choose Folder…", action: chooseWorkspace)
                }
            }

            if let error = store.operationError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.system(size: 11, design: .rounded))
                    .foregroundStyle(SylvanderWorkspacePalette.signal)
            }

            HStack {
                Spacer()
                Button("Cancel") { isPresented = false }
                Button("Create Session") {
                    Task {
                        if await store.createSession(label: label, agentID: selectedAgentID, workspace: workspace) {
                            isPresented = false
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(!canCreate)
            }
        }
        .padding(28)
        .frame(width: 520)
        .background(SylvanderWorkspacePalette.panel)
        .task {
            store.clearOperationError()
            agents = await store.fetchAgents()
            selectedAgentID = agents.first?.id ?? ""
            loadingAgents = false
        }
    }

    private var canCreate: Bool {
        !loadingAgents && !selectedAgentID.isEmpty &&
            !label.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty &&
            store.operationState == .idle
    }

    private var inheritedWorkspaceText: String {
        agents.first(where: { $0.id == selectedAgentID })?.agentWorkspace?.path ?? "Use the Agent default"
    }

    private func chooseWorkspace() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Use Workspace"
        if panel.runModal() == .OK { workspace = panel.url?.path }
    }
}

private extension View {
    func composerLabel() -> some View {
        font(.system(size: 9, weight: .bold, design: .monospaced))
            .tracking(1)
            .foregroundStyle(SylvanderWorkspacePalette.muted)
    }
}
#endif
