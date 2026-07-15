#if os(macOS)
import Foundation
import SwiftUI

struct SylvanderChangesInspector: View {
    let workspace: String
    let close: () -> Void

    @State private var state: LoadingState = .loading

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            Divider().overlay(SylvanderWorkspacePalette.rule)
            content
        }
        .frame(minWidth: 340, idealWidth: 400, maxWidth: 480)
        .background(SylvanderWorkspacePalette.panel)
        .overlay(alignment: .leading) {
            Rectangle().fill(SylvanderWorkspacePalette.rule).frame(width: 1)
        }
        .task(id: workspace) {
            let result = await Task.detached(priority: .userInitiated) {
                LoadingState.loaded(ChangesSnapshot.load(workspace: workspace))
            }.value
            guard !Task.isCancelled else { return }
            state = result
        }
    }

    private var header: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 4) {
                Text("CHANGED FILES")
                    .font(.system(size: 10, weight: .bold, design: .monospaced))
                    .tracking(1.1)
                    .foregroundStyle(SylvanderWorkspacePalette.text)
                Text(URL(fileURLWithPath: workspace).lastPathComponent)
                    .font(.system(size: 9, weight: .medium, design: .monospaced))
                    .foregroundStyle(SylvanderWorkspacePalette.muted)
                    .lineLimit(1)
            }
            Spacer()
            Button(action: close) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .frame(width: 26, height: 26)
            }
            .buttonStyle(.plain)
            .foregroundStyle(SylvanderWorkspacePalette.dim)
            .help("Close changed files")
        }
        .padding(.horizontal, 16)
        .frame(height: 68)
    }

    @ViewBuilder
    private var content: some View {
        switch state {
        case .loading:
            VStack(spacing: 12) {
                ProgressView().controlSize(.small)
                Text("Reading workspace changes…")
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(SylvanderWorkspacePalette.muted)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

        case .loaded(let snapshot):
            if let message = snapshot.message {
                VStack(alignment: .leading, spacing: 10) {
                    Text(snapshot.isClean ? "NO UNCOMMITTED CHANGES" : "CHANGES UNAVAILABLE")
                        .font(.system(size: 10, weight: .bold, design: .monospaced))
                        .tracking(0.8)
                        .foregroundStyle(SylvanderWorkspacePalette.warm)
                    Text(message)
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(SylvanderWorkspacePalette.dim)
                }
                .padding(22)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            } else {
                diffContent(snapshot)
            }
        }
    }

    private func diffContent(_ snapshot: ChangesSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 7) {
                    ForEach(snapshot.files, id: \.self) { file in
                        Label(file, systemImage: "doc")
                            .lineLimit(1)
                            .font(.system(size: 9, weight: .medium, design: .monospaced))
                            .foregroundStyle(SylvanderWorkspacePalette.dim)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
            .overlay(alignment: .bottom) { Divider().overlay(SylvanderWorkspacePalette.rule) }

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(snapshot.diff.split(separator: "\n", omittingEmptySubsequences: false).enumerated()), id: \.offset) { _, line in
                        Text(String(line))
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(diffColor(line))
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, 16)
                            .padding(.vertical, 1)
                            .background(diffBackground(line))
                    }
                }
                .padding(.vertical, 12)
            }
        }
    }

    private func diffColor(_ line: Substring) -> Color {
        if line.hasPrefix("+") && !line.hasPrefix("+++") { return Color(red: 0.49, green: 0.78, blue: 0.55) }
        if line.hasPrefix("-") && !line.hasPrefix("---") { return Color(red: 0.93, green: 0.47, blue: 0.48) }
        if line.hasPrefix("@@") { return SylvanderWorkspacePalette.active }
        return SylvanderWorkspacePalette.dim
    }

    private func diffBackground(_ line: Substring) -> Color {
        if line.hasPrefix("+") && !line.hasPrefix("+++") { return Color.green.opacity(0.08) }
        if line.hasPrefix("-") && !line.hasPrefix("---") { return Color.red.opacity(0.08) }
        return .clear
    }

    private enum LoadingState {
        case loading
        case loaded(ChangesSnapshot)
    }
}

