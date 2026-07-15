#if os(macOS)
import AppKit
import Darwin
import SwiftUI
import WebKit

enum SylvanderPreview: Equatable {
    case image(URL)
    case web(URL)
}

enum SylvanderPreviewPolicy {
    static let maximumImageBytes: UInt64 = 25 * 1024 * 1024
    private static let imageExtensions = Set(["gif", "jpeg", "jpg", "png", "webp"])

    static func resolve(_ request: SylvanderHostBroker.PreviewRequest) throws -> SylvanderPreview {
        switch request.kind {
        case .image:
            return .image(try imageURL(target: request.target, workspace: request.workspace))
        case .web:
            return .web(try webURL(target: request.target))
        }
    }

    static func imageURL(target: String, workspace: String) throws -> URL {
        let workspaceURL = URL(fileURLWithPath: workspace, isDirectory: true)
            .resolvingSymlinksInPath()
            .standardizedFileURL
        let candidate = target.hasPrefix("/")
            ? URL(fileURLWithPath: target)
            : workspaceURL.appendingPathComponent(target)
        let resolved = candidate.resolvingSymlinksInPath().standardizedFileURL
        guard isDescendant(resolved, of: workspaceURL) else {
            throw PolicyError.imageOutsideWorkspace
        }
        guard imageExtensions.contains(resolved.pathExtension.lowercased()) else {
            throw PolicyError.unsupportedImage
        }
        let values = try resolved.resourceValues(forKeys: [.fileSizeKey, .isRegularFileKey])
        guard values.isRegularFile == true else { throw PolicyError.imageMissing }
        guard UInt64(values.fileSize ?? 0) <= maximumImageBytes else {
            throw PolicyError.imageTooLarge
        }
        guard NSImage(contentsOf: resolved) != nil else { throw PolicyError.unsupportedImage }
        return resolved
    }

    static func webURL(target: String) throws -> URL {
        guard let components = URLComponents(string: target),
              let scheme = components.scheme?.lowercased(),
              scheme == "https" || scheme == "http",
              components.user == nil,
              components.password == nil,
              let host = components.host?.lowercased(),
              !host.isEmpty else {
            throw PolicyError.unsafeWebURL
        }
        guard scheme == "https" || isLocalHost(host), let url = components.url else {
            throw PolicyError.unsafeWebURL
        }
        return url
    }

    private static func isDescendant(_ candidate: URL, of workspace: URL) -> Bool {
        let root = workspace.pathComponents
        let child = candidate.pathComponents
        return child.count > root.count && child.prefix(root.count).elementsEqual(root)
    }

    private static func isLocalHost(_ host: String) -> Bool {
        let host = host.trimmingCharacters(in: CharacterSet(charactersIn: "[]"))
        if host == "localhost" || host.hasSuffix(".localhost") || host.hasSuffix(".local") {
            return true
        }
        var ipv4 = in_addr()
        if inet_pton(AF_INET, host, &ipv4) == 1 {
            let address = UInt32(bigEndian: ipv4.s_addr)
            return address >> 24 == 10 || address >> 24 == 127 || address >> 16 == 0xA9FE ||
                address >> 20 == 0xAC1 || address >> 16 == 0xC0A8 || address == 0
        }
        var ipv6 = in6_addr()
        if inet_pton(AF_INET6, host, &ipv6) == 1 {
            let bytes = withUnsafeBytes(of: &ipv6) { Array($0) }
            return bytes == [UInt8](repeating: 0, count: 15) + [1] ||
                bytes.first == 0xFC || bytes.first == 0xFD ||
                (bytes.first == 0xFE && (bytes[1] & 0xC0) == 0x80)
        }
        return false
    }

    enum PolicyError: LocalizedError, Equatable {
        case imageOutsideWorkspace
        case imageMissing
        case imageTooLarge
        case unsupportedImage
        case unsafeWebURL

        var errorDescription: String? {
            switch self {
            case .imageOutsideWorkspace: "Image preview is limited to the session workspace"
            case .imageMissing: "Image does not exist or is not a regular file"
            case .imageTooLarge: "Image exceeds the 25 MiB preview limit"
            case .unsupportedImage: "Image format is not supported"
            case .unsafeWebURL: "Use HTTPS, or HTTP for a local development address, without credentials"
            }
        }
    }
}

struct SylvanderPreviewInspector: View {
    let preview: SylvanderPreview
    let close: () -> Void

    @State private var webState: WebPreviewState = .loading
    @State private var reloadID = UUID()
    @State private var showsQuickLook = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Text("\(kindLabel) PREVIEW")
                    .font(.system(size: 10, weight: .bold, design: .monospaced))
                    .tracking(1.2)
                    .foregroundStyle(SylvanderWorkspacePalette.text)
                Text(locationLabel)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(Color(red: 0.400, green: 0.424, blue: 0.447))
                    .lineLimit(1)
                Spacer()
                Button(action: close) {
                    Image(systemName: "xmark")
                        .font(.system(size: 10, weight: .semibold))
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color(red: 0.596, green: 0.608, blue: 0.616))
            }
            .padding(.horizontal, 16)
            .frame(height: 68)
            .overlay(alignment: .bottom) { rule }

            switch preview {
            case .image(let url):
                imagePreview(url)
            case .web(let url):
                webPreview(url)
            }
        }
        .frame(minWidth: 340, idealWidth: 400, maxWidth: 480)
        .background(SylvanderWorkspacePalette.panel)
        .overlay(alignment: .leading) { rule.frame(width: 1) }
    }

    private var rule: some View {
        Rectangle().fill(SylvanderWorkspacePalette.rule).frame(height: 1)
    }

    private var kindLabel: String {
        switch preview {
        case .image: "IMAGE"
        case .web: "WEB"
        }
    }

    private var locationLabel: String {
        switch preview {
        case .image(let url): url.lastPathComponent
        case .web(let url): url.host ?? url.absoluteString
        }
    }

    @ViewBuilder
    private func imagePreview(_ url: URL) -> some View {
        if let image = NSImage(contentsOf: url) {
            ZStack {
                checkerboard
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .scaledToFit()
                    .padding(24)
            }
        } else {
            previewMessage(
                title: "IMAGE UNAVAILABLE",
                detail: "The file was moved, removed, or can no longer be decoded."
            )
        }
    }

    private func webPreview(_ url: URL) -> some View {
        Group {
            if showsQuickLook {
                ZStack {
                    RestrictedWebPreview(url: url, state: $webState)
                        .id(reloadID)

                    switch webState {
                    case .loading:
                        VStack(spacing: 12) {
                            ProgressView().controlSize(.small)
                            Text("Loading restricted Quick Look…")
                                .font(.system(size: 10, design: .monospaced))
                                .foregroundStyle(SylvanderWorkspacePalette.muted)
                        }
                    case .loaded:
                        EmptyView()
                    case .failed(let message):
                        previewMessage(title: "QUICK LOOK UNAVAILABLE", detail: message) {
                            HStack(spacing: 16) {
                                inspectorAction("RETRY") {
                                    webState = .loading
                                    reloadID = UUID()
                                }
                                inspectorAction("OPEN IN BROWSER") { NSWorkspace.shared.open(url) }
                            }
                        }
                    }
                }
            } else {
                VStack(alignment: .leading, spacing: 16) {
                    Image(systemName: "globe")
                        .font(.system(size: 24, weight: .light))
                        .foregroundStyle(SylvanderWorkspacePalette.active)
                    Text(url.absoluteString)
                        .font(.system(size: 11, weight: .medium, design: .monospaced))
                        .foregroundStyle(SylvanderWorkspacePalette.text)
                        .textSelection(.enabled)
                    Text("Use your browser for full JavaScript, sign-in, localhost, and developer tools. Quick Look is isolated and disables JavaScript.")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(SylvanderWorkspacePalette.dim)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 18) {
                        inspectorAction("OPEN IN BROWSER") { NSWorkspace.shared.open(url) }
                        inspectorAction("QUICK LOOK") {
                            webState = .loading
                            showsQuickLook = true
                        }
                    }
                }
                .padding(22)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            }
        }
    }

    private func inspectorAction(_ title: String, action: @escaping () -> Void) -> some View {
        Button(title, action: action)
            .buttonStyle(.plain)
            .font(.system(size: 9, weight: .bold, design: .monospaced))
            .foregroundStyle(SylvanderWorkspacePalette.active)
    }

    private func previewMessage<Actions: View>(
        title: String,
        detail: String,
        @ViewBuilder actions: () -> Actions
    ) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(title)
                .font(.system(size: 10, weight: .bold, design: .monospaced))
                .tracking(0.8)
                .foregroundStyle(SylvanderWorkspacePalette.warm)
            Text(detail)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(SylvanderWorkspacePalette.dim)
            actions()
        }
        .padding(22)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(SylvanderWorkspacePalette.panel)
    }

    private func previewMessage(title: String, detail: String) -> some View {
        previewMessage(title: title, detail: detail) { EmptyView() }
    }

    private var checkerboard: some View {
        Canvas { context, size in
            let cell: CGFloat = 12
            for row in 0...Int(size.height / cell) {
                for column in 0...Int(size.width / cell) where (row + column).isMultiple(of: 2) {
                    context.fill(
                        Path(CGRect(x: CGFloat(column) * cell, y: CGFloat(row) * cell, width: cell, height: cell)),
                        with: .color(Color.white.opacity(0.025))
                    )
                }
            }
        }
        .background(SylvanderWorkspacePalette.canvas)
    }
}