struct ChangesSnapshot: Sendable {
    private static let maximumDiffBytes = 128 * 1024
    private static let maximumUntrackedFileBytes = 32 * 1024

    let files: [String]
    let diff: String
    let message: String?

    var isClean: Bool { diff.isEmpty && message == "This workspace is clean." }

    static func load(workspace: String) -> ChangesSnapshot {
        let primary = runGit(
            workspace: workspace,
            arguments: ["diff", "--no-ext-diff", "--no-color", "--unified=3", "HEAD"]
        )
        var diff: String
        if let primary, primary.status == 0 {
            diff = primary.output
        } else if let staged = runGit(
            workspace: workspace,
            arguments: ["diff", "--cached", "--no-ext-diff", "--no-color", "--unified=3"]
        ), let unstaged = runGit(
            workspace: workspace,
            arguments: ["diff", "--no-ext-diff", "--no-color", "--unified=3"]
        ), staged.status == 0, unstaged.status == 0 {
            // A repository without HEAD still has independent index and worktree diffs.
            diff = staged.output + unstaged.output
        } else {
            return .init(files: [], diff: "", message: "Git could not read changes for this workspace.")
        }

        guard let status = runGit(
            workspace: workspace,
            arguments: ["status", "--porcelain=v1", "-z", "--untracked-files=all"]
        ), status.status == 0 else {
            return .init(files: [], diff: "", message: "Git could not read workspace status.")
        }

        let untracked = status.output.split(separator: "\0").compactMap { entry -> String? in
            guard entry.hasPrefix("?? ") else { return nil }
            return String(entry.dropFirst(3))
        }
        let remainingBytes = max(0, maximumDiffBytes - diff.utf8.count)
        diff += renderUntracked(untracked, workspace: workspace, byteBudget: remainingBytes)

        let trackedFiles = diff.split(separator: "\n").compactMap { line -> String? in
            guard line.hasPrefix("diff --git a/") else { return nil }
            return line.split(separator: " ").last.map { String($0.dropFirst(2)) }
        }
        return .init(
            files: Array(Set(trackedFiles + untracked)).sorted(),
            diff: diff,
            message: diff.isEmpty ? "This workspace is clean." : nil
        )
    }

    private static func renderUntracked(
        _ paths: [String],
        workspace: String,
        byteBudget: Int
    ) -> String {
        let root = URL(fileURLWithPath: workspace, isDirectory: true)
            .resolvingSymlinksInPath().standardizedFileURL
        var remaining = byteBudget
        var rendered = ""

        for path in paths.sorted() where remaining > 0 {
            let file = root.appendingPathComponent(path).resolvingSymlinksInPath().standardizedFileURL
            let header = "\ndiff --git a/\(path) b/\(path)\nnew file (untracked)\n"
            let body: String
            let values = try? file.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
            if file.pathComponents.starts(with: root.pathComponents),
               values?.isRegularFile == true,
               let size = values?.fileSize, size <= maximumUntrackedFileBytes,
               let data = try? Data(contentsOf: file, options: [.mappedIfSafe]),
               !data.contains(0), let text = String(data: data, encoding: .utf8) {
                body = text.split(separator: "\n", omittingEmptySubsequences: false)
                    .map { "+" + $0 }.joined(separator: "\n") + "\n"
            } else {
                body = "Preview omitted: binary or larger than 32 KiB.\n"
            }
            let entry = header + body
            let clipped = String(decoding: entry.utf8.prefix(remaining), as: UTF8.self)
            rendered += clipped
            remaining -= clipped.utf8.count
        }
        return rendered
    }

    private static func runGit(
        workspace: String,
        arguments: [String]
    ) -> (status: Int32, output: String)? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/git")
        process.arguments = ["-C", workspace] + arguments
        let output = Pipe()
        process.standardOutput = output
        process.standardError = output

        do {
            try process.run()
            let data = output.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            return (process.terminationStatus, String(decoding: data.prefix(128 * 1024), as: UTF8.self))
        } catch {
            return nil
        }
    }
}
#endif