private enum WebPreviewState: Equatable {
    case loading
    case loaded
    case failed(String)
}

private struct RestrictedWebPreview: NSViewRepresentable {
    let url: URL
    @Binding var state: WebPreviewState

    func makeCoordinator() -> Coordinator { Coordinator(origin: url, state: $state) }

    func makeNSView(context: Context) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        let view = WKWebView(frame: .zero, configuration: configuration)
        view.navigationDelegate = context.coordinator
        view.setValue(false, forKey: "drawsBackground")
        state = .loading
        view.load(URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 15))
        return view
    }

    func updateNSView(_ view: WKWebView, context: Context) {
        guard view.url != url else { return }
        context.coordinator.origin = url
        state = .loading
        view.load(URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData, timeoutInterval: 15))
    }

    final class Coordinator: NSObject, WKNavigationDelegate {
        var origin: URL
        private var state: Binding<WebPreviewState>

        init(origin: URL, state: Binding<WebPreviewState>) {
            self.origin = origin
            self.state = state
        }

        func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
            state.wrappedValue = .loaded
        }

        func webView(
            _ webView: WKWebView,
            didFail navigation: WKNavigation!,
            withError error: Error
        ) {
            state.wrappedValue = .failed(error.localizedDescription)
        }

        func webView(
            _ webView: WKWebView,
            didFailProvisionalNavigation navigation: WKNavigation!,
            withError error: Error
        ) {
            state.wrappedValue = .failed(error.localizedDescription)
        }

        func webView(
            _ webView: WKWebView,
            decidePolicyFor navigationAction: WKNavigationAction,
            decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
        ) {
            guard let requested = navigationAction.request.url,
                  requested.scheme == "https",
                  requested.host?.caseInsensitiveCompare(origin.host ?? "") == .orderedSame else {
                state.wrappedValue = .failed("Navigation outside the original secure site was blocked.")
                decisionHandler(.cancel)
                return
            }
            decisionHandler(.allow)
        }
    }
}
#endif
